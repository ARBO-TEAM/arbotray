//! An on-demand throughput test, over WinHttp.
//!
//! WinHttp rather than a crate, and rather than spawning the vendor CLI: it is
//! part of Windows, it is already linked through the `windows` crate this app
//! builds on, and it brings proxy configuration, TLS and timeouts with it. A
//! subprocess would be a second thing to install and a second thing to fail.
//!
//! The test is deliberately three short phases — a round trip, a download and
//! an upload — against one public endpoint. What it measures is HTTP
//! throughput on a single connection, which is *not* the same number a
//! multi-stream test reports: a single stream is limited by the window size and
//! the slowest hop in a way eight parallel streams are not. It is an honest
//! reading of the connection this app is monitoring, and the page says so
//! rather than implying it is a line-rate figure.
//!
//! Everything runs on its own thread. The poll loop must never wait on the
//! network, and the UI must stay responsive while a transfer is in flight, so
//! the phase and the running byte count are published as they happen and the
//! page reads them.

use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use windows::Win32::Networking::WinHttp::{
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WinHttpCloseHandle, WinHttpConnect,
    WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts, WinHttpWriteData,
};
use windows::core::{HSTRING, PCWSTR, w};

/// Where the test runs. Any host serving a plain HTTP byte stream would do; the
/// size is a query parameter so the request is one URL rather than a protocol.
///
/// The vendor is named in the page's own note as well as here, because pressing
/// Run sends traffic to a third party and a user is entitled to know which one.
const HOST: &str = "speed.cloudflare.com";
const DOWN_URL: &str = "/__down";
const UP_URL: &str = "/__up";

/// How much to ask for. The download endpoint caps its own answer, so this is a
/// request rather than a guarantee — the loop stops on whichever comes first.
const DOWN_BYTES: u64 = 25_000_000;
const UP_BYTES: usize = 10_000_000;

/// Bytes moved per `WinHttpWriteData` call. Large enough that the call overhead
/// disappears, small enough that progress moves several times a second.
const UP_CHUNK: usize = 256 * 1024;

/// The first slice of a transfer is excluded from the reported rate: TCP slow
/// start means the opening second is a fraction of the connection's capacity,
/// and averaging it in reports a good link as a mediocre one. Standard practice
/// for throughput tools, and the reason the number is taken at the *end* rather
/// than over the whole transfer.
const RAMP_SECS: f64 = 1.5;

/// A download that has not moved this many bytes by `RAMP_SECS` has no steady
/// state to measure — the link is slower than the ramp, so the whole transfer
/// is the reading.
const MIN_STEADY_BYTES: u64 = 64 * 1024;

/// Timeouts, in milliseconds. A test against a dead network has to end rather
/// than hang the page's Run button forever.
const RESOLVE_MS: i32 = 4_000;
const CONNECT_MS: i32 = 6_000;
const SEND_MS: i32 = 20_000;
const RECEIVE_MS: i32 = 30_000;

/// How many completed tests are kept. A short run so the newest results fit on
/// one page; older ones are a different question than "how is my link now".
pub const HISTORY_MAX: usize = 10;

/// Which phase a run is in. Carried rather than inferred from the numbers, so
/// the page can say what is happening during the seconds where nothing has been
/// measured yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Idle,
    Latency,
    Download,
    Upload,
    Done,
    Failed,
}

impl Phase {
    pub fn label(self) -> &'static str {
        match self {
            Phase::Idle => "",
            Phase::Latency => "Measuring round trip",
            Phase::Download => "Downloading",
            Phase::Upload => "Uploading",
            Phase::Done => "Complete",
            Phase::Failed => "Failed",
        }
    }

}

/// One completed measurement.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpeedResult {
    /// Bytes per second.
    pub down_bps: u64,
    pub up_bps: u64,
    /// Round trip to the test server, in milliseconds.
    pub latency_ms: u32,
}

/// What the page draws: the live run, or the last one to finish.
///
/// A clone per poll rather than a lock held across the paint: the worker thread
/// writes this many times a second, and holding it while painting would stall
/// the transfer it is reporting on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpeedStatus {
    pub phase: Phase,
    pub percent: u32,
    /// Live during the transfer, final once it completes.
    pub down_bps: Option<u64>,
    pub up_bps: Option<u64>,
    pub latency_ms: Option<u32>,
    /// Why the last run stopped, when it stopped early.
    pub error: Option<String>,
    pub history: Vec<SpeedResult>,
    /// The moment the last run finished, as a wall-clock string, so a result
    /// from this morning is not mistaken for a live reading.
    pub finished_at: Option<String>,
}

impl SpeedStatus {
    pub fn running(&self) -> bool {
        matches!(self.phase, Phase::Latency | Phase::Download | Phase::Upload)
    }
}

/// The shared state between the worker thread and the page.
#[derive(Debug, Default)]
struct Inner {
    status: SpeedStatus,
    running: bool,
}

/// Starts runs, and answers what the current one is doing.
///
/// `Clone` is an `Arc` clone, not a copy of the state: the page holds one while
/// the worker thread holds the other, and both see the same run.
#[derive(Clone, Default)]
pub struct SpeedTest {
    inner: Arc<Mutex<Inner>>,
}

impl SpeedTest {
    pub fn new() -> Self {
        Self::default()
    }

    /// Start a run, unless one is already going.
    ///
    /// Returns whether this call started one, so the caller can say "already
    /// running" rather than appearing to do nothing.
    pub fn start(&self) -> bool {
        {
            // A poisoned lock is recovered from rather than propagated: the
            // worker holds it only for the assignments below and cannot panic
            // inside one, and refusing here would leave the page unable to
            // start a test for the rest of the session.
            let mut guard = self.lock();
            if guard.running {
                return false;
            }
            guard.running = true;
            guard.status = SpeedStatus {
                phase: Phase::Latency,
                history: std::mem::take(&mut guard.status.history),
                ..Default::default()
            };
        }

        let inner = Arc::clone(&self.inner);
        // Detached on purpose: the run is fire-and-forget, and the page reads
        // its state rather than waiting on a join. A failed spawn leaves the
        // flag set, which would wedge the button — so it is cleared here.
        if let Err(e) = std::thread::Builder::new()
            .name("speedtest".into())
            .spawn(move || run(&inner))
        {
            let mut guard = self.lock();
            guard.running = false;
            guard.status.phase = Phase::Failed;
            guard.status.error = Some(format!("cannot start thread: {e}"));
            return false;
        }
        true
    }

    /// The current state, for the model.
    pub fn snapshot(&self) -> SpeedStatus {
        self.lock().status.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// Publish a partial update.
fn publish(inner: &Arc<Mutex<Inner>>, edit: impl FnOnce(&mut SpeedStatus)) {
    let mut guard = match inner.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    edit(&mut guard.status);
}

/// The whole run, on the worker thread.
fn run(inner: &Arc<Mutex<Inner>>) {
    let outcome = measure(inner);
    let mut guard = match inner.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard.running = false;
    match outcome {
        Ok(result) => {
            guard.status.phase = Phase::Done;
            guard.status.percent = 100;
            guard.status.down_bps = Some(result.down_bps);
            guard.status.up_bps = Some(result.up_bps);
            guard.status.latency_ms = Some(result.latency_ms);
            guard.status.error = None;
            guard.status.finished_at = Some(clock_now());
            // Newest first: the page reads top-down, and the run that just
            // finished is the one being looked for.
            guard.status.history.insert(0, result);
            guard.status.history.truncate(HISTORY_MAX);
        }
        Err(e) => {
            guard.status.phase = Phase::Failed;
            guard.status.error = Some(e);
        }
    }
}

/// The three phases in order.
fn measure(inner: &Arc<Mutex<Inner>>) -> Result<SpeedResult, String> {
    let latency_ms = latency(inner)?;
    publish(inner, |s| {
        s.phase = Phase::Download;
        s.percent = 8;
        s.latency_ms = Some(latency_ms);
    });

    let down_bps = download(inner)?;
    publish(inner, |s| {
        s.phase = Phase::Upload;
        s.percent = 55;
        s.down_bps = Some(down_bps);
    });

    let up_bps = upload(inner)?;
    Ok(SpeedResult {
        down_bps,
        up_bps,
        latency_ms,
    })
}

/// One open request, torn down in the order WinHttp requires.
///
/// A struct with a `Drop` rather than a `close()` call at the end of each
/// phase: every phase has several early-return paths, and a handle leaked on
/// one of them is a connection that stays open until the process exits — which
/// matters here precisely because these run repeatedly.
struct Request {
    session: *mut c_void,
    connect: *mut c_void,
    request: *mut c_void,
}

impl Drop for Request {
    fn drop(&mut self) {
        // SAFETY: each handle was created by the matching open call below and is
        // closed exactly once, in the documented order — request, then
        // connection, then session. A null handle is not passed: the fields are
        // only ever set from a successful call.
        unsafe {
            let _ = WinHttpCloseHandle(self.request);
            let _ = WinHttpCloseHandle(self.connect);
            let _ = WinHttpCloseHandle(self.session);
        }
    }
}

/// Open a request and hand it back ready to send.
///
/// `verb` is `"GET"` or `"POST"`; `total` is the body length WinHttp should
/// expect, or 0 for a request with none.
///
/// Each failure path closes what it had already opened by hand, because the
/// `Request` that would own them does not exist until all three calls have
/// succeeded. Leaking here would leave a connection open until the process
/// exits, and this function runs again on every press of Run.
fn open(verb: PCWSTR, path: &str, total: u32) -> Result<Request, String> {
    let agent = HSTRING::from("ArboTray");
    let host = HSTRING::from(HOST);
    let wide_path = HSTRING::from(path);

    // SAFETY: every handle is checked before the next call uses it, and all
    // three `HSTRING`s outlive every call that borrows one.
    unsafe {
        // A session per request rather than one reused: a measurement must not
        // inherit a connection the previous phase left open, or the ramp
        // exclusion would be excluding a transfer that had already happened.
        let session = WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, None, None, 0);
        if session.is_null() {
            return Err("WinHttpOpen failed".into());
        }

        let connect = WinHttpConnect(session, &host, 443, 0);
        if connect.is_null() {
            let _ = WinHttpCloseHandle(session);
            return Err(format!("cannot reach {HOST}"));
        }

        let request = WinHttpOpenRequest(
            connect,
            verb,
            &wide_path,
            None,
            None,
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if request.is_null() {
            let _ = WinHttpCloseHandle(connect);
            let _ = WinHttpCloseHandle(session);
            return Err("cannot open request".into());
        }

        // A test against a dead network has to end rather than hang the page's
        // Run button forever.
        let _ = WinHttpSetTimeouts(session, RESOLVE_MS, CONNECT_MS, SEND_MS, RECEIVE_MS);

        // `total` is the length of the body that follows. For the upload it is
        // the full size, sent here with an empty body and then written in
        // chunks — which is what lets the page show progress through it.
        if let Err(e) = WinHttpSendRequest(request, None, None, 0, total, 0) {
            let _ = WinHttpCloseHandle(request);
            let _ = WinHttpCloseHandle(connect);
            let _ = WinHttpCloseHandle(session);
            return Err(format!("send failed: {e}"));
        }

        Ok(Request {
            session,
            connect,
            request,
        })
    }
}

/// The wall clock, as `HH:MM:SS`, for stamping a completed run.
///
/// Local time rather than UTC: the page is read by the person sitting at the
/// machine, and "when was this measured" is a question about their afternoon.
///
/// Deliberately not a date. The history lives in memory for the length of the
/// session, so a date would only ever repeat today's.
fn clock_now() -> String {
    // SAFETY: `GetLocalTime` fills a struct we own and cannot fail.
    let t: windows::Win32::Foundation::SYSTEMTIME = unsafe {
        windows::Win32::System::SystemInformation::GetLocalTime()
    };
    format!("{:02}:{:02}:{:02}", t.wHour, t.wMinute, t.wSecond)
}

/// Round trip to the test server, taking the best of a few attempts.
///
/// The *minimum* rather than the mean: one attempt that waits on a scheduler
/// slice or a retransmit is not a reading of the link, and averaging it in
/// makes a good connection look jittery. The same reason the download excludes
/// its own ramp.
///
/// This is a request/response time over TLS, not an ICMP echo — it is what the
/// test server costs to talk to, which is the number that belongs beside the
/// throughput figures measured against that same server.
fn latency(inner: &Arc<Mutex<Inner>>) -> Result<u32, String> {
    let mut best: Option<u32> = None;
    for i in 0..3 {
        let req = open(w!("GET"), "/__down?bytes=0", 0)?;
        let start = Instant::now();
        // SAFETY: `req.request` is a live handle from `open`.
        let received = unsafe { WinHttpReceiveResponse(req.request, std::ptr::null_mut()) };
        if let Err(e) = received {
            return Err(format!("no response: {e}"));
        }
        let ms = start.elapsed().as_millis().min(u128::from(u32::MAX)) as u32;
        best = Some(best.map_or(ms, |b: u32| b.min(ms)));
        publish(inner, |s| s.percent = 2 + (i + 1) * 2);
    }
    best.ok_or_else(|| "no round trip measured".into())
}

/// Download and time it.
fn download(inner: &Arc<Mutex<Inner>>) -> Result<u64, String> {
    let path = format!("{DOWN_URL}?bytes={DOWN_BYTES}");
    let req = open(w!("GET"), &path, 0)?;
    // SAFETY: a live request handle from `open`.
    unsafe {
        WinHttpReceiveResponse(req.request, std::ptr::null_mut())
            .map_err(|e| format!("download refused: {e}"))?;
    }

    let start = Instant::now();
    let mut total = 0u64;
    // The byte count and elapsed time at the moment the ramp ended. Until both
    // are set, the rate is measured over the whole transfer.
    let mut base: Option<(u64, f64)> = None;
    let mut buf = vec![0u8; 64 * 1024];

    loop {
        let mut available = 0u32;
        // SAFETY: a live request handle and a live out-parameter.
        let asked = unsafe { WinHttpQueryDataAvailable(req.request, &mut available) };
        if let Err(e) = asked {
            return Err(format!("read stalled: {e}"));
        }
        if available == 0 {
            break;
        }
        let want = available.min(buf.len() as u32);
        let mut read = 0u32;
        // SAFETY: the buffer is `want` bytes wide and `read` is written by the
        // call with how many actually landed.
        let got = unsafe {
            WinHttpReadData(
                req.request,
                buf.as_mut_ptr() as *mut c_void,
                want,
                &mut read,
            )
        };
        if let Err(e) = got {
            return Err(format!("read failed: {e}"));
        }
        if read == 0 {
            break;
        }
        total += u64::from(read);

        let elapsed = start.elapsed().as_secs_f64();
        if base.is_none() && elapsed >= RAMP_SECS && total >= MIN_STEADY_BYTES {
            // Everything up to here is slow start; ignore exactly it.
            base = Some((total, elapsed));
        }
        let rate = steady_rate(total, elapsed, base);
        if let Some(rate) = rate {
            // 8% to 55%: the latency phase owns the first stretch, the upload
            // the last.
            let pct = 8 + ((total.min(DOWN_BYTES) * 47) / DOWN_BYTES) as u32;
            publish(inner, |s| {
                s.percent = pct.min(55);
                s.down_bps = Some(rate);
            });
        }
    }

    if total == 0 {
        return Err("the download returned nothing".into());
    }
    steady_rate(total, start.elapsed().as_secs_f64(), base)
        .map(|r| r.max(1))
        .ok_or_else(|| "the download was too short to time".into())
}

/// Upload in chunks, so progress has something to report.
///
/// A single `WinHttpSendRequest` with a ten-megabyte body would measure the
/// same thing and show nothing at all until it finished — the page would sit on
/// one number for however long the link takes. Writing it in pieces costs a
/// handful of calls and makes the phase observable.
fn upload(inner: &Arc<Mutex<Inner>>) -> Result<u64, String> {
    let req = open(w!("POST"), UP_URL, UP_BYTES as u32)?;
    let chunk = vec![0u8; UP_CHUNK];
    let start = Instant::now();
    let mut written_total = 0usize;

    while written_total < UP_BYTES {
        let want = UP_CHUNK.min(UP_BYTES - written_total);
        let mut written = 0u32;
        // SAFETY: the chunk is `want` bytes wide and `written` is live.
        let put = unsafe {
            WinHttpWriteData(
                req.request,
                Some(chunk.as_ptr() as *const c_void),
                want as u32,
                &mut written,
            )
        };
        if let Err(e) = put {
            return Err(format!("upload failed: {e}"));
        }
        if written == 0 {
            break;
        }
        written_total += written as usize;
        let pct = 55 + ((written_total * 44) / UP_BYTES) as u32;
        publish(inner, |s| s.percent = pct.min(99));
    }

    // The response completes the exchange. Its body is not read: what is being
    // measured is the time to push the bytes, and the server's answer is the
    // acknowledgement that they arrived.
    // SAFETY: a live request handle.
    unsafe {
        WinHttpReceiveResponse(req.request, std::ptr::null_mut())
            .map_err(|e| format!("upload refused: {e}"))?;
    }

    let elapsed = start.elapsed().as_secs_f64();
    if written_total == 0 || elapsed <= 0.0 {
        return Err("the upload sent nothing".into());
    }
    Ok(((written_total as f64 / elapsed) as u64).max(1))
}

/// Bytes per second over the steady part of a transfer, or `None` while the
/// measurement would still be dominated by slow start.
///
/// Once the base is set this is `(bytes since the ramp) / (time since the
/// ramp)`. Before it is set, the whole transfer is used — which is the right
/// answer for a transfer short enough to have no steady state at all.
fn steady_rate(total: u64, elapsed: f64, base: Option<(u64, f64)>) -> Option<u64> {
    let (bytes, secs) = match base {
        Some((at_bytes, at_elapsed)) => (
            total.saturating_sub(at_bytes),
            elapsed - at_elapsed,
        ),
        None => (total, elapsed),
    };
    if secs <= 0.0 || bytes == 0 {
        return None;
    }
    Some((bytes as f64 / secs) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ramp_is_excluded_from_the_rate() {
        // 2 MB in the first 2 s (slow start), then 10 MB in the next 1 s.
        // Measuring the whole thing gives 6 MB/s; measuring from the ramp
        // gives the 10 MB/s the link was actually doing.
        let rate = steady_rate(12_000_000, 3.0, Some((2_000_000, 2.0))).unwrap();
        assert_eq!(rate, 10_000_000);
        let naive = steady_rate(12_000_000, 3.0, None).unwrap();
        assert_eq!(naive, 4_000_000);
        assert!(rate > naive, "the ramp must not drag the reading down");
    }

    #[test]
    fn a_transfer_shorter_than_the_ramp_is_measured_whole() {
        // No base means the whole transfer is the reading, which is correct
        // when there was never a steady state to find.
        assert_eq!(steady_rate(500, 0.5, None), Some(1000));
    }

    #[test]
    fn a_rate_with_no_time_or_no_bytes_is_not_a_number() {
        assert_eq!(steady_rate(0, 0.0, None), None);
        assert_eq!(steady_rate(100, 0.0, None), None);
        assert_eq!(steady_rate(100, 1.0, Some((100, 1.0))), None);
        // A clock that appears to go backwards must not produce a huge rate.
        assert_eq!(steady_rate(100, 1.0, Some((50, 2.0))), None);
    }

    #[test]
    fn a_second_run_is_refused_while_one_is_in_flight() {
        let test = SpeedTest::new();
        {
            let mut guard = test.lock();
            guard.running = true;
        }
        assert!(!test.start(), "a running test must not be started again");
        {
            let mut guard = test.lock();
            guard.running = false;
        }
        // Not started here for real: that would put a request on the wire from
        // a unit test. The flag is the whole contract this asserts.
    }

    /// Only the three measuring phases are "running", so the page never offers
    /// to start a second test over one that has finished — and never refuses to
    /// start one because a *previous* run is still marked busy.
    #[test]
    fn only_a_measuring_phase_counts_as_running() {
        for phase in [Phase::Latency, Phase::Download, Phase::Upload] {
            assert!(
                SpeedStatus { phase, ..Default::default() }.running(),
                "{phase:?} must keep the Run button disabled"
            );
        }
        for phase in [Phase::Idle, Phase::Done, Phase::Failed] {
            assert!(
                !SpeedStatus { phase, ..Default::default() }.running(),
                "{phase:?} must leave the Run button live"
            );
        }
    }

    #[test]
    fn every_phase_has_a_label_except_idle() {
        for phase in [
            Phase::Latency,
            Phase::Download,
            Phase::Upload,
            Phase::Done,
            Phase::Failed,
        ] {
            assert!(!phase.label().is_empty(), "{phase:?} has no label");
        }
        assert_eq!(Phase::Idle.label(), "");
    }

    /// The endpoint is a live one and the whole path works end to end.
    ///
    /// Ignored by default: it puts up to 35 MB on the wire and needs a working
    /// internet connection, neither of which belongs in a normal test run. It
    /// exists because the WinHttp call sequence cannot be checked any other way,
    /// and a wrong handle order fails here in a second rather than in the field.
    ///
    /// Run with `cargo test -- --ignored the_endpoint`.
    #[test]
    #[ignore = "needs the network and moves ~35 MB"]
    fn the_endpoint_answers_and_the_run_completes() {
        let test = SpeedTest::new();
        assert!(test.start(), "the first run must start");
        for _ in 0..600 {
            let s = test.snapshot();
            if !s.running() {
                assert_eq!(s.phase, Phase::Done, "run failed: {:?}", s.error);
                assert!(s.down_bps.unwrap_or(0) > 0, "no download rate");
                assert!(s.up_bps.unwrap_or(0) > 0, "no upload rate");
                assert_eq!(s.history.len(), 1, "the run was not recorded");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
        panic!("the run did not finish within five minutes");
    }
}

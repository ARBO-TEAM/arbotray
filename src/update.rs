//! Whether a newer release exists, asked once per launch.
//!
//! One `GET` against the releases API on a detached thread, and the answer kept
//! in a static for whoever paints it. Deliberately **not** on the telemetry
//! thread: it is a network call with a multi-second ceiling, and putting it on
//! the tick path would stall the taskbar readout behind a request that only
//! matters once a week.
//!
//! The whole feature is opt-out by being silent: no update, no unreachable
//! network and no rate limit all look the same from here — nothing is shown.
//! There is no "could not check" line, because a monitor that reports its own
//! failure to phone home is worse than one that does not phone home.
//!
//! # Why this is not in `telemetry`
//!
//! Nothing else here reaches a host the user did not ask it to. The speed test
//! is a button; this fires on its own. So the request is built to be the
//! smallest one that answers the question — no query string, no body, no
//! cookies, one path, and the answer never written anywhere but memory. A user
//! who does not want it can turn it off, which is what `update_check` is for.
//!
//! # Ceiling
//!
//! The version is compared as three numbers, so a tag with a suffix
//! (`v0.8.0-rc1`) compares as `0.8.0` and a release whose version does not
//! parse is ignored rather than reported. That is the correct direction to be
//! wrong in: a suffix nobody ships is not worth an upgrade prompt, and a
//! misparsed release reported as newer would nag every user on every launch.

use std::sync::OnceLock;

/// The repository's latest-release endpoint. One path, no query string.
const API: &str = "/repos/ARBO-TEAM/arbotray/releases/latest";
const HOST: &str = "api.github.com";

/// Where a user goes to get it. Shown beside the version, because "an update
/// exists" without "where" is a notification rather than a next step.
pub const DOWNLOADS: &str = "github.com/ARBO-TEAM/arbotray/releases";

/// GitHub refuses a request with no `User-Agent`, and the version identifies
/// which build asked. Sent as a header rather than as a WinHttp agent string,
/// because the agent is what a proxy sees and the API is what reads this.
fn agent() -> String {
    format!("ArboTray/{}", current())
}

/// This build's version, as `Cargo.toml` declares it.
pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The newer version found, or `None`.
///
/// A `OnceLock` rather than a `Mutex<Option<..>>`: the value is written exactly
/// once by the checking thread and never changed, so the lock would be there to
/// protect a write that cannot race with itself.
static FOUND: OnceLock<Option<String>> = OnceLock::new();

/// Start the check, once per process.
///
/// Called from `app::run` before the message loop. Returns immediately — the
/// request happens on its own thread — so nothing about startup waits on the
/// network, and a launch with no connectivity is indistinguishable from one
/// that found nothing new.
pub fn check_soon() {
    // Detached on purpose: nothing joins it, and the process exiting mid-request
    // simply ends it. Holding a handle to join at shutdown would make quitting
    // wait on a request the user never asked for.
    let _ = std::thread::Builder::new()
        .name("update".into())
        .spawn(|| {
            let _ = FOUND.set(latest());
        });
}

/// The newer version, if the check found one. `None` until it finishes, and
/// `None` forever if it found nothing or could not run.
pub fn available() -> Option<&'static str> {
    FOUND.get().and_then(|v| v.as_deref())
}

/// The whole check: ask, read, and decide whether the answer is newer.
fn latest() -> Option<String> {
    let body = get(API)?;
    let tag = tag_of(&body)?;
    if is_newer(&tag, current()) {
        Some(tag)
    } else {
        None
    }
}

/// The `tag_name` out of a release object.
///
/// Deliberately not a JSON parser. The response is one object of a shape this
/// API has kept for years, and the only string wanted out of it is one whose key
/// is unique in it — pulling in a dependency, or hand-rolling a parser for a
/// document with nested objects and escapes, is a great deal of machinery for
/// one field. A response whose shape has changed yields `None`, which reads as
/// "no update" and is the safe direction.
fn tag_of(body: &str) -> Option<String> {
    let rest = body.split("\"tag_name\"").nth(1)?;
    let rest = rest.split_once(':')?.1;
    let rest = rest.trim_start().strip_prefix('"')?;
    let value = rest.split('"').next()?;
    if value.is_empty() || value.len() > 32 || value.contains('\\') {
        return None;
    }
    Some(value.to_string())
}

/// Whether `candidate` is a later version than `current`.
///
/// Three numbers compared as numbers: a string comparison reads `0.10.0` as
/// older than `0.9.0`, which is wrong the first time the minor version reaches
/// double digits and would then be wrong for the rest of the project's life.
///
/// Anything unparseable on either side is `false`. A build whose own version is
/// nonsense should not be told to upgrade to a version that is also nonsense.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (triple(candidate), triple(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// `v0.8.1` or `0.8.1` as three numbers. A missing component is zero, so `v1.0`
/// is `1.0.0` rather than a parse failure.
fn triple(text: &str) -> Option<(u32, u32, u32)> {
    let text = text.trim().trim_start_matches(['v', 'V']);
    let mut parts = text.split('.');
    // Only the leading digits of each component, so a suffixed tag
    // (`0.8.0-rc1`) compares as the release it was cut from.
    let component = |raw: &str| -> Option<u32> {
        raw.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()
    };
    // The first component is the one that must parse: a tag with no number in
    // it at all is not a version, while `v1` and `v1.0` are both complete
    // enough to compare.
    let major = component(parts.next()?)?;
    let minor = parts.next().and_then(component).unwrap_or(0);
    let patch = parts.next().and_then(component).unwrap_or(0);
    Some((major, minor, patch))
}

/// One `GET` over TLS, returning the body. `None` on any failure at all.
fn get(path: &str) -> Option<String> {
    use windows::Win32::Networking::WinHttp::{
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_FLAG_SECURE, WinHttpCloseHandle,
        WinHttpConnect, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryDataAvailable,
        WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
        WinHttpSetTimeouts, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    };
    use windows::core::HSTRING;

    /// A check is a courtesy and must never be the reason a process is busy, so
    /// every phase is far shorter than the speed test's.
    const RESOLVE_MS: i32 = 3_000;
    const CONNECT_MS: i32 = 3_000;
    const SEND_MS: i32 = 3_000;
    const RECEIVE_MS: i32 = 6_000;
    /// More than the response has ever been, and small enough that a host
    /// answering with something else cannot fill memory.
    const MAX_BODY: usize = 64 * 1024;

    let host = HSTRING::from(HOST);
    let wide_path = HSTRING::from(path);
    let agent = HSTRING::from(agent());

    // SAFETY: every handle is created by the call above it and closed exactly
    // once on every path out — by the `Drop` guard once the request exists, and
    // by hand on the paths where it does not. All three `HSTRING`s outlive
    // every call that borrows one.
    unsafe {
        let session = WinHttpOpen(&agent, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, None, None, 0);
        if session.is_null() {
            return None;
        }
        let connect = WinHttpConnect(session, &host, 443, 0);
        if connect.is_null() {
            let _ = WinHttpCloseHandle(session);
            return None;
        }
        let request = WinHttpOpenRequest(
            connect,
            windows::core::w!("GET"),
            &wide_path,
            None,
            None,
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        );
        if request.is_null() {
            let _ = WinHttpCloseHandle(connect);
            let _ = WinHttpCloseHandle(session);
            return None;
        }
        // From here the three handles are owned by `Request`, whose `Drop`
        // closes them in the order WinHttp requires.
        let req = Request {
            session,
            connect,
            request,
        };
        let _ = WinHttpSetTimeouts(req.session, RESOLVE_MS, CONNECT_MS, SEND_MS, RECEIVE_MS);

        // No headers at all. GitHub needs a `User-Agent`, which `WinHttpOpen`'s
        // agent string already supplies, and takes the default JSON content
        // type — so the one header that would be worth setting is the one the
        // session already sets. `None` here also keeps this identical in shape
        // to the speed test's proven call.
        if WinHttpSendRequest(req.request, None, None, 0, 0, 0).is_err() {
            return None;
        }
        if WinHttpReceiveResponse(req.request, std::ptr::null_mut()).is_err() {
            return None;
        }

        // A non-200 is a failure rather than an answer: an unauthenticated call
        // that has hit the anonymous hourly limit gets 403, and its body is a
        // JSON object with a `message` field and no `tag_name` — but reading the
        // status is what makes that a decision rather than a coincidence.
        //
        // `WINHTTP_QUERY_FLAG_NUMBER` is load-bearing and was missing here: the
        // status code comes back as the **string** `"200"` without it, so a
        // four-byte buffer read as a `u32` holds `0x303032` and *every* response
        // — including every successful one — compares unequal to 200. The whole
        // feature then reports "no update" forever while looking like it worked.
        // `the_release_endpoint_answers` is what caught it.
        let mut status = 0u32;
        let mut len = size_of::<u32>() as u32;
        let got = WinHttpQueryHeaders(
            req.request,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            None,
            Some(&mut status as *mut u32 as *mut core::ffi::c_void),
            &mut len,
            std::ptr::null_mut(),
        );
        if got.is_err() || status != 200 {
            return None;
        }

        let mut body: Vec<u8> = Vec::new();
        loop {
            let mut ready = 0u32;
            if WinHttpQueryDataAvailable(req.request, &mut ready).is_err() || ready == 0 {
                break;
            }
            let want = (ready as usize).min(MAX_BODY - body.len());
            let mut chunk = vec![0u8; want];
            let mut read = 0u32;
            if WinHttpReadData(
                req.request,
                chunk.as_mut_ptr() as *mut core::ffi::c_void,
                want as u32,
                &mut read,
            )
            .is_err()
                || read == 0
            {
                break;
            }
            chunk.truncate(read as usize);
            body.extend_from_slice(&chunk);
            if body.len() >= MAX_BODY {
                break;
            }
        }
        // The body is UTF-8 JSON, and the tag is ASCII; lossy is right for a
        // field that is thrown away the moment anything in it is unexpected.
        Some(String::from_utf8_lossy(&body).into_owned())
    }
}

/// The three WinHttp handles, closed in the documented order on every path.
///
/// The same shape the speed test uses, and for the same reason: this has nine
/// early returns, and a handle leaked on one of them is a connection left open
/// for the life of the process.
struct Request {
    session: *mut core::ffi::c_void,
    connect: *mut core::ffi::c_void,
    request: *mut core::ffi::c_void,
}

impl Drop for Request {
    fn drop(&mut self) {
        // SAFETY: each handle came from the matching open call and is closed
        // exactly once, request first.
        unsafe {
            use windows::Win32::Networking::WinHttp::WinHttpCloseHandle;
            let _ = WinHttpCloseHandle(self.request);
            let _ = WinHttpCloseHandle(self.connect);
            let _ = WinHttpCloseHandle(self.session);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_later_version_is_newer_and_an_earlier_one_is_not() {
        assert!(is_newer("v0.8.0", "0.7.1"));
        assert!(is_newer("0.7.2", "0.7.1"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(!is_newer("v0.7.1", "0.7.1"));
        assert!(!is_newer("v0.7.0", "0.7.1"));
        assert!(!is_newer("v0.6.9", "0.7.0"));
    }

    #[test]
    fn the_minor_version_is_compared_as_a_number() {
        // The whole reason this is not a string comparison. `"0.10.0" <
        // "0.9.0"` is true lexically and false in every sense that matters, and
        // it would be wrong for the rest of the project's life.
        assert!(is_newer("v0.10.0", "0.9.5"));
        assert!(!is_newer("v0.9.5", "0.10.0"));
        assert!(is_newer("v0.7.10", "0.7.9"));
    }

    #[test]
    fn a_missing_component_reads_as_zero() {
        assert!(is_newer("v0.8", "0.7.9"));
        assert!(!is_newer("v0.8", "0.8.0"));
        assert!(is_newer("v1", "0.99.99"));
    }

    #[test]
    fn anything_unparseable_is_never_newer() {
        // Fails in the safe direction: a tag this cannot read must not become
        // an upgrade prompt nobody can act on.
        assert!(!is_newer("nightly", "0.7.1"));
        assert!(!is_newer("", "0.7.1"));
        assert!(!is_newer("release-2025", "0.7.1"));
        assert!(!is_newer("v0.8.0", "not a version"));
    }

    #[test]
    fn a_suffixed_tag_compares_as_the_release_it_was_cut_from() {
        // An `-rc1` tag must not read as newer than the release it precedes.
        assert!(!is_newer("v0.8.0-rc1", "0.8.0"));
        assert!(is_newer("v0.8.0-rc1", "0.7.9"));
    }

    #[test]
    fn the_tag_comes_out_of_a_real_release_object() {
        // Trimmed from an actual response, nested objects and all: the key is
        // unique in the document, which is what lets the scan be this crude.
        let body = r#"{
          "url": "https://api.github.com/repos/ARBO-TEAM/arbotray/releases/1",
          "assets_url": "https://api.github.com/x",
          "upload_url": "https://uploads.github.com/x{?name,label}",
          "tag_name": "v0.7.1",
          "target_commitish": "main",
          "name": "ArboTray v0.7.1",
          "draft": false,
          "prerelease": false,
          "body": "fix(widget): \"quoted\" text in a body"
        }"#;
        assert_eq!(tag_of(body).as_deref(), Some("v0.7.1"));
    }

    #[test]
    fn a_response_with_no_tag_is_not_an_update() {
        // The shape a rate-limited call answers with: a `message`, no `tag_name`.
        let body = r#"{"message":"API rate limit exceeded","documentation_url":"x"}"#;
        assert_eq!(tag_of(body), None);
        assert_eq!(tag_of(""), None);
        assert_eq!(tag_of("not json"), None);
    }

    #[test]
    fn a_tag_that_would_be_a_path_is_refused() {
        // The tag is painted on a page. It is never a path here, but a value
        // that could not have come from a real release is not one to carry into
        // a renderer on the strength of a substring scan.
        let body = r#"{"tag_name": "v1.0.0\\..\\..\\etc\\passwd"}"#;
        assert_eq!(tag_of(body), None);
        let long = format!(r#"{{"tag_name": "{}"}}"#, "9".repeat(64));
        assert_eq!(tag_of(&long), None);
    }

    /// The one test that touches the network, and the only way to know the
    /// request is actually well-formed: a header GitHub rejects, a status query
    /// that reads nothing, or a body loop that stops early all look exactly like
    /// "no update available" from the outside.
    ///
    /// `#[ignore]`d because CI must not fail when GitHub is rate-limiting an
    /// anonymous caller or the runner has no route out. Run it by hand:
    ///
    /// ```text
    /// cargo test --lib -- --ignored the_release_endpoint_answers
    /// ```
    #[test]
    #[ignore = "needs the network"]
    fn the_release_endpoint_answers() {
        let body = get(API).expect("the request must produce a body");
        let tag = tag_of(&body).unwrap_or_else(|| panic!("no tag_name in {body:.200}"));
        assert!(triple(&tag).is_some(), "unreadable tag: {tag}");
        // And the same answer the app would draw, from the same functions.
        let _ = latest();
    }

    #[test]
    fn the_version_this_build_reports_is_well_formed() {
        // `is_newer` returns false for an unparseable *current*, so a version
        // written into `Cargo.toml` in a form this cannot read would silently
        // disable the whole feature.
        assert!(triple(current()).is_some(), "Cargo version: {}", current());
        assert_eq!(agent(), format!("ArboTray/{}", current()));
    }
}

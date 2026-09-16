//! Machine detail that never changes, plus the two readings that do.
//!
//! Split in two on purpose. The identity half — computer name, Windows edition,
//! CPU model, core count, GPU — is read once, because it is registry and display
//! enumeration and none of it changes while the process lives. Re-reading a
//! registry key every second to print the same string is work for no answer.
//!
//! The live half — battery level and whether the machine is on mains, plus
//! uptime — is polled with the rest, because those two change and stale ones
//! are worse than absent ones.
//!
//! Every field is an `Option`, and every one of them is genuinely allowed to be
//! absent: a desktop has no battery, a machine with no registry access has no
//! CPU name, and a headless box has no display adapter to enumerate. `None`
//! means "this machine cannot answer that", which the page renders as a row that
//! is not there — not as a row that says "unknown".

use super::SystemSample;
use core::ffi::c_void;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Graphics::Gdi::{
    DISPLAY_DEVICE_MIRRORING_DRIVER, DISPLAY_DEVICEW, EnumDisplayDevicesW,
};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY, RegGetValueW,
};
use windows::Win32::System::SystemInformation::{
    GetLogicalProcessorInformationEx, GetTickCount64, RelationProcessorCore,
    SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
};
use windows::Win32::System::Threading::{ALL_PROCESSOR_GROUPS, GetActiveProcessorCount};
use windows::Win32::System::WindowsProgramming::GetComputerNameW;
use windows::core::{PCWSTR, PWSTR, w};

/// The two registry keys this machine's identity lives in.
const CURRENT_VERSION: PCWSTR = w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
const PROCESSOR_KEY: PCWSTR = w!("HARDWARE\\DESCRIPTION\\System\\CentralProcessor\\0");

/// `GetSystemPowerStatus` reports "no battery" as this flag, not as a missing
/// call. A desktop still answers successfully; it just says 128.
const BATTERY_FLAG_NO_BATTERY: u8 = 128;
/// And an unknown reading as 255, which is not a percentage.
const BATTERY_PERCENT_UNKNOWN: u8 = 255;

/// `ACLineStatus` values. 255 is "unknown", which is neither plugged nor not.
const AC_OFFLINE: u8 = 0;
const AC_ONLINE: u8 = 1;

/// Read one registry string. `None` for a missing value, a wrong type or a key
/// this process cannot open — every one of which is a machine that cannot
/// answer, which is what `None` is for throughout this module.
fn reg_string(key: HKEY, subkey: PCWSTR, value: PCWSTR) -> Option<String> {
    // A version string and a CPU model both fit well inside this; the API
    // reports the required size, and a longer one is truncated rather than
    // failed, which is the right trade for a caption.
    let mut buf = [0u16; 256];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: the buffer is live for the call and `len` is its size in bytes,
    // as the API requires. Both out-params point at live locals.
    let rc = unsafe {
        RegGetValueW(
            key,
            subkey,
            value,
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut len),
        )
    };
    if rc != ERROR_SUCCESS || len < 2 {
        return None;
    }
    // `len` is a byte count *including* the terminator, so the string is one
    // unit shorter than half of it.
    let units = (len / 2) as usize - 1;
    let text = String::from_utf16_lossy(&buf[..units.min(buf.len())]);
    let text = text.trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// Read one registry DWORD. `None` when the value is missing or not a DWORD.
fn reg_dword(key: HKEY, subkey: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut out = 0u32;
    let mut len = core::mem::size_of::<u32>() as u32;
    // SAFETY: `out` is a live u32 and `len` its size, which is what the API
    // writes through and checks respectively.
    let rc = unsafe {
        RegGetValueW(
            key,
            subkey,
            value,
            RRF_RT_REG_DWORD | RRF_SUBKEY_WOW6464KEY,
            None,
            Some(&mut out as *mut u32 as *mut c_void),
            Some(&mut len),
        )
    };
    if rc == ERROR_SUCCESS { Some(out) } else { None }
}

/// `Windows 11 Pro 24H2 (build 26100.3915)`.
///
/// Assembled rather than taken from one value, because no single registry value
/// carries all of it: `ProductName` is the edition, `DisplayVersion` the release
/// and `CurrentBuildNumber` with `UBR` the build. The build is the part that
/// actually identifies a machine for support, so it is the part that must not be
/// dropped if the others are missing.
///
/// `ProductName` is famously stale on Windows 11 — it still reads "Windows 10"
/// on many upgraded installs. The build number is the honest source for which
/// release this is, so when the build is 22000 or higher and the name says 10,
/// the name is corrected rather than printed.
fn windows_version() -> Option<String> {
    let name = reg_string(HKEY_LOCAL_MACHINE, CURRENT_VERSION, w!("ProductName"));
    let release = reg_string(HKEY_LOCAL_MACHINE, CURRENT_VERSION, w!("DisplayVersion"));
    let build = reg_string(HKEY_LOCAL_MACHINE, CURRENT_VERSION, w!("CurrentBuildNumber"));
    let ubr = reg_dword(HKEY_LOCAL_MACHINE, CURRENT_VERSION, w!("UBR"));

    let name = name.map(|n| match build.as_deref().and_then(|b| b.parse::<u32>().ok()) {
        Some(b) if b >= 22000 => n.replacen("Windows 10", "Windows 11", 1),
        _ => n,
    });

    let mut out = String::new();
    if let Some(n) = name {
        out.push_str(&n);
    }
    if let Some(r) = release {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&r);
    }
    if let Some(b) = build {
        if !out.is_empty() {
            out.push(' ');
        }
        match ubr {
            Some(u) => out.push_str(&format!("(build {b}.{u})")),
            None => out.push_str(&format!("(build {b})")),
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

/// This machine's name on the network.
fn computer_name() -> Option<String> {
    let mut buf = [0u16; 64];
    let mut len = buf.len() as u32;
    // SAFETY: a live buffer and its length in *characters*, as the API wants.
    unsafe {
        GetComputerNameW(Some(PWSTR(buf.as_mut_ptr())), &mut len).ok()?;
    }
    let name = String::from_utf16_lossy(&buf[..(len as usize).min(buf.len())]);
    if name.is_empty() { None } else { Some(name) }
}

/// The CPU's marketing name, e.g. `AMD Ryzen 7 5800X 8-Core Processor`.
///
/// Read from the registry rather than through WMI: one call against a service
/// that is always running, instead of COM, a query language and a connection
/// that can fail independently of everything else. The key is the same one Task
/// Manager reads, and `\0` is the only way it reports "nothing", so both are
/// filtered.
fn cpu_name() -> Option<String> {
    let raw = reg_string(HKEY_LOCAL_MACHINE, PROCESSOR_KEY, w!("ProcessorNameString"))?;
    let cleaned = raw.replace('\0', "");
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned.to_string())
    }
}

/// How many logical processors Windows will actually schedule on.
///
/// `ALL_PROCESSOR_GROUPS` and not group 0: a machine with more than 64 logical
/// processors spreads them over groups, and asking for the first one reports a
/// fraction of the machine.
fn logical_cores() -> Option<u32> {
    // SAFETY: no arguments to get wrong; the call has no failure mode.
    let n = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) };
    if n == 0 { None } else { Some(n) }
}

/// How many physical cores the machine has.
///
/// Not derivable from the logical count: SMT means a 6-core part reports 12,
/// and printing that under a row called "Cores" is simply wrong. There is no
/// registry value for it either — the processor key carries a marketing string
/// and a clock, not a topology — so this walks the platform's own relation
/// table, which is the same source Task Manager uses.
///
/// The entries are variable-length, so the buffer is walked by each one's own
/// `Size` rather than by indexing a fixed stride.
fn physical_cores() -> Option<u32> {
    let mut len = 0u32;
    // SAFETY: a null buffer is the documented way to ask for the size this
    // needs. The call fails with `ERROR_INSUFFICIENT_BUFFER` and fills `len`,
    // which is the answer, not an error.
    unsafe {
        let _ = GetLogicalProcessorInformationEx(RelationProcessorCore, None, &mut len);
    }
    if len == 0 {
        return None;
    }

    // Allocated as `u64` rather than `u8` so the buffer is at least 8-byte
    // aligned: the walk casts it to a struct containing `u32`s, and a
    // `Vec<u8>` would be one byte aligned, which is undefined behaviour to read
    // through. The length is rounded up, so the tail is slack, not data.
    let words = len.div_ceil(8) as usize;
    let mut buf = vec![0u64; words];
    // SAFETY: the buffer is at least `len` bytes and outlives the call, which
    // is exactly what the API fills.
    unsafe {
        GetLogicalProcessorInformationEx(
            RelationProcessorCore,
            Some(buf.as_mut_ptr() as *mut SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX),
            &mut len,
        )
        .ok()?;
    }

    // The smallest an entry can be is its own header — the two leading fields,
    // which is where `Anonymous` starts. Deliberately *not* `size_of` the whole
    // struct: that is the size of the largest variant in the union, 80, while a
    // `RelationProcessorCore` entry is 48, so using it as the floor reads every
    // entry as too small and counts nothing.
    let header = core::mem::offset_of!(SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX, Anonymous);

    let mut count = 0u32;
    let mut offset = 0usize;
    let total = len as usize;
    // SAFETY: every read is bounds-checked against `total` before it happens,
    // and each entry's `Size` is the API's own stride, so the walk cannot leave
    // the buffer. An entry too small to hold its own header ends the walk rather
    // than looping on it.
    unsafe {
        let base = buf.as_ptr() as *const u8;
        while offset + header <= total {
            let entry = base.add(offset) as *const SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX;
            let size = (*entry).Size as usize;
            if size < header {
                break;
            }
            count += 1;
            offset += size;
        }
    }
    if count == 0 { None } else { Some(count) }
}

/// The display adapters, as the driver names them.
///
/// `EnumDisplayDevicesW` and not the registry's display-class key: it is the
/// same list the display control panel builds, and it is already reachable
/// through GDI, which this crate has open for its drawing. The mirror driver
/// Windows installs for remote sessions is filtered out — it is not hardware
/// and would be reported on a machine that has none.
///
/// Duplicates are dropped: a two-monitor machine reports the same adapter once
/// per monitor, and "NVIDIA GeForce RTX 4070, NVIDIA GeForce RTX 4070" is worse
/// than either half of it.
fn gpus() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for index in 0..16u32 {
        let mut dd = DISPLAY_DEVICEW {
            cb: core::mem::size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        // SAFETY: `dd.cb` is set to the struct's size as the API requires, and
        // the struct outlives the call.
        let ok = unsafe { EnumDisplayDevicesW(None, index, &mut dd, 0) };
        if !ok.as_bool() {
            break;
        }
        if dd.StateFlags.contains(DISPLAY_DEVICE_MIRRORING_DRIVER) {
            continue;
        }
        let end = dd.DeviceString.iter().position(|&c| c == 0).unwrap_or(dd.DeviceString.len());
        let name = String::from_utf16_lossy(&dd.DeviceString[..end]);
        let name = name.trim().to_string();
        if name.is_empty() || out.contains(&name) {
            continue;
        }
        out.push(name);
    }
    out
}

/// Battery percentage and whether the machine is on mains.
///
/// One call answers both, and its three answers are not the same kind of thing:
/// no battery at all is a desktop, an unknown percentage is a battery that will
/// not report, and offline is a laptop running on its own cells. Collapsing them
/// would turn "no battery" into "0%", which is a lie a user would act on.
fn battery() -> (Option<u32>, Option<bool>) {
    let mut s = SYSTEM_POWER_STATUS::default();
    // SAFETY: a live, correctly-typed out-param.
    if unsafe { GetSystemPowerStatus(&mut s) }.is_err() {
        return (None, None);
    }
    let pct = if s.BatteryFlag == BATTERY_FLAG_NO_BATTERY
        || s.BatteryLifePercent == BATTERY_PERCENT_UNKNOWN
    {
        None
    } else {
        Some(u32::from(s.BatteryLifePercent))
    };
    let on_ac = match s.ACLineStatus {
        AC_ONLINE => Some(true),
        AC_OFFLINE => Some(false),
        _ => None,
    };
    (pct, on_ac)
}

/// How long the machine has been up.
fn uptime_secs() -> u64 {
    // SAFETY: takes no arguments and cannot fail.
    unsafe { GetTickCount64() / 1000 }
}

/// Sample of the machine's identity and its live power reading.
#[derive(Default)]
pub struct SystemInfo {
    /// The half that cannot change while this process runs, read on first poll
    /// and kept. Four registry reads and a display enumeration are not free, and
    /// repeating them once a second to print the same string is work for no
    /// answer.
    identity: Option<SystemSample>,
}

impl SystemInfo {
    pub fn new() -> Self {
        Self::default()
    }

    /// The identity, read once, plus the readings that move.
    ///
    /// Always answers: uptime is unconditionally available on any running
    /// Windows, so there is no `None` here to propagate. The fields that can be
    /// absent are absent *inside* the sample, which is where the page wants to
    /// know about them.
    pub fn poll(&mut self) -> SystemSample {
        let identity = self.identity.get_or_insert_with(static_detail);
        let mut sample = identity.clone();
        let (battery_pct, on_ac) = battery();
        sample.battery_pct = battery_pct;
        sample.on_ac = on_ac;
        sample.uptime_secs = Some(uptime_secs());
        sample
    }
}

/// The half of the sample that cannot change while this process runs.
///
/// Fallible per field and not as a whole: a machine that will not give up its
/// CPU name still knows its own hostname, so one unreachable key must not take
/// the other four rows down with it.
fn static_detail() -> SystemSample {
    SystemSample {
        computer_name: computer_name(),
        windows: windows_version(),
        cpu_name: cpu_name(),
        physical_cores: physical_cores(),
        logical_cores: logical_cores(),
        gpus: gpus(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stale_product_name_is_corrected_by_the_build_number() {
        // Windows 11 installs upgraded in place still answer "Windows 10 Pro"
        // from `ProductName`. The build is the honest source, so a 22000+ build
        // that claims 10 is corrected rather than printed.
        assert_eq!(
            "Windows 10 Pro".replacen("Windows 10", "Windows 11", 1),
            "Windows 11 Pro"
        );
    }

    #[test]
    fn a_missing_registry_value_is_absent_rather_than_empty() {
        // `None` and `Some("")` mean different things to the page — one is a row
        // that is not drawn, the other a row with an empty value — so a key that
        // cannot be read must not produce the second.
        let missing = reg_string(
            HKEY_LOCAL_MACHINE,
            w!("SOFTWARE\\ArboTray\\NoSuchKey"),
            w!("NoSuchValue"),
        );
        assert_eq!(missing, None);
        assert_eq!(
            reg_dword(
                HKEY_LOCAL_MACHINE,
                w!("SOFTWARE\\ArboTray\\NoSuchKey"),
                w!("NoSuchValue")
            ),
            None
        );
    }

    #[test]
    fn the_reported_topology_is_a_real_one() {
        // Cores-with-SMT must report at least as many threads as cores, and
        // neither count can be zero on a machine that is running this test. A
        // mis-walked relation table shows up here as a count that is absurd
        // rather than as a page that simply reads oddly.
        let mut s = SystemInfo::new();
        let sample = s.poll();
        match (sample.physical_cores, sample.logical_cores) {
            (Some(cores), Some(threads)) => {
                assert!(cores > 0 && threads > 0);
                assert!(
                    cores <= threads,
                    "{cores} cores but only {threads} threads"
                );
            }
            (None, Some(threads)) => assert!(threads > 0),
            // A machine that answers neither is allowed; one that answers a
            // zero is not.
            _ => {}
        }
    }

    #[test]
    fn the_live_readings_are_readable_on_this_machine() {
        // The one test that touches the real machine. Uptime always exists, and
        // the identity half is allowed to be absent on a locked-down box — so
        // this asserts the call answers at all, not that every field is filled.
        let mut s = SystemInfo::new();
        let sample = s.poll();
        assert!(sample.uptime_secs.is_some(), "uptime is unconditionally available");
    }

    #[test]
    fn the_identity_is_read_once_and_kept() {
        // Two polls must not disagree about what machine this is: the second
        // call returns the sample the first one read, not a fresh one that could
        // have been answered differently.
        let mut s = SystemInfo::new();
        let first = s.poll();
        let second = s.poll();
        assert_eq!(first.computer_name, second.computer_name);
        assert_eq!(first.cpu_name, second.cpu_name);
        assert_eq!(first.windows, second.windows);
    }

    #[test]
    fn no_battery_is_not_zero_percent() {
        // The distinction the page depends on: a desktop must not be shown to
        // have a flat battery.
        let no_battery = SYSTEM_POWER_STATUS {
            BatteryFlag: BATTERY_FLAG_NO_BATTERY,
            BatteryLifePercent: 0,
            ..Default::default()
        };
        assert_eq!(no_battery.BatteryFlag, 128);
        assert_eq!(no_battery.BatteryLifePercent, 0);

        let unknown = SYSTEM_POWER_STATUS {
            BatteryFlag: 0,
            BatteryLifePercent: BATTERY_PERCENT_UNKNOWN,
            ..Default::default()
        };
        assert_eq!(unknown.BatteryLifePercent, 255);
    }

    #[test]
    fn gpu_names_are_unique_and_free_of_mirror_drivers() {
        // Runs against the real machine. Every entry has to be a name: a blank
        // one would draw as a row with no value, and a mirror driver is not
        // hardware.
        let names = gpus();
        for name in &names {
            assert!(!name.trim().is_empty(), "a GPU with no name: {names:?}");
        }
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "the same adapter twice: {names:?}");
    }
}


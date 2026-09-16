//! Start-with-Windows, via the per-user `Run` key.
//!
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` rather than a scheduled
//! task: the app needs no elevation (`asInvoker`, no manifest), so the key that
//! Windows checks at logon is the whole mechanism. A task would buy the ability
//! to run elevated and a trigger delay, neither of which this app wants — the
//! delay would only paper over the taskbar race that [`crate::app`] waits out
//! properly instead.
//!
//! The registry is the single source of truth. There is deliberately no
//! `autostart: bool` in the config: a copy can desync from the key — from
//! `install.ps1 -Autostart`, from Task Manager's Startup tab, or from a hand
//! edit — and then the checkbox would report a state Windows does not agree
//! with.

use std::ffi::c_void;
use std::path::PathBuf;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RRF_RT_REG_SZ,
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegGetValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows::core::{PCWSTR, w};

/// The per-user startup key. Matches what `install.ps1 -Autostart` writes, so
/// the two are idempotent against each other rather than fighting.
const RUN_KEY: PCWSTR = w!("SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Run");

/// The value name. Also matches `install.ps1`.
const VALUE: PCWSTR = w!("ArboTray");

/// This executable's path, or `None` if the process cannot name itself — which
/// would leave us with nothing to register.
pub fn exe_path() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

/// A path as a `Run` value wants it: quoted.
///
/// The quotes are not decoration. The value is parsed as a command line, so an
/// unquoted path containing a space is read as an executable and arguments —
/// `C:\Program Files\ArboTray\arbotray.exe` would try to run `C:\Program`.
fn quoted(path: &str) -> String {
    format!("\"{path}\"")
}

/// A `Run` value with any surrounding quotes removed, for comparison.
fn unquoted(value: &str) -> &str {
    value.trim().trim_matches('"').trim()
}

/// Whether a registered value already refers to `exe`.
///
/// Compared rather than merely presence-checked because a `Run` value outlives
/// the file it names: move or reinstall the executable and the key still points
/// at the old path, where Windows will fail to start it. That stale entry is
/// indistinguishable from a working one if you only ask "is the value there",
/// so it reads as *off* here and the toggle rewrites it.
fn refers_to(registered: &str, exe: &str) -> bool {
    unquoted(registered).eq_ignore_ascii_case(unquoted(exe))
}

/// Read a `Run` string value. `None` when it is missing or unreadable.
fn read_value() -> Option<String> {
    // Longer than any path Windows will hand a `Run` entry; the API reports the
    // size it wanted, and a truncated path simply fails the comparison above.
    let mut buf = [0u16; 512];
    let mut len = (buf.len() * 2) as u32;
    // SAFETY: the buffer is live for the call and `len` is its size in bytes, as
    // the API requires; both out-params point at live locals.
    let rc = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            VALUE,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr() as *mut c_void),
            Some(&mut len),
        )
    };
    if rc != ERROR_SUCCESS || len < 2 {
        return None;
    }
    // `len` counts bytes *including* the terminator.
    let units = (len / 2) as usize - 1;
    Some(String::from_utf16_lossy(&buf[..units.min(buf.len())]))
}

/// Whether this executable is registered to start with Windows.
pub fn enabled() -> bool {
    let Some(exe) = exe_path() else {
        return false;
    };
    let Some(value) = read_value() else {
        return false;
    };
    refers_to(&value, &exe.to_string_lossy())
}

/// Register or unregister this executable, returning whether the registry
/// agreed.
///
/// The answer comes from the API rather than from re-reading, because a
/// write that failed and a write that succeeded then read back are the same
/// thing to a caller that only wants to show a checkbox.
pub fn set(on: bool) -> bool {
    let Some(exe) = exe_path() else {
        return false;
    };
    let text = exe.to_string_lossy();
    if on {
        write(&quoted(&text))
    } else {
        remove()
    }
}

/// Write the `Run` value, creating the key if it is somehow absent.
fn write(command: &str) -> bool {
    let mut key = HKEY::default();
    // SAFETY: `key` is a live local the API writes a handle into; the subkey and
    // class strings are static literals, and `None` for the security attributes
    // and disposition are documented as "not wanted".
    let rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    };
    if rc != ERROR_SUCCESS {
        return false;
    }

    // A `REG_SZ` is UTF-16 with a terminator, and the API wants it as bytes.
    let wide: Vec<u16> = command.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is live for the call and `bytes` is exactly its length in
    // bytes, which is the size the API writes.
    let bytes = unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) };
    // SAFETY: `key` was opened above with KEY_SET_VALUE, so the write is
    // permitted, and the name is a static literal.
    let rc = unsafe { RegSetValueExW(key, VALUE, None, REG_SZ, Some(bytes)) };
    // SAFETY: `key` is a handle this function opened and does not use again.
    let _ = unsafe { RegCloseKey(key) };
    rc == ERROR_SUCCESS
}

/// Delete the `Run` value.
///
/// A value that is not there is a success: the intent was "not registered", and
/// it is not. Deleting an absent value returns ERROR_FILE_NOT_FOUND, which is
/// the ordinary case for anyone who never turned this on.
fn remove() -> bool {
    let mut key = HKEY::default();
    // SAFETY: as in `write` — `key` is a live local for the out-handle.
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN_KEY, None, KEY_SET_VALUE, &mut key) };
    if rc != ERROR_SUCCESS {
        // No key at all means nothing was ever registered here.
        return rc == ERROR_FILE_NOT_FOUND;
    }
    // SAFETY: `key` was opened above with KEY_SET_VALUE.
    let rc = unsafe { RegDeleteValueW(key, VALUE) };
    // SAFETY: `key` is a handle this function opened and does not use again.
    let _ = unsafe { RegCloseKey(key) };
    rc == ERROR_SUCCESS || rc == ERROR_FILE_NOT_FOUND
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The path form that makes quoting load-bearing.
    const SPACED: &str = r"C:\Users\some one\AppData\Local\Programs\ArboTray\arbotray.exe";

    #[test]
    fn a_path_is_quoted_so_a_space_is_not_an_argument_break() {
        assert_eq!(quoted(SPACED), format!("\"{SPACED}\""));
        // Unquoted, the command line parser would look for `C:\Users\some`.
        assert!(quoted(SPACED).starts_with('"'));
        assert!(quoted(SPACED).ends_with('"'));
    }

    #[test]
    fn the_quotes_we_write_come_back_off() {
        assert_eq!(unquoted(&quoted(SPACED)), SPACED);
        // And an unquoted value, as a hand edit might leave it, is handled too.
        assert_eq!(unquoted(SPACED), SPACED);
        assert_eq!(unquoted("  \"x\"  "), "x");
    }

    #[test]
    fn a_registered_path_is_recognised_however_it_was_written() {
        assert!(refers_to(&quoted(SPACED), SPACED));
        assert!(refers_to(SPACED, SPACED), "hand-written, unquoted");
        assert!(
            refers_to(&quoted(SPACED), &SPACED.to_uppercase()),
            "paths are case-insensitive on Windows"
        );
    }

    #[test]
    fn a_stale_registration_is_not_a_registration() {
        // The failure this whole comparison exists for: the exe was moved or
        // reinstalled, so the key names a path that no longer runs. Reporting
        // that as "on" would leave a checkbox ticked for something Windows
        // silently fails to start.
        let moved = r"C:\old-location\arbotray.exe";
        assert!(!refers_to(&quoted(moved), SPACED));
        // Neither is an empty or absent value.
        assert!(!refers_to("", SPACED));
        assert!(!refers_to("\"\"", SPACED));
    }

    #[test]
    fn this_test_binary_can_name_itself() {
        // `set`/`enabled` both hinge on this; if it ever returns `None` the
        // toggle would be inert rather than wrong, and this is where that shows.
        let exe = exe_path().expect("a running process has a path");
        assert!(exe.is_absolute(), "got {exe:?}");
    }

    #[test]
    fn reading_a_value_that_was_never_written_is_none() {
        // The real key exists on any Windows machine, but this value should not
        // be there during a test run. Not an assertion about the user's machine:
        // it asserts the read path returns `None` rather than an error or an
        // empty string, which is what `enabled()` treats as "off".
        if !enabled() {
            assert!(read_value().is_none_or(|v| !v.is_empty()));
        }
    }
}

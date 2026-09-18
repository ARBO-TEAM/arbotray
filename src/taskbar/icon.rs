//! System-tray icon and its right-click menu.
//!
//! The docked taskbar window has no caption, no close button and no taskbar
//! button of its own, so this icon is the *only* way to quit the app. That
//! makes it load-bearing rather than cosmetic.

use crate::taskbar::alert::{Balloon, Level};
use crate::taskbar::dock::AttachResult;
use windows::Win32::Foundation::{HINSTANCE, HWND, POINT};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIIF_WARNING, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, GetSystemMetrics, HICON,
    IDI_APPLICATION, IMAGE_ICON, LR_SHARED, LoadIconW, LoadImageW, MF_STRING, PostMessageW,
    SM_CXSMICON, SM_CYSMICON, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, WM_APP, WM_NULL,
};
use windows::core::{PCWSTR, w};

/// Posted to the tray window when the icon is clicked. `lparam`'s low word is
/// the mouse message.
pub const WM_TRAY_ICON: u32 = WM_APP + 2;

/// Menu command ids. `TPM_RETURNCMD` hands the chosen id back directly instead
/// of routing a `WM_COMMAND`.
pub const CMD_OPEN: usize = 1;
pub const CMD_QUIT: usize = 2;

/// Resource id `build.rs` links the app icon in under. The two have to agree.
pub const ICON_RESOURCE: u16 = 1;

/// The icon to show: our own when `build.rs` embedded it, the stock
/// application icon otherwise. Never null — a tray slot with no icon is a
/// blank the user cannot find.
pub fn app_icon(instance: HINSTANCE) -> HICON {
    // SAFETY: `instance` is this module. The "pointer" is a resource ordinal,
    // which is what `LoadImageW` expects for `MAKEINTRESOURCE`, and the metrics
    // give the size the notification area actually draws.
    unsafe {
        // LR_SHARED: the system owns the handle and keeps it valid for the
        // process, so nobody has to free it — including the dashboard window,
        // which shows the same icon.
        let loaded = LoadImageW(
            Some(instance),
            PCWSTR(ICON_RESOURCE as usize as *const u16),
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_SHARED,
        );
        if let Ok(handle) = loaded {
            if !handle.is_invalid() {
                return HICON(handle.0);
            }
        }
        // A build with no Windows SDK lands here. Losing the icon is a
        // cosmetic loss; losing the tray entry would not be.
        LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
    }
}

/// The live notification icon. `Drop` removes it, so the tray never leaves an
/// orphaned ghost icon behind when the process exits.
pub struct Icon {
    data: NOTIFYICONDATAW,
}

impl Icon {
    /// Add the icon for `hwnd`, with `tooltip` as its hover text. `instance` is
    /// the module the icon resource lives in.
    pub fn install(hwnd: HWND, instance: HINSTANCE, tooltip: &str) -> AttachResult<Self> {
        let mut data = NOTIFYICONDATAW {
            cbSize: core::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: 1,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: WM_TRAY_ICON,
            ..Default::default()
        };
        data.hIcon = app_icon(instance);
        write_tip(&mut data.szTip, tooltip);

        // SAFETY: `data` is a fully initialised NOTIFYICONDATAW that outlives
        // the call, and `cbSize` matches its real size.
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool() {
            return Err("Shell_NotifyIconW(NIM_ADD) refused".to_string());
        }
        Ok(Self { data })
    }

    /// Replace the hover text. Called once per sample, so it stays cheap: one
    /// copy into the existing buffer and one `NIM_MODIFY`.
    pub fn set_tip(&mut self, tooltip: &str) {
        write_tip(&mut self.data.szTip, tooltip);
        // Only `NIF_TIP` is claimed, so this cannot disturb the icon or the
        // callback message.
        self.data.uFlags = NIF_TIP;
        // SAFETY: `data` is the same live struct that was added.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &self.data);
        }
        self.data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    }

    /// Put a balloon in front of the user.
    ///
    /// `NIF_INFO` is a *modify*, not an add: the icon already exists, and this
    /// borrows it for one notification. Only `NIF_INFO` is claimed in `uFlags`
    /// for the call, so this cannot disturb the tooltip, the icon or the
    /// callback message — the same discipline `set_tip` keeps.
    ///
    /// The fields are written and then **cleared again**. `szInfo` is part of
    /// the same struct every later `NIM_MODIFY` sends, so a body left in place
    /// would be re-shown by the next `set_tip` if the flag were ever set with
    /// it: clearing is what makes "one balloon" mean one.
    ///
    /// Whether the balloon is actually drawn is the shell's decision, not
    /// ours — Focus Assist, a full-screen app and the per-app notification
    /// switch all suppress it. That is the correct behaviour and the reason
    /// nothing here treats a refusal as an error: the user's Do Not Disturb
    /// outranks our threshold.
    pub fn balloon(&mut self, balloon: &Balloon) {
        write_tip(&mut self.data.szInfoTitle, &balloon.title);
        write_tip(&mut self.data.szInfo, &balloon.body);
        self.data.dwInfoFlags = match balloon.level {
            Level::Info => NIIF_INFO,
            Level::Warning => NIIF_WARNING,
        };
        self.data.uFlags = NIF_INFO;
        // SAFETY: `data` is the same live struct that was added.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &self.data);
        }
        self.data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
        self.data.szInfo.fill(0);
        self.data.szInfoTitle.fill(0);
    }
}

impl Drop for Icon {
    fn drop(&mut self) {
        // SAFETY: same struct we added with, still alive.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.data);
        }
    }
}

/// Show the right-click menu at the cursor. Returns the chosen command id, or
/// `None` if the user dismissed it.
pub fn show_menu(hwnd: HWND) -> Option<usize> {
    // SAFETY: every handle created here is destroyed before returning, and
    // `hwnd` is our live window.
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let _ = AppendMenuW(menu, MF_STRING, CMD_OPEN, w!("Open ArboTray"));
        let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT, w!("Exit"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);

        // A popup owned by a non-foreground window never dismisses when the
        // user clicks elsewhere, leaving it stuck on screen. `WS_EX_NOACTIVATE`
        // means this may not take, but the `WM_NULL` below covers the case.
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
            pt.x,
            pt.y,
            None,
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, windows::Win32::Foundation::WPARAM(0), windows::Win32::Foundation::LPARAM(0));
        let _ = DestroyMenu(menu);

        // With TPM_RETURNCMD the return value is the id, not a BOOL; 0 means
        // the menu was dismissed.
        let id = cmd.0;
        (id > 0).then_some(id as usize)
    }
}

/// Copy `s` into a fixed-size UTF-16 tooltip buffer, NUL-terminated and never
/// overrunning. The buffer arrives zeroed, but a shorter string left over from
/// a previous write would otherwise show through.
pub fn write_tip(dest: &mut [u16], s: &str) {
    dest.fill(0);
    // Leave room for the terminator. `.take` is the guard that makes a string
    // longer than the buffer truncate instead of panicking.
    let room = dest.len().saturating_sub(1);
    for (slot, ch) in dest.iter_mut().zip(s.encode_utf16()).take(room) {
        *slot = ch;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_fits_and_terminates() {
        let mut buf = [0xFFFFu16; 128];
        write_tip(&mut buf, "ArboTray");
        assert_eq!(buf[0], 'A' as u16);
        assert_eq!(buf[8], 0, "must be NUL-terminated");
        // Everything after the terminator is cleared, not stale.
        assert!(buf[9..].iter().all(|&c| c == 0));
    }

    #[test]
    fn an_overlong_tooltip_truncates_instead_of_overrunning() {
        let mut buf = [0u16; 128];
        write_tip(&mut buf, &"x".repeat(500));
        assert_eq!(buf[126], 'x' as u16);
        assert_eq!(buf[127], 0, "the last slot stays reserved for the NUL");
    }

    #[test]
    fn the_icon_message_sits_beside_the_update_message() {
        assert_eq!(WM_TRAY_ICON, 0x8002);
        assert_ne!(WM_TRAY_ICON, crate::taskbar::events::WM_TRAY_UPDATE);
    }

    #[test]
    fn no_menu_command_collides_with_a_dismissal() {
        // 0 is how TrackPopupMenu reports "the user clicked away", so no
        // command may be numbered 0, and two commands may not share an id.
        assert_ne!(CMD_OPEN, 0);
        assert_ne!(CMD_QUIT, 0);
        assert_ne!(CMD_OPEN, CMD_QUIT);
    }

    #[test]
    fn the_icon_resource_id_matches_what_build_rs_links() {
        // build.rs writes `1 ICON "..."`; a mismatch silently falls back to
        // the stock icon, which looks like the resource simply did not load.
        assert_eq!(ICON_RESOURCE, 1);
    }

    #[test]
    fn the_buffer_the_tooltip_is_fitted_to_is_the_one_it_is_written_into() {
        // Two files, one number: `TrayModel::tooltip` fits to `TOOLTIP_UNITS`
        // and this writes into `szTip`. `szTip` is followed immediately by
        // `dwState`, so the distance to it is the field's real width — and if it
        // ever moves, the fit has to move with it. A silent truncation is the
        // failure neither side would notice.
        assert_eq!(
            core::mem::offset_of!(NOTIFYICONDATAW, dwState)
                - core::mem::offset_of!(NOTIFYICONDATAW, szTip),
            (crate::taskbar::TOOLTIP_UNITS + 1) * size_of::<u16>(),
            "szTip is no longer {} units",
            crate::taskbar::TOOLTIP_UNITS + 1
        );
    }
}

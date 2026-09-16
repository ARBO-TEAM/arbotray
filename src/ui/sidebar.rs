//! The page list down the left.

use crate::ui::consts::LIST_ID;
use crate::ui::pages::PAGES;
use crate::ui::theme::sidebar_w;
use crate::ui::UiState;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, HMENU, LB_ADDSTRING, LB_SETCURSEL, LBS_NOINTEGRALHEIGHT, LBS_NOTIFY,
    SendMessageW, WINDOW_EX_STYLE, WINDOW_STYLE, WS_BORDER, WS_CHILD, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{PCWSTR, w};

/// Build the page list. A `LISTBOX` rather than anything we draw: it already
/// knows about selection, keyboard navigation and scrolling, and it is part of
/// user32, so it costs nothing to ship.
pub(crate) fn create_sidebar(parent: HWND, state: &mut UiState) {
    // SAFETY: `parent` is the window being created, mid-`WM_CREATE`; the class
    // name is a static literal and the strings outlive their `SendMessageW`.
    unsafe {
        let style = WINDOW_STYLE(
            WS_CHILD.0
                | WS_VISIBLE.0
                | WS_VSCROLL.0
                | WS_BORDER.0
                // The `LBS_` flags are plain `i32` in the bindings while the
                // `WS_` ones are a newtype, hence the mixed casts.
                | LBS_NOTIFY as u32
                | LBS_NOINTEGRALHEIGHT as u32,
        );
        let list = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("LISTBOX"),
            PCWSTR::null(),
            style,
            0,
            0,
            sidebar_w(state.dpi),
            0,
            Some(parent),
            // The child id arrives as an `HMENU`, which is how `WM_COMMAND`
            // knows which control spoke.
            Some(HMENU(LIST_ID as *mut core::ffi::c_void)),
            Some(state.instance),
            None,
        );
        let Ok(list) = list else {
            // No sidebar, but the window still works: the content paints from
            // x=0 and every page is reachable — just not switchable.
            return;
        };
        state.list = list;

        for name in PAGES {
            let mut wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            SendMessageW(
                list,
                LB_ADDSTRING,
                None,
                Some(LPARAM(wide.as_mut_ptr() as isize)),
            );
        }
        SendMessageW(list, LB_SETCURSEL, Some(WPARAM(state.page)), None);
    }
}

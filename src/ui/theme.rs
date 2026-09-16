//! Colours and DPI scaling, read from the config's theme.

use crate::config::Config;
use crate::taskbar::render::parse_color;
use crate::ui::layout::SIDEBAR_W;
use windows::Win32::Foundation::COLORREF;

// --- colours --------------------------------------------------------------

/// The window's own background, or a neutral dark when the theme's value is
/// unparseable.
pub(crate) fn background(cfg: &Config) -> COLORREF {
    parse_color(&cfg.theme.background).unwrap_or(COLORREF(0x0020_2020))
}

/// One step away from `bg`, in whichever direction the theme goes: lighter on
/// a dark theme, darker on a light one. This is what lets the sidebar separate
/// itself from the content without a second colour in the config file.
pub(crate) fn shade(bg: COLORREF, step: u32) -> COLORREF {
    let (r, g, b) = (bg.0 & 0xFF, (bg.0 >> 8) & 0xFF, (bg.0 >> 16) & 0xFF);
    // Perceived luminance, so mid-greys pick the sane side.
    let luma = (r * 299 + g * 587 + b * 114) / 1000;
    let f = |v: u32| {
        if luma < 128 {
            (v + step).min(255)
        } else {
            v.saturating_sub(step)
        }
    };
    // COLORREF is 0x00BBGGRR, not RGB.
    COLORREF(f(r) | (f(g) << 8) | (f(b) << 16))
}

/// Scale a 96-DPI metric to the current display, never to zero.
pub(crate) fn scale(value: i32, dpi: u32) -> i32 {
    (value * dpi as i32 / 96).max(1)
}

pub(crate) fn sidebar_w(dpi: u32) -> i32 {
    scale(SIDEBAR_W, dpi)
}

/// The theme's body colour when the configured one cannot be parsed.
pub(crate) fn fg_default() -> COLORREF {
    COLORREF(0x00E6_E6E6)
}

/// The theme's foreground, or a readable stand-in. Undoes the pair of
/// `unwrap_or` fallbacks that `paint` and the control colours would otherwise
/// each carry separately, so the page and the controls on it cannot disagree
/// about what colour the text is.
pub(crate) fn foreground(cfg: &Config) -> COLORREF {
    parse_color(&cfg.theme.foreground).unwrap_or(fg_default())
}

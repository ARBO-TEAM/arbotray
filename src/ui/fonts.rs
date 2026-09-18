//! The three faces the window draws with.

use crate::config::Config;
use windows::core::w;
use windows::Win32::Graphics::Gdi::{
    CreateFontW, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, FW_NORMAL, HFONT,
    OUT_DEFAULT_PRECIS,
};

/// Building a row font at `dpi`. `extra` is added to the configured point size
/// — values lead, headings more so. A failed `CreateFontW` yields a null
/// `HFONT`, which GDI reads as "the default font" — degraded, not fatal.
pub(crate) fn create_font(cfg: &Config, dpi: u32, extra: i32, bold: bool) -> HFONT {
    let points = cfg.theme.font_size.max(9) as i32 + extra;
    let height = -(points * dpi as i32 / 96);
    // SAFETY: the face name is a static literal; everything else is by value.
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            if bold { 700 } else { FW_NORMAL.0 as i32 },
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        )
    }
}

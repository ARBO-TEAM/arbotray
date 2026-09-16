//! Fonts, colours and geometry: everything the panel needs to turn rows into
//! pixels.
//!
//! The renderer owns the two fonts and the four colours the panel uses, all
//! derived from the theme, and answers the two questions the layout asks — how
//! wide is the label column, and how big is the panel. Measuring lives here
//! beside drawing because both need the same font table, and a layout that
//! measured with a different font than it drew with is a bug nobody would see
//! until it clipped.

use super::metrics::{BODY_EXTRA, BLOCK_GAP, CLOSE, CLOSE_INSET, COL_GAP, LINE_GAP, PAD, TITLE_EXTRA};
use super::rows::{Role, Row};
use crate::config::Config;
use crate::taskbar::render::parse_color;
use windows::Win32::Foundation::{COLORREF, SIZE};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, DEFAULT_CHARSET, DeleteObject, FW_BOLD,
    FW_NORMAL, GetDC, GetTextExtentPoint32W, HDC, HGDIOBJ, HFONT, OUT_DEFAULT_PRECIS, ReleaseDC,
    SelectObject, TextOutW,
};
use windows::core::w;

/// The font pair and the panel's palette, derived once per config change.
pub struct Renderer {
    pub(super) title: HFONT,
    pub(super) body: HFONT,
    dpi: u32,
    pub fg: COLORREF,
    pub dim: COLORREF,
    pub alert: COLORREF,
    pub bg: COLORREF,
    pub border: COLORREF,
}

impl Renderer {
    pub fn new(cfg: &Config, dpi: u32) -> Self {
        let size = cfg.theme.font_size.max(6);
        let bg = parse_color(&cfg.theme.background).unwrap_or(COLORREF(0x0020_2020));
        let fg = parse_color(&cfg.theme.foreground).unwrap_or(COLORREF(0x00E6_E6E6));
        let alert = parse_color(&cfg.theme.alert).unwrap_or(COLORREF(0x006B_6BFF));
        Self {
            title: create_font(size + TITLE_EXTRA, true, dpi),
            body: create_font(size + BODY_EXTRA, false, dpi),
            dpi,
            fg,
            // Labels sit back from their values by the same fraction in either
            // theme, because blending towards the background is what "dimmer"
            // means on a light panel and a dark one alike — where a fixed grey
            // is only ever right on one of them.
            dim: blend(fg, bg, 0.45),
            alert,
            bg,
            border: blend(fg, bg, 0.7),
        }
    }

    pub fn scaled(&self, v: i32) -> i32 {
        (v * self.dpi as i32 / 96).max(1)
    }

    pub fn font_for(&self, role: Role) -> HFONT {
        match role {
            Role::Title => self.title,
            _ => self.body,
        }
    }

    /// A string's extent in the font its role selects, as `(width, height)`.
    ///
    /// The screen DC is the right one to measure with, and it is taken and
    /// released per call rather than held: the panel repaints once a second, so
    /// a cached DC would be state to own for no measurable gain.
    fn extent(&self, text: &str, role: Role) -> (i32, i32) {
        if text.is_empty() {
            return (0, 0);
        }
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: the DC is released and the font restored before returning;
        // `wide` and `size` are live locals.
        unsafe {
            let dc = GetDC(None);
            if dc.is_invalid() {
                return (0, 0);
            }
            let old = SelectObject(dc, HGDIOBJ(self.font_for(role).0));
            let mut size = SIZE::default();
            let ok = GetTextExtentPoint32W(dc, &wide, &mut size).as_bool();
            SelectObject(dc, old);
            ReleaseDC(None, dc);
            if ok { (size.cx, size.cy) } else { (0, 0) }
        }
    }

    /// The width a label column needs: every label measured, widest wins.
    ///
    /// One column for the whole panel rather than one per block, so values line
    /// up down the page the way a table does. A per-block column would stagger
    /// them, which reads as several unrelated lists.
    pub fn label_column(&self, rows: &[Row]) -> i32 {
        rows.iter()
            .filter(|r| !r.label.is_empty())
            .map(|r| self.extent(&r.label, Role::Label).0)
            .max()
            .unwrap_or(0)
    }

    pub fn line_height(&self, role: Role) -> i32 {
        (self.extent("Ag", role).1 + self.scaled(LINE_GAP)).max(self.scaled(12))
    }

    /// The panel's size, from the rows it will hold.
    ///
    /// Walked in the same order the paint path walks it — headings take a block
    /// gap before them, every line takes its own height — because a size that
    /// disagrees with the paint is a panel that clips its last row.
    pub fn size(&self, rows: &[Row]) -> (i32, i32) {
        let col = self.label_column(rows);
        let gap = self.scaled(COL_GAP);
        let mut w = 0;
        let mut h = self.scaled(PAD) * 2;
        let mut first = true;
        for row in rows {
            if row.role == Role::Title && !first {
                h += self.scaled(BLOCK_GAP);
            }
            first = false;
            h += self.line_height(row.role);
            let text = if row.label.is_empty() {
                self.extent(&row.value, row.role).0
            } else {
                col + gap + self.extent(&row.value, row.role).0
            };
            w = w.max(text);
        }
        // The close box sits in the corner over the first heading, which never
        // reaches it — but the minimum keeps a one-row panel from clipping it.
        let min = self.scaled(PAD) * 2 + self.scaled(CLOSE) + self.scaled(CLOSE_INSET);
        (w.max(min), h.max(self.scaled(48)))
    }

    pub fn text(&self, dc: HDC, x: i32, y: i32, s: &str) {
        let wide: Vec<u16> = s.encode_utf16().collect();
        // SAFETY: `dc` is the memory DC this renderer is drawing into, and its
        // font is already selected by the caller.
        unsafe {
            let _ = TextOutW(dc, x, y, &wide);
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // SAFETY: both fonts are ours and are never selected into a DC between
        // paints — the paint path restores the old object before returning.
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.title.0));
            let _ = DeleteObject(HGDIOBJ(self.body.0));
        }
    }
}

/// Mix `t` of the way from `a` towards `b`.
///
/// Derives the dim and border colours from the two the user actually set, so a
/// theme edit moves all of them together instead of leaving half the panel on
/// the old palette.
fn blend(a: COLORREF, b: COLORREF, t: f32) -> COLORREF {
    let mix = |shift: u32| {
        let x = ((a.0 >> shift) & 0xFF) as f32;
        let y = ((b.0 >> shift) & 0xFF) as f32;
        ((x + (y - x) * t).round() as u32) & 0xFF
    };
    COLORREF(mix(0) | (mix(8) << 8) | (mix(16) << 16))
}

/// A failed `CreateFontW` yields a null `HFONT`, which GDI reads as the stock
/// font — degraded, not fatal, the same bargain the strip's renderer makes.
fn create_font(size: u32, bold: bool, dpi: u32) -> HFONT {
    let height = -(size as i32 * dpi as i32 / 96);
    // SAFETY: the face name is a literal that outlives the call; the rest are
    // by value.
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            if bold { FW_BOLD.0 as i32 } else { FW_NORMAL.0 as i32 },
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0, // DEFAULT_PITCH | FF_DONTCARE
            w!("Segoe UI"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_half_pixel_rounds_up_rather_than_truncating() {
        // The blend runs on every channel of two colours, so a truncating
        // instead of rounding cast is a visible tint, not a missing pixel.
        // 255 + (0 - 255) * 0.5 = 127.5 -> 128
        assert_eq!(blend(COLORREF(0x00FF_FFFF), COLORREF(0), 0.5).0, 0x0080_8080);
    }

    #[test]
    fn blending_towards_the_background_keeps_every_channel_in_range() {
        let dim = blend(COLORREF(0x00FF_FFFF), COLORREF(0x0000_0000), 0.45);
        // 255 * 0.55 = 140.25 -> 140
        assert_eq!(dim.0 & 0xFF, 140);
        assert_eq!((dim.0 >> 8) & 0xFF, 140);
        assert_eq!((dim.0 >> 16) & 0xFF, 140);
    }

    #[test]
    fn a_blend_with_no_travel_is_the_colour_it_started_from() {
        let same = blend(COLORREF(0x0012_3456), COLORREF(0x00AB_CDEF), 0.0);
        assert_eq!(same.0, 0x0012_3456);
    }
}

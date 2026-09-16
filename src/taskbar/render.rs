//! GDI renderer for the docked tray window.
//!
//! Deliberately GDI, not Direct2D: this draws a short run of text and a
//! 60-point sparkline, once per second. Direct2D would add a device context,
//! an error ladder and a few hundred KB of binary for no visible gain.
//!
//! Everything is drawn into a memory DC and blitted once, so the text does not
//! flicker at 1 Hz the way a direct-to-window `TextOutW` would.

use crate::config::{Config, Theme};
use crate::taskbar::TrayModel;
use windows::Win32::Foundation::{COLORREF, HWND, POINT, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateCompatibleBitmap, CreateCompatibleDC,
    CreateFontW, CreateSolidBrush, DEFAULT_CHARSET, DeleteDC, DeleteObject, FW_NORMAL,
    FillRect, GetTextExtentPoint32W, HGDIOBJ, HFONT, OUT_DEFAULT_PRECIS, Polyline,
    SRCCOPY, SelectObject, SetBkMode, SetTextColor, TRANSPARENT, TextOutW,
};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::w;

/// Horizontal padding inside the window, and the gap between the text run and
/// the sparkline. Scaled by DPI so the tray stays legible at 150%+.
const PAD: i32 = 6;
const SPARK_W: i32 = 40;
const SPARK_H: i32 = 12;
const SEPARATOR: &str = "  ";

// --- pure helpers ---------------------------------------------------------

/// `#RRGGBB` to a GDI `COLORREF` (which is `0x00BBGGRR`).
///
/// Returns `None` for anything malformed — a hand-edited config must not take
/// the tray down, so the caller substitutes a default.
pub fn parse_color(s: &str) -> Option<COLORREF> {
    let hex = s.strip_prefix('#')?;
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(COLORREF(
        u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16),
    ))
}

/// The non-empty fields, left to right. Empty ones are skipped so a fully
/// hidden tile costs no width.
pub fn visible_segments(model: &TrayModel) -> Vec<&str> {
    [
        model.down_text.as_str(),
        model.up_text.as_str(),
        model.latency_text.as_str(),
        model.cpu_text.as_str(),
        model.ram_text.as_str(),
        model.wifi_text.as_str(),
        model.usage_text.as_str(),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect()
}

/// Points for the sparkline, scaled to fit `w` x `h`.
///
/// The y axis is normalised against the window's own maximum rather than an
/// absolute scale, so idle traffic still shows shape instead of a flat line.
/// A zero maximum (fully idle, or one sample) must not divide by zero.
pub fn sparkline_points(history: &[u64], w: i32, h: i32) -> Vec<POINT> {
    if history.is_empty() || w <= 0 || h <= 0 {
        return Vec::new();
    }
    let max = history.iter().copied().max().unwrap_or(0).max(1);
    let n = history.len() as i32;
    // A single sample has no span to spread over; pin it to the left edge.
    let step = if n > 1 { w as f32 / (n - 1) as f32 } else { 0.0 };
    history
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let x = (i as f32 * step).round() as i32;
            let y = h - ((v as f64 / max as f64) * f64::from(h)).round() as i32;
            POINT { x, y: y.clamp(0, h) }
        })
        .collect()
}

/// Sample the taskbar's background colour at the screen position where our
/// window sits. Reading the *parent's* pixels rather than the screen keeps
/// this correct even while something else overlaps us.
///
/// Falls back to the classic taskbar grey when sampling fails — wrong-but-
/// readable beats invisible.
pub fn sample_taskbar_color(hwnd: HWND) -> COLORREF {
    // SAFETY: `hwnd` is a live window; the DC and rect are used transiently.
    unsafe {
        let parent = windows::Win32::UI::WindowsAndMessaging::GetParent(hwnd);
        let target = parent.unwrap_or(hwnd);
        let dc = windows::Win32::Graphics::Gdi::GetDC(Some(target));
        if dc.is_invalid() {
            return COLORREF(0x00F0_F0F0);
        }
        let mut rect = RECT::default();
        let color = if windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect)
            .is_err()
        {
            COLORREF(0x00F0_F0F0)
        } else {
            // A point just left of our window is still the parent's surface,
            // but never covered by our own text.
            windows::Win32::Graphics::Gdi::GetPixel(dc, rect.left - 2, (rect.top + rect.bottom) / 2)
        };
        windows::Win32::Graphics::Gdi::ReleaseDC(Some(target), dc);
        color
    }
}

/// Owns the GDI font. Created once — creating a font per repaint would leak a
/// handle every second.
pub struct Renderer {
    font: HFONT,
    /// Device pixels per inch, for scaling padding on high-DPI displays.
    dpi: u32,
}

impl Renderer {
    pub fn new(cfg: &Config, dpi: u32) -> Self {
        Self {
            font: create_font(cfg, dpi),
            dpi,
        }
    }

    /// Rebuild the font after a DPI change or a settings edit.
    pub fn set_metrics(&mut self, cfg: &Config, dpi: u32) {
        // SAFETY: the old font is ours and is not selected into any DC between
        // paints, so deleting it here cannot pull a live font out from under a
        // paint.
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.font.0));
        }
        self.font = create_font(cfg, dpi);
        self.dpi = dpi;
    }

    fn scaled(&self, value: i32) -> i32 {
        (value * self.dpi as i32 / 96).max(1)
    }

    /// Width in pixels of `text` in the tray font.
    pub fn measure(&self, text: &str) -> i32 {
        if text.is_empty() {
            return 0;
        }
        let wide: Vec<u16> = text.encode_utf16().collect();
        // SAFETY: a screen DC is valid for measurement; both objects are
        // restored before it is released.
        unsafe {
            let dc = windows::Win32::Graphics::Gdi::GetDC(None);
            if dc.is_invalid() {
                return 0;
            }
            let old = SelectObject(dc, HGDIOBJ(self.font.0));
            let mut size = SIZE::default();
            let ok = GetTextExtentPoint32W(dc, &wide, &mut size).as_bool();
            SelectObject(dc, old);
            windows::Win32::Graphics::Gdi::ReleaseDC(None, dc);
            if ok { size.cx } else { 0 }
        }
    }

    /// Width the tray window needs to hold `model` without clipping.
    pub fn needed_width(&self, model: &TrayModel) -> i32 {
        let segments = visible_segments(model);
        let text: i32 = segments.iter().map(|s| self.measure(s)).sum();
        let separators = if segments.len() > 1 {
            self.measure(SEPARATOR) * (segments.len() as i32 - 1)
        } else {
            0
        };
        let spark = if model.history.is_empty() {
            0
        } else {
            self.scaled(SPARK_W) + self.scaled(PAD)
        };
        self.scaled(PAD) * 2 + text + separators + spark
    }

    /// Paint `model` into `hwnd`, double-buffered.
    ///
    /// Transparency note: `WS_EX_LAYERED` is refused for taskbar children, so
    /// "transparent" is done by sampling the taskbar's own background colour
    /// (one `GetPixel` under where we sit) and filling with that. Under a
    /// solid taskbar this is pixel-identical to true transparency; it goes
    /// visibly wrong only over taskbar wallpapers or acrylic, where the
    /// honest fix is per-pixel alpha via `UpdateLayeredWindow`.
    pub fn paint(&self, hwnd: HWND, model: &TrayModel, cfg: &Config) {
        // SAFETY: `hwnd` is our live window; every GDI object created here is
        // either selected-and-restored or deleted before returning.
        unsafe {
            let mut rect = RECT::default();
            if GetClientRect(hwnd, &mut rect).is_err() {
                return;
            }
            let w = rect.right - rect.left;
            let h = rect.bottom - rect.top;
            if w <= 0 || h <= 0 {
                return;
            }

            let dc = windows::Win32::Graphics::Gdi::GetDC(Some(hwnd));
            if dc.is_invalid() {
                return;
            }
            let mem = CreateCompatibleDC(Some(dc));
            let bitmap = CreateCompatibleBitmap(dc, w, h);
            let old_bitmap = SelectObject(mem, HGDIOBJ(bitmap.0));

            // Background: sample the taskbar surface underneath us when the
            // user wants transparency; otherwise use the configured colour.
            let bg = if cfg.theme.opacity == 0 {
                sample_taskbar_color(hwnd)
            } else {
                parse_color(&cfg.theme.background).unwrap_or(COLORREF(0))
            };
            let brush = CreateSolidBrush(bg);
            FillRect(mem, &rect, brush);
            let _ = DeleteObject(HGDIOBJ(brush.0));

            let old_font = SelectObject(mem, HGDIOBJ(self.font.0));
            SetBkMode(mem, TRANSPARENT);
            let fg = if model.quota_alert {
                parse_color(&cfg.theme.alert).unwrap_or(COLORREF(0x0000_00FF))
            } else {
                parse_color(&cfg.theme.foreground).unwrap_or(COLORREF(0x00FF_FFFF))
            };
            SetTextColor(mem, fg);

            // Text run, left to right.
            let mut x = self.scaled(PAD);
            let segments = visible_segments(model);
            for (i, seg) in segments.iter().enumerate() {
                if i > 0 {
                    x += self.measure(SEPARATOR);
                }
                let wide: Vec<u16> = seg.encode_utf16().collect();
                // Vertically centre using the font's own cell height.
                let text_h = self.measure_height();
                let y = (h - text_h) / 2;
                let _ = TextOutW(mem, x, y, &wide);
                x += self.measure(seg);
            }

            // Sparkline, right-aligned.
            if !model.history.is_empty() {
                let spark_w = self.scaled(SPARK_W);
                let spark_h = self.scaled(SPARK_H);
                let points = sparkline_points(&model.history, spark_w, spark_h);
                if points.len() > 1 {
                    let ox = w - self.scaled(PAD) - spark_w;
                    let oy = (h - spark_h) / 2;
                    let moved: Vec<POINT> = points
                        .into_iter()
                        .map(|p| POINT {
                            x: p.x + ox,
                            y: p.y + oy,
                        })
                        .collect();
                    let pen = windows::Win32::Graphics::Gdi::CreatePen(
                        windows::Win32::Graphics::Gdi::PS_SOLID,
                        1,
                        fg,
                    );
                    let old_pen = SelectObject(mem, HGDIOBJ(pen.0));
                    let _ = Polyline(mem, &moved);
                    SelectObject(mem, old_pen);
                    let _ = DeleteObject(HGDIOBJ(pen.0));
                }
            }

            SelectObject(mem, old_font);
            let _ = BitBlt(dc, 0, 0, w, h, Some(mem), 0, 0, SRCCOPY);
            SelectObject(mem, old_bitmap);
            let _ = DeleteObject(HGDIOBJ(bitmap.0));
            let _ = DeleteDC(mem);
            windows::Win32::Graphics::Gdi::ReleaseDC(Some(hwnd), dc);
        }
    }

    /// Height of the font's character cell.
    fn measure_height(&self) -> i32 {
        // SAFETY: screen DC, restored in place.
        unsafe {
            let dc = windows::Win32::Graphics::Gdi::GetDC(None);
            if dc.is_invalid() {
                return 0;
            }
            let old = SelectObject(dc, HGDIOBJ(self.font.0));
            let mut size = SIZE::default();
            let ok = GetTextExtentPoint32W(dc, &[b'A' as u16], &mut size).as_bool();
            SelectObject(dc, old);
            windows::Win32::Graphics::Gdi::ReleaseDC(None, dc);
            if ok { size.cy } else { 0 }
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // SAFETY: the font is ours, created in `create_font`.
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.font.0));
        }
    }
}

/// Build the tray font at `dpi`. A failed `CreateFontW` yields a null `HFONT`,
/// which GDI treats as "the default font" — degraded, not fatal.
fn create_font(cfg: &Config, dpi: u32) -> HFONT {
    let height = -(cfg.theme.font_size as i32 * dpi as i32 / 96);
    // SAFETY: the face name outlives the call; all other args are by value.
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
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

/// Foreground/background resolved for drawing, with malformed config values
/// replaced by usable defaults.
pub fn resolved_colors(theme: &Theme) -> (COLORREF, COLORREF) {
    (
        parse_color(&theme.foreground).unwrap_or(COLORREF(0x00FF_FFFF)),
        parse_color(&theme.background).unwrap_or(COLORREF(0)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_parse_to_bgr_order() {
        // COLORREF is 0x00BBGGRR, so #FF8000 is stored as 0x0080FF.
        assert_eq!(parse_color("#FF8000"), Some(COLORREF(0x0080FF)));
        assert_eq!(parse_color("#000000"), Some(COLORREF(0)));
        assert_eq!(parse_color("#FFFFFF"), Some(COLORREF(0x00FF_FFFF)));
    }

    #[test]
    fn malformed_colours_are_rejected_not_guessed() {
        for bad in ["", "FF8000", "#FFF", "#GGGGGG", "#FF80000", "blurple"] {
            assert_eq!(parse_color(bad), None, "{bad:?} should not parse");
        }
    }

    #[test]
    fn malformed_theme_falls_back_to_readable_defaults() {
        let theme = Theme {
            foreground: "nonsense".into(),
            background: "#GG0000".into(),
            ..Default::default()
        };
        let (fg, bg) = resolved_colors(&theme);
        assert_eq!(fg, COLORREF(0x00FF_FFFF)); // white, not invisible
        assert_eq!(bg, COLORREF(0)); // black
    }

    #[test]
    fn empty_fields_are_skipped() {
        let model = TrayModel {
            down_text: "1.0M/s".into(),
            up_text: String::new(),
            latency_text: "8ms".into(),
            cpu_text: String::new(),
            ram_text: "44%".into(),
            wifi_text: "5G 78%".into(),
            wifi_name: None,
            usage_text: "1.4G".into(),
            quota_alert: false,
            history: Vec::new(),
            ..Default::default()
        };
        assert_eq!(
            visible_segments(&model),
            vec!["1.0M/s", "8ms", "44%", "5G 78%", "1.4G"]
        );
    }

    #[test]
    fn a_fully_hidden_tray_has_no_segments() {
        assert!(visible_segments(&TrayModel::default()).is_empty());
    }

    #[test]
    fn page_detail_never_reaches_the_strip() {
        // These six exist for the Network page and have no tile of their own:
        // the fields are filled and the run must still be empty, which is what
        // makes the taskbar exactly as wide as the user asked for. Adding one of
        // them to `visible_segments` breaks this, deliberately.
        let model = TrayModel {
            gateway_text: "192.168.1.1".into(),
            internet_text: "14ms".into(),
            loss_text: "0%".into(),
            adapter_text: "Wi-Fi".into(),
            ip_text: "192.168.1.10".into(),
            dns_text: "192.168.1.1, 8.8.8.8".into(),
            ..Default::default()
        };
        assert!(visible_segments(&model).is_empty());
        // And the tooltip is built from the same run, so it cannot leak there
        // either — the SSID is the only thing the tooltip adds.
        assert_eq!(model.tooltip(), "ArboTray");
    }

    #[test]
    fn empty_history_has_no_points() {
        assert!(sparkline_points(&[], 40, 12).is_empty());
    }

    #[test]
    fn sparkline_spans_the_full_width() {
        let pts = sparkline_points(&[0, 100, 50], 40, 12);
        assert_eq!(pts.len(), 3);
        assert_eq!(pts.first().unwrap().x, 0);
        assert_eq!(pts.last().unwrap().x, 40);
    }

    #[test]
    fn sparkline_scales_against_its_own_maximum() {
        // The peak sits on the top edge and the trough on the bottom.
        let pts = sparkline_points(&[0, 100], 10, 12);
        assert_eq!(pts[0].y, 12); // zero -> bottom
        assert_eq!(pts[1].y, 0); // max  -> top
    }

    #[test]
    fn all_zero_history_does_not_divide_by_zero() {
        // Every point must be finite and inside the box.
        for p in sparkline_points(&[0, 0, 0], 40, 12) {
            assert_eq!(p.y, 12);
            assert!(p.x >= 0 && p.x <= 40);
        }
    }

    #[test]
    fn single_sample_is_pinned_not_divided_by_zero() {
        let pts = sparkline_points(&[7], 40, 12);
        assert_eq!(pts.len(), 1);
        assert_eq!(pts[0].x, 0);
    }

    #[test]
    fn degenerate_sizes_yield_no_points() {
        assert!(sparkline_points(&[1, 2], 0, 12).is_empty());
        assert!(sparkline_points(&[1, 2], 40, 0).is_empty());
    }

    #[test]
    fn points_stay_inside_the_box() {
        let history: Vec<u64> = (0..60).map(|i| (i * 997) % 5000).collect();
        for p in sparkline_points(&history, 40, 12) {
            assert!((0..=40).contains(&p.x), "x out of range: {}", p.x);
            assert!((0..=12).contains(&p.y), "y out of range: {}", p.y);
        }
    }
}

//! Design tokens: the spacing scale, corner radii, the colour palette and the
//! icon face.
//!
//! Everything visual in the window is named here rather than spelled at the
//! call site. That is the whole point of the module: a row's height, the gap
//! between two of them and the colour of a separator are each decided once, so
//! the sidebar, the pages and the Settings controls cannot drift apart as they
//! are edited. It is a design system in the sense that matters for a GDI
//! window — one place to change — not a stylesheet.
//!
//! The palette is **derived** from the two colours the config already has. The
//! config exposes a foreground and a background and nothing else, and it is the
//! user's file, so adding "card border" and "muted text" to it would be asking
//! them to pick four more greys to get a window that already looks right. Every
//! other surface is mixed from those two by luminance instead.

use crate::config::Config;
use crate::taskbar::render::parse_color;
use core::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use windows::Win32::Foundation::COLORREF;
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateCompatibleDC, CreateFontW, DEFAULT_CHARSET,
    DeleteDC, DeleteObject, FW_NORMAL, GGI_MARK_NONEXISTING_GLYPHS, GetGlyphIndicesW, HGDIOBJ,
    HFONT, OUT_DEFAULT_PRECIS, SelectObject,
};
use windows::core::{PCWSTR, w};

// --- spacing, in 96-DPI pixels --------------------------------------------
//
// A 4-pixel scale, named by step rather than by use: `S4` is 16 whatever it is
// currently separating, so moving a row to a different gap is a change of name
// and not a change of number.

pub(crate) const S1: i32 = 4;
pub(crate) const S2: i32 = 8;
pub(crate) const S3: i32 = 12;
pub(crate) const S6: i32 = 24;

/// One sidebar entry's height. Taller than a metric row: this is the thing
/// being clicked, and a 30-pixel target that has to be hit with a mouse is a
/// worse control than a 34-pixel one.
pub(crate) const ITEM_H: i32 = 34;

/// Radius of a selection pill, a card or a field. Small enough to read as a
/// rounded rectangle and not as a capsule.
pub(crate) const RADIUS: i32 = 8;

/// The column a sidebar entry spends on its glyph, before its label starts.
pub(crate) const ICON_COL: i32 = 28;

/// The column a group heading occupies on a metric row's band, before its rule
/// starts.
///
/// Wide enough for the longest heading in use — `THIS MACHINE` — with room to
/// spare, and narrow enough that a row's own label still fits beside it at the
/// minimum window width. Too narrow and the longest heading would be cut; too
/// wide and the rule beside it would be a stub.
pub(crate) const GROUP_COL: i32 = 116;

/// How many steps a sidebar entry is lifted from the theme's background.
const SIDEBAR_STEP: u32 = 12;

// --- palette --------------------------------------------------------------

/// Every colour the window draws with, all resolved from the config's theme.
pub(crate) struct Palette {
    /// The page's own background.
    pub surface: COLORREF,
    /// The sidebar, a step off the surface.
    pub sidebar: COLORREF,
    /// Hairlines: the divider under the title, the edge of the sidebar.
    pub border: COLORREF,
    /// Body text.
    pub text: COLORREF,
    /// Captions and empty-state text — mixed toward the background so a label
    /// reads as annotation rather than as a value.
    pub muted: COLORREF,
    /// A selected sidebar entry's fill.
    pub selected: COLORREF,
    /// The one saturated colour in the window.
    pub accent: COLORREF,
    /// Text drawn on top of `accent`.
    pub accent_text: COLORREF,
    /// Over-quota text and the failure notice.
    pub danger: COLORREF,
}

/// Perceived luminance of a `COLORREF`, 0-255.
///
/// The weights are the standard 0.299/0.587/0.114. Integer maths is enough
/// here: the only decision this drives is which side of 128 a colour is on.
pub(crate) fn luma(c: COLORREF) -> u32 {
    let (r, g, b) = (c.0 & 0xFF, (c.0 >> 8) & 0xFF, (c.0 >> 16) & 0xFF);
    (r * 299 + g * 587 + b * 114) / 1000
}

/// Whether the theme's background is dark.
///
/// `pub(crate)` because the window chrome has to know too: Windows title bars
/// and scroll bars are drawn by the shell, and telling DWM which way the theme
/// goes is the only way to stop a dark window wearing a white caption.
pub(crate) fn is_dark(cfg: &Config) -> bool {
    luma(crate::ui::theme::background(cfg)) < 128
}

/// `a` mixed toward `b` by `t` percent, per channel.
pub(crate) fn mix(a: COLORREF, b: COLORREF, t: u32) -> COLORREF {
    let t = t.min(100);
    let ch = |shift: u32| {
        let (av, bv) = ((a.0 >> shift) & 0xFF, (b.0 >> shift) & 0xFF);
        // Implicit, not `round()`: a half-step on a colour channel is not
        // visible, and the arithmetic only has to be off by none.
        (av * (100 - t) + bv * t) / 100
    };
    // COLORREF is 0x00BBGGRR, not RGB, so the shifts run blue-first.
    COLORREF(ch(0) | (ch(8) << 8) | (ch(16) << 16))
}

const BLACK: COLORREF = COLORREF(0x0000_0000);
const WHITE: COLORREF = COLORREF(0x00FF_FFFF);
/// Apple's system blue, the dark-appearance value.
const ACCENT_DARK: COLORREF = COLORREF(0x00FF_840A);
/// And the light-appearance value.
const ACCENT_LIGHT: COLORREF = COLORREF(0x00FF_7A00);

/// Resolve the whole palette from the config's theme.
pub(crate) fn palette(cfg: &Config) -> Palette {
    let surface = crate::ui::theme::background(cfg);
    let text = crate::ui::theme::foreground(cfg);
    let dark = is_dark(cfg);

    Palette {
        surface,
        sidebar: crate::ui::theme::shade(surface, SIDEBAR_STEP),
        border: crate::ui::theme::shade(surface, 40),
        text,
        // Toward the surface, not toward a fixed grey: on a light theme the
        // background is white, so "muted" has to move *away* from white to stay
        // legible, and mixing toward the background gets that direction wrong.
        muted: mix(text, if dark { BLACK } else { WHITE }, 42),
        selected: crate::ui::theme::shade(surface, 26),
        accent: if dark { ACCENT_DARK } else { ACCENT_LIGHT },
        accent_text: WHITE,
        danger: parse_color(&cfg.theme.alert).unwrap_or(COLORREF(0x0000_00FF)),
    }
}

// --- icons ----------------------------------------------------------------
//
// Codepoints from the Windows icon face, not arbitrary: every one below was
// checked for presence in both faces Windows ships and then read off a rendered
// contact sheet, so each is known to exist *and* to be the picture intended.
// Presence alone is not enough — half this range is a blank or a private-use
// box, and a code point that exists but draws nothing looks like a bug in the
// layout rather than a wrong constant.

pub(crate) const ICON_OVERVIEW: u16 = 0xE80F;
pub(crate) const ICON_NETWORK: u16 = 0xE774;
pub(crate) const ICON_HARDWARE: u16 = 0xE9D9;
pub(crate) const ICON_DATA: u16 = 0xE81C;
pub(crate) const ICON_SETTINGS: u16 = 0xE713;
/// An RJ45 plug, for the sockets page.
pub(crate) const ICON_PORTS: u16 = 0xE968;
/// A bolt, for the speed test.
pub(crate) const ICON_SPEED: u16 = 0xE945;
/// A stopwatch — body, stem and all — for the clock page. The face has a plain
/// alarm clock at `0xE917` as well; the stem is what tells the two apart at
/// 16 pixels, and this is the one that is not already a clock reading.
pub(crate) const ICON_STOPWATCH: u16 = 0xE916;
/// The face's own timer glyph — a dial with a hand, distinct from the stopwatch
/// stem above it at 16 pixels.
pub(crate) const ICON_TIMER: u16 = 0xE823;

/// The dark-mode glyph, for the button that puts the window in one.
///
/// One constant rather than a sun/moon pair: the button says what it switches
/// *to*, and it is drawn from the configuration rather than from a stored mode
/// — see `next_preset`, which is also what decides which one it is.
pub(crate) const ICON_MOON: u16 = 0xE708;
/// The light-mode glyph, the other half of the pair above.
pub(crate) const ICON_SUN: u16 = 0xE706;

/// The glyph a sidebar entry leads with, by page index.
/// The glyph for the appearance button: the mode it leads to.
pub(crate) fn theme_glyph(next_is_dark: bool) -> u16 {
    if next_is_dark { ICON_MOON } else { ICON_SUN }
}

/// The named presets the appearance button swaps between.
///
/// A preset is a pair of hex strings and nothing else, because that is all the
/// theme already is: `theme::background`/`foreground` parse `cfg.theme`, and
/// `palette` derives every other colour from the two. A `preset` field beside
/// them would be a second place the appearance is written down; the button
/// instead *types into the two fields*, which is why it lives on the Settings
/// page and is staged behind Save like every other field there.
///
/// Two, deliberately. A light theme is not a checkbox on a dark one — every
/// palette entry is derived — so this is the whole of the choice.
pub(crate) const DARK_PRESET: (&str, &str) = ("#000000", "#E6E6E6");
pub(crate) const LIGHT_PRESET: (&str, &str) = ("#FFFFFF", "#1C1C1E");

/// The two colours the appearance button would write, and the word for them.
///
/// Takes the background as it stands *on the page* rather than the saved config,
/// so a colour the user has just picked and not yet saved still decides which
/// way the toggle goes. The test is the same luma the whole palette is built
/// on, so "the window looks light" and "the button offers dark" cannot disagree
/// — and an unparseable hex falls back to the dark preset, which is the theme
/// the app ships with.
pub(crate) fn next_preset(background: &str) -> (bool, &'static str, &'static str, &'static str) {
    let dark_now = crate::taskbar::render::parse_color(background)
        .map(|c| luma(c) < 128)
        .unwrap_or(true);
    let next_dark = !dark_now;
    let (bg, fg) = if next_dark { DARK_PRESET } else { LIGHT_PRESET };
    (next_dark, if next_dark { "Dark" } else { "Light" }, bg, fg)
}

pub(crate) fn page_icon(page: usize) -> u16 {
    match page {
        crate::ui::pages::OVERVIEW => ICON_OVERVIEW,
        crate::ui::pages::NETWORK => ICON_NETWORK,
        crate::ui::pages::SYSTEM => ICON_HARDWARE,
        crate::ui::pages::DATA => ICON_DATA,
        crate::ui::pages::PORTS => ICON_PORTS,
        crate::ui::pages::SPEEDTEST => ICON_SPEED,
        crate::ui::pages::STOPWATCH => ICON_STOPWATCH,
        crate::ui::pages::TIMER => ICON_TIMER,
        crate::ui::pages::SETTINGS => ICON_SETTINGS,
        // Not reachable: the page is clamped to `PAGES` before it gets here.
        // The overview's glyph is a better answer than a blank column anyway.
        _ => ICON_OVERVIEW,
    }
}

/// The Win11 glyph face. Win10 does not have it.
const FACE_FLUENT: PCWSTR = w!("Segoe Fluent Icons");
/// The Win10 face. Every glyph this module uses is in both.
const FACE_MDL2: PCWSTR = w!("Segoe MDL2 Assets");

/// Which face was found installed: 0 unknown, 1 Fluent, 2 MDL2.
static FACE: AtomicU32 = AtomicU32::new(0);

/// The icon face to build fonts from.
///
/// Resolved once, by asking GDI whether a known glyph actually exists rather
/// than by testing a Windows version. `CreateFontW` for an absent face silently
/// substitutes a system font, so "the font was created" proves nothing — the
/// glyph indices are the only honest test, and a substitution here would draw
/// every icon in the window as a box.
pub(crate) fn icon_face() -> PCWSTR {
    match FACE.load(Ordering::Relaxed) {
        1 => FACE_FLUENT,
        2 => FACE_MDL2,
        _ => {
            let fluent = face_has_glyph(FACE_FLUENT, ICON_OVERVIEW);
            FACE.store(if fluent { 1 } else { 2 }, Ordering::Relaxed);
            if fluent { FACE_FLUENT } else { FACE_MDL2 }
        }
    }
}

/// Whether `face` really carries `probe`, as opposed to being substituted.
fn face_has_glyph(face: PCWSTR, probe: u16) -> bool {
    // SAFETY: a private memory DC and a font created and destroyed here; the
    // probe buffer outlives the call and no other DC holds the font.
    unsafe {
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            return false;
        }
        let font = CreateFontW(
            -16,
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
            0,
            face,
        );
        let old = SelectObject(dc, HGDIOBJ(font.0));
        let text = [probe, 0];
        let mut idx = [0u16; 1];
        // `GGI_MARK_NONEXISTING_GLYPHS` makes an absent glyph come back as
        // 0xFFFF instead of as a valid index into a substitute font.
        let count = GetGlyphIndicesW(
            dc,
            PCWSTR(text.as_ptr()),
            1,
            idx.as_mut_ptr(),
            GGI_MARK_NONEXISTING_GLYPHS,
        );
        SelectObject(dc, old);
        let _ = DeleteObject(HGDIOBJ(font.0));
        let _ = DeleteDC(dc);
        // GDI_ERROR means the call failed, and a failed call must not read as
        // "the glyph is there".
        count != u32::MAX && idx[0] != 0xFFFF
    }
}

/// The last icon font built, and the size it was built at.
static ICON_HFONT: AtomicIsize = AtomicIsize::new(0);
static ICON_KEY: AtomicU32 = AtomicU32::new(0);

/// The icon face at `cfg`'s font size and the current `dpi`.
///
/// Cached because this is asked for once per painted frame and GDI font
/// creation is not free. The key is the point size packed with the DPI, so a
/// font-size change or a move to another monitor rebuilds it and neither
/// rebuilds it in between. The very last font is never deleted — one `HFONT`
/// at process exit, against an `OnExit` hook that would exist only to free it.
pub(crate) fn icon_font(cfg: &Config, dpi: u32) -> HFONT {
    let points = cfg.theme.font_size.max(9) as i32 + 4;
    let key = ((points as u32) << 16) | (dpi & 0xFFFF);
    if ICON_KEY.load(Ordering::Relaxed) == key {
        let handle = ICON_HFONT.load(Ordering::Relaxed);
        if handle != 0 {
            return HFONT(handle as *mut core::ffi::c_void);
        }
    }

    // SAFETY: `CreateFontW` takes only by-value arguments and a static face
    // name; the previous font is deleted after the new one exists, so the slot
    // is never left holding a freed handle.
    unsafe {
        let height = -(points * dpi as i32 / 96);
        let font = CreateFontW(
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
            0,
            icon_face(),
        );
        let previous = ICON_HFONT.swap(font.0 as isize, Ordering::Relaxed);
        if previous != 0 {
            let _ = DeleteObject(HGDIOBJ(previous as *mut core::ffi::c_void));
        }
        ICON_KEY.store(key, Ordering::Relaxed);
        font
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_muted_text_colour_moves_away_from_the_body_colour() {
        // The one thing a derived palette can get wrong in the dark: "muted"
        // that sits next to the body colour is not muted, and one mixed the
        // wrong way is *brighter* than body text on a dark theme — which reads
        // as a second emphasis level rather than as a caption.
        let dark = Palette {
            surface: BLACK,
            sidebar: BLACK,
            border: BLACK,
            text: WHITE,
            muted: mix(WHITE, BLACK, 42),
            selected: BLACK,
            accent: ACCENT_DARK,
            accent_text: WHITE,
            danger: BLACK,
        };
        assert!(
            luma(dark.muted) < luma(dark.text),
            "on a dark theme a caption is dimmer than body text"
        );

        let light = Palette {
            text: BLACK,
            muted: mix(BLACK, WHITE, 42),
            ..dark
        };
        assert!(
            luma(light.muted) > luma(light.text),
            "and on a light theme it is lighter, which is the same direction away from the page"
        );
    }

    #[test]
    fn the_palette_follows_the_configs_theme() {
        let mut cfg = Config::default();
        cfg.theme.background = "#101010".into();
        cfg.theme.foreground = "#F0F0F0".into();
        let dark = palette(&cfg);
        assert!(is_dark(&cfg));
        assert_eq!(dark.surface, crate::ui::theme::background(&cfg));
        assert_eq!(dark.accent, ACCENT_DARK);

        cfg.theme.background = "#FAFAFA".into();
        cfg.theme.foreground = "#101010".into();
        let light = palette(&cfg);
        assert!(!is_dark(&cfg));
        // The accent is not a tint of the theme: a saturated blue has to be
        // chosen per appearance or it loses its contrast on the other one.
        assert_eq!(light.accent, ACCENT_LIGHT);
    }

    #[test]
    fn the_appearance_toggle_offers_the_other_one_and_lands_on_it() {
        // The row reads "the mode this switches to", so the two things to get
        // wrong are offering the mode already in force and writing a pair that
        // does not actually produce the mode named. Both are checked by feeding
        // the preset back in: the second click has to come back where it began.
        let (next_dark, caption, bg, fg) = next_preset(DARK_PRESET.0);
        assert!(!next_dark, "a dark window's button offers the light one");
        assert_eq!(caption, "Light");
        assert_eq!((bg, fg), LIGHT_PRESET);
        let (back_dark, back_caption, back_bg, back_fg) = next_preset(bg);
        assert!(back_dark, "the dark preset has to read back as dark");
        assert_eq!(back_caption, "Dark");
        assert_eq!((back_bg, back_fg), DARK_PRESET);

        // And the two presets are genuinely opposite, which is the property a
        // hand-edited pair of hex strings could quietly lose.
        assert!(luma(parse_color(LIGHT_PRESET.0).unwrap()) >= 128);
        assert!(luma(parse_color(DARK_PRESET.0).unwrap()) < 128);
        // A field holding nothing usable falls back to the theme the app ships
        // with — dark — rather than to a light window nobody asked for, so the
        // button then offers the light one.
        assert!(!next_preset("").0);
        assert!(!next_preset("#GGGGGG").0);
    }

    #[test]
    fn the_sidebar_is_a_step_off_the_surface_not_the_same_colour() {
        // A sidebar the same colour as the page is a sidebar that vanished, and
        // the divider is then the only thing separating them.
        let cfg = Config::default();
        let p = palette(&cfg);
        assert_ne!(p.sidebar, p.surface);
        assert_ne!(p.border, p.surface);
        assert_ne!(p.selected, p.sidebar, "a selected entry has to be visible against its own list");
    }

    #[test]
    fn every_page_has_a_glyph_and_they_are_distinct() {
        // A copied constant here shows up as two identical icons in the
        // sidebar, which is the kind of thing that survives review.
        let glyphs: Vec<u16> = (0..crate::ui::pages::PAGES.len())
            .map(page_icon)
            .collect();
        let mut unique = glyphs.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            glyphs.len(),
            "two pages share an icon: {glyphs:?}"
        );
    }

    #[test]
    fn a_glyph_is_resolved_to_a_face_that_actually_has_it() {
        // The fallback path, exercised for real rather than mocked: if neither
        // face is installed this fails, and a silently substituted font is
        // exactly the failure this resolution exists to prevent.
        let face = icon_face();
        assert!(
            face_has_glyph(face, ICON_OVERVIEW),
            "no installed icon face carries the overview glyph"
        );
        for page in 0..crate::ui::pages::PAGES.len() {
            assert!(
                face_has_glyph(face, page_icon(page)),
                "the resolved face is missing page {page}'s glyph"
            );
        }
    }
}


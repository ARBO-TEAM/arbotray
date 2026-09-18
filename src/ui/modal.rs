//! The window's one dialog: a confirmation popup, drawn rather than asked for.
//!
//! Owner-drawn instead of `MessageBoxW`, for the same reason the sidebar is not
//! a listbox — a system dialog wears the system's face and colours inside a
//! themed window, and it cannot be told that one of its two buttons is
//! destructive. `MB_ICONWARNING` is not a red button, and the question this asks
//! is "kill this process", which is exactly the one that should look like one.
//!
//! **Reusable by shape.** It carries an [`Action`], not a page or an index, so
//! the popup never learns what is being confirmed. A second confirmation
//! elsewhere in the app is one enum variant and no new window; the caller that
//! raises it does the work when [`Choice::Confirm`] comes back.
//!
//! Geometry is a pure function of the client rect and the DPI — see [`rects`] —
//! which is what lets the painter and the hit test agree without either of them
//! holding state the other cannot see. The price is a fixed card height, and it
//! is deliberate: measuring wrapped text needs a DC, and a hit test does not
//! have one. The comment on [`CARD_H`] carries the ceiling that buys.

use crate::ui::components::{draw, draw_block, hairline, rounded_fill, Fonts};
use crate::ui::design::{mix, Palette, RADIUS, S2, S3};
use crate::ui::layout::{PAD, ROW_H, TITLE_EXTRA};
use crate::ui::theme::scale;
use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{SetTextColor, DT_CENTER, DT_LEFT, HDC};

/// How wide the card is before the client rect gets a say.
const CARD_W: i32 = 400;

/// How tall the card is, always.
///
/// Sized for a title, three wrapped lines of body and the button row. Fixed
/// because [`rects`] has to answer the same rectangle to a `WM_LBUTTONDOWN` as
/// it did to the `WM_PAINT`, and measuring text needs a DC that a click does not
/// carry. A body longer than three lines is clipped rather than growing the
/// card; every body in the app is one sentence, and the one that is not is a
/// bug at the call site rather than a case to lay out for.
const CARD_H: i32 = 178;

/// The buttons, and how far they sit from the card's edges.
const BTN_W: i32 = 104;
const BTN_H: i32 = 32;
const BTN_GAP: i32 = S2;
const INSET: i32 = 20;

/// How far the whole window is dimmed behind the card, as a percentage toward
/// the theme's opposite. Enough to push the page back without hiding it: the
/// user should still recognise the window they are standing in.
const SCRIM_PCT: u32 = 58;

/// What confirming does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    /// End the process holding a port: its pid, and what to call it in the
    /// question.
    StopPort { pid: u32, name: String },
    /// Let the power timer do what it was armed for. The action is the engine's
    /// own enum rather than a copy of it here, so the popup cannot agree with
    /// the page while disagreeing with the thing that fires.
    PowerTimer { action: crate::power::Action },
}

impl Action {
    /// The word on the confirming button.
    ///
    /// Per action rather than one word for every question: "Stop" on a port and
    /// "Stop" on a shut-down are not the same commitment, and the button is the
    /// last thing read before the machine goes down.
    pub(crate) fn confirm_label(&self) -> &'static str {
        match self {
            Action::StopPort { .. } => "Stop",
            Action::PowerTimer { action } => action.label(),
        }
    }

    /// Whether confirming this ends something the user cannot get back.
    ///
    /// A suspend is recoverable — the mouse brings the machine back with every
    /// window where it was left — so only a shut-down wears the danger colour.
    /// Killing a process is the other one, and for the same reason.
    pub(crate) fn danger(&self) -> bool {
        match self {
            Action::StopPort { .. } => true,
            Action::PowerTimer { action } => *action == crate::power::Action::Shutdown,
        }
    }
}

/// One question: what it says, and what a yes does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Confirm {
    pub title: String,
    pub body: String,
    pub action: Action,
}

/// What the user did with the popup.
///
/// Confirming carries the [`Action`] rather than leaving the caller to fetch it
/// afterwards: the popup takes its question down as it answers, so a caller
/// that had to ask "which one was that?" after the fact would find nothing
/// there. Handing it over at the moment of the answer is the only shape where
/// the two cannot disagree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Confirmed(Action),
    Cancelled,
}

/// Whether a question is up, and which one.
///
/// Held rather than the answer: the popup knows nothing about what it is asking,
/// so the state it needs is the [`Confirm`] itself.
#[derive(Default)]
pub(crate) struct Modal {
    pending: Option<Confirm>,
}

impl Modal {
    pub(crate) fn is_open(&self) -> bool {
        self.pending.is_some()
    }

    pub(crate) fn open(&mut self, confirm: Confirm) {
        self.pending = Some(confirm);
    }

    /// What the question on screen would do, without answering it.
    ///
    /// Cloned out rather than borrowed because the callers ask *before* the
    /// click — which takes the question down as it answers — and because a
    /// dismissal is an answer to one question and a shrug at another: the power
    /// timer has to be disarmed by a Cancel, while a Stop click that cancels a
    /// port question must leave it alone.
    pub(crate) fn pending_action(&self) -> Option<Action> {
        self.pending.as_ref().map(|c| c.action.clone())
    }

    /// Dismiss without confirming. Also what `Esc` does, and what a page change
    /// does — a question about a row that is no longer on screen is a question
    /// about nothing.
    pub(crate) fn dismiss(&mut self) {
        self.pending = None;
    }

    /// Resolve a click at `(x, y)`. Nothing at all when the pointer is on the
    /// card's body or off it entirely: a popup is modal, so a click outside it
    /// is swallowed rather than treated as a cancel — a stray click that
    /// dismisses a destructive prompt is worse than one that does nothing.
    pub(crate) fn click(&mut self, client: &RECT, dpi: u32, x: i32, y: i32) -> Option<Outcome> {
        // Taken, not borrowed: the question comes down as it is answered, and
        // its action goes out with the answer.
        let confirm = self.pending.take()?;
        let r = rects(client, dpi);
        let hit = |b: &RECT| x >= b.left && x < b.right && y >= b.top && y < b.bottom;
        if hit(&r.confirm) {
            return Some(Outcome::Confirmed(confirm.action));
        }
        if hit(&r.cancel) {
            return Some(Outcome::Cancelled);
        }
        // A click on the body or the scrim dismisses nothing, so the question
        // goes back exactly as it was.
        self.pending = Some(confirm);
        None
    }
}

/// Every rectangle the popup draws in. One value rather than five locals, so
/// the painter and the hit test read the same ones instead of recomputing them.
pub(crate) struct Rects {
    pub card: RECT,
    pub title: RECT,
    pub body: RECT,
    pub cancel: RECT,
    pub confirm: RECT,
}

/// Every rectangle the popup draws in, from the client area alone.
///
/// One function for the painter and the hit test because they *must* agree: a
/// button whose rectangle is computed twice is a button that stops responding
/// the day one of the two is edited.
pub(crate) fn rects(client: &RECT, dpi: u32) -> Rects {
    let client_w = client.right - client.left;
    let client_h = client.bottom - client.top;
    // Never wider than the window it is centred in, with the page's own padding
    // left over on both sides.
    let card_w = scale(CARD_W, dpi)
        .min(client_w - scale(PAD, dpi) * 2)
        .max(scale(220, dpi));
    let card_h = scale(CARD_H, dpi)
        .min(client_h - scale(PAD, dpi) * 2)
        .max(1);
    let cx = client.left + client_w / 2;
    let cy = client.top + client_h / 2;
    let card = RECT {
        left: cx - card_w / 2,
        top: cy - card_h / 2,
        right: cx + card_w / 2,
        bottom: cy + card_h / 2,
    };

    let inset = scale(INSET, dpi);
    let btn_w = scale(BTN_W, dpi);
    let btn_h = scale(BTN_H, dpi);
    let gap = scale(BTN_GAP, dpi);
    let btn_top = card.bottom - inset - btn_h;
    // Confirm on the right and Cancel beside it: the outer edge is where a
    // hand lands first, and the destructive one should not be what a fumbled
    // double-click reaches for. Cancel takes the outer edge for that reason.
    let confirm = RECT {
        left: card.right - inset - btn_w,
        top: btn_top,
        right: card.right - inset,
        bottom: btn_top + btn_h,
    };
    let cancel = RECT {
        left: confirm.left - gap - btn_w,
        top: btn_top,
        right: confirm.left - gap,
        bottom: btn_top + btn_h,
    };

    let text_left = card.left + inset;
    let text_right = card.right - inset;
    let title_top = card.top + inset;
    let header_h = scale(ROW_H + TITLE_EXTRA * 2, dpi);
    Rects {
        card,
        cancel,
        confirm,
        title: RECT {
            left: text_left,
            top: title_top,
            right: text_right,
            bottom: title_top + header_h,
        },
        // The whole strip between the header and the buttons: the body wraps
        // inside it and is clipped by it, which is the fixed-height card's
        // documented ceiling.
        body: RECT {
            left: text_left,
            top: title_top + header_h,
            right: text_right,
            bottom: btn_top - scale(S3, dpi),
        },
    }
}

/// Draw the scrim over the whole client area and then the card.
pub(crate) fn paint(dc: HDC, client: &RECT, modal: &Modal, fonts: &Fonts, pal: &Palette, dpi: u32) {
    let Some(confirm) = &modal.pending else {
        return;
    };
    let r = rects(client, dpi);
    let scrim = scrim_colour(pal);

    // SAFETY: every call below is a draw into the live DC the frame owns, and
    // each helper documents its own contract.
    unsafe {
        // A flat fill rather than an alpha blend: GDI's blended fills need a
        // source bitmap and a per-pixel alpha channel for a result that is one
        // solid rectangle here, because there is exactly one layer behind it.
        rounded_fill(dc, *client, 0, scrim);
        rounded_fill(dc, r.card, RADIUS, pal.sidebar);

        SetTextColor(dc, pal.text);
        draw(
            dc,
            fonts.title,
            &confirm.title,
            r.title.left,
            r.title.top,
            r.title.right,
            DT_LEFT,
        );
        // The body is the one thing here that can be more than one line, so it
        // gets the wrapping call rather than the single-line one.
        SetTextColor(dc, pal.muted);
        draw_block(dc, fonts.body, &confirm.body, r.body, DT_LEFT);

        button(dc, fonts, &r.cancel, "Cancel", false, pal, dpi);
        // Destructive actions wear the danger colour and confirmations wear the
        // accent, which is the whole reason this is not a `MessageBoxW`.
        button(
            dc,
            fonts,
            &r.confirm,
            confirm.action.confirm_label(),
            confirm.action.danger(),
            pal,
            dpi,
        );
    }
}

/// The dimming colour, mixed away from the theme's own background.
pub(crate) fn scrim_colour(pal: &Palette) -> COLORREF {
    // Not a token in `Palette`: it is derived from the surface rather than
    // chosen, and every palette entry that *is* named there exists because
    // something draws with it directly.
    let dark = crate::ui::design::luma(pal.surface) < 128;
    mix(pal.surface, if dark { BLACK } else { WHITE }, SCRIM_PCT)
}

const BLACK: COLORREF = COLORREF(0x0000_0000);
const WHITE: COLORREF = COLORREF(0x00FF_FFFF);

/// One button in the card.
///
/// Filled rather than framed for the confirming one and outlined for the other,
/// so the two do not read as a pair of equals: this is a question with a
/// suggested answer, not a choice between two doors.
fn button(dc: HDC, fonts: &Fonts, rect: &RECT, label: &str, danger: bool, pal: &Palette, dpi: u32) {
    let fill = if danger { pal.danger } else { pal.selected };
    let text = if danger { pal.accent_text } else { pal.text };
    // SAFETY: as above — a draw into the frame's DC.
    unsafe {
        rounded_fill(dc, *rect, RADIUS - 2, fill);
        if !danger {
            // A hairline at the foot of the button, which is the flattest thing
            // that still reads as a control without a second fill colour.
            hairline(
                dc,
                rect.left + scale(S3, dpi),
                rect.right - scale(S3, dpi),
                rect.bottom - 1,
                pal.border,
            );
        }
        // Centred on its band: `draw` lays single-line text out from its *top*,
        // so the offset is half the slack rather than the row's usual nudge.
        let slack = ((rect.bottom - rect.top) - scale(ROW_H, dpi)) / 2;
        SetTextColor(dc, text);
        draw(
            dc,
            fonts.body,
            label,
            rect.left,
            rect.top + slack.max(0),
            rect.right,
            DT_CENTER,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DPI: u32 = 96;
    const CLIENT: RECT = RECT {
        left: 0,
        top: 0,
        right: 900,
        bottom: 600,
    };

    /// The contract the painter and the hit test share. If these two ever stop
    /// agreeing, the popup draws a button in one place and answers clicks in
    /// another — the failure this whole module is arranged to avoid.
    #[test]
    fn the_buttons_sit_inside_the_card_and_do_not_overlap() {
        let r = rects(&CLIENT, DPI);
        for b in [&r.cancel, &r.confirm] {
            assert!(b.left >= r.card.left, "a button hangs off the card's left");
            assert!(
                b.right <= r.card.right,
                "a button hangs off the card's right"
            );
            assert!(
                b.bottom <= r.card.bottom,
                "a button hangs off the card's foot"
            );
            assert!(b.top >= r.body.bottom, "a button overlaps the body text");
            assert!(b.right > b.left && b.bottom > b.top, "a button has no area");
        }
        assert!(r.cancel.right <= r.confirm.left, "the two buttons overlap");
        assert!(r.title.bottom <= r.body.top, "the title overlaps the body");
    }

    /// The card is centred whatever the window is, which is what makes it read
    /// as a modal rather than as an extra panel.
    #[test]
    fn the_card_is_centred_and_never_wider_than_the_window() {
        let r = rects(&CLIENT, DPI);
        assert_eq!(
            r.card.left + r.card.right,
            CLIENT.right,
            "the card is not centred on the client area"
        );
        assert_eq!(
            r.card.top + r.card.bottom,
            CLIENT.bottom,
            "the card is not level with the client area"
        );

        // A window narrower than the card: it has to shrink rather than hang
        // off the edge, and still leave the page's own padding on both sides.
        let narrow = RECT {
            right: 300,
            bottom: 500,
            ..CLIENT
        };
        let r = rects(&narrow, DPI);
        assert!(r.card.left >= scale(PAD, DPI), "the card runs off the left");
        assert!(
            r.card.right <= narrow.right - scale(PAD, DPI),
            "the card runs off the right"
        );
    }

    /// Every DPI, because these are hand-laid-out pixels and `scale` truncates.
    #[test]
    fn the_buttons_stay_usable_at_every_scale() {
        for dpi in [96u32, 120, 144, 192] {
            let r = rects(&CLIENT, dpi);
            assert!(
                r.confirm.right - r.confirm.left > 0,
                "the confirm button collapsed at {dpi} dpi"
            );
            assert!(
                r.cancel.right <= r.confirm.left,
                "the buttons met at {dpi} dpi"
            );
            assert!(
                r.body.bottom > r.body.top,
                "the body has no room at {dpi} dpi"
            );
        }
    }

    /// A click resolves to a choice, clears the question, and a second click on
    /// the same spot does nothing — a swallowed prompt must not fire twice.
    #[test]
    fn a_click_resolves_once_and_then_the_popup_is_gone() {
        let mut m = Modal::default();
        m.open(Confirm {
            title: "Stop the process?".into(),
            body: "This ends the process holding the port.".into(),
            action: Action::StopPort {
                pid: 4,
                name: "System".into(),
            },
        });
        assert!(m.is_open());

        let r = rects(&CLIENT, DPI);
        let mid = |b: &RECT| ((b.left + b.right) / 2, (b.top + b.bottom) / 2);

        // Off the buttons: still up, and no answer.
        assert_eq!(m.click(&CLIENT, DPI, r.card.left + 4, r.body.top + 4), None);
        assert!(m.is_open(), "a click on the body dismissed the popup");

        let (x, y) = mid(&r.confirm);
        // The action comes back *with* the answer, not from the popup
        // afterwards — the question is already down by the time anyone could
        // ask which one it was.
        assert_eq!(
            m.click(&CLIENT, DPI, x, y),
            Some(Outcome::Confirmed(Action::StopPort {
                pid: 4,
                name: "System".into()
            }))
        );
        assert!(!m.is_open(), "the question survived its own answer");
        assert_eq!(
            m.click(&CLIENT, DPI, x, y),
            None,
            "the popup answered twice"
        );
    }

    #[test]
    fn cancelling_answers_without_confirming() {
        let mut m = Modal::default();
        m.open(Confirm {
            title: "t".into(),
            body: "b".into(),
            action: Action::StopPort {
                pid: 1,
                name: "n".into(),
            },
        });
        let r = rects(&CLIENT, DPI);
        let (x, y) = (
            (r.cancel.left + r.cancel.right) / 2,
            (r.cancel.top + r.cancel.bottom) / 2,
        );
        assert_eq!(m.click(&CLIENT, DPI, x, y), Some(Outcome::Cancelled));
        assert!(!m.is_open());
    }

    /// The scrim has to move *away* from the page, or it is not a scrim: on a
    /// dark theme a lighter overlay brightens the window behind the card.
    #[test]
    fn the_scrim_darkens_a_dark_theme_and_lightens_a_light_one() {
        let mut cfg = crate::config::Config::default();
        cfg.theme.background = "#101010".into();
        cfg.theme.foreground = "#F0F0F0".into();
        let dark = crate::ui::design::palette(&cfg);
        assert!(
            crate::ui::design::luma(scrim_colour(&dark)) < crate::ui::design::luma(dark.surface),
            "the popup brightened a dark window"
        );

        cfg.theme.background = "#FAFAFA".into();
        cfg.theme.foreground = "#101010".into();
        let light = crate::ui::design::palette(&cfg);
        assert!(
            crate::ui::design::luma(scrim_colour(&light)) > crate::ui::design::luma(light.surface),
            "the popup darkened a light window"
        );
    }
}

//! The frame painter: the window's face, the sidebar, and the selected page.
//!
//! The page itself is laid out by walking a `Canvas` — see `components` — so
//! this file is a list of contents rather than a column of arithmetic, and it
//! never names an absolute row position. The two exceptions are deliberate and
//! commented where they are: the sparkline fills the *remainder* of the window,
//! and the Settings captions have to sit on the bands that `layout_settings`
//! put the controls on.

use crate::ui::components::{
    Canvas, Fonts, card_h, empty_h, head_h, hero_h, lane_h, meter_h, rounded_fill,
};
use crate::ui::design::{
    CARD_PAD, FIELD_INSET, ICON_DATA, ICON_DESKTOP, ICON_LATENCY, ICON_LIVE, ICON_MACHINE,
    ICON_MEMORY, ICON_NETWORK, ICON_PORTS, ICON_SPEED, ICON_STOPWATCH, ICON_STORAGE, ICON_TIMER,
    ICON_TILES, ICON_TRAFFIC, ICON_TUNE, ICON_UP, ICON_DOWN, ICON_USAGE, RADIUS, S2, S3, palette,
};
use crate::power;
use crate::ui::consts::{CTL_H, FIELD_W};
use crate::ui::pages::{
    DATA, NETWORK, OVERVIEW, PAGES, PORTS, SETTINGS, SPEEDTEST, STOPWATCH, SYSTEM, TIMER,
    connection_rows, health_rows, page_shows_graph,
    pct_of, socket_rows, usage_rows, usage_totals,
};
use crate::ui::settings::{
    FIELD_DROP, FIELD_GAP, LABEL_W, PAGE_SUBTITLE, PREFS_CARD_TITLE, ROW_APPEARANCE,
    SET_ROW_LABELS, TILES_BADGE, TILES_CARD_TITLE, TIMER_LABELS, TILE_ROWS, card_inner_x0,
    foot_button_top, prefs_card_h, swatch_colour, swatch_rect, tiles_card_h,
};
use crate::ui::theme::scale;
use crate::ui::{PAD, ROW_H, SPARK_GAP, TITLE_EXTRA, TITLE_PAD, VALUE_OFFSET, UiState};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    CreateSolidBrush, DEFAULT_GUI_FONT, DeleteObject, FillRect, GetDC, GetStockObject, HGDIOBJ,
    MapWindowPoints, NULL_BRUSH, ReleaseDC, SelectObject, SetBkMode, TRANSPARENT,
};
use windows::Win32::UI::WindowsAndMessaging::{GetClientRect, GetDlgItem, GetWindowRect, IsWindowVisible};

/// The band the Overview's history chart fills, in 96-DPI pixels.
///
/// Fixed rather than "whatever is left": the chart lives in a card now, and a
/// card sized to the window's remainder would grow and shrink as the user
/// resizes, moving the cards above it.
const CHART_H: i32 = 130;

/// Draw one frame: background, sidebar, heading, the page's rows, chart, and
/// the confirmation popup if one is up.
///
/// `&mut` for one reason: the Ports page's rows are laid out here and hit-
/// tested in the window procedure, so their bands are recorded as they are
/// drawn. The alternative — the click path re-deriving the same sums — is two
/// answers to one question, which is the bug this avoids by construction.
pub(crate) fn paint(hwnd: HWND, state: &mut UiState) {
    // SAFETY: every GDI object created here is deleted before returning or
    // selected back out into the DC it came from, and the DC is released.
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

        let pal = palette(&state.cfg);
        let pad = scale(PAD, state.dpi);
        let side = crate::ui::theme::sidebar_w(state.dpi);
        let x0 = side + pad;
        let x1 = w - pad;
        if x1 <= x0 {
            return;
        }

        // One DC for the whole frame, so the background cannot land a frame
        // behind the text drawn on it.
        let dc = GetDC(Some(hwnd));
        if dc.is_invalid() {
            return;
        }
        let brush = CreateSolidBrush(pal.surface);
        FillRect(dc, &rect, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
        SetBkMode(dc, TRANSPARENT);

        crate::ui::sidebar::paint(dc, &rect, state);

        let page = state.page.min(PAGES.len() - 1);
        let row_h = scale(ROW_H, state.dpi);
        let fonts = Fonts {
            body: state.font,
            bold: state.bold,
            title: state.title,
            // The caption shares the body face: a fourth face would need a fourth
            // `HFONT` in `UiState` and a fourth branch in `rebuild_fonts`, which
            // is not worth one header above a day list.
            // ponytail: caption == body; add a face when a page needs two.
            caption: state.font,
            clock: state.clock,
            // Not a field of `UiState` like the four above: the icon face is
            // built and cached in `design` from the config's font size, and the
            // sidebar reads the same one. A fifth handle here would be a second
            // place for it to be built at a second size.
            icon: crate::ui::design::icon_font(&state.cfg, state.dpi),
        };
        // The heading gets its own band — a title is larger than a row and has
        // air under it — and everything after it is rows on `row_h` bands,
        // nudge down from the band's top so a label and its value look level.
        let mut c = Canvas::new(
            dc,
            &fonts,
            &pal,
            x0,
            x1,
            row_h,
            scale(VALUE_OFFSET, state.dpi),
            pad,
        );
        // A title is larger than a row, and the first band is laid out from its
        // *top* — so the title would sit high in its band with a gap under it.
        // The gap above it is padded instead, which is what makes the block of
        // title and rows look centred in the window rather than pushed to the
        // ceiling. `layout_settings` moves its controls by the same amount.
        c.space(scale(TITLE_PAD, state.dpi));
        // Before the heading, because the pill rides the title's own band: it
        // claims no space, so whichever is drawn first is simply underneath.
        // The heading is left-aligned and the pill is pinned right, so the two
        // share the band without meeting.
        if page == OVERVIEW {
            c.date_pill(state.dpi, &power::today_label());
        }
        c.heading(PAGES[page], scale(ROW_H + TITLE_EXTRA * 2, state.dpi));

        // An over-quota window is red top to bottom, not red in a footnote: the
        // colour is a property of the page, so it is set once here.
        if state.model.quota_alert {
            c.emphasise(pal.danger);
        }

        // There is no generic row list any more. Every page but Settings draws
        // itself as cards below — Settings draws native controls over its own
        // painted captions — so a page-wide loop over a row table would print
        // the same figures twice on every page that has a card, and the only
        // thing left for it to draw would be a page that has neither. The
        // exclusivity that used to need a test (`page_has_cards` against
        // `page_rows`) is structural now: there is no second mechanism to
        // disagree with the first.

        // --- Overview: three cards, each one a group of readings ------------
        //
        // Every card is sized from its own contents (`card_h` and the three
        // `*_h` helpers) rather than from a number written here, because a card
        // whose height and contents disagree draws its last row through its own
        // bottom edge — and the disagreement is invisible until a feature is
        // switched off and a row disappears.
        if page == OVERVIEW {
            let m = &state.model;
            let dpi = state.dpi;
            c.subtitle("Live traffic and load at a glance");

            // Traffic: three readings as one lane. The lane needs all three, so
            // a machine whose latency probe is off falls back to rows rather
            // than showing two tiles and a gap where the third belongs.
            let tiles = [
                (pal.tile_blue, ICON_DOWN, m.down_text.as_str(), "Download"),
                (pal.tile_violet, ICON_UP, m.up_text.as_str(), "Upload"),
                (pal.tile_amber, ICON_LATENCY, m.latency_text.as_str(), "Latency"),
            ];
            let traffic: Vec<_> = tiles.iter().filter(|t| !t.2.is_empty()).collect();
            if !traffic.is_empty() {
                let inner = if traffic.len() == 3 {
                    head_h(dpi) + lane_h(dpi)
                } else {
                    head_h(dpi) + traffic.len() as i32 * row_h
                };
                c.card(dpi, card_h(dpi, inner), |c| {
                    c.card_head(dpi, ICON_TRAFFIC, pal.tile_blue, "Traffic", "Live");
                    if traffic.len() == 3 {
                        c.stat_lane(dpi, tiles);
                    } else {
                        for (_, _, value, caption) in &traffic {
                            c.row(caption, value);
                        }
                    }
                });
            }

            // Usage: the two live loads, as meters. A proportion is the whole
            // reading here — "40%" as a number is a figure, the same figure as a
            // bar is a load — so both go through `meter` and neither as a row.
            let loads = [
                ("CPU", m.cpu_text.as_str(), pal.tile_blue),
                ("RAM", m.ram_text.as_str(), pal.tile_green),
            ];
            let live: Vec<_> = loads.iter().filter(|l| !l.1.is_empty()).collect();
            if !live.is_empty() {
                let inner = head_h(dpi) + live.len() as i32 * meter_h(row_h, dpi);
                c.card(dpi, card_h(dpi, inner), |c| {
                    // The right slot carries the first reading, so the card's
                    // head says something rather than repeating its own title.
                    let lead = live[0].1;
                    c.card_head(dpi, ICON_USAGE, pal.tile_green, "Usage", lead);
                    for (label, value, tint) in &live {
                        c.meter(dpi, label, value, pct_of(value), *tint);
                    }
                });
            }

            // History: the chart, inside a card rather than filling the window's
            // remainder. A fixed band instead, so the card is the same height
            // whether or not the history has filled in yet — a chart that grew
            // the card as samples arrived would move everything below it.
            let chart_inner = scale(CHART_H, dpi);
            c.card(dpi, card_h(dpi, head_h(dpi) + chart_inner + scale(S2, dpi)), |c| {
                let right = crate::ui::chart::span_label(m.history.len(), state.cfg.interval_ms);
                c.card_head(dpi, ICON_LIVE, pal.tile_violet, "History", &right);
                c.space(scale(S2, dpi));
                let top = c.y();
                let area = RECT {
                    left: c.x0,
                    top,
                    right: c.x1,
                    bottom: top + chart_inner,
                };
                // The answer is ignored on purpose: a frame with one sample in
                // it draws no chart, and the card keeps its band either way.
                let _ = crate::ui::chart::paint(c, area, &m.history, state.cfg.interval_ms, dpi);
            });
        }

        // --- System: what this machine is, in cards -------------------------
        if page == SYSTEM {
            let m = &state.model;
            let dpi = state.dpi;
            c.subtitle(&m.computer_text);

            let live = [
                ("CPU", m.cpu_text.as_str(), pal.tile_blue),
                ("RAM", m.ram_text.as_str(), pal.tile_violet),
            ];
            let live: Vec<_> = live.iter().filter(|l| !l.1.is_empty()).collect();
            if !live.is_empty() {
                let inner = head_h(dpi) + live.len() as i32 * meter_h(row_h, dpi);
                c.card(dpi, card_h(dpi, inner), |c| {
                    c.card_head(dpi, ICON_LIVE, pal.tile_blue, "Live", "Now");
                    for (label, value, tint) in &live {
                        c.meter(dpi, label, value, pct_of(value), *tint);
                    }
                });
            }

            let hardware = [
                ("Processor", m.cpu_name_text.as_str()),
                ("Cores", m.cores_text.as_str()),
                ("Graphics", m.gpu_text.as_str()),
            ];
            let hardware: Vec<_> = hardware.iter().filter(|h| !h.1.is_empty()).collect();
            if !hardware.is_empty() {
                let inner = head_h(dpi) + hardware.len() as i32 * row_h;
                c.card(dpi, card_h(dpi, inner), |c| {
                    c.card_head(dpi, ICON_MEMORY, pal.tile_violet, "Hardware", "");
                    for (label, value) in &hardware {
                        c.row(label, value);
                    }
                });
            }

            // The machine's own identity, with the update notice inside it: the
            // notice is about the build named two rows above, not about the
            // machine, but it is the same subject and a card of its own for one
            // line would be more furniture than content.
            let newer = crate::update::available();
            let machine = [
                ("Computer", m.computer_text.as_str()),
                ("Windows", m.windows_text.as_str()),
                ("Uptime", m.uptime_text.as_str()),
                ("Version", m.version_text.as_str()),
            ];
            let machine: Vec<_> = machine.iter().filter(|h| !h.1.is_empty()).collect();
            if !machine.is_empty() || newer.is_some() {
                let inner = head_h(dpi)
                    + (machine.len() + usize::from(newer.is_some())) as i32 * row_h;
                c.card(dpi, card_h(dpi, inner), |c| {
                    c.card_head(dpi, ICON_DESKTOP, pal.tile_green, "This machine", "");
                    for (label, value) in &machine {
                        c.row(label, value);
                    }
                    // Where to get a newer build, when one was found. The row
                    // above already says *that* there is one — `0.7.1 → 0.8.0` —
                    // so this line is only the next step. Absent entirely
                    // otherwise: there is no "up to date" line, because a page
                    // that says "you are current" every time you open it has
                    // taught you to stop reading it.
                    if let Some(version) = newer {
                        c.note(
                            &format!(
                                "Version {version} is available \u{2014} {}",
                                crate::update::DOWNLOADS
                            ),
                            pal.accent,
                        );
                    }
                });
            }

            // Power, only on a machine that has any. A desktop reports neither
            // and gets no card, rather than an empty one saying nothing.
            let power_rows = [
                ("Battery", m.battery_text.as_str()),
                ("Power", m.power_text.as_str()),
            ];
            let power_rows: Vec<_> = power_rows.iter().filter(|p| !p.1.is_empty()).collect();
            if !power_rows.is_empty() {
                let inner = head_h(dpi) + power_rows.len() as i32 * row_h;
                c.card(dpi, card_h(dpi, inner), |c| {
                    c.card_head(dpi, ICON_MACHINE, pal.tile_amber, "Power", "");
                    for (label, value) in &power_rows {
                        c.row(label, value);
                    }
                });
            }

            if !m.disks.is_empty() {
                let inner = head_h(dpi) + m.disks.len() as i32 * row_h;
                c.card(dpi, card_h(dpi, inner), |c| {
                    c.card_head(dpi, ICON_STORAGE, pal.tile_amber, "Storage", "");
                    for (mount, usage) in &m.disks {
                        c.row(mount, usage);
                    }
                });
            }
        }

        // --- Network: what the machine is plugged into, then what is on it -----
        if page == NETWORK {
            let dpi = state.dpi;
            // The adapter leads as the subtitle because it is the subject of
            // every address in the card below: six numbers with nothing attached
            // to them is the failure this page exists to avoid.
            c.subtitle(&state.model.adapter_text);

            // Connection: the identity of the link, top to bottom — which
            // network, through which interface, on which address, through which
            // router, with which resolvers. Read as one object rather than as
            // five unrelated rows.
            let connection = connection_rows(&state.model);
            if !connection.is_empty() {
                let inner = head_h(dpi) + connection.len() as i32 * row_h;
                c.card(dpi, card_h(dpi, inner), |c| {
                    c.card_head(dpi, ICON_NETWORK, pal.tile_blue, "Connection", "");
                    for (label, value) in &connection {
                        c.row(label, value);
                    }
                });
            }

            // Traffic: the three live readings as one lane — the same three the
            // Overview leads with, in the same tints, so a number means one thing
            // on both pages. A machine with any of the three switched off falls
            // back to rows rather than to two tiles and a hole where the third
            // belongs.
            let tiles = [
                (pal.tile_blue, ICON_DOWN, state.model.down_text.as_str(), "Download"),
                (pal.tile_violet, ICON_UP, state.model.up_text.as_str(), "Upload"),
                (pal.tile_amber, ICON_LATENCY, state.model.latency_text.as_str(), "Latency"),
            ];
            let traffic: Vec<_> = tiles.iter().filter(|t| !t.2.is_empty()).collect();
            if !traffic.is_empty() {
                let inner = if traffic.len() == 3 {
                    head_h(dpi) + lane_h(dpi)
                } else {
                    head_h(dpi) + traffic.len() as i32 * row_h
                };
                c.card(dpi, card_h(dpi, inner), |c| {
                    c.card_head(dpi, ICON_TRAFFIC, pal.tile_violet, "Traffic", "");
                    if traffic.len() == 3 {
                        c.stat_lane(dpi, tiles);
                    } else {
                        for (_, _, value, caption) in &traffic {
                            c.row(caption, value);
                        }
                    }
                });
            }

            // Health: about the path rather than the load on it. Its own card
            // and only when it has something in it — a machine whose probe is
            // off has no half of it to show, and an empty plate over nothing is
            // furniture.
            let health = health_rows(&state.model);
            if !health.is_empty() {
                let inner = head_h(dpi) + health.len() as i32 * row_h;
                c.card(dpi, card_h(dpi, inner), |c| {
                    c.card_head(dpi, ICON_LATENCY, pal.tile_green, "Health", "");
                    for (label, value) in &health {
                        c.row(label, value);
                    }
                });
            }
        }

        // --- Data: the totals, then the days they are made of ------------------
        if page == DATA {
            let m = &state.model;
            let dpi = state.dpi;
            c.subtitle("What this machine has moved");

            // Totals: today leads because it is the one that moves, the month is
            // the context that makes it mean something, and the plan is the
            // context for the month. The plan is absent on a machine without
            // one, so the card is two rows where the user has set no allowance.
            let totals = usage_totals(&state.cfg, m);
            if !totals.is_empty() {
                let inner = head_h(dpi) + totals.len() as i32 * row_h;
                c.card(dpi, card_h(dpi, inner), |c| {
                    c.card_head(dpi, ICON_USAGE, pal.tile_amber, "Usage", "");
                    for (label, value) in &totals {
                        c.row(label, value);
                    }
                });
            }

            // The day-by-day breakdown. `usage_rows` is already trimmed to the
            // last `USAGE_ROWS` days, so this card is bounded at seven rows and
            // needs no scrolling — which is the only reason a card can hold the
            // list at all. No `section` caption: the card's own head names it,
            // and a caption under a heading is the same word twice.
            let days = usage_rows(m);
            let body = if days.is_empty() {
                empty_h(row_h, dpi)
            } else {
                days.len() as i32 * row_h
            };
            c.card(dpi, card_h(dpi, head_h(dpi) + body), |c| {
                c.card_head(dpi, ICON_DATA, pal.tile_blue, "Daily breakdown", "");
                if days.is_empty() {
                    c.empty("No usage recorded yet.");
                } else {
                    for (day, bytes) in &days {
                        c.row(day, &crate::telemetry::usage::format_size(*bytes));
                    }
                }
            });
        }

        // --- Ports: the totals, then the list they summarise -------------------
        //
        // The rows are also the page's selection: clicking one aims the Stop
        // button at the process holding it, so their bands are recorded here
        // rather than recomputed by the click path. `clear` is outside the page
        // test because a selection on a page the reader has left is a selection
        // by position, and the bands it names belong to the page just hidden.
        state.port_rows.clear();
        if page == PORTS {
            let dpi = state.dpi;
            // The Stop button is pinned to the foot, so nothing on this page may
            // be drawn under it: a card's bottom edge behind a button is an edge
            // the reader cannot see, and a row there is a row that cannot be
            // clicked.
            let limit = foot_button_top(h, dpi) - scale(S3, dpi);
            c.subtitle("What this machine is listening on");

            let counters = socket_rows(&state.model);
            if !counters.is_empty() {
                let inner = head_h(dpi) + counters.len() as i32 * row_h;
                c.card(dpi, card_h(dpi, inner), |c| {
                    c.card_head(dpi, ICON_PORTS, pal.tile_blue, "Sockets", "");
                    for (label, value) in &counters {
                        c.row(label, value);
                    }
                });
            }

            let total = state.model.open_ports.len();
            if total == 0 {
                let body = empty_h(row_h, dpi);
                c.card(dpi, card_h(dpi, head_h(dpi) + body), |c| {
                    c.card_head(dpi, ICON_PORTS, pal.tile_green, "Active ports", "");
                    c.empty("No listening ports were found.");
                });
            } else {
                // Offered only when its head and two rows fit above the Stop
                // button. Two and not one: a card whose list has been trimmed to
                // a single port has been trimmed to a caption, and the reader
                // learns less from it than from the window simply being too
                // short.
                let min_body = head_h(dpi) + 2 * row_h;
                if c.y() + card_h(dpi, min_body) <= limit {
                    // Counted before the card is opened, because the offset has
                    // to be clamped against the room the list will actually
                    // have: a port closing between two samples shrinks the list,
                    // and an offset past its end would draw an empty plate with
                    // a caption over it and no way back up.
                    let body_top = c.y() + scale(CARD_PAD, dpi) + head_h(dpi);
                    let room = limit - scale(CARD_PAD, dpi) - body_top;
                    let visible = (room / row_h).max(2) as usize;
                    state.port_scroll = state.port_scroll.min(total.saturating_sub(visible));
                    let scroll = state.port_scroll;
                    let showing = visible.min(total - scroll);

                    // The position travels in the card's own right slot rather
                    // than in a caption above the list: the list is the tall part
                    // of this page and should keep the room, and the head is
                    // already there.
                    let position = if showing < total {
                        format!("{}-{} of {total}  (scroll)", scroll + 1, scroll + showing)
                    } else {
                        String::new()
                    };
                    c.card(dpi, card_h(dpi, head_h(dpi) + showing as i32 * row_h), |c| {
                        c.card_head(dpi, ICON_PORTS, pal.tile_green, "Active ports", &position);
                        // The card's own inset is the list's edge now, so every
                        // band recorded here is inside the plate. Recorded from
                        // the inset column and not the page's: a hit test against
                        // the page's would arm the Stop button at a port the
                        // reader clicked the card's padding next to.
                        let (left, right) = (c.x0, c.x1);
                        for entry in state.model.open_ports.iter().skip(scroll).take(showing) {
                            let top = c.y();
                            state.port_rows.push((
                                RECT { left, top, right, bottom: top + row_h },
                                entry.pid,
                            ));
                            if state.selected_port == Some(entry.pid) {
                                // Behind the text, not around it: a ring drawn
                                // after the row would clip the descenders of its
                                // own label.
                                c.pill();
                            }
                            c.row(&entry.port, &entry.owner);
                        }
                    });
                }
            }

            // What the last stop did, under everything. A failed one is the
            // expected case rather than the strange one — an unelevated process
            // cannot end a service — so it is a sentence, not an alarm. Outside
            // the cards because it is the result of the page, not a property of
            // any card on it.
            if let Some(notice) = &state.port_notice {
                let colour = if notice.starts_with("stopped") { pal.muted } else { pal.danger };
                c.note(notice, colour);
            }
        }

        // The Speed Test page, above the Run button pinned to the foot of the
        // column. Everything here is conditional on a run having happened, so
        // the rows would otherwise rearrange themselves under the reader's eyes
        // — the price of the button being placed rather than laid out in this
        // list. Nothing may be drawn past the button's own top, which is why
        // the history list is trimmed to the room left rather than allowed to
        // run under it.
        if page == SPEEDTEST {
            let m = &state.model;
            let dpi = state.dpi;
            let limit = foot_button_top(h, dpi) - scale(S3, dpi);
            c.subtitle("Measured throughput and ping");

            let tiles = [
                (pal.tile_blue, ICON_DOWN, m.speed_down_text.as_str(), "Download"),
                (pal.tile_violet, ICON_UP, m.speed_up_text.as_str(), "Upload"),
                (pal.tile_amber, ICON_LATENCY, m.speed_latency_text.as_str(), "Latency"),
            ];
            let live: Vec<_> = tiles.iter().filter(|t| !t.2.is_empty()).collect();
            // Nothing has happened yet: no run in flight, no failure, no result.
            // Asked as one question rather than as three so the card's height and
            // the card's body cannot answer it differently.
            let idle = !m.speed_running && m.speed_error_text.is_empty() && live.is_empty();

            let readings = if idle {
                empty_h(row_h, dpi)
            } else if live.len() == 3 {
                lane_h(dpi)
            } else {
                live.len() as i32 * row_h
            };
            // The three parts are each exactly one band: the progress row
            // carries its own track inside its band, the failure note is a line,
            // and the readings are a lane or a run of rows. Summed as terms, so a
            // part that disappears takes its height with it.
            let body = head_h(dpi)
                + if m.speed_running { row_h } else { 0 }
                + if m.speed_error_text.is_empty() { 0 } else { row_h }
                + readings;

            c.card(dpi, card_h(dpi, body), |c| {
                // The head's right slot is where the state goes: the phase while
                // a run is in flight, the time it finished once it is done. It is
                // the one place on the card that can change without moving
                // anything below it.
                let right = if m.speed_running {
                    m.speed_phase_text.clone()
                } else if m.speed_when_text.is_empty() {
                    String::new()
                } else {
                    format!("Ran {}", m.speed_when_text)
                };
                c.card_head(dpi, ICON_SPEED, pal.tile_blue, "Performance", &right);

                if m.speed_running {
                    // The phase is the row's *label*, not a row of its own:
                    // "which phase" and "how far through it" are one reading, and
                    // one band is what they get.
                    let pct = m.speed_percent.min(100);
                    let label = if m.speed_phase_text.is_empty() {
                        "Progress"
                    } else {
                        m.speed_phase_text.as_str()
                    };
                    c.progress(label, &format!("{pct}%"), pct);
                }
                // A failure is a line in the card, not a footnote under it: it is
                // the result of the run, and it is the thing the reader opened
                // the page to find out.
                if !m.speed_error_text.is_empty() {
                    c.note(&m.speed_error_text, pal.danger);
                }
                if idle {
                    c.empty("No run yet. Press Run test to start.");
                } else if live.len() == 3 {
                    c.stat_lane(dpi, tiles);
                } else {
                    for (_, _, value, caption) in &live {
                        c.row(caption, value);
                    }
                }
            });

            // History, trimmed *before* the card is opened rather than during
            // it: a card's height is the caller's, so the list has to know how
            // many rows it will have before the plate under them is drawn. The
            // room is what is left above the Run button once this card's own
            // padding and head are off it.
            let room = limit - c.y() - scale(2 * CARD_PAD, dpi) - head_h(dpi);
            let fits = ((room / row_h).max(0) as usize).min(m.speed_history.len());
            if m.speed_history.is_empty() {
                let body = empty_h(row_h, dpi);
                if c.y() + card_h(dpi, head_h(dpi) + body) <= limit {
                    c.card(dpi, card_h(dpi, head_h(dpi) + body), |c| {
                        c.card_head(dpi, ICON_TRAFFIC, pal.tile_violet, "Runs", "");
                        c.empty("No runs recorded yet.");
                    });
                }
            } else if fits > 0 {
                c.card(dpi, card_h(dpi, head_h(dpi) + fits as i32 * row_h), |c| {
                    c.card_head(dpi, ICON_TRAFFIC, pal.tile_violet, "Runs", "");
                    for (when, result) in m.speed_history.iter().take(fits) {
                        c.row(when, result);
                    }
                });
            }
        }

        // The Stopwatch page. Its whole content is the clock, so it is drawn
        // where the button that drives it sits rather than in the row list: a
        // clock is a readout *and* a control, and a number up here with its
        // Start 400 pixels below it in the corner would read as two features.
        // The button itself is a real child window, placed at the foot by
        // `layout_settings`.
        if page == STOPWATCH {
            let dpi = state.dpi;
            let running = state.watch.is_running();
            let resting = !running && state.watch.elapsed().is_zero();
            c.subtitle("Elapsed time");

            let digits = state.watch.text(running);
            // The hint is a row of the card and not a line under it: when there
            // is nothing to read, the card is what remains, and a card holding a
            // zero and nothing else says less than the sentence that tells you
            // what to press.
            let body = head_h(dpi) + hero_h(dpi) + if resting { empty_h(row_h, dpi) } else { 0 };
            c.card(dpi, card_h(dpi, body), |c| {
                c.card_head(dpi, ICON_STOPWATCH, pal.tile_violet, "Stopwatch", "");
                // The digits go through the hero and not through a row: the face
                // is `CLOCK_EXTRA` above the body, and a band laid out for one
                // line of body text would clip them. The clock is *also* the
                // control this page is named for, which is why the card and the
                // Start button are the only two things on it.
                c.hero(dpi, &digits);
                if resting {
                    c.empty("Press Start to begin.");
                }
            });
        }

        // The Timer page. First the four captions, on the bands
        // `layout_settings` puts their controls on — the same arithmetic as the
        // Settings page below, from the same `form_top` and the same
        // `FIELD_DROP`, because a control placed by one function and labelled by
        // another has only those constants keeping them together.
        if page == TIMER {
            let dpi = state.dpi;
            // The four captions, on the bands `layout_settings` puts their
            // controls on — the same arithmetic as the Settings page, from the
            // same `form_top` and the same `FIELD_DROP`, because a control
            // placed by one function and labelled by another has only those
            // constants keeping them together.
            for label in TIMER_LABELS.iter() {
                c.row_aligned(label, "", scale(FIELD_DROP, dpi));
            }

            // What the four above add up to, in a card under them. Read from
            // the config and not from the controls, so the card can only ever
            // name an instant that was actually saved — a typed `23:00` that was
            // never armed is not a timer, and the page must not draw it as one.
            let t = state.cfg.timer.clone();
            let action = power::action_of(&t).label();
            let target = if t.enabled { power::fire_at(&t, &power::now()) } else { None };
            let countdown = target.map(|when| {
                let secs = power::seconds_until(&when, &power::now());
                (power::format_stamp(&when), power::format_countdown(secs))
            });

            // The instant in the clock's own face, as on the Stopwatch page: it
            // is the number this card exists to show, and a row would clip a
            // face taller than a band of body text. The countdown under it is
            // recomputed from the same instant the watcher grades, so the number
            // on screen and the number in the confirmation are one number.
            let body = head_h(dpi)
                + match &countdown {
                    Some(_) => hero_h(dpi) + row_h,
                    None => empty_h(row_h, dpi),
                };
            // Only when it clears the Arm button. A card whose bottom edge is
            // behind a child window is a card the reader sees the top half of,
            // and the window it happens in is the short one — where the form
            // above already fills the page and the card is the part that can go.
            if c.y() + card_h(dpi, body) <= foot_button_top(h, dpi) {
                c.card(dpi, card_h(dpi, body), |c| {
                    // The right slot carries the state, which is the one thing
                    // here that changes without the rest of the card moving:
                    // what it will do, or that it is off.
                    let right = if t.enabled { action } else { "Off" };
                    c.card_head(dpi, ICON_TIMER, pal.tile_amber, "Power timer", right);
                    match &countdown {
                        Some((stamp, left)) => {
                            c.hero(dpi, stamp);
                            // The action is already in the head, so the row does
                            // not repeat it: "Sleep" twice on one card is one
                            // word too many.
                            c.row("Fires in", left);
                        }
                        None if t.enabled => c.empty("No target time set."),
                        None => c.empty("Off. Press Arm to switch it on."),
                    }
                });
            }
        }

        // The Settings page: a header, then two cards. The controls are native
        // children and are placed by `layout_settings`; every band they sit on
        // is claimed here either as a caption or as deliberate blank space, so
        // the cards' heights and the controls' rows are one expression seen from
        // two sides.
        if page == SETTINGS {
            let dpi = state.dpi;
            let form = &state.settings;

            c.subtitle(PAGE_SUBTITLE);

            // --- card 1: the tile grid ------------------------------------------
            c.card(dpi, tiles_card_h(dpi), |c| {
                c.card_head(dpi, ICON_TILES, pal.tile_blue, TILES_CARD_TITLE, TILES_BADGE);
                // The checkboxes carry their own labels, so nothing is drawn on
                // these bands — they are claimed so that the card is exactly as
                // tall as the grid `layout_settings` walks.
                for _ in 0..TILE_ROWS {
                    c.space(row_h);
                }
            });

            // --- card 2: the form -----------------------------------------------
            let ix0 = card_inner_x0(x0, dpi);
            let field_x = ix0 + scale(LABEL_W, dpi);
            let field_w = scale(FIELD_W, dpi);
            let gap = scale(FIELD_GAP, dpi);
            let ctl_h = scale(CTL_H, dpi);
            let nudge = scale(VALUE_OFFSET, dpi);
            c.card(dpi, prefs_card_h(dpi), |c| {
                c.card_head(dpi, ICON_TUNE, pal.tile_violet, PREFS_CARD_TITLE, "");
                for (row, label) in SET_ROW_LABELS.iter().enumerate() {
                    let top = c.y();
                    if label.is_empty() {
                        // No card-2 row is blank today, but the page is a table
                        // and the next row added to it should not have to know
                        // that. A blank caption claims its band and nothing else.
                        c.space(row_h);
                        continue;
                    }
                    if row == ROW_APPEARANCE {
                        // The one caption here that leads with a glyph, and the
                        // one that names the mode the button beside it switches
                        // *to* rather than the setting it edits. Read from the
                        // field under it and not from the saved config, so a
                        // background the user has picked but not yet saved
                        // already decides which way the toggle goes.
                        let (next_dark, caption, _, _) =
                            crate::ui::design::next_preset(&form.background());
                        c.row_glyph(
                            crate::ui::design::theme_glyph(next_dark),
                            caption,
                            scale(FIELD_DROP, dpi),
                        );
                        continue;
                    }
                    // The caption drops to the middle of the field beside it: a
                    // native field centres its own text while a row draws from
                    // the top of its band, and at this one place on the page a
                    // caption sits *beside* a control rather than above it.
                    c.row_aligned(label, "", scale(FIELD_DROP, dpi));
                    // The three colour rows preview what is typed beside them,
                    // painted between the box and the Pick button — the column
                    // `layout_settings` leaves free for exactly this. On the band
                    // the caption just claimed, so the swatch moves with the row
                    // and not with a sum written here. A frame first: a black
                    // swatch on a dark card is a swatch that vanished.
                    if let Some(colour) = swatch_colour(row, form) {
                        let r = swatch_rect(field_x, field_w, gap, top + nudge, ctl_h, dpi);
                        let radius = scale(RADIUS, dpi) / 2;
                        rounded_fill(c.dc, r, radius, pal.border);
                        rounded_fill(
                            c.dc,
                            RECT {
                                left: r.left + 1,
                                top: r.top + 1,
                                right: r.right - 1,
                                bottom: r.bottom - 1,
                            },
                            radius,
                            colour,
                        );
                    }
                }
            });

            // The notice sits under the second card rather than beside a button:
            // it is the result of the whole page, and it needs the full width to
            // name a path that did not write.
            if let Some(notice) = &state.notice {
                let colour = if notice.starts_with("saved") || notice.starts_with("reset") {
                    pal.text
                } else {
                    pal.danger
                };
                c.note(notice, colour);
            }
        }

        // The chart fills whatever room is left, so the picture grows with the
        // window instead of sitting in a fixed corner. It decides for itself
        // whether the rectangle is big enough once its gutter and its time strip
        // are taken out of it, which is a question only it can answer.
        let spark_gap = scale(SPARK_GAP, state.dpi);
        let spark_top = c.y() + spark_gap;
        let spark_h = h - spark_gap - pad - spark_top;
        if page_shows_graph(page) && spark_h > 0 && x1 > x0 {
            crate::ui::chart::paint(
                &c,
                RECT {
                    left: x0,
                    top: spark_top,
                    right: x1,
                    bottom: spark_top + spark_h,
                },
                &state.model.history,
                state.cfg.interval_ms,
                state.dpi,
            );
        }

        // The edit boxes' wells, after the page and before the popup: an `EDIT`
        // is the one control class that cannot be owner-drawn, so its rounded
        // border is painted here on the rectangle the control already has. After
        // the page, because the well is a shape cut into the card the walk just
        // drew; before the popup, because a field outlined through the scrim
        // would be the one thing on the window the modal fails to cover.
        //
        // Read from the controls rather than from `layout_settings`' arithmetic:
        // there is one placement of a field in this program and it is the one
        // the window made, so re-deriving the rectangle here would be a second
        // answer that could disagree by a pixel and look like a rendering fault.
        let wells = crate::ui::settings::EDIT_IDS.map(|id| {
            // SAFETY: `hwnd` is our own window and `id` names our own child; the
            // rect and points are written only when the calls succeed.
            let ctl = match GetDlgItem(Some(hwnd), id) {
                Ok(ctl) => ctl,
                Err(_) => return None,
            };
            // A field on another page is still laid out but hidden, and a well
            // painted for a control nobody can see is a plate on the wrong
            // page's rows.
            if !IsWindowVisible(ctl).as_bool() {
                return None;
            }
            let mut r = RECT::default();
            if GetWindowRect(ctl, &mut r).is_err() {
                return None;
            }
            // Screen pixels to client pixels: the DC is the window's, so a well
            // drawn from the screen rect would be off by the frame's own origin.
            let mut pts = [POINT { x: r.left, y: r.top }, POINT { x: r.right, y: r.bottom }];
            MapWindowPoints(None, Some(hwnd), &mut pts);
            Some(RECT {
                left: pts[0].x,
                top: pts[0].y,
                right: pts[1].x,
                bottom: pts[1].y,
            })
        });
        for mut r in wells.into_iter().flatten() {
            // Grown by the ring `FIELD_INSET` leaves visible: the control covers
            // its own rectangle, so the outline has to stand *outside* it to be
            // seen at all, and the fill under it is what the control's own
            // background brush agrees with.
            let inset = scale(FIELD_INSET, state.dpi);
            r.left -= inset;
            r.top -= inset;
            r.right += inset;
            r.bottom += inset;
            crate::ui::components::well(dc, r, &pal, state.dpi);
        }

        // The confirmation popup, last and over everything. It dims the whole
        // client area rather than the content column, because the sidebar is
        // part of what the popup is standing in front of — and a modal that
        // leaves its own window's navigation clickable is not modal.
        crate::ui::modal::paint(dc, &rect, &state.modal, &fonts, &pal, state.dpi);

        // Leave the DC holding stock objects rather than ours.
        SelectObject(dc, GetStockObject(NULL_BRUSH));
        SelectObject(dc, GetStockObject(DEFAULT_GUI_FONT));
        ReleaseDC(Some(hwnd), dc);
    }
}

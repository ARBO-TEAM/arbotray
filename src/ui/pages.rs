//! Which rows each page shows, and the Data page's day list.

use crate::taskbar::TrayModel;

/// The pages, in the order the list shows them. Their indices are the page
/// numbers used throughout, so the constants below name the slots rather than
/// leaving magic numbers in the row functions.
///
/// `Settings` is **appended**. These indices are positional, so inserting it
/// anywhere but the end would renumber every page after it — the labels would
/// still read correctly and the routing would be wrong.
pub(crate) const PAGES: [&str; 5] = ["Overview", "Network", "System", "Data", "Settings"];
pub(crate) const OVERVIEW: usize = 0;
pub(crate) const NETWORK: usize = 1;
pub(crate) const SYSTEM: usize = 2;
pub(crate) const DATA: usize = 3;
pub(crate) const SETTINGS: usize = 4;

// --- pages ----------------------------------------------------------------

/// The rows for one page, in order, skipping anything switched off.
///
/// Splitting them is the point of the sidebar: the traffic numbers are what
/// people open this for, so they lead the overview, and the adapter and
/// hardware detail moves one click away instead of burying them.
pub(crate) fn page_rows(page: usize, model: &TrayModel) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut push = |label: &'static str, text: &str| {
        if !text.is_empty() {
            out.push((label, text.to_string()));
        }
    };
    match page {
        NETWORK => {
            if let Some(name) = &model.wifi_name {
                push("Network", name);
            }
            // The interface first, then what is on it: "which adapter is this"
            // is the question every address below is an answer to, and the
            // local IP sits directly above the gateway so the two ends of the
            // connection read as a pair.
            push("Adapter", &model.adapter_text);
            push("IP", &model.ip_text);
            push("DNS", &model.dns_text);
            push("Wi-Fi", &model.wifi_text);
            push("Download", &model.down_text);
            push("Upload", &model.up_text);
            push("Gateway", &model.gateway_text);
            push("Latency", &model.latency_text);
            // Right after the gateway it depends on: they are read together to
            // tell a local fault from a provider one.
            push("Internet", &model.internet_text);
            push("Loss", &model.loss_text);
        }
        // Ordered as the questions get asked: what this machine is, what is
        // running in it, and how long it has been up. The two live metrics lead
        // because they are the ones that move — everything under them is a
        // reading of something that does not.
        SYSTEM => {
            push("CPU", &model.cpu_text);
            push("RAM", &model.ram_text);
            push("Processor", &model.cpu_name_text);
            push("Cores", &model.cores_text);
            push("Graphics", &model.gpu_text);
            push("Computername", &model.computer_text);
            push("Windows", &model.windows_text);
            push("Uptime", &model.uptime_text);
            push("Battery", &model.battery_text);
            push("Power", &model.power_text);
        }
        DATA => {
            push("Today", &model.usage_text);
            // The caption carries the caveat, not the number. When the file no
            // longer reaches back to the first of the month the sum is of the
            // recent past only, and "Month so far" says so where a reader will
            // actually look.
            push(month_label(model), &model.month_text);
        }
        // The Settings page has no metric on it: every line it shows is a
        // caption from `SET_ROW_LABELS` beside a control. Without this arm it
        // would fall through to the overview below and paint the traffic
        // figures in the gaps between its own fields.
        SETTINGS => {}
        // OVERVIEW, and the fallback for an index that cannot happen: showing
        // the traffic is always better than showing nothing.
        _ => {
            push("Download", &model.down_text);
            push("Upload", &model.up_text);
            push("Latency", &model.latency_text);
            push("CPU", &model.cpu_text);
            push("RAM", &model.ram_text);
        }
    }
    out
}

/// The heading a row opens, or `None` for a row that continues the group above
/// it.
///
/// A metric page is a wall of label-value pairs, and ten of them with nothing
/// between them is a wall ten rows tall. Grouping turns each page into a few
/// short blocks — and it costs nothing, because a heading is drawn *at* the row
/// it names rather than above it. Nothing moves, so the order a page reads in
/// cannot change because of where a caption went.
///
/// Each group is anchored on its own first row, so a group whose first row is
/// switched off — or blank, because the collector had nothing to say — has no
/// header at all, rather than leaving one hanging over unrelated rows.
///
/// `prev` is the heading the page has already drawn, and is what lets **two**
/// rows open one group: the SSID row and the adapter row both begin
/// "Connection", and which of them is present depends on whether the machine is
/// on Wi-Fi. Whichever comes first draws the heading and the other continues
/// the group it opened; without this the same heading would be drawn on two
/// adjacent rows.
pub(crate) fn page_section(
    page: usize,
    label: &str,
    prev: Option<&'static str>,
) -> Option<&'static str> {
    let name = match page {
        NETWORK => match label {
            // Both open "Connection", deliberately — see above.
            "Network" | "Adapter" => Some("Connection"),
            "Wi-Fi" => Some("Signal"),
            "Download" => Some("Traffic"),
            "Gateway" => Some("Health"),
            _ => None,
        },
        SYSTEM => match label {
            "CPU" => Some("Live"),
            "Processor" => Some("Hardware"),
            "Computername" => Some("This machine"),
            "Battery" => Some("Power"),
            _ => None,
        },
        // The day list under these two has a standalone caption of its own
        // (`"Recent days"`, in the painter), because its rows are runtime dates
        // rather than page rows. This is the level above it: the totals the
        // list adds up to.
        DATA => match label {
            "Today" => Some("Totals"),
            _ => None,
        },
        // OVERVIEW, and the fallback for an index that cannot happen — the same
        // split the row function makes, for the same reason.
        _ => match label {
            "Download" => Some("Traffic"),
            "CPU" => Some("Usage"),
            _ => None,
        },
    };
    // A heading already drawn on the row above is a continuation, not a new
    // group. The only table with two anchors for one name needs this; keeping
    // the rule here rather than in each table means the next one cannot forget
    // it and silently draw the same caption on two adjacent rows.
    match name {
        Some(n) if prev == Some(n) => None,
        other => other,
    }
}

/// How many trailing days the Data page lists. The whole retention window is
/// often a month of rows; the last week is what the page is opened to see, and
/// the month total above it already accounts for the rest.
pub(crate) const USAGE_ROWS: usize = 7;

/// The Data page's day rows, oldest first, as `(MM-DD, total)`.
///
/// Rendered here rather than through `page_rows` because their labels are
/// runtime dates, not the `&'static str` captions the metric pages use.
pub(crate) fn usage_rows(model: &TrayModel) -> Vec<(String, u64)> {
    let skip = model.usage_days.len().saturating_sub(USAGE_ROWS);
    model
        .usage_days
        .iter()
        .skip(skip)
        .map(|(day, bytes)| (day.clone(), *bytes))
        .collect()
}

/// The month row's caption. Empty — and therefore rowless — until the counter
/// has a day to belong to.
pub(crate) fn month_label(model: &TrayModel) -> &'static str {
    if model.month_text.is_empty() {
        ""
    } else if model.month_partial {
        "Month so far"
    } else {
        "Month"
    }
}

/// Whether a page has anything for the sparkline to say. The traffic history
/// belongs with the traffic figures, and the daily total is the same story at
/// a coarser grain — the hardware and settings pages have no history to draw.
pub(crate) fn page_shows_graph(page: usize) -> bool {
    matches!(page, OVERVIEW | DATA)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk a page the way the painter does — threading the last heading drawn
    /// into the next `page_section` — and return the pairs the painter would
    /// draw, as `(Option<heading>, label)`.
    fn painted(
        page: usize,
        model: &crate::taskbar::TrayModel,
    ) -> Vec<(Option<&'static str>, &'static str)> {
        let mut drawn: Option<&'static str> = None;
        page_rows(page, model)
            .into_iter()
            .map(|(label, _)| {
                let head = page_section(page, label, drawn);
                if head.is_some() {
                    drawn = head;
                }
                (head, label)
            })
            .collect()
    }

    /// The headings a page ends up with, in the order it draws them. This is
    /// the check that the `prev` rule works: the two `Connection` anchors must
    /// collapse to one heading when both rows are present.
    fn headings(page: usize) -> Vec<&'static str> {
        painted(page, &full())
            .into_iter()
            .filter_map(|(head, _)| head)
            .collect()
    }

    /// A model with every field filled, so a page's own ordering shows.
    fn full() -> crate::taskbar::TrayModel {
        crate::taskbar::TrayModel {
            down_text: "1.4M/s".into(),
            up_text: "0.2M/s".into(),
            latency_text: "8ms".into(),
            cpu_text: "10%".into(),
            ram_text: "40%".into(),
            gateway_text: "192.168.1.1".into(),
            internet_text: "14ms".into(),
            loss_text: "0%".into(),
            wifi_text: "5G 78%".into(),
            wifi_name: Some("HomeNet".into()),
            adapter_text: "Wi-Fi".into(),
            ip_text: "192.168.1.10".into(),
            dns_text: "192.168.1.1, 8.8.8.8".into(),
            usage_text: "1.4G".into(),
            month_text: "41.2G".into(),
            cpu_name_text: "AMD Ryzen 5 7600 6-Core Processor".into(),
            cores_text: "6 cores / 12 threads".into(),
            gpu_text: "AMD Radeon RX 6600".into(),
            computer_text: "ULIN-PC".into(),
            windows_text: "Windows 11 Pro 24H2 (build 26100.2033)".into(),
            uptime_text: "3d 4h".into(),
            battery_text: "88%".into(),
            power_text: "Plugged in".into(),
            ..Default::default()
        }
    }

    /// The anchor tables are string matches, and a typo in one is a heading that
    /// silently never draws — the rows still appear, just ungrouped, which looks
    /// like a missing feature rather than a misspelled literal.
    ///
    /// The labels themselves are pinned exactly by the ordering tests in
    /// `ui::tests`, so between them a renamed row fails there and a renamed
    /// anchor fails here. What is checked here is the shape the headings have to
    /// keep: every page grouped, no heading used twice on one page, and the
    /// first row of a page always under a heading — a page that opens ungrouped
    /// and then starts grouping halfway down reads as a mistake.
    #[test]
    fn every_section_is_anchored_on_a_row_that_exists() {
        for page in 0..PAGES.len() {
            if PAGES[page] == "Settings" {
                // No rows at all: a caption per control, drawn by the layout.
                continue;
            }
            let mut heads = headings(page);
            assert!(!heads.is_empty(), "{} has no groups", PAGES[page]);
            assert!(
                painted(page, &full())[0].0.is_some(),
                "{} opens ungrouped",
                PAGES[page]
            );
            let found = heads.len();
            heads.sort_unstable();
            heads.dedup();
            assert_eq!(found, heads.len(), "{} repeats a heading", PAGES[page]);
        }
    }

    /// The other half of the anchor contract: a heading may only be spelled
    /// where a row can reach it. Checked by listing every name the tables
    /// produce and requiring each to be one the page would draw.
    #[test]
    fn no_heading_is_spelled_for_a_row_that_cannot_reach_it() {
        // Each table's anchors, by the row they sit on. A typo on the left of
        // one of these is a heading that never draws; a typo on the right is a
        // heading nobody sees. Both are silent, so both are pinned here.
        let cases: &[(usize, &str, &str)] = &[
            (OVERVIEW, "Download", "Traffic"),
            (OVERVIEW, "CPU", "Usage"),
            (NETWORK, "Network", "Connection"),
            (NETWORK, "Adapter", "Connection"),
            (NETWORK, "Wi-Fi", "Signal"),
            (NETWORK, "Download", "Traffic"),
            (NETWORK, "Gateway", "Health"),
            (SYSTEM, "CPU", "Live"),
            (SYSTEM, "Processor", "Hardware"),
            (SYSTEM, "Computername", "This machine"),
            (SYSTEM, "Battery", "Power"),
            (DATA, "Today", "Totals"),
        ];
        for (page, label, name) in cases {
            // `prev: None` so the second of a doubled anchor still answers.
            assert_eq!(
                page_section(*page, label, None),
                Some(*name),
                "{}: {label} no longer opens {name}",
                PAGES[*page]
            );
            assert!(
                page_rows(*page, &full())
                    .iter()
                    .any(|(l, _)| l == label),
                "{}: no row is labelled {label}",
                PAGES[*page]
            );
        }
    }

    #[test]
    fn the_doubled_anchor_draws_one_heading_not_two() {
        // "Connection" is opened by whichever of the SSID and the adapter is
        // there, because a plugged-in machine has no SSID and a Wi-Fi machine
        // has both. Both cases have to produce exactly one heading.
        let both = headings(NETWORK);
        assert_eq!(both.iter().filter(|h| **h == "Connection").count(), 1);

        let no_ssid = crate::taskbar::TrayModel {
            wifi_name: None,
            wifi_text: String::new(),
            ..full()
        };
        let heads: Vec<&str> = painted(NETWORK, &no_ssid)
            .into_iter()
            .filter_map(|(h, _)| h)
            .collect();
        assert_eq!(
            heads.iter().filter(|h| **h == "Connection").count(),
            1,
            "the adapter row lost its heading: {heads:?}"
        );
    }

    #[test]
    fn a_switched_off_metric_does_not_leave_a_header_over_nothing() {
        // The anchors are on the rows, so with the live pair off the page opens
        // on Hardware — no "Live" caption hanging over unrelated rows.
        let model = crate::taskbar::TrayModel {
            cpu_name_text: "AMD Ryzen 5 7600".into(),
            ..Default::default()
        };
        let rows = page_rows(SYSTEM, &model);
        assert_eq!(rows.len(), 1);
        assert_eq!(page_section(SYSTEM, rows[0].0, None), Some("Hardware"));
    }

    #[test]
    fn every_page_groups_its_rows_in_the_order_it_reads_them() {
        // The headings a page ends up with, in row order, from a model that
        // answered everything. This is the page's table of contents: if a row
        // moves, this fails rather than the page quietly reading differently.
        assert_eq!(headings(OVERVIEW), vec!["Traffic", "Usage"]);
        assert_eq!(
            headings(NETWORK),
            vec!["Connection", "Signal", "Traffic", "Health"]
        );
        assert_eq!(
            headings(SYSTEM),
            vec!["Live", "Hardware", "This machine", "Power"]
        );
        assert_eq!(headings(DATA), vec!["Totals"]);
        // A heading over a single row is not a group, it is a caption: two
        // headings for two rows would be more furniture than content. The Data
        // page is the one exception — its day list gives "Totals" a second row
        // in practice, above a standalone caption of its own.
        for page in [OVERVIEW, NETWORK, SYSTEM] {
            let rows = page_rows(page, &full());
            let heads = headings(page).len();
            assert!(
                rows.len() >= heads * 2,
                "{}: {heads} headings over {} rows is more caption than content",
                PAGES[page],
                rows.len()
            );
        }
    }

    #[test]
    fn the_system_page_names_what_this_machine_is() {
        let model = crate::taskbar::TrayModel {
            computer_text: "ULIN-PC".into(),
            windows_text: "Windows 11 Pro".into(),
            cpu_name_text: "AMD Ryzen 5 7600 6-Core Processor".into(),
            ..Default::default()
        };
        let rows = page_rows(SYSTEM, &model);
        let at = |l| rows.iter().position(|(label, _)| *label == l);
        assert!(at("Computername").unwrap() < at("Windows").unwrap());
        assert_eq!(rows[at("Computername").unwrap()].1, "ULIN-PC");
    }
}

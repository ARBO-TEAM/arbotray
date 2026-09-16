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

/// The group a System-page row opens, or `None` for a row that continues the
/// group above it.
///
/// Ten label-value pairs with no grouping is a wall, and the wall is what made
/// the detail feel like a dump rather than a page. Each group is anchored on its
/// own first row, so a group whose first row is switched off simply has no
/// header instead of leaving one hanging over unrelated rows.
pub(crate) fn system_section(label: &str) -> Option<&'static str> {
    match label {
        "CPU" => Some("Live"),
        "Processor" => Some("Hardware"),
        "Computername" => Some("This machine"),
        "Battery" => Some("Power"),
        _ => None,
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

    #[test]
    fn every_system_group_is_anchored_on_a_row_that_exists() {
        // The anchors in `system_section` are strings, and a typo in one is a
        // group header that silently never draws — the rows still appear, just
        // with no heading over them, which looks like a missing feature rather
        // than a misspelled literal.
        let model = crate::taskbar::TrayModel {
            cpu_text: "10%".into(),
            ram_text: "40%".into(),
            cpu_name_text: "AMD Ryzen 5 7600 6-Core Processor".into(),
            cores_text: "6 cores / 12 threads".into(),
            gpu_text: "AMD Radeon RX 6600".into(),
            computer_text: "ULIN-PC".into(),
            windows_text: "Windows 11 Pro 24H2 (build 26100.2033)".into(),
            uptime_text: "3d 4h".into(),
            battery_text: "88%".into(),
            power_text: "Plugged in".into(),
            ..Default::default()
        };
        let rows = page_rows(SYSTEM, &model);
        let headers: Vec<&str> = rows
            .iter()
            .filter_map(|(label, _)| system_section(label))
            .collect();
        assert_eq!(
            headers,
            vec!["Live", "Hardware", "This machine", "Power"],
            "a group lost its header: {:?}",
            rows.iter().map(|(l, _)| *l).collect::<Vec<_>>()
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
        assert_eq!(system_section(rows[0].0), Some("Hardware"));
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

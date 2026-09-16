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
        SYSTEM => {
            push("CPU", &model.cpu_text);
            push("RAM", &model.ram_text);
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

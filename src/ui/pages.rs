//! What each page shows inside its cards, and the Data page's day list.

use crate::config::Config;
use crate::taskbar::TrayModel;

/// The pages, in the order the list shows them. Their indices are the page
/// numbers used throughout, so the constants below name the slots rather than
/// leaving magic numbers in the card tables.
///
/// `Settings` is **appended**. These indices are positional, so inserting it
/// anywhere but the end would renumber every page after it — the labels would
/// still read correctly and the routing would be wrong.
pub(crate) const PAGES: [&str; 9] = [
    "Overview",
    "Network",
    "System",
    "Data",
    "Ports",
    "Speed Test",
    "Stopwatch",
    "Timer",
    "Settings",
];
pub(crate) const OVERVIEW: usize = 0;
pub(crate) const NETWORK: usize = 1;
pub(crate) const SYSTEM: usize = 2;
pub(crate) const DATA: usize = 3;
pub(crate) const PORTS: usize = 4;
pub(crate) const SPEEDTEST: usize = 5;
pub(crate) const STOPWATCH: usize = 6;
pub(crate) const TIMER: usize = 7;
pub(crate) const SETTINGS: usize = 8;

// --- pages ----------------------------------------------------------------
//
// There is no `page_has_cards` predicate any more. It existed to keep the
// painter's generic row loop and the card arms from both drawing a page, and
// the row loop is gone: the painter draws each page from its own `page == X`
// arm, so a page that grew a card could not also be walked as rows. What it
// guarded is now structural rather than asserted.

// --- card contents --------------------------------------------------------
//
// The tables below are what each carded page puts *inside* its cards. They share
// a shape — a label and its value, in the order the card reads them — and a
// rule: an empty value is no row at all, rather than a caption standing beside
// nothing.

/// The Network page's identity card: which network this machine is on, and
/// which adapter is carrying it.
///
/// Ordered identity-first: the card leads with the SSID when there is one, then
/// the adapter and the addresses that belong to it, because "which network am I
/// on" is the question the card is opened to answer; the gateway follows the IP
/// so the two ends of the connection read as a pair.
pub(crate) fn connection_rows(model: &TrayModel) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut push = |label: &'static str, text: &str| {
        if !text.is_empty() {
            out.push((label, text.to_string()));
        }
    };
    if let Some(name) = &model.wifi_name {
        push("Network", name);
    }
    push("Adapter", &model.adapter_text);
    push("IP", &model.ip_text);
    push("Gateway", &model.gateway_text);
    push("DNS", &model.dns_text);
    push("Wi-Fi", &model.wifi_text);
    out
}

/// The Network page's health card: whether the far end answers, and how much of
/// what was sent had to be sent again.
///
/// The two are read together — a lossy link and a dead one look the same from a
/// single latency number — which is why they are one card and not two rows on
/// the identity card above.
pub(crate) fn health_rows(model: &TrayModel) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut push = |label: &'static str, text: &str| {
        if !text.is_empty() {
            out.push((label, text.to_string()));
        }
    };
    push("Internet", &model.internet_text);
    push("Loss", &model.loss_text);
    out
}

/// The Ports page's socket counters, widest first: what the machine is listening
/// on, then what is actually talking, then the connectionless sockets, then how
/// many processes that adds up to.
///
/// The open-port list itself is drawn after these by the painter, because its
/// labels are runtime port numbers rather than the captions this table can hold.
pub(crate) fn socket_rows(model: &TrayModel) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let mut push = |label: &'static str, text: &str| {
        if !text.is_empty() {
            out.push((label, text.to_string()));
        }
    };
    push("Listening", &model.listeners_text);
    push("Established", &model.established_text);
    push("UDP bound", &model.udp_text);
    push("Processes", &model.port_owners_text);
    out
}

/// The Data page's plan row, or `None` when there is no plan to report.
///
/// `quota_gb <= 0.0` is the config's own documented "no plan" value, so it hides
/// the row entirely rather than drawing `0 GB` — the same test `quota_pct` makes
/// before it will produce a percentage, so the row and the bar that would read
/// off it cannot disagree about whether a plan exists.
///
/// The over-plan case is said in the row's own value rather than by colouring
/// it: the window already turns red top to bottom when the quota is blown (see
/// `Canvas::emphasise`), and a second red mark inside a red page says nothing
/// the first one did not.
pub(crate) fn plan_row(cfg: &Config, model: &TrayModel) -> Option<(&'static str, String)> {
    if cfg.quota_gb <= 0.0 {
        return None;
    }
    let gb = cfg.quota_gb;
    Some((
        "Plan",
        if model.quota_alert {
            format!("{gb} GB \u{2014} over plan")
        } else {
            format!("{gb} GB")
        },
    ))
}

/// The Data page's totals card: today, the month so far, and the plan they are
/// both a fraction of.
///
/// The month's caption is `month_label`, which carries the caveat rather than
/// the number — when the usage file no longer reaches back to the first of the
/// month the sum is of the recent past only, and "Month so far" says so where a
/// reader will actually look. Empty text means rowless, so a counter that has
/// not seen a day yet contributes no caption.
pub(crate) fn usage_totals(cfg: &Config, model: &TrayModel) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    if !model.usage_text.is_empty() {
        out.push(("Today", model.usage_text.clone()));
    }
    if !model.month_text.is_empty() {
        out.push((month_label(model), model.month_text.clone()));
    }
    if let Some(plan) = plan_row(cfg, model) {
        out.push(plan);
    }
    out
}

/// How many trailing days the Data page lists. The whole retention window is
/// often a month of rows; the last week is what the page is opened to see, and
/// the month total above it already accounts for the rest.
pub(crate) const USAGE_ROWS: usize = 7;

/// The Data page's day rows, oldest first, as `(MM-DD, total)`.
///
/// Rendered here rather than as a typed table because their labels are runtime
/// dates, not the `&'static str` captions the metric rows use.
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

/// `"41%"` → `0.41`. Strips the sign, parses, clamps; unparseable → `0.0`.
///
/// Shared by the Overview and System meters, so one string never means two
/// things on two pages. It parses model strings, not GDI, which is why it lives
/// beside the card tables rather than with the components.
pub(crate) fn pct_of(text: &str) -> f32 {
    text.trim()
        .strip_suffix('%')
        .unwrap_or(text.trim())
        .trim()
        .parse::<f32>()
        .unwrap_or(0.0)
        .clamp(0.0, 100.0)
        / 100.0
}

/// Whether a page has anything for the sparkline to say. The traffic history
/// belongs with the traffic figures, and the daily total is the same story at
/// a coarser grain — the hardware and settings pages have no history to draw.
pub(crate) fn page_shows_graph(page: usize) -> bool {
    matches!(page, DATA)
}

#[cfg(test)]
mod tests {
    use super::*;

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
            // The two pages whose rows are all live readings rather than
            // hardware facts: an empty model has neither, and the grouping
            // checks below require every page to open under a heading.
            listeners_text: "24".into(),
            established_text: "87".into(),
            udp_text: "31".into(),
            port_owners_text: "42".into(),
            speed_down_text: "94.2M/s".into(),
            speed_up_text: "11.8M/s".into(),
            speed_latency_text: "14ms".into(),
            speed_phase_text: "Done".into(),
            ..Default::default()
        }
    }

    // The anchor-heading tests that stood here — `every_section_is_anchored_on_a_row_that_exists`,
    // `no_heading_is_spelled_for_a_row_that_cannot_reach_it`,
    // `the_doubled_anchor_draws_one_heading_not_two`,
    // `every_page_groups_its_rows_in_the_order_it_reads_them` and
    // `a_switched_off_metric_does_not_leave_a_header_over_nothing` — pinned the
    // `page_section` anchor table, and went with it: grouping is drawn on the
    // cards now, each group named by its own `card_head` rather than by an
    // anchor table. The contract they guarded is checked on the rows the cards
    // actually draw: `the_connection_card_*`,
    // `the_socket_counters_come_back_in_reading_order`,
    // `the_usage_totals_carry_a_plan_row_only_when_a_plan_is_set` and
    // `a_health_card_with_nothing_to_report_has_no_rows`.
    //
    // `the_system_page_names_what_this_machine_is` went with them and is not
    // replaced here: the System card builds its identity rows inline in
    // `paint.rs` — "Computer", "Windows", "Uptime", "Version", in that order —
    // so nothing reachable from this file can assert their order, and the page's
    // own test in `paint.rs` is what pins it.
    //
    // `every_carded_page_is_also_walked_as_rows` stood here too, pinning the
    // exclusivity `page_has_cards` and `page_rows` had to keep between them: a
    // page that was a card *and* a row list printed the same figures twice, once
    // as a list and again inside the card below it. That is structural now — the
    // painter has no generic row loop left to disagree with the cards, so there
    // is no second mechanism for a page to be listed under.

    #[test]
    fn the_connection_card_leads_with_the_network_then_the_adapter() {
        // Identity first, then the addresses that belong to it, then the far
        // end. The identity card is the only thing that draws these rows, so a
        // card that quietly reordered them would fail here rather than reading
        // slightly wrong on screen.
        let labels: Vec<&str> = connection_rows(&full()).iter().map(|(l, _)| *l).collect();
        assert_eq!(labels[0], "Network", "the SSID is the identity: {labels:?}");
        let at = |l| labels.iter().position(|x| *x == l).unwrap();
        assert!(at("Adapter") < at("IP"), "{labels:?}");
        assert!(at("IP") < at("Gateway"), "{labels:?}");
    }

    #[test]
    fn the_connection_card_drops_the_ssid_row_when_there_is_no_ssid() {
        // A wired machine has no SSID, and a caption standing beside nothing is
        // worse than no caption at all.
        let wired = crate::taskbar::TrayModel {
            wifi_name: None,
            ..full()
        };
        let labels: Vec<&str> = connection_rows(&wired).iter().map(|(l, _)| *l).collect();
        assert!(labels.iter().all(|l| *l != "Network"), "{labels:?}");
        assert_eq!(labels[0], "Adapter", "the adapter leads instead");
    }

    #[test]
    fn the_usage_totals_carry_a_plan_row_only_when_a_plan_is_set() {
        // `quota_gb == 0.0` is the config's own "no plan", so the card must not
        // grow a `0 GB` row that says the user has a plan of zero.
        let no_plan = Config::default();
        assert_eq!(no_plan.quota_gb, 0.0, "the default really is no plan");
        assert!(
            usage_totals(&no_plan, &full())
                .iter()
                .all(|(l, _)| *l != "Plan"),
            "a plan row appeared for a machine with no plan"
        );

        let planned = Config {
            quota_gb: 250.0,
            ..Config::default()
        };
        let rows = usage_totals(&planned, &full());
        assert_eq!(rows.last().unwrap().0, "Plan", "the plan closes the card");
        assert_eq!(rows.last().unwrap().1, "250 GB");

        // And the overage is said in the value rather than by colour: the whole
        // window already goes red when the quota is blown.
        let over = crate::taskbar::TrayModel {
            quota_alert: true,
            ..full()
        };
        let rows = usage_totals(&planned, &over);
        assert!(
            rows.last().unwrap().1.contains("over plan"),
            "{:?}",
            rows.last().unwrap()
        );
    }

    #[test]
    fn the_socket_counters_come_back_in_reading_order() {
        // Widest first: what the machine listens on, what is talking, what is
        // connectionless, and how many processes that is. The open-port list is
        // drawn under these, so this is the summary the detail belongs to.
        let labels: Vec<&str> = socket_rows(&full()).iter().map(|(l, _)| *l).collect();
        assert_eq!(
            labels,
            vec!["Listening", "Established", "UDP bound", "Processes"]
        );
    }

    #[test]
    fn a_health_card_with_nothing_to_report_has_no_rows() {
        // Both probes off — no internet reading, no loss figure — has to be an
        // empty card, not a card holding two captions with nothing beside them.
        let quiet = crate::taskbar::TrayModel {
            internet_text: String::new(),
            loss_text: String::new(),
            ..full()
        };
        assert!(health_rows(&quiet).is_empty());
        assert_eq!(health_rows(&full()).len(), 2, "and both rows when both report");
    }
}

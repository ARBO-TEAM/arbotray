//! What the panel says, as data.
//!
//! Kept apart from the window so the panel's contents can be checked without
//! creating one — the same bargain `visible_segments` strikes for the strip. A
//! row is a label, a value, and a role; nothing here knows about fonts, colour
//! or pixels.

use crate::config::Widget;
use crate::taskbar::TrayModel;

/// A row's role, which is the only thing that decides its colour and weight.
///
/// A role rather than a colour, so [`rows`] — the part with the readings in it
/// — stays pure data and the palette is applied in one place at paint time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// A block's heading.
    Title,
    /// A row's name, dimmer than its value.
    Label,
    /// A row's reading.
    Value,
    /// A reading that has tripped the data plan.
    Alert,
}

/// One row of the panel.
///
/// `label` is empty for a heading, which spans the full width.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub label: String,
    pub value: String,
    pub role: Role,
}

impl Row {
    fn title(text: &str) -> Self {
        Self {
            label: String::new(),
            value: text.to_string(),
            role: Role::Title,
        }
    }

    fn item(label: &str, value: &str) -> Self {
        Self {
            label: label.to_string(),
            value: value.to_string(),
            role: Role::Value,
        }
    }
}

/// The readings the panel shows, grouped the way the pages already group them.
///
/// Every block is skipped outright when it is switched off, and every row
/// within it when its reading is blank: a machine with no battery must not get
/// an empty `Battery` row, and a reading the collector could not take must not
/// be drawn as a zero. A block that loses all its rows takes its heading with
/// it — a column of headings over nothing is worse than no panel at all.
pub fn rows(model: &TrayModel, w: &Widget) -> Vec<Row> {
    let mut out = Vec::new();
    let push = |out: &mut Vec<Row>, title: &str, block: Vec<Row>| {
        if !block.is_empty() {
            out.push(Row::title(title));
            out.extend(block);
        }
    };

    if w.show.net {
        let mut b = Vec::new();
        if !model.down_text.is_empty() {
            b.push(Row::item("Download", &model.down_text));
        }
        if !model.up_text.is_empty() {
            b.push(Row::item("Upload", &model.up_text));
        }
        push(&mut out, "Traffic", b);
    }

    if w.show.latency {
        let mut b = Vec::new();
        if !model.latency_text.is_empty() {
            b.push(Row::item("Gateway", &model.latency_text));
        }
        if !model.internet_text.is_empty() {
            b.push(Row::item("Internet", &model.internet_text));
        }
        if !model.loss_text.is_empty() {
            b.push(Row::item("Loss", &model.loss_text));
        }
        push(&mut out, "Latency", b);
    }

    if w.show.hardware {
        let mut b = Vec::new();
        if !model.cpu_text.is_empty() {
            b.push(Row::item("CPU", &model.cpu_text));
        }
        if !model.ram_text.is_empty() {
            b.push(Row::item("RAM", &model.ram_text));
        }
        push(&mut out, "Usage", b);
    }

    if w.show.sensors {
        let mut b = Vec::new();
        if !model.gpu_text.is_empty() {
            b.push(Row::item("GPU", &model.gpu_text));
        }
        if !model.battery_text.is_empty() {
            b.push(Row::item("Battery", &model.battery_text));
        }
        if !model.power_text.is_empty() {
            b.push(Row::item("Power", &model.power_text));
        }
        push(&mut out, "Sensors", b);
    }

    if w.show.network {
        let mut b = Vec::new();
        if !model.adapter_text.is_empty() {
            b.push(Row::item("Adapter", &model.adapter_text));
        }
        if !model.wifi_text.is_empty() {
            b.push(Row::item("Wi-Fi", &model.wifi_text));
        }
        if let Some(name) = &model.wifi_name {
            b.push(Row::item("Network", name));
        }
        if !model.ip_text.is_empty() {
            b.push(Row::item("Address", &model.ip_text));
        }
        if !model.gateway_text.is_empty() {
            b.push(Row::item("Router", &model.gateway_text));
        }
        if !model.dns_text.is_empty() {
            b.push(Row::item("DNS", &model.dns_text));
        }
        push(&mut out, "Network", b);
    }

    if w.show.usage {
        let mut b = Vec::new();
        if !model.usage_text.is_empty() {
            let role = if model.quota_alert {
                Role::Alert
            } else {
                Role::Value
            };
            b.push(Row {
                label: "Today".into(),
                value: model.usage_text.clone(),
                role,
            });
        }
        if !model.month_text.is_empty() {
            // The month is a projection until it ends, and saying so is the
            // difference between a reading and a wrong reading.
            let label = if model.month_partial {
                "This month (so far)"
            } else {
                "This month"
            };
            b.push(Row::item(label, &model.month_text));
        }
        for (day, total) in &model.usage_days {
            b.push(Row::item(day, &human_bytes(*total)));
        }
        push(&mut out, "Data", b);
    }

    if w.show.system {
        let mut b = Vec::new();
        if !model.computer_text.is_empty() {
            b.push(Row::item("Machine", &model.computer_text));
        }
        if !model.windows_text.is_empty() {
            b.push(Row::item("Windows", &model.windows_text));
        }
        if !model.cpu_name_text.is_empty() {
            b.push(Row::item("Processor", &model.cpu_name_text));
        }
        if !model.cores_text.is_empty() {
            b.push(Row::item("Cores", &model.cores_text));
        }
        if !model.uptime_text.is_empty() {
            b.push(Row::item("Uptime", &model.uptime_text));
        }
        for (mount, usage) in &model.disks {
            b.push(Row::item(mount, usage));
        }
        push(&mut out, "System", b);
    }

    out
}

/// A day's byte total in the units the strip already uses.
///
/// A miniature of the taskbar's formatter rather than a call into it: that one
/// formats a *rate* and takes a different argument, so sharing it would mean a
/// signature that serves neither caller.
fn human_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        return format!("{bytes} B");
    }
    if b < KB * KB {
        return format!("{:.1}K", b / KB);
    }
    if b < KB * KB * KB {
        return format!("{:.1}M", b / (KB * KB));
    }
    format!("{:.2}G", b / (KB * KB * KB))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;

    /// Every block switched on, so a test that is about a *row* is not silently
    /// testing which blocks the defaults happen to include.
    fn all() -> config::Widget {
        config::Widget {
            show: config::WidgetShow {
                net: true,
                latency: true,
                hardware: true,
                sensors: true,
                network: true,
                usage: true,
                system: true,
            },
            ..Default::default()
        }
    }

    fn model() -> TrayModel {
        TrayModel {
            down_text: "1.2M/s".into(),
            up_text: "84.0K/s".into(),
            latency_text: "4ms".into(),
            gateway_text: "192.168.1.1".into(),
            cpu_text: "12%".into(),
            ram_text: "43%".into(),
            usage_text: "264.6M".into(),
            adapter_text: "Wi-Fi".into(),
            ip_text: "192.168.1.10".into(),
            computer_text: "DESKTOP".into(),
            windows_text: "Windows 11 Pro 26100".into(),
            uptime_text: "3d 4h".into(),
            disks: vec![("C:".into(), "210G free of 931G".into())],
            ..Default::default()
        }
    }

    #[test]
    fn every_block_is_switched_by_its_own_flag() {
        let m = model();
        let mut w = all();
        assert!(!rows(&m, &w).is_empty());
        w.show.net = false;
        assert!(!rows(&m, &w).iter().any(|r| r.value == "Traffic"));
        assert!(rows(&m, &w).iter().any(|r| r.value == "Usage"));
    }

    #[test]
    fn the_default_panel_is_traffic_and_usage_and_nothing_else() {
        // The whole shape of the panel in one assertion: four rows, two
        // headings, and not one row of page detail. This is the measure of
        // "small enough to leave on the desktop".
        let rs = rows(&model(), &config::Widget::default());
        let headings: Vec<_> = rs
            .iter()
            .filter(|r| r.role == Role::Title)
            .map(|r| r.value.as_str())
            .collect();
        assert_eq!(headings, ["Traffic", "Usage"]);
        let labels: Vec<_> = rs
            .iter()
            .filter(|r| !r.label.is_empty())
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(labels, ["Download", "Upload", "CPU", "RAM"]);
    }

    #[test]
    fn a_blank_reading_gets_no_row() {
        // The battery is the case that matters: most desktops report none, and
        // an empty "Battery" row is the panel admitting it looked.
        let m = model();
        let w = all();
        assert!(!rows(&m, &w).iter().any(|r| r.label == "Battery"));
        assert!(rows(&m, &w).iter().any(|r| r.label == "CPU"));
    }

    #[test]
    fn sensors_are_their_own_block_so_cpu_and_ram_stand_alone() {
        // A machine that reports a GPU must not drag one into the block the
        // default panel is built from — that is the whole reason the split
        // exists.
        let m = TrayModel {
            gpu_text: "7%".into(),
            cpu_text: "12%".into(),
            ram_text: "43%".into(),
            ..Default::default()
        };
        let rs = rows(&m, &config::Widget::default());
        assert!(!rs.iter().any(|r| r.label == "GPU"));
        assert!(rows(&m, &all()).iter().any(|r| r.label == "GPU"));
    }

    #[test]
    fn an_over_quota_total_is_the_only_alert_row() {
        let mut m = model();
        m.quota_alert = true;
        let rs = rows(&m, &all());
        let alerts: Vec<_> = rs.iter().filter(|r| r.role == Role::Alert).collect();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].label, "Today");
    }

    #[test]
    fn an_empty_model_makes_an_empty_panel() {
        // Not a column of headings over nothing.
        let rs = rows(&TrayModel::default(), &all());
        assert!(rs.is_empty(), "got {rs:?}");
    }

    #[test]
    fn a_heading_always_has_a_row_under_it() {
        let m = TrayModel {
            gpu_text: "7%".into(),
            ..Default::default()
        };
        let rs = rows(&m, &all());
        assert_eq!(rs[0].role, Role::Title);
        assert!(rs.len() > 1, "a heading with no rows must not be emitted");
    }

    #[test]
    fn a_partial_month_says_so_in_its_label() {
        let mut m = model();
        m.month_text = "41.2G".into();
        m.month_partial = true;
        assert!(
            rows(&m, &all())
                .iter()
                .any(|r| r.label == "This month (so far)")
        );
    }

    #[test]
    fn every_block_in_a_full_model_is_reachable() {
        // Guards the wiring, not the formatting: a block behind the wrong flag
        // or a row behind the wrong field would silently never appear.
        let mut m = model();
        m.gpu_text = "7%".into();
        let rs = rows(&m, &all());
        for heading in [
            "Traffic", "Latency", "Usage", "Sensors", "Network", "Data", "System",
        ] {
            assert!(
                rs.iter().any(|r| r.role == Role::Title && r.value == heading),
                "missing {heading}"
            );
        }
    }

    #[test]
    fn day_totals_are_formatted_in_bytes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(2048), "2.0K");
        assert_eq!(human_bytes(5 * 1024 * 1024), "5.0M");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.00G");
    }
}

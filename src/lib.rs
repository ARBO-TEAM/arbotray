//! ArboTray — native Windows taskbar monitor.
//!
//! Layering: `telemetry` collects samples, `taskbar` renders them into the
//! taskbar, `config` persists user settings, `app` wires the three together.

pub mod app;
pub mod autostart;
pub mod config;
pub mod power;
pub mod taskbar;
pub mod telemetry;
pub mod ui;
pub mod update;
pub mod widget;

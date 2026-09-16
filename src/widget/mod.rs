//! The desktop widget: a floating panel for the readings the taskbar strip has
//! no room for.
//!
//! The strip is one run of text in whatever width is left beside the clock, so
//! everything that does not fit — the adapter, the address, the disk list, the
//! machine name — is page detail that only the dashboard can show. This is the
//! third surface: a window on the desktop that draws the same detail as
//! labelled rows, so it can be read without opening anything.
//!
//! It lives on the strip's thread and is driven by the strip's own update, so
//! there is no second telemetry path to keep in step. Being a top-level window
//! is what it buys over the strip: `WS_EX_LAYERED` is accepted here, so the
//! panel takes a real alpha instead of sampling a colour underneath itself, and
//! it can be dragged anywhere on the desktop. The taskbar child is refused both.
//!
//! Layered the way `ui` is, and for the same reason:
//!
//! - [`metrics`] — the geometry, in unscaled pixels
//! - [`rows`] — what the panel says, as pure data, and the tests over it
//! - [`render`] — fonts, palette and measured sizes
//! - [`window`] — the window itself: creation, messages, paint
//!
//! The split is what makes the panel checkable without a window: [`rows`] and
//! the renderer's sizing are both ordinary functions over values, the way
//! `visible_segments` lets the strip be.

mod metrics;
mod render;
mod rows;
mod window;

pub use rows::{Role, Row, rows};
pub use window::Widget;

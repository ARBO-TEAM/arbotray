//! The panel's geometry, in unscaled pixels.
//!
//! Every value here is multiplied by the DPI at draw time, so they are written
//! the way the panel should look at 100% and nothing in them is absolute. Split
//! out the way the dashboard's `layout` is: the numbers are what make the panel
//! read well, and they want to be readable together rather than buried in the
//! paint path.

/// Padding inside the panel, and the air between two blocks.
pub const PAD: i32 = 14;
pub const BLOCK_GAP: i32 = 10;
/// How much taller a line is than the text in it.
pub const LINE_GAP: i32 = 4;
/// The gap between the label column and its values.
pub const COL_GAP: i32 = 12;
/// Side of the close box, and its inset from the top-right corner.
pub const CLOSE: i32 = 18;
pub const CLOSE_INSET: i32 = 8;
/// Distance a first-run panel keeps from the work area's edges.
pub const MARGIN: i32 = 24;

/// The panel is read at arm's length rather than glanced at beside a clock, so
/// its body text is this many points above the strip's, and its headings more.
pub const BODY_EXTRA: u32 = 2;
pub const TITLE_EXTRA: u32 = 5;

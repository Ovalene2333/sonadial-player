//! Shared design tokens for the TUI surface.
//!
//! The interface is built on a 4 px grid, square corners and a fixed set of
//! control heights. Layout code should pick a token instead of scattering
//! magic numbers through the draw calls.

/// 4 px spacing grid.
pub const SPACE_1: f32 = 4.0;
pub const SPACE_2: f32 = 8.0;
pub const SPACE_3: f32 = 12.0;
pub const SPACE_4: f32 = 16.0;
pub const SPACE_5: f32 = 20.0;
pub const SPACE_6: f32 = 24.0;

/// Half step for hairlines and one-pixel nudges.
pub const HAIRLINE: f32 = 2.0;

/// Tight label-to-control rhythm.
pub const SPACE_TIGHT: f32 = 6.0;

/// Side gutter used by the sidebar, stage labels and cards.
pub const GUTTER: f32 = 14.0;

/// The TUI uses square corners everywhere.
pub const RADIUS: f32 = 0.0;
/// Small radius reserved for pictogram details.
pub const RADIUS_SM: f32 = 2.0;

/// Reusable component metrics.
pub const CONTROL_H: f32 = 22.0;
pub const FIELD_H: f32 = 20.0;
pub const LINE_H: f32 = 18.0;
pub const ROW_H: f32 = 26.0;
pub const MENU_ITEM_H: f32 = 24.0;
pub const MENU_W: f32 = 232.0;
pub const CHIP_H: f32 = 28.0;

/// Window sizes and insets.
pub const WINDOW_PAD: f32 = SPACE_5;
pub const SETTINGS_W: f32 = 430.0;
pub const SETTINGS_BODY_H: f32 = 360.0;
pub const EQ_W: f32 = 660.0;
pub const DOWNLOAD_W: f32 = 520.0;
pub const DIALOG_W: f32 = 420.0;
pub const TOAST_W: f32 = 440.0;

//! Spacing, size and type scale (UI audit section 2). All spacing values are
//! multiples of 2 on a 4 px base.
//!
//! Use the constants instead of literals: `.p(px(S6))`, `.gap(px(S4))`,
//! `.h(px(H_CONTROL))`, `.rounded(px(R_MD))`, `.text_size(px(TX_SM))`.
//! Convert call sites file by file when you touch them; do not mass-replace
//! (Lucide icon sizes and the grid's own `ROW_H` are separate metrics).
//!
//! * Spacing: `S1` hairline gaps / seg padding, `S2` tight icon–label gap, `S3` control
//!   inner gap and pill `px`, `S4` default gap / input `px` / card gutter, `S5` section and
//!   dialog row gap / button `px`, `S6` panel padding / dialog field gap, `S7` page section
//!   gap / dialog body padding (the mock's 18–20), `S8` page outer padding.
//! * Heights: chips and badges `H_BADGE` 20 / `H_COMPACT` 24, buttons / inputs / header
//!   buttons `H_CONTROL` 28, sidebar / nav / history rows `H_ROW` 32, active tab `H_TAB`
//!   44, title bar `H_TITLEBAR` 36, status and prod bar `H_BAR` 28.
//! * Radii: `R_XS` kbd / badge, `R_SM` icon button / input / seg, `R_MD` row / tab / button,
//!   `R_LG` card / dialog; pills use `rounded_full`.
//! * Text: 11 / 12 / 13 (body) / 14 / 16, mono 11.5 for kbd and code. CJK text: 12 minimum.
//! * Widths: rail `W_RAIL` 40, dialogs `W_DLG_S` / `_M` / `_L` = 480 / 560 / 720.
#![allow(dead_code)] // the constants are consumed by the per-screen workers

pub(crate) const S1: f32 = 2.0;
pub(crate) const S2: f32 = 4.0;
pub(crate) const S3: f32 = 6.0;
pub(crate) const S4: f32 = 8.0;
pub(crate) const S5: f32 = 12.0;
pub(crate) const S6: f32 = 16.0;
pub(crate) const S7: f32 = 24.0;
pub(crate) const S8: f32 = 32.0;

pub(crate) const H_BADGE: f32 = 20.0;
pub(crate) const H_COMPACT: f32 = 24.0;
pub(crate) const H_CONTROL: f32 = 28.0;
pub(crate) const H_ROW: f32 = 32.0;
pub(crate) const H_TITLEBAR: f32 = 36.0;
pub(crate) const H_BAR: f32 = 28.0;
pub(crate) const H_TAB: f32 = 44.0;

pub(crate) const R_XS: f32 = 4.0;
pub(crate) const R_SM: f32 = 6.0;
pub(crate) const R_MD: f32 = 8.0;
pub(crate) const R_LG: f32 = 12.0;
/// Dialogs, cards and the welcome card.
pub(crate) const R_XL: f32 = 16.0;

pub(crate) const TX_XS: f32 = 11.0;
pub(crate) const TX_SM: f32 = 12.0;
pub(crate) const TX_BASE: f32 = 13.0;
pub(crate) const TX_MD: f32 = 14.0;
pub(crate) const TX_LG: f32 = 16.0;
pub(crate) const TX_MONO: f32 = 11.5;

pub(crate) const W_RAIL: f32 = 40.0;
pub(crate) const W_DLG_S: f32 = 480.0;
pub(crate) const W_DLG_M: f32 = 560.0;
pub(crate) const W_DLG_L: f32 = 720.0;

/// Right side panel (Schema): the user chose 264; the results toolbar drops to icons when narrow.
pub(crate) const W_RIGHT_PANEL: f32 = 264.0;
/// Segmented control: widest row before items wrap (`seg.rs`).
pub(crate) const W_SEG_MAX: f32 = 420.0;

//! Default editor / results split of a query pane (UI audit E7): the editor gets 60 % of the
//! space the pane has, the results 40 %.

/// Share of the pane height the editor gets by default.
pub const EDITOR_SHARE: f32 = 0.6;
/// Smallest default editor height in px.
pub const EDITOR_MIN: f32 = 120.0;

/// Default editor height for a pane that is `pane_h` px high (never above the pane minus a
/// 120 px results strip, never below `EDITOR_MIN`).
pub fn default_editor_height(pane_h: f32) -> f32 {
    let want = pane_h * EDITOR_SHARE;
    want.min(pane_h - 120.0).max(EDITOR_MIN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sixty_forty() {
        assert_eq!(default_editor_height(600.0), 360.0);
    }

    #[test]
    fn small_pane_keeps_a_results_strip_and_a_minimum() {
        assert_eq!(default_editor_height(200.0), EDITOR_MIN);
        assert_eq!(default_editor_height(0.0), EDITOR_MIN);
    }
}

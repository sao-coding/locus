//! Where the in-place cell editor puts its text.
//!
//! The table pads every cell (`Size::table_cell_padding`) and draws the
//! selection frame (a 1 px border over the whole cell box, padding included)
//! on top. The editor therefore fills the *whole* cell box, like that frame,
//! and insets its text by `cell padding - border`, so the text starts at the
//! same x as the cell's own text. Nothing here depends on the column offset,
//! the row-number gutter or the scroll position: the editor is laid out inside
//! the cell, the table places the cell.

/// Width of the selection frame the table draws around the active cell.
pub const SELECTION_BORDER: f32 = 1.0;

/// Left and right padding of the editor's text, inside its own border, so the
/// text sits where the cell text does.
pub fn editor_padding(cell_pad_left: f32, cell_pad_right: f32) -> (f32, f32) {
    (
        (cell_pad_left - SELECTION_BORDER).max(0.),
        (cell_pad_right - SELECTION_BORDER).max(0.),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Left edge of column `col` in viewport coordinates.
    fn cell_x(gutter: f32, widths: &[f32], col: usize, scroll_x: f32) -> f32 {
        gutter + widths[..col].iter().sum::<f32>() - scroll_x
    }

    /// x of the first glyph: the display text sits after the cell padding; the
    /// editor after its border plus `editor_padding`.
    fn display_text_x(cell_x: f32, pad_l: f32) -> f32 {
        cell_x + pad_l
    }
    fn editor_text_x(cell_x: f32, pad_l: f32, pad_r: f32) -> f32 {
        cell_x + SELECTION_BORDER + editor_padding(pad_l, pad_r).0
    }

    #[test]
    fn editor_text_starts_where_cell_text_does() {
        let widths = [60., 120., 200., 90.];
        for gutter in [0., 44.] {
            // first and last column, at several (also fractional) scroll offsets
            for col in [0, 3] {
                for scroll in [0., 1., 37.5, 333.25] {
                    let x = cell_x(gutter, &widths, col, scroll);
                    assert_eq!(display_text_x(x, 8.), editor_text_x(x, 8., 8.));
                }
            }
        }
    }

    #[test]
    fn padding_subtracts_the_border() {
        assert_eq!(editor_padding(8., 8.), (7., 7.));
        assert_eq!(editor_padding(12., 6.), (11., 5.));
    }

    #[test]
    fn padding_never_goes_negative() {
        assert_eq!(editor_padding(0., 0.5), (0., 0.));
    }

    #[test]
    fn column_offsets_add_up() {
        let widths = [60., 120., 200.];
        assert_eq!(cell_x(44., &widths, 0, 0.), 44.);
        assert_eq!(cell_x(44., &widths, 2, 0.), 224.);
        assert_eq!(cell_x(44., &widths, 2, 100.), 124.);
    }
}

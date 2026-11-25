//! The opaque colours of the two themes (text levels and surfaces) and the WCAG
//! contrast helper that checks them. `app/src/shell/theme.rs` builds its tokens
//! from these constants, so the tests here guard what the UI shows.

/// Opaque colours of one theme, `0xRRGGBB`.
pub struct Pal {
    pub tx: u32,
    pub tx1: u32,
    pub tx2: u32,
    pub tx3: u32,
    pub tx4: u32,
    /// Decoration only (chevrons, disabled glyphs); exempt from the 4.5 : 1 rule.
    pub tx5: u32,
    /// Accent / green / yellow / red as *text* (the plain colours stay for fills and strokes).
    pub acc_text: u32,
    pub grn_text: u32,
    pub yel_text: u32,
    pub red_text: u32,
    pub side: u32,
    pub main: u32,
    pub ed: u32,
    pub panel: u32,
    pub pop: u32,
    /// Result grid: an ink colour laid over the card at `grid_a` (body) and `grid_alt_a`
    /// (alternate rows). Translucent so the Space colour of the card still shows.
    pub grid: u32,
    pub grid_a: f32,
    pub grid_alt_a: f32,
}

pub const DARK: Pal = Pal {
    tx: 0xe6dfd5,
    tx1: 0xf3ece1,
    tx2: 0xbdb4a6,
    tx3: 0xa39b8f,
    tx4: 0x948c80,
    tx5: 0x6f685e,
    acc_text: 0xe8803a,
    grn_text: 0x7fb685,
    yel_text: 0xd9a441,
    red_text: 0xe59a8a,
    side: 0x151311,
    main: 0x181614,
    ed: 0x1b1917,
    panel: 0x1f1c1a,
    pop: 0x272320,
    grid: 0xfff4e6,
    grid_a: 0.0,
    grid_alt_a: 0.028,
};

pub const LIGHT: Pal = Pal {
    tx: 0x2b2621,
    tx1: 0x16120e,
    tx2: 0x4a433a,
    tx3: 0x645c50,
    tx4: 0x6a6256,
    tx5: 0x8a8174,
    acc_text: 0xa84e16,
    grn_text: 0x386b40,
    yel_text: 0x8f6410,
    red_text: 0xa8402c,
    side: 0xe6dfd5,
    main: 0xfbf9f6,
    ed: 0xfdfcfa,
    panel: 0xf2ede6,
    pop: 0xffffff,
    grid: 0x3c2814,
    grid_a: 0.02,
    grid_alt_a: 0.04,
};

fn lin(c: u8) -> f64 {
    let v = c as f64 / 255.0;
    if v <= 0.03928 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG relative luminance of `0xRRGGBB`.
pub fn luminance(c: u32) -> f64 {
    0.2126 * lin((c >> 16) as u8) + 0.7152 * lin((c >> 8) as u8) + 0.0722 * lin(c as u8)
}

/// WCAG contrast ratio (1..21) of two opaque colours.
pub fn contrast(a: u32, b: u32) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wcag_reference_values() {
        assert!((contrast(0x000000, 0xffffff) - 21.0).abs() < 0.01);
        assert!((contrast(0x777777, 0xffffff) - 4.48).abs() < 0.02);
    }

    fn check(name: &str, p: &Pal) {
        let surfaces = [
            ("side", p.side),
            ("main", p.main),
            ("ed", p.ed),
            ("panel", p.panel),
            ("pop", p.pop),
        ];
        let mut bad = vec![];
        for (tn, t) in [
            ("tx", p.tx),
            ("tx1", p.tx1),
            ("tx2", p.tx2),
            ("tx3", p.tx3),
            ("tx4", p.tx4),
        ] {
            for (sn, s) in surfaces {
                if contrast(t, s) < 4.5 {
                    bad.push(format!("{name} {tn} on {sn}: {:.2}", contrast(t, s)));
                }
            }
        }
        // Status colours as text are used on cards and popups, not on the glass sidebar.
        for (tn, t) in [
            ("acc_text", p.acc_text),
            ("grn_text", p.grn_text),
            ("yel_text", p.yel_text),
            ("red_text", p.red_text),
        ] {
            for (sn, s) in surfaces.iter().skip(1) {
                if contrast(t, *s) < 4.5 {
                    bad.push(format!("{name} {tn} on {sn}: {:.2}", contrast(t, *s)));
                }
            }
        }
        assert!(bad.is_empty(), "below 4.5 : 1: {bad:#?}");
    }

    fn blend(over: u32, a: f32, under: u32) -> u32 {
        let ch = |s: u32| {
            let (o, u) = (((over >> s) & 0xff) as f32, ((under >> s) & 0xff) as f32);
            (o * a + u * (1.0 - a)).round() as u32
        };
        (ch(16) << 16) | (ch(8) << 8) | ch(0)
    }

    #[test]
    fn grid_rows_keep_text_readable_and_stripes_subtle() {
        for (n, p) in [("dark", &DARK), ("light", &LIGHT)] {
            // The table paints `grid` on two nested layers, the alternate rows add `grid_alt_a`.
            let body = blend(p.grid, p.grid_a, blend(p.grid, p.grid_a, p.main));
            let alt = blend(p.grid, p.grid_alt_a, body);
            for (tn, t) in [("tx", p.tx), ("tx2", p.tx2), ("tx3", p.tx3), ("tx4", p.tx4)] {
                for (sn, s) in [("body", body), ("alt", alt)] {
                    assert!(
                        contrast(t, s) >= 4.5,
                        "{n} {tn} on {sn}: {:.2}",
                        contrast(t, s)
                    );
                }
            }
            // Rows tell apart (> 1.02 : 1) but stay a hint (< 1.15 : 1).
            let r = contrast(body, alt);
            assert!((1.02..1.15).contains(&r), "{n} stripe contrast {r:.3}");
        }
        // The light body is off-white, not the card white.
        let body = blend(
            LIGHT.grid,
            LIGHT.grid_a,
            blend(LIGHT.grid, LIGHT.grid_a, LIGHT.main),
        );
        assert!(contrast(body, 0xffffff) > 1.05);
    }

    #[test]
    fn text_tokens_are_readable() {
        check("dark", &DARK);
        check("light", &LIGHT);
    }

    #[test]
    fn tx5_is_the_only_decorative_level() {
        // tx5 stays visible (>= 2.5 : 1 on the card) but is not for text.
        for p in [&DARK, &LIGHT] {
            assert!(contrast(p.tx5, p.main) >= 2.5);
            assert!(contrast(p.tx5, p.main) < contrast(p.tx4, p.main));
        }
    }
}

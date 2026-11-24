//! A Space's look: one to three colours (hue + saturation), how strongly they tint the
//! window, and whether the Space follows the app's light / dark choice or pins one.
//!
//! One colour is a flat tint; two or three are a soft top-to-bottom gradient. Spaces saved
//! before themes only have a hue (`color` preset index or `custom_hue`): [`SpaceTheme::of`]
//! turns that into a one-colour theme that draws exactly as before.
//!
//! The colour picker's pad maps x to hue and y to saturation (top = full); the functions
//! here hold that mapping so it is unit-tested without a window.

use serde::{Deserialize, Serialize};

/// Most colours a Space can have.
pub const MAX_COLORS: usize = 3;

/// Saturation a hue-only (pre-theme) Space draws with: the old fixed shade
/// (`hsl(h, 60 %, 48 %)` light, `hsl(h, 45 %, 50 %)` dark).
pub const LEGACY_SAT: f32 = 0.6;

/// Intensity that draws the tint at the old strength (the multiplier is `2 × intensity`).
pub const DEFAULT_INTENSITY: f32 = 0.5;

/// One key colour: hue and saturation, both 0..1. Lightness is the theme's business.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpaceColor {
    pub h: f32,
    pub s: f32,
}

impl SpaceColor {
    pub fn new(h: f32, s: f32) -> Self {
        Self {
            h: h.rem_euclid(1.0),
            s: s.clamp(0.0, 1.0),
        }
    }
}

/// Light / dark choice of a Space: `Auto` follows the app setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    #[default]
    Auto,
    Light,
    Dark,
}

impl Appearance {
    /// The theme name it pins (`"light"` / `"dark"`), `None` for `Auto`.
    pub fn pinned(self) -> Option<&'static str> {
        match self {
            Appearance::Auto => None,
            Appearance::Light => Some("light"),
            Appearance::Dark => Some("dark"),
        }
    }
}

fn default_intensity() -> f32 {
    DEFAULT_INTENSITY
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SpaceTheme {
    /// 1..=3 key colours, top to bottom.
    pub colors: Vec<SpaceColor>,
    /// 0..1; 0.5 is the default strength.
    #[serde(default = "default_intensity")]
    pub intensity: f32,
    #[serde(default)]
    pub appearance: Appearance,
}

/// Nine one-click colours (rose to violet), as (hue, saturation).
pub const PRESETS: [SpaceColor; 9] = [
    SpaceColor { h: 0.97, s: 0.62 }, // rose
    SpaceColor { h: 0.92, s: 0.70 }, // pink
    SpaceColor { h: 0.83, s: 0.55 }, // magenta
    SpaceColor { h: 0.07, s: 0.80 }, // orange
    SpaceColor { h: 0.13, s: 0.85 }, // yellow
    SpaceColor { h: 0.38, s: 0.55 }, // green
    SpaceColor { h: 0.52, s: 0.65 }, // cyan
    SpaceColor { h: 0.60, s: 0.70 }, // blue
    SpaceColor { h: 0.73, s: 0.55 }, // violet
];

impl SpaceTheme {
    /// The theme of a saved Space: its `theme` when it has one (kept in range), else one made
    /// from its old hue so it draws as before.
    pub fn of(color: usize, custom_hue: Option<f32>, theme: Option<&SpaceTheme>) -> SpaceTheme {
        match theme {
            Some(t) if !t.colors.is_empty() => t.clone().normalized(),
            _ => SpaceTheme {
                colors: vec![SpaceColor::new(
                    crate::spaces::space_hue(color, custom_hue),
                    LEGACY_SAT,
                )],
                intensity: DEFAULT_INTENSITY,
                appearance: Appearance::Auto,
            },
        }
    }

    /// Fields kept in range (spaces.json is the user's to edit).
    pub fn normalized(mut self) -> SpaceTheme {
        self.colors.truncate(MAX_COLORS);
        for c in &mut self.colors {
            *c = SpaceColor::new(c.h, c.s);
        }
        if self.colors.is_empty() {
            self.colors.push(SpaceColor::new(0.60, LEGACY_SAT));
        }
        self.intensity = if self.intensity.is_finite() {
            self.intensity.clamp(0.0, 1.0)
        } else {
            DEFAULT_INTENSITY
        };
        self
    }

    pub fn primary(&self) -> SpaceColor {
        self.colors[0]
    }

    /// Multiplier of the tint weights: 1.0 at the default intensity, 0..2.
    pub fn strength(&self) -> f32 {
        self.intensity * 2.0
    }

    /// Adds a colour after the last one, 1/6 of the wheel further on; `None` at the limit.
    /// Returns the new colour's index.
    pub fn add_color(&mut self) -> Option<usize> {
        if self.colors.len() >= MAX_COLORS {
            return None;
        }
        let last = *self.colors.last()?;
        self.colors
            .push(SpaceColor::new(last.h + 1.0 / 6.0, last.s));
        Some(self.colors.len() - 1)
    }

    /// Removes the last colour; the first always stays. Returns whether it removed one.
    pub fn remove_color(&mut self) -> bool {
        if self.colors.len() > 1 {
            self.colors.pop();
            true
        } else {
            false
        }
    }

    /// The gradient's stops (position 0..1, colour), top to bottom: two equal stops for one
    /// colour, evenly spread ones for two or three.
    pub fn stops(&self) -> Vec<(f32, SpaceColor)> {
        if let [c] = self.colors[..] {
            return vec![(0.0, c), (1.0, c)];
        }
        gradient_stops(self.colors.len())
            .into_iter()
            .zip(self.colors.iter().copied())
            .collect()
    }

    /// The key colour at `t` (0 top .. 1 bottom) along the gradient, interpolated in hue
    /// (the short way round) and saturation.
    pub fn color_at(&self, t: f32) -> SpaceColor {
        let stops = self.stops();
        let t = t.clamp(0.0, 1.0);
        for w in stops.windows(2) {
            let ((p0, c0), (p1, c1)) = (w[0], w[1]);
            if t <= p1 {
                let k = if p1 > p0 { (t - p0) / (p1 - p0) } else { 0.0 };
                return lerp_color(c0, c1, k);
            }
        }
        stops.last().map(|s| s.1).unwrap_or(self.colors[0])
    }
}

/// Positions of `n` evenly spread gradient stops (0..1).
pub fn gradient_stops(n: usize) -> Vec<f32> {
    match n {
        0 => vec![],
        1 => vec![0.0],
        n => (0..n).map(|i| i as f32 / (n - 1) as f32).collect(),
    }
}

/// `a` toward `b` by `k`, hue the short way round the wheel.
pub fn lerp_color(a: SpaceColor, b: SpaceColor, k: f32) -> SpaceColor {
    let k = k.clamp(0.0, 1.0);
    let mut d = b.h - a.h;
    if d > 0.5 {
        d -= 1.0;
    } else if d < -0.5 {
        d += 1.0;
    }
    SpaceColor::new(a.h + d * k, a.s + (b.s - a.s) * k)
}

/// The colour under a point of the picker's pad, `x` and `y` in 0..1 (clamped): x is the
/// hue, y the saturation from 1 at the top to 0 at the bottom.
pub fn pad_to_color(x: f32, y: f32) -> SpaceColor {
    let x = x.clamp(0.0, 1.0);
    let y = y.clamp(0.0, 1.0);
    // x = 1 is the same hue as 0; keep it at the right edge rather than jumping left.
    SpaceColor {
        h: if x >= 1.0 { 0.999_9 } else { x },
        s: 1.0 - y,
    }
}

/// Where a colour sits on the pad (inverse of [`pad_to_color`]).
pub fn color_to_pad(c: SpaceColor) -> (f32, f32) {
    (c.h.rem_euclid(1.0), 1.0 - c.s.clamp(0.0, 1.0))
}

/// HSL (hue, saturation, lightness) a key colour is drawn with in the light or dark theme.
/// At [`LEGACY_SAT`] it is the old fixed shade.
pub fn tint_hsl(c: SpaceColor, dark: bool) -> (f32, f32, f32) {
    if dark {
        (c.h, c.s * 0.75, 0.50)
    } else {
        (c.h, c.s, 0.48)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn pad_round_trips() {
        for &(x, y) in &[(0.0, 0.0), (0.25, 0.4), (0.5, 1.0), (0.9, 0.1)] {
            let (x2, y2) = color_to_pad(pad_to_color(x, y));
            assert!(close(x, x2) && close(y, y2), "{x},{y} -> {x2},{y2}");
        }
    }

    #[test]
    fn pad_corners_and_clamping() {
        let top_left = pad_to_color(0.0, 0.0);
        assert!(close(top_left.h, 0.0) && close(top_left.s, 1.0));
        let bottom = pad_to_color(0.3, 1.0);
        assert!(close(bottom.s, 0.0));
        // Outside the pad (dragging past its edge) clamps.
        let out = pad_to_color(-0.5, 2.0);
        assert!(close(out.h, 0.0) && close(out.s, 0.0));
        let right = pad_to_color(1.5, -1.0);
        assert!(right.h < 1.0 && right.h > 0.999 && close(right.s, 1.0));
    }

    #[test]
    fn legacy_tint_is_unchanged() {
        // The old space_color: hsl(h, .6, .48) light, hsl(h, .45, .50) dark.
        let c = SpaceColor::new(0.33, LEGACY_SAT);
        assert_eq!(tint_hsl(c, false), (0.33, 0.6, 0.48));
        let (_, s, l) = tint_hsl(c, true);
        assert!(close(s, 0.45) && close(l, 0.50));
    }

    #[test]
    fn stops_for_one_two_three() {
        assert_eq!(gradient_stops(1), vec![0.0]);
        assert_eq!(gradient_stops(2), vec![0.0, 1.0]);
        assert_eq!(gradient_stops(3), vec![0.0, 0.5, 1.0]);
        let mut t = SpaceTheme::of(0, Some(0.1), None);
        let one = t.stops();
        assert_eq!(one.len(), 2);
        assert_eq!(one[0].1, one[1].1);
        assert_eq!(t.add_color(), Some(1));
        assert_eq!(t.add_color(), Some(2));
        assert_eq!(t.add_color(), None);
        let three = t.stops();
        assert_eq!(
            three.iter().map(|s| s.0).collect::<Vec<_>>(),
            vec![0.0, 0.5, 1.0]
        );
        assert_eq!(t.color_at(0.5), t.colors[1]);
        assert!(t.remove_color() && t.remove_color() && !t.remove_color());
        assert_eq!(t.colors.len(), 1);
    }

    #[test]
    fn color_at_takes_the_short_way_round() {
        let t = SpaceTheme {
            colors: vec![SpaceColor::new(0.95, 0.5), SpaceColor::new(0.05, 1.0)],
            intensity: 0.5,
            appearance: Appearance::Auto,
        };
        let mid = t.color_at(0.5);
        assert!(mid.h < 0.01 || mid.h > 0.99, "{}", mid.h);
        assert!(close(mid.s, 0.75));
    }

    #[test]
    fn old_spaces_migrate_to_one_colour() {
        let t = SpaceTheme::of(1, None, None);
        assert_eq!(
            t.colors,
            vec![SpaceColor::new(crate::spaces::SPACE_HUES[1], LEGACY_SAT)]
        );
        assert!(close(t.strength(), 1.0));
        assert_eq!(t.appearance, Appearance::Auto);
        let t = SpaceTheme::of(1, Some(0.25), None);
        assert!(close(t.colors[0].h, 0.25));
    }

    #[test]
    fn saved_theme_is_kept_in_range() {
        let raw = r#"{"colors":[{"h":1.25,"s":3},{"h":0.1,"s":0.5},{"h":0.2,"s":0.5},{"h":0.3,"s":0.5}],"intensity":9}"#;
        let t: SpaceTheme = serde_json::from_str(raw).unwrap();
        let t = SpaceTheme::of(0, None, Some(&t));
        assert_eq!(t.colors.len(), MAX_COLORS);
        assert!(close(t.colors[0].h, 0.25) && close(t.colors[0].s, 1.0));
        assert!(close(t.intensity, 1.0));
        assert_eq!(t.appearance, Appearance::Auto);
        let empty = SpaceTheme {
            colors: vec![],
            intensity: 0.5,
            appearance: Appearance::Dark,
        };
        // No colours: falls back to the hue, as an old Space.
        assert!(close(
            SpaceTheme::of(0, Some(0.4), Some(&empty)).colors[0].h,
            0.4
        ));
    }
}

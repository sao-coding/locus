//! Orthogonal routing of one relation between two cards (world units).

pub type Rect = (f32, f32, f32, f32); // x, y, w, h

pub const STUB: f32 = 14.0;

/// A routed relation: polyline with only horizontal / vertical segments, and
/// the outward direction (+1 right, -1 left) at each end.
pub struct Route {
    pub pts: [(f32, f32); 4],
    /// Outward side at the referencing (many) end.
    pub dir_from: f32,
    /// Outward side at the referenced (one) end.
    pub dir_to: f32,
}

/// `ya` / `yb`: row centre on card `a` (referencing) and `b` (referenced).
/// `slot` spreads parallel verticals apart.
pub fn route(a: Rect, ya: f32, b: Rect, yb: f32, slot: usize) -> Route {
    let (ar, br) = (a.0 + a.2, b.0 + b.2);
    let jitter = ((slot % 5) as f32 - 2.0) * 6.0;
    if ar + 2.0 * STUB <= b.0 {
        let xm = ((ar + b.0) / 2.0 + jitter).clamp(ar + STUB * 0.6, b.0 - STUB * 0.6);
        Route {
            pts: [(ar, ya), (xm, ya), (xm, yb), (b.0, yb)],
            dir_from: 1.0,
            dir_to: -1.0,
        }
    } else if br + 2.0 * STUB <= a.0 {
        let xm = ((br + a.0) / 2.0 + jitter).clamp(br + STUB * 0.6, a.0 - STUB * 0.6);
        Route {
            pts: [(a.0, ya), (xm, ya), (xm, yb), (br, yb)],
            dir_from: -1.0,
            dir_to: 1.0,
        }
    } else {
        // overlapping columns (or the same table): leave on the right, loop around
        let xm = ar.max(br) + STUB * 1.6 + (slot % 4) as f32 * 5.0;
        Route {
            pts: [(ar, ya), (xm, ya), (xm, yb), (br, yb)],
            dir_from: 1.0,
            dir_to: 1.0,
        }
    }
}

/// Distance from `p` to the nearest segment of the route.
pub fn distance(r: &Route, p: (f32, f32)) -> f32 {
    let mut best = f32::MAX;
    for w in r.pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (minx, maxx) = (a.0.min(b.0), a.0.max(b.0));
        let (miny, maxy) = (a.1.min(b.1), a.1.max(b.1));
        let dx = (minx - p.0).max(0.0).max(p.0 - maxx);
        let dy = (miny - p.1).max(0.0).max(p.1 - maxy);
        best = best.min((dx * dx + dy * dy).sqrt());
    }
    best
}

pub fn bbox(r: &Route) -> Rect {
    let minx = r.pts.iter().map(|p| p.0).fold(f32::MAX, f32::min);
    let maxx = r.pts.iter().map(|p| p.0).fold(f32::MIN, f32::max);
    let miny = r.pts.iter().map(|p| p.1).fold(f32::MAX, f32::min);
    let maxy = r.pts.iter().map(|p| p.1).fold(f32::MIN, f32::max);
    (minx, miny, maxx - minx, maxy - miny)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_by_side_attaches_nearer_sides() {
        let r = route(
            (0.0, 0.0, 100.0, 80.0),
            20.0,
            (300.0, 50.0, 100.0, 80.0),
            70.0,
            0,
        );
        assert_eq!((r.pts[0], r.pts[3]), ((100.0, 20.0), (300.0, 70.0)));
        assert_eq!((r.dir_from, r.dir_to), (1.0, -1.0));
        let l = route(
            (300.0, 0.0, 100.0, 80.0),
            20.0,
            (0.0, 0.0, 100.0, 80.0),
            20.0,
            0,
        );
        assert_eq!((l.pts[0].0, l.pts[3].0), (300.0, 100.0));
        assert_eq!((l.dir_from, l.dir_to), (-1.0, 1.0));
    }

    #[test]
    fn segments_are_axis_aligned_and_stay_outside_cards() {
        for (a, b) in [
            ((0.0, 0.0, 100.0, 80.0), (50.0, 200.0, 100.0, 80.0)),
            ((0.0, 0.0, 100.0, 80.0), (0.0, 0.0, 100.0, 80.0)),
        ] {
            let r = route(a, 20.0, b, 30.0, 3);
            for w in r.pts.windows(2) {
                assert!(w[0].0 == w[1].0 || w[0].1 == w[1].1);
            }
            assert!(r.pts[1].0 > 100.0, "loop leaves on the right");
        }
    }
}

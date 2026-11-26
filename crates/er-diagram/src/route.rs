//! Routing of one relation between two cards (world units): an orthogonal
//! 4-point route, turned into a smooth edge — "smoothstep" (the orthogonal
//! route with rounded corners) or a cubic bezier, both leaving the cards
//! horizontally — plus the flattened polyline used for hit-testing.
//!
//! The edge shapes follow React Flow's `getSmoothStepPath` / `getBezierPath`
//! (reactflow.dev, MIT); written from the description, no code copied.

pub type Rect = (f32, f32, f32, f32); // x, y, w, h
pub type Pt = (f32, f32);

/// Shortest straight run at each end, so the end marks sit on a straight piece.
pub const STUB: f32 = 22.0;
/// Corner radius of a smoothstep edge.
pub const RADIUS: f32 = 8.0;
/// Distance between parallel verticals of edges between the same two cards.
const SLOT_GAP: f32 = 7.0;

/// How relation lines are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EdgeStyle {
    /// Orthogonal with rounded corners (default: reads best on dense schemas).
    #[default]
    Smooth,
    /// Cubic bezier.
    Bezier,
}

/// A routed relation: polyline with only horizontal / vertical segments, and
/// the outward direction (+1 right, -1 left) at each end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Route {
    pub pts: [Pt; 4],
    /// Outward side at the referencing (many) end.
    pub dir_from: f32,
    /// Outward side at the referenced (one) end.
    pub dir_to: f32,
}

/// Offset of parallel edge `slot` of `count` between the same two cards,
/// centred on 0 (0, ±7, ±14, ...).
pub fn slot_offset(slot: usize, count: usize) -> f32 {
    if count <= 1 {
        0.0
    } else {
        (slot as f32 - (count as f32 - 1.0) / 2.0) * SLOT_GAP
    }
}

/// `ya` / `yb`: row centre on card `a` (referencing) and `b` (referenced).
/// `offset` (see [`slot_offset`]) spreads parallel verticals apart.
pub fn route(a: Rect, ya: f32, b: Rect, yb: f32, offset: f32) -> Route {
    let (ar, br) = (a.0 + a.2, b.0 + b.2);
    if ar + 2.0 * STUB <= b.0 {
        let xm = ((ar + b.0) / 2.0 + offset).clamp(ar + STUB, b.0 - STUB);
        Route {
            pts: [(ar, ya), (xm, ya), (xm, yb), (b.0, yb)],
            dir_from: 1.0,
            dir_to: -1.0,
        }
    } else if br + 2.0 * STUB <= a.0 {
        let xm = ((br + a.0) / 2.0 + offset).clamp(br + STUB, a.0 - STUB);
        Route {
            pts: [(a.0, ya), (xm, ya), (xm, yb), (br, yb)],
            dir_from: -1.0,
            dir_to: 1.0,
        }
    } else {
        // overlapping columns (or the same table): leave on the right, loop around
        let xm = ar.max(br) + STUB * 1.4 + offset.abs();
        Route {
            pts: [(ar, ya), (xm, ya), (xm, yb), (br, yb)],
            dir_from: 1.0,
            dir_to: 1.0,
        }
    }
}

/// One drawing command of an edge (world units).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Seg {
    Line(Pt),
    /// Quadratic curve: control, end.
    Quad(Pt, Pt),
    /// Cubic curve: control 1, control 2, end.
    Cubic(Pt, Pt, Pt),
}

/// A drawable edge: start point and commands, its flattened polyline (for
/// hit-testing), bounding box and the two ends with their outward direction.
#[derive(Clone, Debug, PartialEq)]
pub struct EdgePath {
    pub start: Pt,
    pub segs: Vec<Seg>,
    pub poly: Vec<Pt>,
    pub bbox: Rect,
    pub from: (Pt, f32),
    pub to: (Pt, f32),
}

fn lerp(a: Pt, b: Pt, t: f32) -> Pt {
    (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t)
}

fn quad_at(p0: Pt, c: Pt, p1: Pt, t: f32) -> Pt {
    lerp(lerp(p0, c, t), lerp(c, p1, t), t)
}

fn cubic_at(p0: Pt, c1: Pt, c2: Pt, p1: Pt, t: f32) -> Pt {
    let a = lerp(p0, c1, t);
    let b = lerp(c1, c2, t);
    let c = lerp(c2, p1, t);
    lerp(lerp(a, b, t), lerp(b, c, t), t)
}

/// Turn a route into an edge of `style`.
pub fn edge_path(r: &Route, style: EdgeStyle) -> EdgePath {
    let [p0, p1, p2, p3] = r.pts;
    let mut segs = Vec::with_capacity(5);
    match style {
        EdgeStyle::Smooth => {
            let dy = (p2.1 - p1.1).abs();
            let rad = RADIUS
                .min(dy / 2.0)
                .min((p1.0 - p0.0).abs())
                .min((p3.0 - p2.0).abs());
            if dy < 0.5 || rad < 0.5 {
                segs.push(Seg::Line(p1));
                segs.push(Seg::Line(p2));
                segs.push(Seg::Line(p3));
            } else {
                let sx1 = (p1.0 - p0.0).signum();
                let sy = (p2.1 - p1.1).signum();
                let sx2 = (p3.0 - p2.0).signum();
                segs.push(Seg::Line((p1.0 - sx1 * rad, p1.1)));
                segs.push(Seg::Quad(p1, (p1.0, p1.1 + sy * rad)));
                segs.push(Seg::Line((p2.0, p2.1 - sy * rad)));
                segs.push(Seg::Quad(p2, (p2.0 + sx2 * rad, p2.1)));
                segs.push(Seg::Line(p3));
            }
        }
        EdgeStyle::Bezier => {
            // straight stubs at both ends carry the end marks
            let s0 = (p0.0 + r.dir_from * STUB * 0.6, p0.1);
            let s3 = (p3.0 + r.dir_to * STUB * 0.6, p3.1);
            let dx = (s3.0 - s0.0).abs();
            let dy = (s3.1 - s0.1).abs();
            let k = if r.dir_from == r.dir_to {
                // loop on one side (self reference, overlapping columns)
                (p1.0 - s0.0).abs().max(STUB) * 1.6 + dy * 0.15
            } else if (s3.0 - s0.0) * r.dir_from >= 0.0 {
                (dx * 0.5).max(20.0)
            } else {
                // the target lies behind: curve out further
                (dx * 0.25).max(40.0) + dy * 0.1
            };
            segs.push(Seg::Line(s0));
            segs.push(Seg::Cubic(
                (s0.0 + r.dir_from * k, s0.1),
                (s3.0 + r.dir_to * k, s3.1),
                s3,
            ));
            segs.push(Seg::Line(p3));
        }
    }
    let mut poly = Vec::with_capacity(32);
    poly.push(p0);
    let mut cur = p0;
    for s in &segs {
        match *s {
            Seg::Line(p) => {
                poly.push(p);
                cur = p;
            }
            Seg::Quad(c, p) => {
                for i in 1..=4 {
                    poly.push(quad_at(cur, c, p, i as f32 / 4.0));
                }
                cur = p;
            }
            Seg::Cubic(c1, c2, p) => {
                for i in 1..=20 {
                    poly.push(cubic_at(cur, c1, c2, p, i as f32 / 20.0));
                }
                cur = p;
            }
        }
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in &poly {
        x0 = x0.min(p.0);
        y0 = y0.min(p.1);
        x1 = x1.max(p.0);
        y1 = y1.max(p.1);
    }
    EdgePath {
        start: p0,
        segs,
        poly,
        bbox: (x0, y0, x1 - x0, y1 - y0),
        from: (p0, r.dir_from),
        to: (p3, r.dir_to),
    }
}

/// Distance from `p` to the nearest point of the edge's polyline.
pub fn distance(e: &EdgePath, p: Pt) -> f32 {
    let mut best = f32::MAX;
    for w in e.poly.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (vx, vy) = (b.0 - a.0, b.1 - a.1);
        let len2 = vx * vx + vy * vy;
        let t = if len2 <= f32::EPSILON {
            0.0
        } else {
            (((p.0 - a.0) * vx + (p.1 - a.1) * vy) / len2).clamp(0.0, 1.0)
        };
        let (qx, qy) = (a.0 + vx * t - p.0, a.1 + vy * t - p.1);
        best = best.min(qx * qx + qy * qy);
    }
    best.sqrt()
}

/// `p` is within `tol` of the box `b`.
pub fn near_box(b: Rect, p: Pt, tol: f32) -> bool {
    p.0 >= b.0 - tol && p.0 <= b.0 + b.2 + tol && p.1 >= b.1 - tol && p.1 <= b.1 + b.3 + tol
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: Rect = (0.0, 0.0, 100.0, 80.0);

    #[test]
    fn side_by_side_attaches_nearer_sides() {
        let r = route(A, 20.0, (300.0, 50.0, 100.0, 80.0), 70.0, 0.0);
        assert_eq!((r.pts[0], r.pts[3]), ((100.0, 20.0), (300.0, 70.0)));
        assert_eq!((r.dir_from, r.dir_to), (1.0, -1.0));
        let l = route((300.0, 0.0, 100.0, 80.0), 20.0, A, 20.0, 0.0);
        assert_eq!((l.pts[0].0, l.pts[3].0), (300.0, 100.0));
        assert_eq!((l.dir_from, l.dir_to), (-1.0, 1.0));
    }

    #[test]
    fn segments_are_axis_aligned_and_stay_outside_cards() {
        for b in [(50.0, 200.0, 100.0, 80.0), A] {
            let r = route(A, 20.0, b, 30.0, 7.0);
            for w in r.pts.windows(2) {
                assert!(w[0].0 == w[1].0 || w[0].1 == w[1].1);
            }
            assert!(r.pts[1].0 >= 100.0 + STUB, "loop leaves on the right");
        }
    }

    #[test]
    fn parallel_edges_separate() {
        let b = (400.0, 0.0, 100.0, 80.0);
        let xs: Vec<f32> = (0..3)
            .map(|s| route(A, 20.0, b, 40.0, slot_offset(s, 3)).pts[1].0)
            .collect();
        assert!(xs[0] < xs[1] && xs[1] < xs[2], "{xs:?}");
        assert_eq!(xs[1], 250.0, "middle slot stays centred");
        assert_eq!(slot_offset(0, 1), 0.0);
        // a narrow gap clamps the vertical so the stubs stay straight
        let near = route(A, 20.0, (150.0, 0.0, 100.0, 80.0), 40.0, 30.0);
        assert!(near.pts[1].0 <= 150.0 - STUB);
    }

    #[test]
    fn smoothstep_rounds_corners_and_ends_horizontally() {
        let r = route(A, 20.0, (300.0, 200.0, 100.0, 80.0), 240.0, 0.0);
        let e = edge_path(&r, EdgeStyle::Smooth);
        assert_eq!(e.segs.len(), 5);
        assert!(matches!(e.segs[1], Seg::Quad(c, _) if c == r.pts[1]));
        assert_eq!(e.segs[4], Seg::Line((300.0, 240.0)));
        // first and last pieces are horizontal
        assert_eq!(e.poly[0].1, e.poly[1].1);
        let n = e.poly.len();
        assert_eq!(e.poly[n - 1].1, e.poly[n - 2].1);
        // the curve passes inside the corner, not through it
        assert!(distance(&e, r.pts[1]) > 1.0);
        assert!(distance(&e, (r.pts[1].0, 120.0)) < 0.01);
        // same height: a straight line, no corners
        let flat = edge_path(
            &route(A, 20.0, (300.0, 0.0, 100.0, 80.0), 20.0, 0.0),
            EdgeStyle::Smooth,
        );
        assert!(flat.segs.iter().all(|s| matches!(s, Seg::Line(_))));
        // a small step clamps the radius to half the step
        let small = edge_path(
            &route(A, 20.0, (300.0, 0.0, 100.0, 80.0), 26.0, 0.0),
            EdgeStyle::Smooth,
        );
        assert!(matches!(small.segs[0], Seg::Line(p) if (p.0 - (200.0 - 3.0)).abs() < 1e-4));
    }

    #[test]
    fn bezier_leaves_horizontally_and_stays_near_its_ends() {
        let r = route(A, 20.0, (300.0, 200.0, 100.0, 80.0), 240.0, 0.0);
        let e = edge_path(&r, EdgeStyle::Bezier);
        assert!(matches!(e.segs[0], Seg::Line(p) if p.1 == 20.0 && p.0 > 100.0));
        assert!(matches!(e.segs[1], Seg::Cubic(c1, c2, _) if c1.1 == 20.0 && c2.1 == 240.0));
        assert!(distance(&e, (100.0, 20.0)) < 0.01);
        assert!(distance(&e, (300.0, 240.0)) < 0.01);
        assert!(
            distance(&e, (200.0, 130.0)) < 3.0,
            "passes through the middle"
        );
        assert_eq!(e.from, ((100.0, 20.0), 1.0));
        assert_eq!(e.to, ((300.0, 240.0), -1.0));
    }

    #[test]
    fn self_reference_loops_on_the_right() {
        let r = route(A, 60.0, A, 40.0, 0.0);
        for style in [EdgeStyle::Smooth, EdgeStyle::Bezier] {
            let e = edge_path(&r, style);
            assert!(e.poly.iter().all(|p| p.0 >= 100.0 - 1e-3), "{style:?}");
            assert!(e.bbox.0 + e.bbox.2 > 100.0 + STUB, "{style:?} sticks out");
            assert_eq!((e.from.1, e.to.1), (1.0, 1.0));
        }
    }

    #[test]
    fn hit_tolerance_is_a_distance() {
        let e = edge_path(
            &route(A, 20.0, (300.0, 0.0, 100.0, 80.0), 20.0, 0.0),
            EdgeStyle::Smooth,
        );
        assert!((distance(&e, (200.0, 24.0)) - 4.0).abs() < 1e-4);
        assert!(near_box(e.bbox, (200.0, 24.0), 4.0));
        assert!(!near_box(e.bbox, (200.0, 30.0), 4.0));
    }
}

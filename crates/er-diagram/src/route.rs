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
const SLOT_GAP: f32 = 10.0;

/// How relation lines are drawn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EdgeStyle {
    /// Orthogonal with rounded corners.
    Smooth,
    /// Cubic bezier (default).
    #[default]
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

/// Offset of edge `slot` of `count` sharing one channel, centred on 0
/// (0, ±5, ±10, ...: 10 apart).
pub fn slot_offset(slot: usize, count: usize) -> f32 {
    if count <= 1 {
        0.0
    } else {
        (slot as f32 - (count as f32 - 1.0) / 2.0) * SLOT_GAP
    }
}

/// Where the referencing card `a` lies against the referenced card `b`:
/// which gap the edge's vertical runs in (same tests as [`route`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    /// `a` left of `b`: the vertical is in the gap left of `b`.
    Left,
    /// `a` right of `b`: the gap right of `b`.
    Right,
    /// Overlapping columns or the same card: a loop on the right.
    Loop,
}

pub fn side(a: Rect, b: Rect) -> Side {
    if a.0 + a.2 + 2.0 * STUB <= b.0 {
        Side::Left
    } else if b.0 + b.2 + 2.0 * STUB <= a.0 {
        Side::Right
    } else {
        Side::Loop
    }
}

/// Channel offsets: edges entering the same card from the same side share
/// one gap, so each gets its own vertical. `edges[i]` = (referenced card,
/// side, |source row y - target row y|). In a group the nearest source gets
/// the vertical nearest itself, which keeps the group's lines from crossing
/// each other. `scratch` is reused between calls.
pub fn channel_offsets(edges: &[(usize, Side, f32)], scratch: &mut Vec<usize>, out: &mut Vec<f32>) {
    scratch.clear();
    scratch.extend(0..edges.len());
    scratch.sort_by(|&i, &j| {
        let (a, b) = (edges[i], edges[j]);
        (a.0, a.1)
            .cmp(&(b.0, b.1))
            .then(a.2.total_cmp(&b.2))
            .then(i.cmp(&j))
    });
    out.clear();
    out.resize(edges.len(), 0.0);
    let mut start = 0;
    while start < scratch.len() {
        let g = (edges[scratch[start]].0, edges[scratch[start]].1);
        let mut end = start;
        while end < scratch.len() && (edges[scratch[end]].0, edges[scratch[end]].1) == g {
            end += 1;
        }
        let n = end - start;
        for (k, &i) in scratch[start..end].iter().enumerate() {
            out[i] = match g.1 {
                // nearest source: vertical nearest the source side
                Side::Left => slot_offset(k, n),
                Side::Right => -slot_offset(k, n),
                // loops: the nearest loop is the innermost
                Side::Loop => k as f32 * SLOT_GAP,
            };
        }
        start = end;
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

/// Radius of the circle of an optional ("zero or one") end.
pub const MARK_R: f32 = 3.5;

/// The strokes of an end mark (crow's-foot notation) at `p`, the line
/// leaving the card towards `d` (+1 right, -1 left), in world units: up to
/// two segments (`count` of them are used) and, for an optional end, the
/// centre of a hollow circle of radius [`MARK_R`].
pub fn end_mark(end: crate::model::End, p: Pt, d: f32) -> ([(Pt, Pt); 2], usize, Option<Pt>) {
    use crate::model::End;
    let (x, y) = p;
    let bar = |at: f32| ((x + d * at, y - 6.0), (x + d * at, y + 6.0));
    match end {
        End::Many => {
            let tip = (x + d * 12.0, y);
            ([(tip, (x, y - 6.5)), (tip, (x, y + 6.5))], 2, None)
        }
        End::One => ([bar(9.0), bar(9.0)], 1, None),
        End::OnlyOne => ([bar(7.0), bar(12.0)], 2, None),
        End::ZeroOrOne => ([bar(7.0), bar(7.0)], 1, Some((x + d * 15.0, y))),
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
    fn edges_into_one_card_get_their_own_verticals() {
        let target = (400.0, 300.0, 100.0, 80.0);
        // three sources in a column on the left, at different heights
        let src = [(0.0, 0.0), (0.0, 120.0), (0.0, 600.0)];
        let ya: [f32; 3] = [20.0, 140.0, 620.0];
        let yb: f32 = 320.0;
        let edges: Vec<(usize, Side, f32)> = src
            .iter()
            .zip(ya)
            .map(|(s, y)| {
                let a = (s.0, s.1, 100.0, 80.0);
                (7, side(a, target), (y - yb).abs())
            })
            .collect();
        let (mut scratch, mut off) = (vec![], vec![]);
        channel_offsets(&edges, &mut scratch, &mut off);
        let xs: Vec<f32> = (0..3)
            .map(|i| route((0.0, src[i].1, 100.0, 80.0), ya[i], target, yb, off[i]).pts[1].0)
            .collect();
        // all different; the nearest source (the second) is leftmost, the farthest rightmost
        assert!(xs[1] < xs[0] && xs[0] < xs[2], "{xs:?}");
        // a lone edge stays centred; another target is another channel
        let lone = [(7, Side::Left, 10.0), (8, Side::Left, 50.0)];
        channel_offsets(&lone, &mut scratch, &mut off);
        assert_eq!(off, [0.0, 0.0]);
        // from the right the order is mirrored
        let right = [(1, Side::Right, 10.0), (1, Side::Right, 90.0)];
        channel_offsets(&right, &mut scratch, &mut off);
        assert!(off[0] > off[1]);
        assert_eq!(side(target, (0.0, 0.0, 100.0, 80.0)), Side::Right);
        assert_eq!(side(target, target), Side::Loop);
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

//! Layered (Sugiyama-style) layout: referenced tables left, referencing
//! tables right. Steps: break cycles, rank by longest path, add dummy nodes
//! for edges that span several layers, order each layer by barycentre
//! sweeps plus adjacent swaps (keeping the order with the fewest crossings),
//! then place each layer's tables near their neighbours (pool adjacent
//! violators keeps them in order without overlap). Tall layers wrap into
//! several columns; tables without relations go into a grid below. Pure
//! function, so it can run on a background thread.

const GAP_X: f32 = 80.0;
const GAP_Y: f32 = 24.0;
const ISLAND_GAP: f32 = 72.0;
const SWEEPS: usize = 24;

/// `sizes[i]` = (w, h) of table i; `edges` = (from, to) = (referencing,
/// referenced). Returns the top-left of each table.
pub fn auto_layout(sizes: &[(f32, f32)], edges: &[(usize, usize)]) -> Vec<(f32, f32)> {
    let n = sizes.len();
    let mut out = vec![(0.0f32, 0.0f32); n];
    if n == 0 {
        return out;
    }
    // adjacency without self loops / duplicates; parents[v] = tables v references
    let mut parents: Vec<Vec<usize>> = vec![vec![]; n];
    let mut children: Vec<Vec<usize>> = vec![vec![]; n];
    let mut pairs: Vec<(usize, usize)> = vec![];
    {
        let mut seen = std::collections::HashSet::new();
        for &(f, t) in edges {
            if f != t && f < n && t < n && seen.insert((f, t)) {
                parents[f].push(t);
                children[t].push(f);
                pairs.push((f, t));
            }
        }
    }
    let connected: Vec<usize> = (0..n)
        .filter(|&v| !parents[v].is_empty() || !children[v].is_empty())
        .collect();
    let isolated: Vec<usize> = (0..n)
        .filter(|&v| parents[v].is_empty() && children[v].is_empty())
        .collect();
    let rank = ranks(n, &connected, &parents);
    let nlayers = connected.iter().map(|&v| rank[v] + 1).max().unwrap_or(0);

    // proper layering: dummies on edges that span several layers
    let mut up: Vec<Vec<usize>> = vec![vec![]; n]; // neighbours one layer to the left
    let mut down: Vec<Vec<usize>> = vec![vec![]; n]; // one layer to the right
    let mut layers: Vec<Vec<usize>> = vec![vec![]; nlayers];
    for &v in &connected {
        layers[rank[v]].push(v);
    }
    for &(f, t) in &pairs {
        let (lo, hi) = if rank[f] < rank[t] { (f, t) } else { (t, f) };
        if rank[lo] == rank[hi] {
            continue;
        }
        let mut prev = lo;
        for layer in layers.iter_mut().take(rank[hi]).skip(rank[lo] + 1) {
            let d = up.len();
            up.push(vec![prev]);
            down.push(vec![]);
            down[prev].push(d);
            layer.push(d);
            prev = d;
        }
        down[prev].push(hi);
        up[hi].push(prev);
    }
    order_layers(&mut layers, &up, &down);

    let area: f32 = sizes.iter().map(|s| (s.0 + GAP_X) * (s.1 + GAP_Y)).sum();
    let max_col_h = (area.sqrt() * 1.15).max(900.0);

    // columns of real tables: a layer taller than max_col_h wraps into several
    let mut cols: Vec<(Vec<usize>, bool)> = vec![]; // (tables, part of a wrapped layer)
    for l in &layers {
        let mut parts: Vec<Vec<usize>> = vec![];
        let mut cur: Vec<usize> = vec![];
        let mut h = 0.0;
        for &v in l.iter().filter(|&&v| v < n) {
            if !cur.is_empty() && h + sizes[v].1 > max_col_h {
                parts.push(std::mem::take(&mut cur));
                h = 0.0;
            }
            h += sizes[v].1 + GAP_Y;
            cur.push(v);
        }
        if !cur.is_empty() {
            parts.push(cur);
        }
        let wrapped = parts.len() > 1;
        cols.extend(parts.into_iter().map(|p| (p, wrapped)));
    }
    let mut x = 0.0f32;
    let mut ys = vec![0.0f32; n];
    for (col, _) in &cols {
        let w = col.iter().map(|&v| sizes[v].0).fold(0.0, f32::max);
        let mut y = (max_col_h - layer_height(col, sizes)) / 2.0;
        for &v in col {
            out[v].0 = x;
            ys[v] = y;
            y += sizes[v].1 + GAP_Y;
        }
        x += w + GAP_X;
    }
    // vertical placement: pull each table towards its neighbours, keeping order
    for pass in 0..6 {
        let range: Vec<usize> = if pass % 2 == 0 {
            (0..cols.len()).collect()
        } else {
            (0..cols.len()).rev().collect()
        };
        for ci in range {
            let (col, wrapped) = &cols[ci];
            if *wrapped {
                continue;
            }
            let want: Vec<f32> = col
                .iter()
                .map(|&v| {
                    let nb = parents[v].iter().chain(&children[v]);
                    let (sum, k) = nb.fold((0.0f32, 0usize), |(s, k), &u| {
                        (s + ys[u] + sizes[u].1 / 2.0, k + 1)
                    });
                    if k == 0 {
                        ys[v]
                    } else {
                        sum / k as f32 - sizes[v].1 / 2.0
                    }
                })
                .collect();
            let heights: Vec<f32> = col.iter().map(|&v| sizes[v].1).collect();
            for (&v, y) in col.iter().zip(place_in_order(&want, &heights)) {
                ys[v] = y;
            }
        }
    }
    let top = connected.iter().map(|&v| ys[v]).fold(f32::MAX, f32::min);
    let mut main_h = 0.0f32;
    for &v in &connected {
        out[v].1 = ys[v] - top;
        main_h = main_h.max(out[v].1 + sizes[v].1);
    }
    let main_w = if cols.is_empty() { 0.0 } else { x - GAP_X };

    // isolated tables: a grid below the connected part
    if !isolated.is_empty() {
        let cell_w = isolated.iter().map(|&v| sizes[v].0).fold(0.0, f32::max) + GAP_X / 2.0;
        let row_w = main_w.max(1500.0);
        let per_row = ((row_w + GAP_X / 2.0) / cell_w).floor().max(1.0) as usize;
        let mut y = if connected.is_empty() {
            0.0
        } else {
            main_h + ISLAND_GAP
        };
        for row in isolated.chunks(per_row) {
            let row_h = row.iter().map(|&v| sizes[v].1).fold(0.0, f32::max);
            for (k, &v) in row.iter().enumerate() {
                out[v] = (k as f32 * cell_w, y);
            }
            y += row_h + GAP_Y;
        }
    }
    out
}

/// Longest-path rank (parents first) after dropping the back edges of a DFS.
fn ranks(n: usize, connected: &[usize], parents: &[Vec<usize>]) -> Vec<usize> {
    let mut kept_parents: Vec<Vec<usize>> = vec![vec![]; n];
    let mut state = vec![0u8; n]; // 0 new, 1 on stack, 2 done
    for &root in connected {
        if state[root] != 0 {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(root, 0)];
        state[root] = 1;
        while let Some(top) = stack.last_mut() {
            let v = top.0;
            if top.1 < parents[v].len() {
                let p = parents[v][top.1];
                top.1 += 1;
                match state[p] {
                    0 => {
                        kept_parents[v].push(p);
                        state[p] = 1;
                        stack.push((p, 0));
                    }
                    2 => kept_parents[v].push(p),
                    _ => {} // back edge
                }
            } else {
                state[v] = 2;
                stack.pop();
            }
        }
    }
    let mut rank = vec![usize::MAX; n];
    for &v in connected {
        let mut stack = vec![v];
        while let Some(&u) = stack.last() {
            if rank[u] != usize::MAX {
                stack.pop();
                continue;
            }
            let pending: Vec<usize> = kept_parents[u]
                .iter()
                .copied()
                .filter(|&p| rank[p] == usize::MAX)
                .collect();
            if pending.is_empty() {
                rank[u] = kept_parents[u]
                    .iter()
                    .map(|&p| rank[p] + 1)
                    .max()
                    .unwrap_or(0);
                stack.pop();
            } else {
                stack.extend(pending);
            }
        }
    }
    rank
}

/// Crossings between the edges leaving `left` for the next layer.
fn pair_crossings(left: &[usize], down: &[Vec<usize>], pos: &[usize]) -> usize {
    let mut ends: Vec<(usize, usize)> = vec![];
    for &u in left {
        for &v in &down[u] {
            ends.push((pos[u], pos[v]));
        }
    }
    ends.sort_unstable();
    let mut n = 0;
    for i in 0..ends.len() {
        for j in i + 1..ends.len() {
            if ends[i].0 < ends[j].0 && ends[i].1 > ends[j].1 {
                n += 1;
            }
        }
    }
    n
}

fn total_crossings(layers: &[Vec<usize>], down: &[Vec<usize>], pos: &[usize]) -> usize {
    layers
        .iter()
        .take(layers.len().saturating_sub(1))
        .map(|l| pair_crossings(l, down, pos))
        .sum()
}

/// Barycentre sweeps (alternating direction) and adjacent swaps; keeps the
/// ordering with the fewest crossings seen.
fn order_layers(layers: &mut [Vec<usize>], up: &[Vec<usize>], down: &[Vec<usize>]) {
    let mut pos = vec![0usize; up.len()];
    for l in layers.iter() {
        for (i, &v) in l.iter().enumerate() {
            pos[v] = i;
        }
    }
    let mut best = layers.to_vec();
    let mut best_c = total_crossings(layers, down, &pos);
    for sweep in 0..SWEEPS {
        if best_c == 0 {
            break;
        }
        let forward = sweep % 2 == 0;
        let order: Vec<usize> = if forward {
            (1..layers.len()).collect()
        } else {
            (0..layers.len().saturating_sub(1)).rev().collect()
        };
        let nb = if forward { up } else { down };
        for li in order {
            let mut keyed: Vec<(f32, usize, usize)> = layers[li]
                .iter()
                .map(|&v| {
                    let b = if nb[v].is_empty() {
                        pos[v] as f32
                    } else {
                        nb[v].iter().map(|&u| pos[u] as f32).sum::<f32>() / nb[v].len() as f32
                    };
                    (b, pos[v], v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            layers[li] = keyed.into_iter().map(|k| k.2).collect();
            for (i, &v) in layers[li].iter().enumerate() {
                pos[v] = i;
            }
        }
        transpose(layers, up, down, &mut pos);
        let c = total_crossings(layers, down, &pos);
        if c < best_c {
            best_c = c;
            best = layers.to_vec();
        }
    }
    layers.clone_from_slice(&best);
}

/// Swap neighbours in a layer while that removes crossings.
fn transpose(layers: &mut [Vec<usize>], up: &[Vec<usize>], down: &[Vec<usize>], pos: &mut [usize]) {
    // crossings among the edges of a and b if a stays before b
    let cost = |a: usize, b: usize, pos: &[usize]| {
        let mut c = 0;
        for nb in [up, down] {
            for &x in &nb[a] {
                for &y in &nb[b] {
                    if pos[x] > pos[y] {
                        c += 1;
                    }
                }
            }
        }
        c
    };
    for _ in 0..4 {
        let mut improved = false;
        for l in layers.iter_mut() {
            for i in 0..l.len().saturating_sub(1) {
                let (a, b) = (l[i], l[i + 1]);
                if cost(b, a, pos) < cost(a, b, pos) {
                    l.swap(i, i + 1);
                    pos[a] = i + 1;
                    pos[b] = i;
                    improved = true;
                }
            }
        }
        if !improved {
            break;
        }
    }
}

/// Tops for boxes of `heights`, kept in this order with `GAP_Y` between them,
/// as close as possible (least squares) to the wanted tops: pool adjacent
/// violators on the tops minus the stacked height before each box.
fn place_in_order(want: &[f32], heights: &[f32]) -> Vec<f32> {
    let mut off = Vec::with_capacity(want.len());
    let mut acc = 0.0;
    for h in heights {
        off.push(acc);
        acc += h + GAP_Y;
    }
    // blocks of (sum, count)
    let mut blocks: Vec<(f32, usize)> = vec![];
    for (w, o) in want.iter().zip(&off) {
        blocks.push((w - o, 1));
        while blocks.len() > 1 {
            let (s2, c2) = blocks[blocks.len() - 1];
            let (s1, c1) = blocks[blocks.len() - 2];
            if s1 / c1 as f32 > s2 / c2 as f32 {
                blocks.pop();
                *blocks.last_mut().unwrap() = (s1 + s2, c1 + c2);
            } else {
                break;
            }
        }
    }
    let mut out = Vec::with_capacity(want.len());
    for (s, c) in blocks {
        for _ in 0..c {
            let k = out.len();
            out.push(s / c as f32 + off[k]);
        }
    }
    out
}

/// Layout for a centred scope: one column per hop distance (`levels[i]`, 0 =
/// centre), columns left to right, each stacked top to bottom and centred
/// vertically on the tallest column. Within a column tables keep input order.
pub fn level_layout(sizes: &[(f32, f32)], levels: &[u32]) -> Vec<(f32, f32)> {
    let n = sizes.len().min(levels.len());
    let nlev = levels[..n]
        .iter()
        .map(|&l| l as usize + 1)
        .max()
        .unwrap_or(0);
    let mut cols: Vec<Vec<usize>> = vec![vec![]; nlev];
    for i in 0..n {
        cols[levels[i] as usize].push(i);
    }
    let heights: Vec<f32> = cols
        .iter()
        .map(|c| {
            if c.is_empty() {
                0.0
            } else {
                layer_height(c, sizes)
            }
        })
        .collect();
    let tallest = heights.iter().copied().fold(0.0, f32::max);
    let mut out = vec![(0.0f32, 0.0f32); sizes.len()];
    let mut x = 0.0;
    for (c, col) in cols.iter().enumerate() {
        if col.is_empty() {
            continue;
        }
        let mut y = (tallest - heights[c]) / 2.0;
        for &v in col {
            out[v] = (x, y);
            y += sizes[v].1 + GAP_Y;
        }
        x += col.iter().map(|&v| sizes[v].0).fold(0.0, f32::max) + GAP_X;
    }
    out
}

fn layer_height(l: &[usize], sizes: &[(f32, f32)]) -> f32 {
    l.iter().map(|&v| sizes[v].1).sum::<f32>() + GAP_Y * (l.len() as f32 - 1.0)
}

/// Number of crossings between relation lines, each drawn straight from the
/// facing side of one card to the facing side of the other (row centres are
/// not known here, so card centres are used). Pairs that share a table are
/// not counted. A measure for comparing layouts, O(edges²).
pub fn crossings(pos: &[(f32, f32)], sizes: &[(f32, f32)], edges: &[(usize, usize)]) -> usize {
    let seg = |&(a, b): &(usize, usize)| {
        let (ca, cb) = (
            (pos[a].0 + sizes[a].0 / 2.0, pos[a].1 + sizes[a].1 / 2.0),
            (pos[b].0 + sizes[b].0 / 2.0, pos[b].1 + sizes[b].1 / 2.0),
        );
        let side = |i: usize, toward: f32| {
            if toward >= pos[i].0 + sizes[i].0 / 2.0 {
                pos[i].0 + sizes[i].0
            } else {
                pos[i].0
            }
        };
        ((side(a, cb.0), ca.1), (side(b, ca.0), cb.1))
    };
    let segs: Vec<_> = edges
        .iter()
        .filter(|(a, b)| a != b && *a < pos.len() && *b < pos.len())
        .map(|e| (*e, seg(e)))
        .collect();
    let orient = |p: (f32, f32), q: (f32, f32), r: (f32, f32)| {
        let v = (q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0);
        if v > 1e-3 {
            1
        } else if v < -1e-3 {
            -1
        } else {
            0
        }
    };
    let mut n = 0;
    for i in 0..segs.len() {
        for j in i + 1..segs.len() {
            let ((a, b), (p1, p2)) = segs[i];
            let ((c, d), (q1, q2)) = segs[j];
            if a == c || a == d || b == c || b == d {
                continue;
            }
            let (o1, o2) = (orient(p1, p2, q1), orient(p1, p2, q2));
            let (o3, o4) = (orient(q1, q2, p1), orient(q1, q2, p2));
            if o1 * o2 < 0 && o3 * o4 < 0 {
                n += 1;
            }
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{card_size, sample};

    fn overlaps(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
        a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
    }

    fn inputs(n: usize) -> (Vec<(f32, f32)>, Vec<(usize, usize)>) {
        let m = sample::generate(n);
        let idx: std::collections::HashMap<String, usize> = m
            .tables
            .iter()
            .enumerate()
            .map(|(i, t)| (t.key(), i))
            .collect();
        let sizes: Vec<_> = m.tables.iter().map(card_size).collect();
        let edges: Vec<_> = m
            .relations
            .iter()
            .map(|r| (idx[&r.from_table], idx[&r.to_table]))
            .collect();
        (sizes, edges)
    }

    #[test]
    fn no_overlap_and_deterministic() {
        for n in [1, 2, 7, 30, 200, 600] {
            let (sizes, edges) = inputs(n);
            let a = auto_layout(&sizes, &edges);
            assert_eq!(a, auto_layout(&sizes, &edges), "deterministic");
            for i in 0..n {
                for j in i + 1..n {
                    let (ra, rb) = (
                        (a[i].0, a[i].1, sizes[i].0, sizes[i].1),
                        (a[j].0, a[j].1, sizes[j].0, sizes[j].1),
                    );
                    assert!(!overlaps(ra, rb), "tables {i} and {j} overlap at n={n}");
                }
            }
        }
    }

    #[test]
    fn cycles_self_loops_and_isolated_do_not_overlap() {
        let sizes = vec![(170.0, 100.0); 8];
        let edges = [(0, 1), (1, 2), (2, 0), (3, 3), (4, 0), (4, 1)];
        let a = auto_layout(&sizes, &edges);
        for i in 0..8 {
            for j in i + 1..8 {
                assert!(!overlaps(
                    (a[i].0, a[i].1, 170.0, 100.0),
                    (a[j].0, a[j].1, 170.0, 100.0)
                ));
            }
        }
        // a referenced table sits left of its referencing table
        let b = auto_layout(&sizes, &[(1, 0)]);
        assert!(b[0].0 < b[1].0);
    }

    #[test]
    fn levels_become_columns_without_overlap() {
        let sizes = vec![
            (200.0, 100.0),
            (180.0, 60.0),
            (180.0, 80.0),
            (190.0, 70.0),
            (170.0, 50.0),
        ];
        let p = level_layout(&sizes, &[0, 1, 1, 2, 2]);
        assert_eq!(p[0].0, 0.0);
        assert!(p[1].0 > p[0].0 + 200.0 && p[1].0 == p[2].0);
        assert!(p[3].0 > p[1].0 + 180.0 && p[3].0 == p[4].0);
        for i in 0..5 {
            for j in i + 1..5 {
                assert!(!overlaps(
                    (p[i].0, p[i].1, sizes[i].0, sizes[i].1),
                    (p[j].0, p[j].1, sizes[j].0, sizes[j].1)
                ));
            }
        }
        // the centre is vertically centred against the tallest column (60+24+80 = 164)
        assert_eq!(p[0].1, (164.0 - 100.0) / 2.0);
        assert!(level_layout(&[], &[]).is_empty());
    }

    #[test]
    fn large_schema_is_fast() {
        let (sizes, edges) = inputs(200);
        let t = std::time::Instant::now();
        auto_layout(&sizes, &edges);
        assert!(t.elapsed().as_millis() < 200, "{:?}", t.elapsed());
    }

    #[test]
    fn ordering_removes_avoidable_crossings() {
        // parents 0, 1; children listed so that the input order crosses:
        // 2 -> 1, 3 -> 0, 4 -> 1, 5 -> 0
        let sizes = vec![(170.0, 80.0); 6];
        let edges = [(2, 1), (3, 0), (4, 1), (5, 0)];
        let p = auto_layout(&sizes, &edges);
        assert_eq!(crossings(&p, &sizes, &edges), 0);
        // children sit next to their parent
        assert!((p[3].1 - p[0].1).abs() < (p[3].1 - p[1].1).abs());
    }

    #[test]
    fn sample_crossings_stay_low() {
        // the previous layout (plain barycentre sweeps, stacked layers) had
        // 56 / 224 / 2671 crossings on these samples
        for (n, max) in [(30, 30), (60, 164), (200, 2166)] {
            let (sizes, edges) = inputs(n);
            let c = crossings(&auto_layout(&sizes, &edges), &sizes, &edges);
            assert!(c <= max, "n={n}: {c} crossings, expected at most {max}");
        }
    }

    #[test]
    fn placement_keeps_order_and_gaps() {
        let h = [40.0, 60.0, 20.0];
        // everyone wants the same top: they stack around it
        let y = place_in_order(&[100.0, 100.0, 100.0], &h);
        assert!(y[1] >= y[0] + 40.0 + GAP_Y - 1e-3 && y[2] >= y[1] + 60.0 + GAP_Y - 1e-3);
        // wishes that already fit are kept
        let y = place_in_order(&[0.0, 200.0, 400.0], &h);
        assert_eq!(y, vec![0.0, 200.0, 400.0]);
    }

    #[test]
    fn isolated_tables_form_a_grid_below() {
        let sizes = vec![(170.0, 80.0), (170.0, 80.0), (200.0, 60.0), (150.0, 90.0)];
        let p = auto_layout(&sizes, &[(1, 0)]);
        assert!(p[2].1 > 80.0 && p[2].1 == p[3].1, "same row: {p:?}");
        assert_eq!(p[3].0 - p[2].0, 200.0 + GAP_X / 2.0, "uniform cells");
    }

    #[test]
    fn crossing_count() {
        let sizes = vec![(10.0, 10.0); 4];
        // 0 -> 3 and 1 -> 2 drawn as an X
        let pos = vec![(0.0, 0.0), (0.0, 100.0), (100.0, 0.0), (100.0, 100.0)];
        assert_eq!(crossings(&pos, &sizes, &[(0, 3), (1, 2)]), 1);
        assert_eq!(crossings(&pos, &sizes, &[(0, 2), (1, 3)]), 0);
        // lines that share a table never count
        assert_eq!(crossings(&pos, &sizes, &[(0, 3), (0, 2)]), 0);
    }
}

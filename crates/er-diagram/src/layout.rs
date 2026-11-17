//! Deterministic layered layout: referenced tables left, referencing tables
//! right; barycenter ordering inside a layer; tall layers wrap into several
//! columns; tables without relations are shelf-packed below. Pure function,
//! so it can run on a background thread.

const GAP_X: f32 = 90.0;
const GAP_Y: f32 = 28.0;
const ISLAND_GAP: f32 = 80.0;

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
    {
        let mut seen = std::collections::HashSet::new();
        for &(f, t) in edges {
            if f != t && f < n && t < n && seen.insert((f, t)) {
                parents[f].push(t);
                children[t].push(f);
            }
        }
    }
    let connected: Vec<usize> = (0..n)
        .filter(|&v| !parents[v].is_empty() || !children[v].is_empty())
        .collect();
    let isolated: Vec<usize> = (0..n)
        .filter(|&v| parents[v].is_empty() && children[v].is_empty())
        .collect();

    // break cycles: iterative DFS along child -> parent, dropping back edges
    let mut kept_parents: Vec<Vec<usize>> = vec![vec![]; n];
    {
        let mut state = vec![0u8; n]; // 0 new, 1 on stack, 2 done
        for &root in &connected {
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
    }
    // longest-path rank, parents first
    let mut rank = vec![usize::MAX; n];
    for &v in &connected {
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
    let nlayers = connected.iter().map(|&v| rank[v] + 1).max().unwrap_or(0);
    let mut layers: Vec<Vec<usize>> = vec![vec![]; nlayers];
    for &v in &connected {
        layers[rank[v]].push(v);
    }

    let area: f32 = sizes.iter().map(|s| (s.0 + GAP_X) * (s.1 + GAP_Y)).sum();
    let max_col_h = (area.sqrt() * 1.15).max(900.0);

    // barycenter sweeps on center-y estimates from stacking
    let mut cy = vec![0.0f32; n];
    let stack_y = |layers: &Vec<Vec<usize>>, cy: &mut Vec<f32>| {
        for l in layers {
            let mut y = 0.0;
            for &v in l {
                cy[v] = y + sizes[v].1 / 2.0;
                y += sizes[v].1 + GAP_Y;
            }
            let total = y - GAP_Y;
            for &v in l {
                cy[v] -= total / 2.0;
            }
        }
    };
    stack_y(&layers, &mut cy);
    for sweep in 0..6 {
        let down = sweep % 2 == 0;
        let order: Vec<usize> = if down {
            (0..nlayers).collect()
        } else {
            (0..nlayers).rev().collect()
        };
        for li in order {
            let mut keyed: Vec<(f32, usize, usize)> = layers[li]
                .iter()
                .enumerate()
                .map(|(pos, &v)| {
                    let nb: Vec<usize> = parents[v]
                        .iter()
                        .chain(children[v].iter())
                        .copied()
                        .filter(|&u| (down && rank[u] < li) || (!down && rank[u] > li))
                        .collect();
                    let b = if nb.is_empty() {
                        cy[v]
                    } else {
                        nb.iter().map(|&u| cy[u]).sum::<f32>() / nb.len() as f32
                    };
                    (b, pos, v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            layers[li] = keyed.into_iter().map(|k| k.2).collect();
        }
        stack_y(&layers, &mut cy);
    }

    // coordinates: wrap each layer into columns no taller than max_col_h
    let mut x = 0.0f32;
    let mut main_h = 0.0f32;
    for l in &layers {
        let mut cols: Vec<Vec<usize>> = vec![];
        let mut cur: Vec<usize> = vec![];
        let mut h = 0.0;
        for &v in l {
            if !cur.is_empty() && h + sizes[v].1 > max_col_h {
                cols.push(std::mem::take(&mut cur));
                h = 0.0;
            }
            h += sizes[v].1 + GAP_Y;
            cur.push(v);
        }
        if !cur.is_empty() {
            cols.push(cur);
        }
        let single = cols.len() == 1;
        for col in cols {
            let w = col.iter().map(|&v| sizes[v].0).fold(0.0, f32::max);
            let colh = layer_height(&col, sizes);
            let mut y = if single {
                (max_col_h - colh) / 2.0
            } else {
                0.0
            };
            y = y.max(0.0);
            for &v in &col {
                out[v] = (x, y);
                y += sizes[v].1 + GAP_Y;
            }
            main_h = main_h.max(y);
            x += w + GAP_X;
        }
    }
    let main_w = if layers.is_empty() { 0.0 } else { x - GAP_X };

    // isolated tables: shelves below the connected part
    if !isolated.is_empty() {
        let row_w = main_w.max(1500.0);
        let mut y = if main_h > 0.0 {
            main_h + ISLAND_GAP
        } else {
            0.0
        };
        let (mut cx, mut row_h) = (0.0f32, 0.0f32);
        for &v in &isolated {
            if cx > 0.0 && cx + sizes[v].0 > row_w {
                y += row_h + GAP_Y;
                cx = 0.0;
                row_h = 0.0;
            }
            out[v] = (cx, y);
            cx += sizes[v].0 + GAP_X / 2.0;
            row_h = row_h.max(sizes[v].1);
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
        // the centre is vertically centred against the tallest column (60+28+80 = 168)
        assert_eq!(p[0].1, (168.0 - 100.0) / 2.0);
        assert!(level_layout(&[], &[]).is_empty());
    }

    #[test]
    fn large_schema_is_fast() {
        let (sizes, edges) = inputs(200);
        let t = std::time::Instant::now();
        auto_layout(&sizes, &edges);
        assert!(t.elapsed().as_millis() < 200, "{:?}", t.elapsed());
    }
}

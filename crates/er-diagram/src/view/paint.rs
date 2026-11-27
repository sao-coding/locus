//! Painting of the diagram: dotted background, relation lines with their
//! crow's-foot ends, cards, the minimap and the search-hit pulse.
use super::*;
use crate::route::Seg;

fn rc(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
    Bounds::new(point(px(x), px(y)), size(px(w), px(h)))
}

fn pt(x: f32, y: f32) -> Point<Pixels> {
    point(px(x), px(y))
}

/// Largest minimap box (screen pixels) and its distance from the corner.
const MM_W: f32 = 200.0;
const MM_H: f32 = 140.0;
const MM_MARGIN: f32 = 12.0;
const MM_PAD: f32 = 6.0;

impl ErDiagram {
    pub(super) fn paint_scene(
        &mut self,
        b: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.nodes.is_empty() || self.layouting {
            self.mm.set(None);
            return;
        }
        let (vw, vh) = (b.size.width.as_f32(), b.size.height.as_f32());
        if self.pending_fit && vw > 1.0 {
            let old = self.zoom;
            self.do_fit(vw, vh);
            self.pending_fit = false;
            self.zoom_event(old, cx);
        }
        let mut animating = self.step_anim(cx);
        let pal = {
            let t = cx.theme();
            Pal {
                bg: t.background,
                card: t.popover,
                header: t.muted,
                border: t.border,
                fg: t.foreground,
                muted: t.muted_foreground,
                primary: t.primary,
                key: t.warning,
                info: t.info,
                line: t.muted_foreground,
                mono: t.mono_font_family.clone(),
                tints: [t.blue, t.green, t.yellow, t.magenta, t.cyan, t.red],
                warn: t.warning,
            }
        };
        let z = self.zoom;
        let f = Frame {
            z,
            ox: b.origin.x.as_f32() + self.pan.0,
            oy: b.origin.y.as_f32() + self.pan.1,
            bx: b.origin.x.as_f32(),
            by: b.origin.y.as_f32(),
            vw,
            vh,
        };
        self.refresh_geoms();
        self.compute_focus();
        // highlighted rows: key columns of the hovered / selected relation
        let mut rows: Vec<(usize, usize)> = vec![];
        for ri in [self.sel_rel, self.hover_rel].into_iter().flatten() {
            let r = &self.rels[ri];
            rows.extend(r.from_cols.iter().map(|&c| (r.from, c)));
            rows.extend(r.to_cols.iter().map(|&c| (r.to, c)));
        }
        let fpx = (BASE_FONT * z).round().max(5.0);
        let lh = fpx * 1.25;
        if z >= 0.3 {
            self.ensure_shared(fpx, &pal, window);
        }
        let sc = Scene {
            f,
            pal: &pal,
            fpx,
            lh,
            rows: &rows,
        };
        window.with_content_mask(Some(ContentMask { bounds: b }), |window| {
            self.paint_dots(f, &pal, window);
            self.paint_relations(&sc, window);
            self.paint_cards(&sc, window, cx);
            if self.paint_pulse(&sc, window) {
                animating = true;
            }
            // rubber band
            if let Drag::Rubber { start, cur, .. } = &self.drag {
                let (x0, x1) = (start.0.min(cur.0), start.0.max(cur.0));
                let (y0, y1) = (start.1.min(cur.1), start.1.max(cur.1));
                window.paint_quad(quad(
                    rc(f.bx + x0, f.by + y0, x1 - x0, y1 - y0),
                    px(0.0),
                    pal.primary.opacity(0.12),
                    px(1.0),
                    pal.primary,
                    BorderStyle::Solid,
                ));
            }
            self.paint_minimap(f, &pal, window);
        });
        if animating {
            window.request_animation_frame();
        }
    }

    /// Tables and relations in focus this frame: the hovered card or line,
    /// the selection, and search hits. Everything else is dimmed while
    /// something is in focus.
    fn compute_focus(&mut self) {
        let (nt, nr) = (self.nodes.len(), self.rels.len());
        self.focus_t.clear();
        self.focus_t.resize(nt, false);
        self.focus_r.clear();
        self.focus_r.resize(nr, false);
        let hover_t = if matches!(self.drag, Drag::None) {
            self.hover_table
        } else {
            None
        };
        let mut active = false;
        // a JOIN path shows only itself, not every neighbour of the selection
        let by_sel = self.path_hl.is_none();
        for ri in 0..nr {
            if !self.rel_vis[ri] {
                continue;
            }
            let r = &self.rels[ri];
            let on = self.sel_rel == Some(ri)
                || self.hover_rel == Some(ri)
                || (by_sel && (self.sel.contains(&r.from) || self.sel.contains(&r.to)))
                || hover_t == Some(r.from)
                || hover_t == Some(r.to);
            if on {
                self.focus_r[ri] = true;
                self.focus_t[r.from] = true;
                self.focus_t[r.to] = true;
                active = true;
            }
        }
        for &i in &self.sel {
            self.focus_t[i] = true;
            active = true;
        }
        if let Some(t) = hover_t {
            self.focus_t[t] = true;
            active = true;
        }
        if let Some((ts, rs)) = &self.path_hl {
            for &i in ts {
                self.focus_t[i] = true;
            }
            for &ri in rs {
                self.focus_r[ri] = true;
            }
            active = true;
        }
        if let Some(s) = &self.search
            && !s.query.trim().is_empty()
        {
            for &i in &s.matches {
                self.focus_t[i] = true;
            }
            active = true;
        }
        self.focus_active = active;
    }

    fn ensure_text(
        &mut self,
        i: usize,
        fpx: f32,
        room: (f32, f32),
        pal: &Pal,
        window: &mut Window,
    ) {
        let key = (fpx * 2.0) as u32;
        let room_key = (room.0.round() as i32, room.1.round() as i32);
        let sig = (pal.fg, pal.muted);
        if let Some(t) = &self.nodes[i].text
            && t.px == key
            && t.room == room_key
            && t.sig == sig
        {
            return;
        }
        let font = window.text_style().font();
        let mut bold = font.clone();
        bold.weight = FontWeight::SEMIBOLD;
        let mut mono = font;
        mono.family = pal.mono.clone();
        let ts = window.text_system().clone();
        let shape = |s: &str, f: &Font, size: f32, color: Hsla| {
            let run = TextRun {
                len: s.len(),
                font: f.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            ts.shape_line(SharedString::from(s.to_string()), px(size), &[run], None)
        };
        let t = &self.model.tables[i];
        let count = self
            .counts
            .get(&self.nodes[i].key)
            .map(|c| shape(&fmt_count(*c), &mono, fpx * 0.88, pal.muted));
        // A row's name and type share the card's width: when both do not fit,
        // the name gives way first (never below half the room), then the type;
        // each is cut with an ellipsis instead of painting over its neighbour.
        let (avail, gap) = room;
        let fit = |s: &str, f: &Font, size: f32, color: Hsla, budget: f32| {
            let full = shape(s, f, size, color);
            if full.width.as_f32() <= budget {
                return full;
            }
            let chars: Vec<usize> = s.char_indices().map(|(b, _)| b).collect();
            let (mut lo, mut hi) = (0, chars.len());
            while lo < hi {
                let mid = (lo + hi).div_ceil(2);
                let cut = format!("{}…", &s[..chars[mid - 1]]);
                if shape(&cut, f, size, color).width.as_f32() <= budget {
                    lo = mid;
                } else {
                    hi = mid - 1;
                }
            }
            let cut = if lo == 0 {
                "…".to_string()
            } else {
                format!("{}…", &s[..chars[lo - 1]])
            };
            shape(&cut, f, size, color)
        };
        let mut names = Vec::with_capacity(t.columns.len());
        let mut types = Vec::with_capacity(t.columns.len());
        for c in &t.columns {
            let ty = if c.nullable {
                format!("{}?", c.type_name)
            } else {
                c.type_name.clone()
            };
            let name = shape(&c.name, &mono, fpx * 0.96, pal.fg);
            let kind = shape(&ty, &mono, fpx * 0.85, pal.muted);
            if name.width.as_f32() + gap + kind.width.as_f32() <= avail {
                names.push(name);
                types.push(kind);
                continue;
            }
            let name_budget = (avail - gap - kind.width.as_f32()).max(avail * 0.5);
            let name = fit(&c.name, &mono, fpx * 0.96, pal.fg, name_budget);
            let type_budget = (avail - gap - name.width.as_f32()).max(0.0);
            names.push(name);
            types.push(fit(&ty, &mono, fpx * 0.85, pal.muted, type_budget));
        }
        let cache = TextCache {
            px: key,
            room: room_key,
            sig,
            header: shape(&t.name, &bold, fpx, pal.fg),
            count,
            names,
            types,
        };
        self.nodes[i].text = Some(cache);
    }

    fn ensure_shared(&mut self, fpx: f32, pal: &Pal, window: &mut Window) {
        let key = (fpx * 2.0) as u32;
        let sig = (pal.key, pal.info, pal.primary, self.centre_label.clone());
        if let Some(s) = &self.shared
            && s.px == key
            && s.sig == sig
        {
            return;
        }
        let font = window.text_style().font();
        let mut bold = font;
        bold.weight = FontWeight::SEMIBOLD;
        let ts = window.text_system().clone();
        let shape = |s: &str, f: &Font, size: f32, color: Hsla| {
            let run = TextRun {
                len: s.len(),
                font: f.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            ts.shape_line(SharedString::from(s.to_string()), px(size), &[run], None)
        };
        self.shared = Some(SharedText {
            px: key,
            sig: sig.clone(),
            pk: shape("PK", &bold, fpx * 0.72, pal.key),
            fk: shape("FK", &bold, fpx * 0.72, pal.info),
            centre: shape(&sig.3, &bold, fpx * 0.83, pal.primary),
        });
    }

    /// Dotted canvas background: a fixed 20 px grid that follows the pan.
    fn paint_dots(&self, f: Frame, pal: &Pal, window: &mut Window) {
        let Frame { bx, by, vw, vh, .. } = f;
        let dot = 2.0f32;
        let dc = pal.fg.opacity(0.22);
        let gx = (self.pan.0).rem_euclid(DOT_GAP);
        let gy = (self.pan.1).rem_euclid(DOT_GAP);
        let mut y = gy;
        while y < vh {
            let mut x = gx;
            while x < vw {
                window.paint_quad(fill(
                    rc(bx + x - dot / 2.0, by + y - dot / 2.0, dot, dot),
                    dc,
                ));
                x += DOT_GAP;
            }
            y += DOT_GAP;
        }
    }

    /// Relation lines (highlighted ones on top), one path per line with its
    /// end marks, so a line that did not change shape reuses its
    /// tessellation (gpui-fast keeps paths by shape, wherever they are drawn).
    fn paint_relations(&self, sc: &Scene, window: &mut Window) {
        let Frame {
            z,
            ox,
            oy,
            bx,
            by,
            vw,
            vh,
        } = sc.f;
        let pal = sc.pal;
        let sx = |x: f32| ox + x * z;
        let sy = |y: f32| oy + y * z;
        let lw = (1.5 * z).clamp(1.2, 2.4);
        let marks = z >= 0.35;
        for pass in [false, true] {
            for ri in 0..self.rels.len() {
                if !self.rel_vis[ri] || self.focus_r[ri] != pass {
                    continue;
                }
                let rel = &self.rels[ri];
                let Some((_, e)) = &rel.geom else { continue };
                let m = 16.0 * z; // room for the end marks
                let (x, y, w, h) = (
                    sx(e.bbox.0) - m,
                    sy(e.bbox.1) - m,
                    e.bbox.2 * z + 2.0 * m,
                    e.bbox.3 * z + 2.0 * m,
                );
                if x + w < bx || x > bx + vw || y + h < by || y > by + vh {
                    continue;
                }
                let inferred = self.model.relations[rel.model_ix].inferred;
                let base = if inferred {
                    pal.line.opacity(0.6)
                } else {
                    pal.line
                };
                let (col, width) = if pass {
                    (pal.primary, lw * 1.4)
                } else if self.focus_active {
                    (base.opacity(base.a * 0.35), lw)
                } else {
                    (base, lw)
                };
                let mut pb = PathBuilder::stroke(px(width));
                if inferred {
                    // an inferred relation (no declared key) is dashed
                    pb = pb.dash_array(&[px(5.0 * z), px(4.0 * z)]);
                }
                pb.move_to(pt(sx(e.start.0), sy(e.start.1)));
                for s in &e.segs {
                    match *s {
                        Seg::Line(p) => pb.line_to(pt(sx(p.0), sy(p.1))),
                        Seg::Quad(c, p) => pb.curve_to(pt(sx(p.0), sy(p.1)), pt(sx(c.0), sy(c.1))),
                        Seg::Cubic(c1, c2, p) => pb.cubic_bezier_to(
                            pt(sx(p.0), sy(p.1)),
                            pt(sx(c1.0), sy(c1.1)),
                            pt(sx(c2.0), sy(c2.1)),
                        ),
                    }
                }
                let mut circles: [Option<(f32, f32)>; 2] = [None, None];
                if marks {
                    for (k, (end, (p, d))) in [(rel.ends.0, e.from), (rel.ends.1, e.to)]
                        .into_iter()
                        .enumerate()
                    {
                        let (segs, n, circle) = route::end_mark(end, p, d);
                        for (a, b) in &segs[..n] {
                            pb.move_to(pt(sx(a.0), sy(a.1)));
                            pb.line_to(pt(sx(b.0), sy(b.1)));
                        }
                        circles[k] = circle.map(|c| (sx(c.0), sy(c.1)));
                    }
                }
                if let Ok(p) = pb.build() {
                    window.paint_path(p, col);
                }
                // the circle of an optional end is hollow: background inside
                let r = route::MARK_R * z;
                for (cx_, cy_) in circles.into_iter().flatten() {
                    window.paint_quad(quad(
                        rc(cx_ - r, cy_ - r, 2.0 * r, 2.0 * r),
                        px(r),
                        pal.bg,
                        px(width.min(1.6)),
                        col,
                        BorderStyle::Solid,
                    ));
                }
            }
        }
    }

    /// Cards: frame, header and, when zoomed in enough, the column rows.
    fn paint_cards(&mut self, sc: &Scene, window: &mut Window, cx: &mut Context<Self>) {
        let Frame {
            z,
            ox,
            oy,
            bx,
            by,
            vw,
            vh,
        } = sc.f;
        let (pal, fpx) = (sc.pal, sc.fpx);
        let order = std::mem::take(&mut self.order);
        for &i in &order {
            if !self.vis[i] {
                continue;
            }
            let (nx, ny, nw, nh) = {
                let n = &self.nodes[i];
                (ox + n.x * z, oy + n.y * z, n.w * z, n.h * z)
            };
            if nx + nw < bx || nx > bx + vw || ny + nh < by || ny > by + vh {
                continue;
            }
            let selected = self.sel.contains(&i);
            let is_centre = self.centre == Some(i);
            let hovered = self.hover_table == Some(i) && matches!(self.drag, Drag::None);
            let in_focus = self.focus_t[i];
            let (bc, bw) = if selected || is_centre {
                (pal.primary, 2.0)
            } else if hovered {
                (pal.primary.opacity(0.8), 1.5)
            } else if in_focus && self.focus_active {
                (pal.primary.opacity(0.55), 1.5)
            } else {
                (pal.border, 1.0)
            };
            let rad = 8.0 * z;
            window.paint_quad(quad(
                rc(nx, ny, nw, nh),
                px(rad),
                pal.card,
                px(bw),
                bc,
                BorderStyle::Solid,
            ));
            let hh = HEADER_H * z;
            let header_only = self.nodes[i].shown.is_empty() && nh <= hh + 0.5;
            let cb = CardBox { nx, ny, nw, nh, hh };
            window.paint_quad(quad(
                rc(
                    nx + 1.0,
                    ny + 1.0,
                    nw - 2.0,
                    if header_only { hh - 2.0 } else { hh - 1.0 },
                ),
                if header_only {
                    Corners::all(px((rad - 1.0).max(0.0)))
                } else {
                    Corners {
                        top_left: px(rad),
                        top_right: px(rad),
                        bottom_right: px(0.0),
                        bottom_left: px(0.0),
                    }
                },
                match self.groups.1.get(i).copied().flatten() {
                    Some(g) if self.color_groups => pal
                        .header
                        .blend(pal.tints[g % pal.tints.len()].opacity(0.24)),
                    _ => pal.header,
                },
                px(0.0),
                transparent_black(),
                BorderStyle::Solid,
            ));
            if !header_only {
                window.paint_quad(fill(rc(nx + 1.0, ny + hh, nw - 2.0, 1.0), pal.border));
            }
            if z >= 0.3 {
                self.ensure_text(i, fpx, ((nw - 47.0 * z).max(0.0), 10.0 * z), pal, window);
                self.paint_card_header(i, cb, is_centre, hovered, sc, window, cx);
                if z >= 0.4 {
                    self.paint_card_rows(i, cb, sc, window, cx);
                }
            }
            if self.focus_active && !in_focus {
                // dim to about 35 %
                window.paint_quad(quad(
                    rc(nx - 1.0, ny - 1.0, nw + 2.0, nh + 2.0),
                    px(rad),
                    pal.bg.opacity(0.65),
                    px(0.0),
                    transparent_black(),
                    BorderStyle::Solid,
                ));
            }
        }
        self.order = order;
    }

    /// Table icon, name, centre tag, row count and the collapse chevron.
    #[allow(clippy::too_many_arguments)]
    fn paint_card_header(
        &self,
        i: usize,
        cb: CardBox,
        is_centre: bool,
        hovered: bool,
        sc: &Scene,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let CardBox { nx, ny, nw, nh, hh } = cb;
        let (z, pal, lh) = (sc.f.z, sc.pal, sc.lh);
        let t = self.nodes[i].text.as_ref().unwrap();
        let sh = self.shared.as_ref().unwrap();
        let flipped = self.flipped.contains(&i);
        window.with_content_mask(
            Some(ContentMask {
                bounds: rc(nx, ny, nw, nh),
            }),
            |window| {
                // table icon: a small grid
                let (ix, iw, ih) = (nx + 10.0 * z, 13.0 * z, 11.0 * z);
                let iy = ny + (hh - ih) / 2.0;
                window.paint_quad(quad(
                    rc(ix, iy, iw, ih),
                    px(2.0 * z),
                    transparent_black(),
                    px(1.2),
                    pal.muted,
                    BorderStyle::Solid,
                ));
                window.paint_quad(fill(rc(ix, iy + ih / 3.0, iw, 1.0), pal.muted));
                window.paint_quad(fill(
                    rc(ix + iw / 2.0, iy + ih / 3.0, 1.0, ih * 2.0 / 3.0),
                    pal.muted,
                ));
                let _ = t.header.paint(
                    pt(nx + 30.0 * z, ny + (hh - lh) / 2.0),
                    px(lh),
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
                // right side: chevron, centre tag, then the row count
                let chev_x = nx + nw - CHEVRON_W * z / 2.0 - 2.0 * z;
                if z >= 0.5 && (hovered || flipped) {
                    let shows_rows = !self.nodes[i].shown.is_empty();
                    let cy = ny + hh / 2.0;
                    let u = z;
                    let mut pb = PathBuilder::stroke(px((1.4 * z).clamp(1.0, 2.0)));
                    if shows_rows {
                        // ▾: click to collapse
                        pb.move_to(pt(chev_x - 3.5 * u, cy - 1.75 * u));
                        pb.line_to(pt(chev_x, cy + 1.75 * u));
                        pb.line_to(pt(chev_x + 3.5 * u, cy - 1.75 * u));
                    } else {
                        // ▸: click to expand
                        pb.move_to(pt(chev_x - 1.75 * u, cy - 3.5 * u));
                        pb.line_to(pt(chev_x + 1.75 * u, cy));
                        pb.line_to(pt(chev_x - 1.75 * u, cy + 3.5 * u));
                    }
                    if let Ok(p) = pb.build() {
                        window.paint_path(p, if hovered { pal.fg } else { pal.muted });
                    }
                }
                let mut right = nx + nw - CHEVRON_W * z;
                if is_centre {
                    let (tw, th) = (sh.centre.width.as_f32() + 10.0 * z, 16.0 * z);
                    let tx = right - tw;
                    window.paint_quad(quad(
                        rc(tx, ny + (hh - th) / 2.0, tw, th),
                        px(3.0 * z),
                        pal.primary.opacity(0.15),
                        px(0.0),
                        transparent_black(),
                        BorderStyle::Solid,
                    ));
                    let _ = sh.centre.paint(
                        pt(tx + 5.0 * z, ny + (hh - lh) / 2.0),
                        px(lh),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                    right = tx - 8.0 * z;
                }
                if let Some(c) = &t.count {
                    let _ = c.paint(
                        pt(right - c.width.as_f32(), ny + (hh - lh) / 2.0),
                        px(lh),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                }
            },
        );
    }

    /// Shown column rows of one card (tag, name and type), only those in view.
    fn paint_card_rows(
        &self,
        i: usize,
        cb: CardBox,
        sc: &Scene,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let CardBox { nx, ny, nw, hh, .. } = cb;
        let (z, pal, lh, rows) = (sc.f.z, sc.pal, sc.lh, sc.rows);
        let (by, vh) = (sc.f.by, sc.f.vh);
        let t = self.nodes[i].text.as_ref().unwrap();
        let sh = self.shared.as_ref().unwrap();
        for (slot, &r) in self.nodes[i].shown.iter().enumerate() {
            let ry = ny + hh + slot as f32 * ROW_H * z;
            let rh = ROW_H * z;
            if ry + rh < by || ry > by + vh {
                continue;
            }
            if rows.contains(&(i, r)) {
                window.paint_quad(fill(
                    rc(nx + 2.0, ry, nw - 4.0, rh),
                    pal.primary.opacity(0.22),
                ));
            }
            let col = &self.model.tables[i].columns[r];
            let tag = if col.primary_key {
                Some((pal.key, &sh.pk))
            } else if self.nodes[i].fk[r] {
                Some((pal.info, &sh.fk))
            } else {
                None
            };
            window.with_content_mask(
                Some(ContentMask {
                    bounds: rc(nx, ry, nw, rh),
                }),
                |window| {
                    let ty = ry + (rh - lh) / 2.0;
                    if let Some((c, line)) = tag {
                        let (tw, th) = (20.0 * z, 14.0 * z);
                        let (tx, tyy) = (nx + 10.0 * z, ry + (rh - th) / 2.0);
                        window.paint_quad(quad(
                            rc(tx, tyy, tw, th),
                            px(3.0 * z),
                            c.opacity(0.15),
                            px(0.0),
                            transparent_black(),
                            BorderStyle::Solid,
                        ));
                        let _ = line.paint(
                            pt(tx + (tw - line.width.as_f32()) / 2.0, ty),
                            px(lh),
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                    let _ = t.names[r].paint(
                        pt(nx + 37.0 * z, ty),
                        px(lh),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                    let tw = t.types[r].width.as_f32();
                    let _ = t.types[r].paint(
                        pt(nx + nw - 10.0 * z - tw, ty),
                        px(lh),
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                },
            );
            if self.fk_warn[i][r] {
                // foreign key without an index: a warning dot on the tag's corner
                let d = 9.0 * z;
                window.paint_quad(quad(
                    rc(
                        nx + 32.0 * z - d / 2.0,
                        ry + rh / 2.0 - 7.0 * z - d / 2.0,
                        d,
                        d,
                    ),
                    px(d / 2.0),
                    pal.warn,
                    px(1.5),
                    pal.card,
                    BorderStyle::Solid,
                ));
            }
        }
    }

    /// One expanding, fading ring around the card a search flew to; true
    /// while it runs.
    fn paint_pulse(&mut self, sc: &Scene, window: &mut Window) -> bool {
        let Some((i, t0)) = self.pulse else {
            return false;
        };
        let p = t0.elapsed().as_secs_f32() / PULSE_S;
        if p >= 1.0 || !self.vis[i] {
            self.pulse = None;
            return false;
        }
        let Frame { z, ox, oy, .. } = sc.f;
        let n = &self.nodes[i];
        let grow = 2.0 + 10.0 * p;
        let a = (1.0 - p) * 0.9;
        window.paint_quad(quad(
            rc(
                ox + n.x * z - grow,
                oy + n.y * z - grow,
                n.w * z + 2.0 * grow,
                n.h * z + 2.0 * grow,
            ),
            px(8.0 * z + grow),
            transparent_black(),
            px(2.5),
            sc.pal.primary.opacity(a),
            BorderStyle::Solid,
        ));
        true
    }

    /// Minimap (bottom right): every visible card as a box and the viewport.
    /// About 200 quads; its mapping is kept for hit-testing.
    fn paint_minimap(&self, f: Frame, pal: &Pal, window: &mut Window) {
        if !self.minimap {
            self.mm.set(None);
            return;
        }
        let Some((x0, y0, x1, y1)) = self.world_bbox() else {
            self.mm.set(None);
            return;
        };
        let view = self.to_world((0.0, 0.0));
        let view2 = self.to_world((f.vw, f.vh));
        let mm = match &self.drag {
            Drag::Minimap(mm) => *mm,
            _ => {
                // cards and viewport together, so the viewport box is always on it
                let (wx0, wy0) = (x0.min(view.0), y0.min(view.1));
                let (wx1, wy1) = (x1.max(view2.0), y1.max(view2.1));
                let (ww, wh) = ((wx1 - wx0).max(1.0), (wy1 - wy0).max(1.0));
                let s = (MM_W / ww).min(MM_H / wh);
                let (iw, ih) = (ww * s, wh * s);
                let (w, h) = (iw + 2.0 * MM_PAD, ih + 2.0 * MM_PAD);
                // left of the detail panel when it is open
                let right = if self.panel_table().is_some() {
                    PANEL_W
                } else {
                    0.0
                };
                let (x, y) = (f.vw - MM_MARGIN - right - w, f.vh - MM_MARGIN - h);
                MiniMap {
                    x,
                    y,
                    w,
                    h,
                    ox: x + MM_PAD,
                    oy: y + MM_PAD,
                    wx: wx0,
                    wy: wy0,
                    s,
                }
            }
        };
        if mm.x < 0.0 || mm.y < 0.0 {
            self.mm.set(None);
            return; // the view is too small for it
        }
        self.mm.set(Some(mm));
        let (bx, by) = (f.bx, f.by);
        let map = |wx: f32, wy: f32| {
            (
                bx + mm.ox + (wx - mm.wx) * mm.s,
                by + mm.oy + (wy - mm.wy) * mm.s,
            )
        };
        window.paint_quad(quad(
            rc(bx + mm.x, by + mm.y, mm.w, mm.h),
            px(6.0),
            pal.card.opacity(0.94),
            px(1.0),
            pal.border,
            BorderStyle::Solid,
        ));
        window.with_content_mask(
            Some(ContentMask {
                bounds: rc(bx + mm.x + 1.0, by + mm.y + 1.0, mm.w - 2.0, mm.h - 2.0),
            }),
            |window| {
                for (i, n) in self.nodes.iter().enumerate() {
                    if !self.vis[i] {
                        continue;
                    }
                    let (x, y) = map(n.x, n.y);
                    let c = if self.sel.contains(&i) || self.pulse.is_some_and(|p| p.0 == i) {
                        pal.primary
                    } else if self.focus_active && !self.focus_t[i] {
                        pal.muted.opacity(0.22)
                    } else {
                        pal.muted.opacity(0.5)
                    };
                    window.paint_quad(fill(
                        rc(x, y, (n.w * mm.s).max(1.5), (n.h * mm.s).max(1.5)),
                        c,
                    ));
                }
                let (vx, vy) = map(view.0, view.1);
                let (vx2, vy2) = map(view2.0, view2.1);
                window.paint_quad(quad(
                    rc(vx, vy, vx2 - vx, vy2 - vy),
                    px(2.0),
                    pal.primary.opacity(0.08),
                    px(1.0),
                    pal.primary,
                    BorderStyle::Solid,
                ));
            },
        );
    }
}

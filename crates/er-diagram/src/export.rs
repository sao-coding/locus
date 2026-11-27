//! Export of the diagram as SVG (vector paths and text) and PNG (the SVG
//! rendered with resvg). The view collects what is on the canvas into an
//! [`SvgScene`] in its current theme; this module only formats it.

use crate::model::{CARD_PAD_BOTTOM, End, HEADER_H, ROW_H};
use crate::route::{self, EdgePath, Seg};
use std::fmt::Write as _;

/// Colours as `#rrggbb` / `#rrggbbaa`.
#[derive(Clone, Debug, Default)]
pub struct SvgColors {
    pub bg: String,
    pub card: String,
    pub header: String,
    pub border: String,
    pub fg: String,
    pub muted: String,
    pub line: String,
    pub key: String,
    pub info: String,
    pub warn: String,
}

#[derive(Clone, Debug, Default)]
pub struct SvgRow {
    pub name: String,
    pub type_name: String,
    /// "PK" / "FK".
    pub tag: Option<&'static str>,
    /// A foreign key without an index.
    pub warn: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SvgCard {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub title: String,
    pub count: Option<String>,
    pub rows: Vec<SvgRow>,
    /// Colour-group tint of the header.
    pub tint: Option<String>,
}

#[derive(Clone, Debug)]
pub struct SvgEdge {
    pub path: EdgePath,
    pub ends: (End, End),
    pub dashed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct SvgScene {
    pub cards: Vec<SvgCard>,
    pub edges: Vec<SvgEdge>,
    pub colors: SvgColors,
    pub font: String,
    pub mono: String,
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn n(v: f32) -> String {
    let r = (v * 100.0).round() / 100.0;
    if r == r.trunc() {
        format!("{}", r as i64)
    } else {
        format!("{r}")
    }
}

const MARGIN: f32 = 24.0;

/// The scene as an SVG document (world units = SVG pixels).
pub fn to_svg(s: &SvgScene) -> String {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for c in &s.cards {
        x0 = x0.min(c.x);
        y0 = y0.min(c.y);
        x1 = x1.max(c.x + c.w);
        y1 = y1.max(c.y + c.h);
    }
    for e in &s.edges {
        let b = e.path.bbox;
        x0 = x0.min(b.0 - 16.0);
        y0 = y0.min(b.1 - 16.0);
        x1 = x1.max(b.0 + b.2 + 16.0);
        y1 = y1.max(b.1 + b.3 + 16.0);
    }
    if x0 > x1 {
        (x0, y0, x1, y1) = (0.0, 0.0, 0.0, 0.0);
    }
    let (vx, vy) = (x0 - MARGIN, y0 - MARGIN);
    let (w, h) = (x1 - x0 + 2.0 * MARGIN, y1 - y0 + 2.0 * MARGIN);
    let c = &s.colors;
    let mut o = String::new();
    let _ = write!(
        o,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" viewBox="{} {} {} {}" font-family="{}" font-size="12">"#,
        n(w),
        n(h),
        n(vx),
        n(vy),
        n(w),
        n(h),
        esc(&s.font)
    );
    let _ = write!(
        o,
        r#"<rect x="{}" y="{}" width="{}" height="{}" fill="{}"/>"#,
        n(vx),
        n(vy),
        n(w),
        n(h),
        c.bg
    );
    // lines first, cards on top (as on screen)
    o.push_str(r#"<g class="relations" fill="none" stroke-linecap="round">"#);
    for e in &s.edges {
        let p = &e.path;
        let mut d = format!("M{} {}", n(p.start.0), n(p.start.1));
        for seg in &p.segs {
            let _ = match *seg {
                Seg::Line(q) => write!(d, " L{} {}", n(q.0), n(q.1)),
                Seg::Quad(k, q) => write!(d, " Q{} {} {} {}", n(k.0), n(k.1), n(q.0), n(q.1)),
                Seg::Cubic(k1, k2, q) => write!(
                    d,
                    " C{} {} {} {} {} {}",
                    n(k1.0),
                    n(k1.1),
                    n(k2.0),
                    n(k2.1),
                    n(q.0),
                    n(q.1)
                ),
            };
        }
        let mut circles = vec![];
        for (end, (pt, dir)) in [(e.ends.0, p.from), (e.ends.1, p.to)] {
            let (segs, k, circle) = route::end_mark(end, pt, dir);
            for (a, b) in &segs[..k] {
                let _ = write!(d, " M{} {} L{} {}", n(a.0), n(a.1), n(b.0), n(b.1));
            }
            circles.extend(circle);
        }
        let dash = if e.dashed {
            r#" stroke-dasharray="5 4""#
        } else {
            ""
        };
        let _ = write!(
            o,
            r#"<path d="{d}" stroke="{}" stroke-width="1.5"{dash}/>"#,
            c.line
        );
        for (cx, cy) in circles {
            let _ = write!(
                o,
                r#"<circle cx="{}" cy="{}" r="{}" fill="{}" stroke="{}" stroke-width="1.5"/>"#,
                n(cx),
                n(cy),
                n(route::MARK_R),
                c.bg,
                c.line
            );
        }
    }
    o.push_str("</g>");
    for card in &s.cards {
        card_svg(&mut o, card, s);
    }
    o.push_str("</svg>\n");
    o
}

fn card_svg(o: &mut String, card: &SvgCard, s: &SvgScene) {
    let c = &s.colors;
    let (x, y, w, h) = (card.x, card.y, card.w, card.h);
    let _ = write!(o, r#"<g class="table" data-name="{}">"#, esc(&card.title));
    let _ = write!(
        o,
        r#"<rect x="{}" y="{}" width="{}" height="{}" rx="8" fill="{}" stroke="{}"/>"#,
        n(x),
        n(y),
        n(w),
        n(h),
        c.card,
        c.border
    );
    // header: rounded on top (square bottom unless the card is header only)
    let hh = HEADER_H.min(h);
    let header = |fill: &str| {
        if card.rows.is_empty() {
            format!(
                r#"<rect x="{}" y="{}" width="{}" height="{}" rx="7.5" fill="{fill}"/>"#,
                n(x + 0.5),
                n(y + 0.5),
                n(w - 1.0),
                n(hh - 1.0)
            )
        } else {
            format!(
                r#"<path d="M{} {} V{} A7.5 7.5 0 0 1 {} {} H{} A7.5 7.5 0 0 1 {} {} V{} Z" fill="{fill}"/>"#,
                n(x + 0.5),
                n(y + hh),
                n(y + 8.0),
                n(x + 8.0),
                n(y + 0.5),
                n(x + w - 8.0),
                n(x + w - 0.5),
                n(y + 8.0),
                n(y + hh)
            )
        }
    };
    o.push_str(&header(&c.header));
    if let Some(t) = &card.tint {
        o.push_str(&header(t));
    }
    if !card.rows.is_empty() {
        let _ = write!(
            o,
            r#"<path d="M{} {} H{}" stroke="{}"/>"#,
            n(x + 1.0),
            n(y + hh),
            n(x + w - 1.0),
            c.border
        );
    }
    let mid = y + hh / 2.0;
    let _ = write!(
        o,
        r#"<text x="{}" y="{}" dominant-baseline="central" font-weight="600" fill="{}">{}</text>"#,
        n(x + 12.0),
        n(mid),
        c.fg,
        esc(&card.title)
    );
    if let Some(cnt) = &card.count {
        let _ = write!(
            o,
            r#"<text x="{}" y="{}" dominant-baseline="central" text-anchor="end" font-family="{}" font-size="10.5" fill="{}">{}</text>"#,
            n(x + w - 12.0),
            n(mid),
            esc(&s.mono),
            c.muted,
            esc(cnt)
        );
    }
    for (k, r) in card.rows.iter().enumerate() {
        let ry = y + hh + k as f32 * ROW_H;
        let cy = ry + ROW_H / 2.0;
        if let Some(tag) = r.tag {
            let col = if tag == "PK" { &c.key } else { &c.info };
            let _ = write!(
                o,
                r#"<rect x="{}" y="{}" width="20" height="14" rx="3" fill="{col}" fill-opacity="0.15"/><text x="{}" y="{}" dominant-baseline="central" text-anchor="middle" font-size="8.5" font-weight="600" fill="{col}">{tag}</text>"#,
                n(x + 10.0),
                n(cy - 7.0),
                n(x + 20.0),
                n(cy)
            );
        }
        let _ = write!(
            o,
            r#"<text x="{}" y="{}" dominant-baseline="central" font-family="{}" font-size="11.5" fill="{}">{}</text>"#,
            n(x + 37.0),
            n(cy),
            esc(&s.mono),
            c.fg,
            esc(&r.name)
        );
        let _ = write!(
            o,
            r#"<text x="{}" y="{}" dominant-baseline="central" text-anchor="end" font-family="{}" font-size="10" fill="{}">{}</text>"#,
            n(x + w - 10.0),
            n(cy),
            esc(&s.mono),
            c.muted,
            esc(&r.type_name)
        );
        if r.warn {
            let _ = write!(
                o,
                r#"<circle class="fk-warning" cx="{}" cy="{}" r="4.5" fill="{}"/>"#,
                n(x + 32.0),
                n(cy - 7.0),
                c.warn
            );
        }
    }
    let _ = CARD_PAD_BOTTOM;
    o.push_str("</g>");
}

/// The system fonts, loaded once (the first load takes seconds on Windows).
fn system_fonts() -> std::sync::Arc<resvg::usvg::fontdb::Database> {
    static DB: std::sync::OnceLock<std::sync::Arc<resvg::usvg::fontdb::Database>> =
        std::sync::OnceLock::new();
    DB.get_or_init(|| {
        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_system_fonts();
        std::sync::Arc::new(db)
    })
    .clone()
}

/// A font list for SVG: GPUI's `.SystemUIFont` alias is no real font, so
/// it becomes the platform UI font; a generic family is always appended.
pub fn css_font(family: &str, generic: &str) -> String {
    let f = if family.starts_with('.') || family.is_empty() {
        if cfg!(windows) {
            "Segoe UI"
        } else if cfg!(target_os = "macos") {
            "Helvetica Neue"
        } else {
            "Noto Sans"
        }
    } else {
        family
    };
    format!("'{}', {generic}", f.replace('\x27', ""))
}

/// Render an SVG document to PNG bytes at `scale` (2 = twice the size),
/// with the system's fonts for the text.
pub fn svg_to_png(svg: &str, scale: f32) -> Result<Vec<u8>, String> {
    use resvg::{tiny_skia, usvg};
    let opt = usvg::Options {
        fontdb: system_fonts(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(svg, &opt).map_err(|e| e.to_string())?;
    let size = tree
        .size()
        .to_int_size()
        .scale_by(scale)
        .ok_or("the diagram is too large to render")?;
    let mut pixmap = tiny_skia::Pixmap::new(size.width(), size.height())
        .ok_or("the diagram is too large to render")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::{EdgeStyle, edge_path, route};

    fn scene() -> SvgScene {
        let card = |x: f32, title: &str, rows: Vec<SvgRow>| SvgCard {
            x,
            y: 0.0,
            w: 180.0,
            h: HEADER_H + rows.len() as f32 * ROW_H + CARD_PAD_BOTTOM,
            title: title.into(),
            count: Some("1.2k".into()),
            rows,
            tint: None,
        };
        let row = |n: &str, tag: Option<&'static str>, warn: bool| SvgRow {
            name: n.into(),
            type_name: "int".into(),
            tag,
            warn,
        };
        let a = (0.0, 0.0, 180.0, 80.0);
        let b = (400.0, 0.0, 180.0, 80.0);
        let edge = |dashed: bool, style: EdgeStyle| SvgEdge {
            path: edge_path(&route(a, 41.0, b, 41.0, 0.0), style),
            ends: (End::Many, End::ZeroOrOne),
            dashed,
        };
        SvgScene {
            cards: vec![
                card(
                    0.0,
                    "orders <&>",
                    vec![
                        row("id", Some("PK"), false),
                        row("customer_id", Some("FK"), true),
                    ],
                ),
                card(400.0, "customers", vec![row("id", Some("PK"), false)]),
            ],
            edges: vec![
                edge(false, EdgeStyle::Bezier),
                edge(true, EdgeStyle::Smooth),
            ],
            colors: SvgColors {
                bg: "#ffffff".into(),
                line: "#888888".into(),
                warn: "#f59e0b".into(),
                ..Default::default()
            },
            font: "Inter".into(),
            mono: "JetBrains Mono".into(),
        }
    }

    #[test]
    fn svg_has_vector_paths_and_text() {
        let s = to_svg(&scene());
        assert!(s.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(s.trim_end().ends_with("</svg>"));
        assert_eq!(s.matches("<g class=\"table\"").count(), 2);
        // one path per line: bezier has a cubic, smoothstep a quadratic corner or lines
        assert!(s.contains(" C"), "cubic segment");
        assert_eq!(s.matches("stroke-dasharray=\"5 4\"").count(), 1);
        // the crow's foot and the optional end's circle (one per line)
        assert_eq!(s.matches("<circle cx").count(), 2);
        // text is text, escaped
        assert!(s.contains(">orders &lt;&amp;&gt;</text>"));
        assert!(s.contains(">customer_id</text>"));
        assert!(s.contains(">PK</text>") && s.contains(">FK</text>"));
        assert_eq!(s.matches("class=\"fk-warning\"").count(), 1);
        // the view box covers the cards with a margin
        assert!(s.contains("viewBox=\"-24 "), "{}", &s[..200]);
        // well-formed enough for resvg
        assert!(resvg::usvg::Tree::from_str(&s, &resvg::usvg::Options::default()).is_ok());
    }

    #[test]
    fn png_renders() {
        let png = svg_to_png(&to_svg(&scene()), 1.0).unwrap();
        assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
        let empty = to_svg(&SvgScene::default());
        assert!(svg_to_png(&empty, 1.0).is_ok());
    }
}

#[cfg(test)]
mod font_tests {
    #[test]
    fn gpui_alias_becomes_a_real_font() {
        let ui = super::css_font(".SystemUIFont", "sans-serif");
        assert!(!ui.contains(".SystemUIFont") && ui.ends_with(", sans-serif"));
        assert_eq!(
            super::css_font("Consolas", "monospace"),
            "'Consolas', monospace"
        );
    }
}

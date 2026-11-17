//! Pure parts of the value viewer: image sniffing, hex rows, the JSON tree
//! flattened to the visible rows, the auto-chosen view and text statistics.
use serde_json::Value;
use std::collections::HashSet;

/// Children listed per container; the rest is one "N more" row.
pub const MAX_CHILDREN: usize = 20_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    Png,
    Jpeg,
    Gif,
    Webp,
    Bmp,
}

impl ImageKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
            Self::Gif => "GIF",
            Self::Webp => "WebP",
            Self::Bmp => "BMP",
        }
    }
    pub fn ext(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Gif => "gif",
            Self::Webp => "webp",
            Self::Bmp => "bmp",
        }
    }
}

/// Image type by magic bytes.
pub fn sniff_image(b: &[u8]) -> Option<ImageKind> {
    if b.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        Some(ImageKind::Png)
    } else if b.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(ImageKind::Jpeg)
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some(ImageKind::Gif)
    } else if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(ImageKind::Webp)
    } else if b.starts_with(b"BM") && b.len() >= 26 {
        Some(ImageKind::Bmp)
    } else {
        None
    }
}

/// Pixel size from the header (no decode).
pub fn image_size(b: &[u8]) -> Option<(usize, usize)> {
    imagesize::blob_size(b).ok().map(|s| (s.width, s.height))
}

pub const HEX_COLS: usize = 16;

pub fn hex_row_count(len: usize) -> usize {
    len.div_ceil(HEX_COLS)
}

/// One hex-view row: `(offset, 16 hex bytes, ASCII)`.
pub fn hex_row(b: &[u8], row: usize) -> (String, String, String) {
    let start = row * HEX_COLS;
    let chunk = b.get(start..(start + HEX_COLS).min(b.len())).unwrap_or(&[]);
    let lower = crate::hexfmt::lower(chunk);
    let mut hex = String::with_capacity(HEX_COLS * 3 + 1);
    for i in 0..HEX_COLS {
        if i == 8 {
            hex.push(' ');
        }
        match crate::hexfmt::pair(&lower, i) {
            Some(x) => {
                hex.push_str(x);
                hex.push(' ');
            }
            None => hex.push_str("   "),
        }
    }
    let ascii = crate::hexfmt::ascii(chunk);
    (format!("{start:08x}"), hex, ascii)
}

/// Which tab opens first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Text,
    Json,
    Hex,
    Image,
}

/// Parses `text` as JSON. Without `force` (column type is not JSON) only
/// objects and arrays count, so `42` or `"x"` stay text.
pub fn parse_json(text: &str, force: bool) -> Option<Value> {
    let t = text.trim();
    if t.is_empty() {
        return None;
    }
    if !force && !(t.starts_with('{') || t.starts_with('[')) {
        return None;
    }
    serde_json::from_str(t).ok()
}

pub fn is_json_type(type_name: &str) -> bool {
    type_name.to_ascii_uppercase().contains("JSON")
}

/// Best view for a value: image / hex for binary, JSON when it parses, else text.
pub fn auto_view(bytes: Option<&[u8]>, json: bool) -> View {
    match bytes {
        Some(b) if sniff_image(b).is_some() => View::Image,
        Some(b) if std::str::from_utf8(b).is_err() => View::Hex,
        _ if json => View::Json,
        _ => View::Text,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Object,
    Array,
    Str,
    Num,
    Bool,
    Null,
    /// "… N more" placeholder.
    More,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TreeRow {
    pub depth: usize,
    /// `$.a[0].b`; unique per node, also the expansion key.
    pub path: String,
    pub key: String,
    pub kind: Kind,
    /// Scalar text, or `{3}` / `[5]` for containers.
    pub preview: String,
    pub expandable: bool,
    pub expanded: bool,
}

fn child_path(parent: &str, key: &str) -> String {
    if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        format!("{parent}.{key}")
    } else {
        format!(
            "{parent}[{}]",
            serde_json::to_string(key).unwrap_or_default()
        )
    }
}

fn describe(v: &Value) -> (Kind, String, bool) {
    match v {
        Value::Object(m) => (Kind::Object, format!("{{{}}}", m.len()), !m.is_empty()),
        Value::Array(a) => (Kind::Array, format!("[{}]", a.len()), !a.is_empty()),
        Value::String(s) => {
            let shown: String = s.chars().take(300).collect();
            let more = if s.chars().count() > 300 { "…" } else { "" };
            (
                Kind::Str,
                format!("\"{}{more}\"", shown.replace('\n', "\\n")),
                false,
            )
        }
        Value::Number(n) => (Kind::Num, n.to_string(), false),
        Value::Bool(b) => (Kind::Bool, b.to_string(), false),
        Value::Null => (Kind::Null, "null".into(), false),
    }
}

/// The rows that are visible when exactly the paths in `expanded` are open.
pub fn flatten(root: &Value, expanded: &HashSet<String>) -> Vec<TreeRow> {
    let mut out = Vec::new();
    walk(root, "$", "$", 0, expanded, &mut out);
    out
}

fn walk(
    v: &Value,
    path: &str,
    key: &str,
    depth: usize,
    expanded: &HashSet<String>,
    out: &mut Vec<TreeRow>,
) {
    let (kind, preview, expandable) = describe(v);
    let open = expandable && expanded.contains(path);
    out.push(TreeRow {
        depth,
        path: path.to_string(),
        key: key.to_string(),
        kind,
        preview,
        expandable,
        expanded: open,
    });
    if !open {
        return;
    }
    match v {
        Value::Object(m) => {
            for (i, (k, c)) in m.iter().enumerate() {
                if i >= MAX_CHILDREN {
                    more_row(m.len() - i, path, depth + 1, out);
                    break;
                }
                walk(c, &child_path(path, k), k, depth + 1, expanded, out);
            }
        }
        Value::Array(a) => {
            for (i, c) in a.iter().enumerate() {
                if i >= MAX_CHILDREN {
                    more_row(a.len() - i, path, depth + 1, out);
                    break;
                }
                walk(
                    c,
                    &format!("{path}[{i}]"),
                    &i.to_string(),
                    depth + 1,
                    expanded,
                    out,
                );
            }
        }
        _ => {}
    }
}

fn more_row(n: usize, path: &str, depth: usize, out: &mut Vec<TreeRow>) {
    out.push(TreeRow {
        depth,
        path: format!("{path}#more"),
        key: String::new(),
        kind: Kind::More,
        preview: format!("… {n} more"),
        expandable: false,
        expanded: false,
    });
}

/// Every container path down to `max_depth` levels (None = all), for "expand all".
pub fn container_paths(root: &Value, max_depth: Option<usize>) -> HashSet<String> {
    let mut set = HashSet::new();
    collect(root, "$", 0, max_depth, &mut set);
    set
}

fn collect(v: &Value, path: &str, depth: usize, max: Option<usize>, set: &mut HashSet<String>) {
    if max.is_some_and(|m| depth >= m) {
        return;
    }
    match v {
        Value::Object(m) if !m.is_empty() => {
            set.insert(path.to_string());
            for (k, c) in m.iter().take(MAX_CHILDREN) {
                collect(c, &child_path(path, k), depth + 1, max, set);
            }
        }
        Value::Array(a) if !a.is_empty() => {
            set.insert(path.to_string());
            for (i, c) in a.iter().take(MAX_CHILDREN).enumerate() {
                collect(c, &format!("{path}[{i}]"), depth + 1, max, set);
            }
        }
        _ => {}
    }
}

/// Text of a node: the bare string for strings, pretty JSON otherwise.
pub fn node_value_text(root: &Value, path: &str) -> Option<String> {
    let v = lookup(root, path)?;
    Some(match v {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    })
}

fn lookup<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = root;
    let mut rest = path.strip_prefix('$')?;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('.') {
            let end = r.find(['.', '[']).unwrap_or(r.len());
            cur = cur.get(&r[..end])?;
            rest = &r[end..];
        } else {
            let r = rest.strip_prefix('[')?;
            if r.starts_with('"') {
                let mut de = serde_json::Deserializer::from_str(r).into_iter::<String>();
                let key = de.next()?.ok()?;
                let off = de.byte_offset();
                cur = cur.get(&key)?;
                rest = r[off..].strip_prefix(']')?;
            } else {
                let end = r.find(']')?;
                cur = cur.get(r[..end].parse::<usize>().ok()?)?;
                rest = &r[end + 1..];
            }
        }
    }
    Some(cur)
}

/// Indents XML (one tag per line); `None` when it is not tag-shaped text.
pub fn xml_pretty(text: &str) -> Option<String> {
    let t = text.trim();
    if !t.starts_with('<') || !t.ends_with('>') {
        return None;
    }
    let mut out = String::with_capacity(t.len() * 2);
    let mut depth = 0usize;
    let mut i = 0usize;
    // Previous token was text (so the closing tag stays on its line).
    let mut after_text = false;
    while i < t.len() {
        if t.as_bytes()[i] == b'<' {
            let end = t[i..].find('>')? + i;
            let tag = &t[i..=end];
            let closing = tag.starts_with("</");
            let selfclose = tag.ends_with("/>") || tag.starts_with("<?") || tag.starts_with("<!");
            if closing {
                depth = depth.saturating_sub(1);
            }
            if !(closing && after_text) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&"  ".repeat(depth));
            }
            out.push_str(tag);
            if !closing && !selfclose {
                depth += 1;
            }
            after_text = false;
            i = end + 1;
        } else {
            let end = t[i..].find('<').map(|p| p + i).unwrap_or(t.len());
            let s = t[i..end].trim();
            if !s.is_empty() {
                out.push_str(s);
                after_text = true;
            }
            i = end;
        }
    }
    Some(out)
}

pub struct TextStats {
    pub chars: usize,
    pub bytes: usize,
    pub lines: usize,
}

pub fn text_stats(s: &str) -> TextStats {
    TextStats {
        chars: s.chars().count(),
        bytes: s.len(),
        lines: s.lines().count().max(1),
    }
}

/// Encoding note for the status line.
pub fn encoding_note(b: &[u8]) -> &'static str {
    if b.starts_with(&[0xef, 0xbb, 0xbf]) {
        "UTF-8 (BOM)"
    } else if std::str::from_utf8(b).is_ok() {
        if b.is_ascii() { "ASCII" } else { "UTF-8" }
    } else {
        "binary / not UTF-8"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PNG: &[u8] = &[
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0x0d, b'I', b'H', b'D', b'R', 0,
        0, 0, 3, 0, 0, 0, 2, 8, 6, 0, 0, 0,
    ];

    #[test]
    fn sniff() {
        assert_eq!(sniff_image(PNG), Some(ImageKind::Png));
        assert_eq!(
            sniff_image(&[0xff, 0xd8, 0xff, 0xe0]),
            Some(ImageKind::Jpeg)
        );
        assert_eq!(sniff_image(b"GIF89a.."), Some(ImageKind::Gif));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some(ImageKind::Webp));
        assert_eq!(sniff_image(b"hello"), None);
        assert_eq!(image_size(PNG), Some((3, 2)));
    }

    #[test]
    fn hex() {
        let b: Vec<u8> = (0..20).collect();
        assert_eq!(hex_row_count(20), 2);
        let (o, h, a) = hex_row(&b, 1);
        assert_eq!(o, "00000010");
        assert!(h.starts_with("10 11 12 13    "));
        assert_eq!(a, "....");
        assert_eq!(hex_row(b"AB", 0).2, "AB");
    }

    #[test]
    fn json_tree() {
        let v = parse_json(r#"{"a":[1,{"b":null}],"c d":"x"}"#, false).unwrap();
        let closed = flatten(&v, &HashSet::new());
        assert_eq!(closed.len(), 1);
        assert_eq!(closed[0].preview, "{2}");
        let all = container_paths(&v, None);
        let rows = flatten(&v, &all);
        let paths: Vec<_> = rows.iter().map(|r| r.path.as_str()).collect();
        assert_eq!(
            paths,
            ["$", "$.a", "$.a[0]", "$.a[1]", "$.a[1].b", "$[\"c d\"]"]
        );
        assert_eq!(rows[3].depth, 2);
        let one = container_paths(&v, Some(1));
        assert_eq!(flatten(&v, &one).len(), 3);
        for r in &rows {
            assert!(lookup(&v, &r.path).is_some(), "{}", r.path);
        }
        assert_eq!(node_value_text(&v, "$[\"c d\"]").as_deref(), Some("x"));
    }

    #[test]
    fn json_detect() {
        assert!(parse_json("42", false).is_none());
        assert!(parse_json("42", true).is_some());
        assert!(parse_json("{bad", true).is_none());
        assert!(is_json_type("jsonb"));
        assert_eq!(auto_view(None, true), View::Json);
        assert_eq!(auto_view(Some(PNG), false), View::Image);
        assert_eq!(auto_view(Some(&[0xff, 0xfe]), false), View::Hex);
    }

    #[test]
    fn many_children_capped() {
        let v = Value::Array((0..MAX_CHILDREN + 5).map(Value::from).collect());
        let rows = flatten(&v, &container_paths(&v, Some(1)));
        assert_eq!(rows.len(), 1 + MAX_CHILDREN + 1);
        assert_eq!(rows.last().unwrap().kind, Kind::More);
    }

    #[test]
    fn xml() {
        let p = xml_pretty("<a><b>x</b><c/></a>").unwrap();
        assert_eq!(p, "<a>\n  <b>x</b>\n  <c/>\n</a>");
        assert!(xml_pretty("hello").is_none());
    }
}

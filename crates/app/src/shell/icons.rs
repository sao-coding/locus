//! App glyphs: vendored Lucide icons (https://lucide.dev, ISC licence, lucide-static
//! 0.468.0) in `assets/lucide/`, served under `app/<name>.svg` next to gpui-kit's own
//! icon set. GPUI draws an SVG as a mask tinted with the text colour, so
//! `currentColor` only shapes the mask. The names are ours; each file is the Lucide
//! icon that fits (e.g. `gear` = `settings`, `warn` = `triangle-alert`).

use gpui_kit::component::Icon;
use gpui_kit::*;
use std::borrow::Cow;

macro_rules! glyphs {
    ($($n:literal),* $(,)?) => {
        &[$(($n, include_str!(concat!("../../assets/lucide/", $n, ".svg")))),*]
    };
}

const GLYPHS: &[(&str, &str)] = glyphs![
    "analysis",
    "bar-chart",
    "message",
    "caret-down",
    "case-sensitive",
    "case-sensitive",
    "check",
    "chevron-down",
    "chevron-up",
    "chevron-down",
    "chevron-up",
    "chevron-right",
    "close",
    "copy",
    "db",
    "ddl",
    "download",
    "explain",
    "filter",
    "folder",
    "format",
    "gear",
    "history",
    "key-round",
    "link",
    "lock",
    "maximize",
    "minus",
    "monitor",
    "moon",
    "more",
    "panel-right",
    "pencil",
    "play",
    "plus",
    "pull",
    "push",
    "query",
    "refresh",
    "replace",
    "replace-all",
    "replace",
    "replace-all",
    "restore",
    "save",
    "schema",
    "script",
    "search",
    "sidebar",
    "stop",
    "sun",
    "square",
    "table",
    "trash",
    "unlock",
    "warn",
    "x",
];

/// The Locus mark (colour, gradients). A GPUI `svg()` is a one-colour mask, so the mark is
/// drawn with `img()`: resvg rasterises it once and the image cache keeps it.
const MARK: &str = include_str!("../../assets/mark.svg");

/// gpui-kit's assets plus the glyphs above under `app/<name>.svg`.
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if path == "brand/mark.svg" {
            return Ok(Some(Cow::Borrowed(MARK.as_bytes())));
        }
        if let Some(name) = path
            .strip_prefix("app/")
            .and_then(|p| p.strip_suffix(".svg"))
        {
            return Ok(GLYPHS
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, s)| Cow::Borrowed(s.as_bytes())));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        gpui_kit::assets::Assets.list(path)
    }
}

/// An app glyph as an icon.
pub fn icon(name: &str) -> Icon {
    Icon::default().path(SharedString::from(format!("app/{name}.svg")))
}

/// The Locus mark, `h` px tall (the SVG is 232 x 212).
pub fn mark(h: f32) -> gpui::Img {
    gpui::img("brand/mark.svg")
        .w(gpui::px(h * 232. / 212.))
        .h(gpui::px(h))
        .flex_none()
}

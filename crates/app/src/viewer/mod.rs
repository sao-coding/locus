//! The value viewer: a dialog with Text / JSON / Hex / Image views of one value.
//! Chosen automatically (`locus_core::viewer::auto_view`), switchable by
//! the user. Read-only. Every list is a `uniform_list`, so only the visible
//! rows are laid out, whatever the size of the value.
use crate::shell::scale::*;
use crate::shell::seg::{SegItem, seg};
use crate::shell::*;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::input::{Textarea, TextareaState};
use locus_core::viewer::{self as core, Kind, TreeRow, View};
use rust_i18n::t;
use serde_json::Value;
use std::collections::HashSet;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;

/// Longest text line kept in the line list (the wrapped view shows everything).
const MAX_LINE: usize = 4000;

/// What to show.
pub struct ViewerInput {
    pub title: String,
    pub is_null: bool,
    /// Raw bytes for binary values; `None` for text.
    pub bytes: Option<Vec<u8>>,
    /// Text of the value (lossy decoding of `bytes` when they are binary).
    pub text: String,
    /// Server type name of the column; JSON columns open in the JSON view.
    pub type_name: String,
}

pub fn open(input: ViewerInput, window: &mut Window, cx: &mut App) {
    let title = input.title.clone();
    let view = cx.new(|cx| ValueViewer::new(input, cx));
    window.open_pop_dialog(cx, move |dialog, _, _| {
        dialog
            .title(title.clone())
            .w(px(W_DLG_L))
            .child(view.clone())
    });
}

struct ValueViewer {
    is_null: bool,
    bytes: Option<Vec<u8>>,
    /// The text as stored.
    text: String,
    /// Text shown in the Text view (formatted JSON / XML when they parse).
    shown: String,
    formatted: bool,
    lines: Vec<SharedString>,
    json: Option<Value>,
    expanded: HashSet<String>,
    rows: Vec<TreeRow>,
    selected: Option<String>,
    image: Option<(core::ImageKind, Option<(usize, usize)>, Arc<Image>)>,
    views: Vec<View>,
    view: View,
    wrap: bool,
    area: Option<Entity<TextareaState>>,
    tree_scroll: UniformListScrollHandle,
    hex_scroll: UniformListScrollHandle,
    text_scroll: UniformListScrollHandle,
}

impl ValueViewer {
    fn new(input: ViewerInput, _cx: &mut Context<Self>) -> Self {
        let json_col = core::is_json_type(&input.type_name);
        let json = if input.is_null {
            None
        } else {
            core::parse_json(&input.text, json_col)
        };
        let kind = input.bytes.as_deref().and_then(core::sniff_image);
        let image = kind.map(|k| {
            let bytes = input.bytes.clone().unwrap_or_default();
            let size = core::image_size(&bytes);
            let fmt = match k {
                core::ImageKind::Png => ImageFormat::Png,
                core::ImageKind::Jpeg => ImageFormat::Jpeg,
                core::ImageKind::Gif => ImageFormat::Gif,
                core::ImageKind::Webp => ImageFormat::Webp,
                core::ImageKind::Bmp => ImageFormat::Bmp,
            };
            (k, size, Arc::new(Image::from_bytes(fmt, bytes)))
        });
        let (shown, formatted) = match &json {
            Some(v) => (
                serde_json::to_string_pretty(v).unwrap_or_else(|_| input.text.clone()),
                true,
            ),
            None => match core::xml_pretty(&input.text) {
                Some(x) => (x, true),
                None => (input.text.clone(), false),
            },
        };
        let lines = shown
            .lines()
            .map(|l| {
                SharedString::from(if l.chars().count() > MAX_LINE {
                    l.chars().take(MAX_LINE).collect::<String>()
                } else {
                    l.to_string()
                })
            })
            .collect();
        let mut views = vec![View::Text];
        if json.is_some() {
            views.push(View::Json);
        }
        if input.bytes.is_some() {
            views.push(View::Hex);
        }
        if image.is_some() {
            views.push(View::Image);
        }
        let view = core::auto_view(input.bytes.as_deref(), json.is_some());
        let mut me = Self {
            is_null: input.is_null,
            bytes: input.bytes,
            text: input.text,
            shown,
            formatted,
            lines,
            json,
            expanded: HashSet::new(),
            rows: Vec::new(),
            selected: None,
            image,
            views,
            view,
            wrap: false,
            area: None,
            tree_scroll: UniformListScrollHandle::new(),
            hex_scroll: UniformListScrollHandle::new(),
            text_scroll: UniformListScrollHandle::new(),
        };
        if let Some(v) = &me.json {
            me.expanded = core::container_paths(v, Some(2));
        }
        me.reflow();
        me
    }

    fn reflow(&mut self) {
        if let Some(v) = &self.json {
            self.rows = core::flatten(v, &self.expanded);
        }
    }

    fn toggle(&mut self, path: &str) {
        if !self.expanded.remove(path) {
            self.expanded.insert(path.to_string());
        }
        self.reflow();
    }

    fn raw_bytes(&self) -> Vec<u8> {
        self.bytes
            .clone()
            .unwrap_or_else(|| self.text.as_bytes().to_vec())
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        let ext = self
            .image
            .as_ref()
            .map(|i| i.0.ext())
            .unwrap_or(if self.bytes.is_some() { "bin" } else { "txt" });
        let name = format!("value.{ext}");
        let dir = std::env::var_os("USERPROFILE")
            .map(|h| std::path::PathBuf::from(h).join("Documents"))
            .unwrap_or_default();
        let rx = cx.prompt_for_new_path(&dir, Some(&name));
        let data = self.raw_bytes();
        cx.spawn(async move |_, cx| {
            let Ok(Ok(Some(path))) = rx.await else { return };
            let p = path.clone();
            // A large blob must not block the UI thread.
            let r = cx
                .background_executor()
                .spawn(async move { std::fs::write(&p, data) })
                .await;
            let msg = match r {
                Ok(()) => t!("viewer2.saved", path = path.display().to_string()).into_owned(),
                Err(e) => t!("viewer2.save_failed", err = e.to_string()).into_owned(),
            };
            cx.update(|cx| crate::shell::toast::show(msg, cx));
        })
        .detach();
    }

    fn view_label(v: View) -> String {
        match v {
            View::Text => t!("viewer2.text").into_owned(),
            View::Json => "JSON".into(),
            View::Hex => "Hex".into(),
            View::Image => t!("viewer2.image").into_owned(),
        }
    }

    // ---- bodies -----------------------------------------------------------

    fn tree_row(&self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let Some(r) = self.rows.get(ix) else {
            return div().into_any_element();
        };
        let sel = self.selected.as_deref() == Some(r.path.as_str());
        let vcol = match r.kind {
            Kind::Str => t.grn_text,
            Kind::Num => t.acc_text,
            Kind::Bool => t.yel_text,
            Kind::Null => t.tx4,
            Kind::Object | Kind::Array | Kind::More => t.tx3,
        };
        let path = r.path.clone();
        let expandable = r.expandable;
        let mono = cx.theme().mono_font_family.clone();
        h_flex()
            .id(("vj", ix))
            .h(px(20.))
            .pl(px(S4 + r.depth as f32 * 14.))
            .gap(px(S2))
            .items_center()
            .font_family(mono)
            .text_size(px(TX_MONO))
            .cursor_pointer()
            .when(sel, |d| d.bg(t.sel))
            .when(!sel, |d| d.hover(|h| h.bg(t.hv)))
            .child(div().w(px(12.)).text_color(t.tx3).child(if !r.expandable {
                ""
            } else if r.expanded {
                "▾"
            } else {
                "▸"
            }))
            .when(r.kind != Kind::More && r.depth > 0, |d| {
                d.child(div().text_color(t.acc_text).child(r.key.clone()))
                    .child(div().text_color(t.tx4).child(":"))
            })
            .when(r.kind != Kind::More && r.depth == 0, |d| {
                d.child(div().text_color(t.tx3).child("$"))
            })
            .child(
                div()
                    .text_color(vcol)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(r.preview.clone()),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.selected = Some(path.clone());
                if expandable {
                    this.toggle(&path);
                }
                cx.notify();
            }))
            .into_any_element()
    }

    fn json_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let n = self.rows.len();
        let selected = self.selected.clone();
        let json = self.json.clone();
        let btn = |id: &'static str, label: String| Button::new(id).small().label(label);
        let list = uniform_list(
            "vj-list",
            n,
            cx.processor(|this, range: Range<usize>, _, cx| {
                range.map(|i| this.tree_row(i, cx)).collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.tree_scroll)
        .size_full();
        let path_for_copy = selected.clone();
        let value_path = selected.clone();
        v_flex()
            .size_full()
            .gap(px(S2))
            .child(
                h_flex()
                    .gap(px(S2))
                    .child(
                        btn("vj-expand", t!("viewer2.expand_all").into_owned()).on_click(
                            cx.listener(|this, _, _, cx| {
                                if let Some(v) = &this.json {
                                    this.expanded = core::container_paths(v, None);
                                }
                                this.reflow();
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        btn("vj-collapse", t!("viewer2.collapse_all").into_owned()).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.expanded = HashSet::from(["$".to_string()]);
                                this.reflow();
                                cx.notify();
                            }),
                        ),
                    )
                    .child(div().flex_1())
                    .child(
                        btn("vj-cpath", t!("viewer2.copy_path").into_owned())
                            .disabled(path_for_copy.is_none())
                            .on_click(move |_, _, cx| {
                                if let Some(p) = &path_for_copy {
                                    cx.write_to_clipboard(ClipboardItem::new_string(p.clone()));
                                }
                            }),
                    )
                    .child(
                        btn("vj-cvalue", t!("viewer2.copy_node").into_owned())
                            .disabled(value_path.is_none())
                            .on_click(move |_, _, cx| {
                                if let (Some(p), Some(v)) = (&value_path, &json)
                                    && let Some(text) = core::node_value_text(v, p)
                                {
                                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                                }
                            }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .rounded(px(R_SM))
                    .border_1()
                    .border_color(t.ln)
                    .child(list)
                    .vertical_scrollbar(&self.tree_scroll),
            )
            .child(
                div()
                    .h(px(18.))
                    .text_size(px(TX_XS))
                    .text_color(t.tx3)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .child(selected.unwrap_or_else(|| t!("viewer2.no_selection").into_owned())),
            )
            .into_any_element()
    }

    fn hex_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let n = core::hex_row_count(self.bytes.as_ref().map_or(0, |b| b.len()));
        let list = uniform_list(
            "vh-list",
            n,
            cx.processor(|this, range: Range<usize>, _, cx| {
                let t = crate::shell::theme::tokens(cx);
                let mono = cx.theme().mono_font_family.clone();
                let bytes = this.bytes.as_deref().unwrap_or(&[]);
                range
                    .map(|i| {
                        let (off, hex, ascii) = core::hex_row(bytes, i);
                        h_flex()
                            .id(("vh", i))
                            .h(px(18.))
                            .px(px(S4))
                            .gap(px(S6))
                            .font_family(mono.clone())
                            .text_size(px(TX_MONO))
                            .child(div().text_color(t.tx4).child(off))
                            .child(div().whitespace_nowrap().child(hex))
                            .child(div().text_color(t.tx3).child(ascii))
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.hex_scroll)
        .size_full();
        div()
            .size_full()
            .rounded(px(R_SM))
            .border_1()
            .border_color(t.ln)
            .child(list)
            .vertical_scrollbar(&self.hex_scroll)
            .into_any_element()
    }

    fn image_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        let Some((kind, size, image)) = &self.image else {
            return div().into_any_element();
        };
        let dims = size
            .map(|(w, h)| format!("{w} × {h} px · "))
            .unwrap_or_default();
        let bytes = self.bytes.as_ref().map_or(0, |b| b.len()) as u64;
        v_flex()
            .size_full()
            .gap(px(S2))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .rounded(px(R_SM))
                    .border_1()
                    .border_color(t.ln)
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .child(
                        img(image.clone())
                            .max_w_full()
                            .max_h_full()
                            .object_fit(ObjectFit::Contain),
                    ),
            )
            .child(div().text_size(px(TX_XS)).text_color(t.tx3).child(format!(
                "{} · {dims}{}",
                kind.name(),
                locus_core::redis::fmt_bytes(bytes)
            )))
            .into_any_element()
    }

    fn text_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = crate::shell::theme::tokens(cx);
        if self.wrap {
            if self.area.is_none() {
                let text = self.shown.clone();
                self.area =
                    Some(cx.new(|cx| TextareaState::new(window, cx).rows(16).default_value(text)));
            }
            if let Some(a) = &self.area {
                return Textarea::new(a)
                    .readonly(true)
                    .size_full()
                    .into_any_element();
            }
        }
        let n = self.lines.len();
        let digits = n.to_string().len().max(2) as f32;
        let list = uniform_list(
            "vt-list",
            n,
            cx.processor(move |this, range: Range<usize>, _, cx| {
                let t = crate::shell::theme::tokens(cx);
                let mono = cx.theme().mono_font_family.clone();
                range
                    .map(|i| {
                        h_flex()
                            .id(("vt", i))
                            .h(px(18.))
                            .px(px(S4))
                            .gap(px(S6))
                            .font_family(mono.clone())
                            .text_size(px(TX_MONO))
                            .child(
                                div()
                                    .w(px(digits * 8.))
                                    .flex_none()
                                    .text_right()
                                    .text_color(t.tx4)
                                    .child((i + 1).to_string()),
                            )
                            .child(div().whitespace_nowrap().child(this.lines[i].clone()))
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.text_scroll)
        .size_full();
        div()
            .size_full()
            .rounded(px(R_SM))
            .border_1()
            .border_color(t.ln)
            .child(list)
            .vertical_scrollbar(&self.text_scroll)
            .into_any_element()
    }
}

impl Render for ValueViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::shell::theme::tokens(cx);
        if self.is_null {
            return div()
                .h(px(80.))
                .text_color(t.tx3)
                .child(t!("grid.null").into_owned())
                .into_any_element();
        }
        let items = self
            .views
            .iter()
            .map(|&v| {
                let click: crate::shell::seg::SegClick = {
                    let me = cx.entity();
                    Rc::new(move |_, _, cx| {
                        me.update(cx, |this, cx| {
                            this.view = v;
                            cx.notify();
                        })
                    })
                };
                SegItem::new(
                    format!("vv-{v:?}"),
                    Self::view_label(v),
                    self.view == v,
                    click,
                )
            })
            .collect::<Vec<_>>();
        let body = match self.view {
            View::Json => self.json_body(cx),
            View::Hex => self.hex_body(cx),
            View::Image => self.image_body(cx),
            View::Text => self.text_body(window, cx),
        };
        let text_view = self.view == View::Text;
        let stats = core::text_stats(&self.shown);
        let enc = core::encoding_note(&self.raw_bytes());
        let info = match self.view {
            View::Hex => t!(
                "viewer2.bytes",
                n = locus_core::fmt::count(self.bytes.as_ref().map_or(0, |b| b.len()) as u64)
            )
            .into_owned(),
            View::Image => String::new(),
            _ => format!(
                "{} · {} · {enc}{}",
                t!(
                    "viewer.chars",
                    n = locus_core::fmt::count(stats.chars as u64)
                ),
                t!(
                    "viewer2.lines",
                    n = locus_core::fmt::count(stats.lines as u64)
                ),
                if self.formatted {
                    format!(" · {}", t!("viewer2.formatted"))
                } else {
                    String::new()
                }
            ),
        };
        let text = self.text.clone();
        v_flex()
            .gap(px(S3))
            .child(
                h_flex()
                    .gap(px(S3))
                    .items_center()
                    .child(seg(items, cx))
                    .child(div().flex_1())
                    .when(text_view, |h| {
                        h.child(
                            Button::new("vv-wrap")
                                .small()
                                .label(if self.wrap {
                                    t!("viewer2.wrap_off").into_owned()
                                } else {
                                    t!("viewer2.wrap_on").into_owned()
                                })
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.wrap = !this.wrap;
                                    cx.notify();
                                })),
                        )
                    })
                    .child(
                        Button::new("vv-copy")
                            .small()
                            .label(t!("viewer2.copy").into_owned())
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()))
                            }),
                    )
                    .child(
                        Button::new("vv-save")
                            .small()
                            .label(t!("viewer2.save").into_owned())
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
            .child(div().h(px(360.)).child(body))
            .when(!info.is_empty(), |d| {
                d.child(div().text_size(px(TX_XS)).text_color(t.tx3).child(info))
            })
            .into_any_element()
    }
}

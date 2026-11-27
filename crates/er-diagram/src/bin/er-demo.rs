//! Demo and bench for the ER diagram view.
//!
//! er-demo [tables]            generated schema (default 30)
//! er-demo live                `bench` database of the local MariaDB (BENCH_DSN)
//! er-demo bench-drag <n>      drag one table, one step per frame
//! er-demo bench-pan <n>       pan, one step per frame
//! ER_THEME=dark|light picks the theme. BENCH_WARMUP / BENCH_FRAMES / BENCH_VIEW=fit|zoom.
#![windows_subsystem = "windows"]

use er_diagram::{
    Density, Depth, DiagramModel, EdgeStyle, ErDiagram, ErDiagramEvent, Labels, Scope, join_clause,
    sample,
};
use gpui_kit::component::button::Button;
use gpui_kit::component::{
    ActiveTheme as _, Selectable as _, Sizable as _, Theme, ThemeMode, h_flex,
};
use gpui_kit::*;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq)]
enum ScopeMode {
    All,
    Centre,
    List,
}

fn env_n(k: &str, d: usize) -> usize {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Show,
    Drag,
    Pan,
}

struct Demo {
    diagram: Entity<ErDiagram>,
    mode: Mode,
    warmup: usize,
    frames: usize,
    frame: usize,
    last: Option<Instant>,
    samples: Vec<f64>,
    t_start: Option<Instant>,
    started: bool,
    n: usize,
    drag_target: usize,
    note: String,
    // toolbar state (Show mode)
    scope: ScopeMode,
    depth: Depth,
    zoom_pct: u32,
    status: String,
    hub: String,
    list: Vec<String>,
}

impl Demo {
    /// SVG or PNG of the diagram (the selection when tables are selected) into
    /// ER_EXPORT_DIR (default: the temp folder).
    fn export(&mut self, png: bool, cx: &mut Context<Self>) {
        let svg = self.diagram.update(cx, |d, cx| d.export_svg(true, cx));
        let dir = std::env::var("ER_EXPORT_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir());
        self.status = "exporting…".into();
        cx.notify();
        cx.spawn(async move |this, cx| {
            // rendering loads the system fonts: keep it off the UI thread
            let msg = cx
                .background_executor()
                .spawn(async move {
                    let (path, bytes) = if png {
                        let b = er_diagram::svg_to_png(&svg, 2.0)?;
                        (dir.join("er-demo-export.png"), b)
                    } else {
                        (dir.join("er-demo-export.svg"), svg.into_bytes())
                    };
                    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
                    Ok::<_, String>(format!("saved {}", path.display()))
                })
                .await
                .unwrap_or_else(|e| format!("export failed: {e}"));
            this.update(cx, |t, cx| {
                t.status = msg;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn apply_scope(&mut self, cx: &mut Context<Self>) {
        let centre = self
            .diagram
            .read(cx)
            .selection()
            .tables
            .first()
            .cloned()
            .unwrap_or_else(|| self.hub.clone());
        let scope = match self.scope {
            ScopeMode::All => Scope::All,
            ScopeMode::Centre => Scope::Centre {
                table: centre,
                depth: self.depth,
            },
            ScopeMode::List => Scope::Tables(self.list.clone()),
        };
        self.diagram.update(cx, |d, cx| d.set_scope(scope, cx));
        cx.notify();
    }

    fn on_event(&mut self, ev: &ErDiagramEvent, cx: &mut Context<Self>) {
        match ev {
            ErDiagramEvent::ZoomChanged(z) => {
                self.zoom_pct = (z * 100.0).round() as u32;
                cx.notify();
            }
            ErDiagramEvent::RelationClicked(r) => {
                let j = join_clause(r);
                cx.write_to_clipboard(ClipboardItem::new_string(j.clone()));
                self.status = format!("copied: {j}");
                cx.notify();
            }
            ErDiagramEvent::TableContextMenu { table, .. } => {
                // a host would show a menu here; the demo hides the table directly
                let t = table.clone();
                self.diagram.update(cx, |d, cx| d.hide_table(&t, cx));
                self.status = format!("hidden: {t}");
                cx.notify();
            }
            ErDiagramEvent::OpenTable(t) => {
                self.status = format!("open: {t}");
                cx.notify();
            }
            ErDiagramEvent::InsertSql(s) => {
                cx.write_to_clipboard(ClipboardItem::new_string(s.clone()));
                self.status = format!("JOIN path copied: {}", s.replace('\n', " "));
                cx.notify();
            }
            ErDiagramEvent::InsertSelect(t) | ErDiagramEvent::OpenStructure(t) => {
                self.status = format!("{ev:?}: {t}");
                cx.notify();
            }
            _ => {}
        }
    }

    fn toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let btn = |id: &'static str, text: String| Button::new(id).small().label(text);
        let (nvis, nrel, nhidden) = {
            let d = self.diagram.read(cx);
            (
                d.visible_count(),
                d.visible_relation_count(),
                d.hidden_tables().len(),
            )
        };
        let theme = cx.theme().clone();
        let mut bar = h_flex()
            .w_full()
            .flex_none()
            .gap_1()
            .px_2()
            .py_1()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(
                btn("sc-all", "All".into())
                    .selected(self.scope == ScopeMode::All)
                    .on_click(cx.listener(|t, _, _, cx| {
                        t.scope = ScopeMode::All;
                        t.apply_scope(cx);
                    })),
            )
            .child(
                btn("sc-centre", "Centre".into())
                    .selected(self.scope == ScopeMode::Centre)
                    .on_click(cx.listener(|t, _, _, cx| {
                        t.scope = ScopeMode::Centre;
                        t.apply_scope(cx);
                    })),
            )
            .child(
                btn("sc-list", "List".into())
                    .selected(self.scope == ScopeMode::List)
                    .on_click(cx.listener(|t, _, _, cx| {
                        t.scope = ScopeMode::List;
                        t.apply_scope(cx);
                    })),
            );
        for (id, label, d) in [
            ("d1", "1 hop", Depth::Hops(1)),
            ("d2", "2 hops", Depth::Hops(2)),
            ("da", "all", Depth::All),
        ] {
            bar = bar.child(
                btn(id, label.into())
                    .selected(self.depth == d)
                    .on_click(cx.listener(move |t, _, _, cx| {
                        t.depth = d;
                        if t.scope == ScopeMode::Centre {
                            t.apply_scope(cx);
                        } else {
                            cx.notify();
                        }
                    })),
            );
        }
        bar =
            bar.child(div().w(px(12.)))
                .child(btn("zout", "−".into()).on_click(
                    cx.listener(|t, _, _, cx| t.diagram.update(cx, |d, cx| d.zoom_out(cx))),
                ))
                .child(
                    div()
                        .w(px(44.))
                        .text_center()
                        .text_sm()
                        .child(format!("{}%", self.zoom_pct)),
                )
                .child(btn("zin", "+".into()).on_click(
                    cx.listener(|t, _, _, cx| t.diagram.update(cx, |d, cx| d.zoom_in(cx))),
                ))
                .child(div().w(px(12.)))
                .child(btn("arrange", "Auto arrange".into()).on_click(
                    cx.listener(|t, _, _, cx| t.diagram.update(cx, |d, cx| d.auto_arrange(cx))),
                ))
                .child(btn("mermaid", "Copy Mermaid".into()).on_click(cx.listener(
                    |t, _, _, cx| {
                        let s = t.diagram.read(cx).mermaid();
                        t.status = format!("copied Mermaid ({} lines)", s.lines().count());
                        cx.write_to_clipboard(ClipboardItem::new_string(s));
                        cx.notify();
                    },
                )));
        let (style, density, minimap) = {
            let d = self.diagram.read(cx);
            (d.edge_style(), d.density(), d.minimap())
        };
        bar = bar.child(div().w(px(12.)));
        for (id, label, s) in [
            ("e-smooth", "Smooth", EdgeStyle::Smooth),
            ("e-bezier", "Bezier", EdgeStyle::Bezier),
        ] {
            bar = bar.child(
                btn(id, label.into())
                    .selected(style == s)
                    .on_click(cx.listener(move |t, _, _, cx| {
                        t.diagram.update(cx, |d, cx| d.set_edge_style(s, cx));
                        cx.notify();
                    })),
            );
        }
        bar = bar.child(div().w(px(12.)));
        for (id, label, v) in [
            ("dn-full", "Columns", Density::Full),
            ("dn-keys", "Keys", Density::Keys),
            ("dn-head", "Headers", Density::Header),
        ] {
            bar = bar.child(
                btn(id, label.into())
                    .selected(density == v)
                    .on_click(cx.listener(move |t, _, _, cx| {
                        t.diagram.update(cx, |d, cx| d.set_density(v, cx));
                        cx.notify();
                    })),
            );
        }
        bar = bar
            .child(div().w(px(12.)))
            .child(
                btn("minimap", "Minimap".into())
                    .selected(minimap)
                    .on_click(cx.listener(move |t, _, _, cx| {
                        t.diagram.update(cx, |d, cx| d.set_minimap(!minimap, cx));
                        cx.notify();
                    })),
            )
            .child(btn("find", "Find (Ctrl+F)".into()).on_click(cx.listener(
                |t, _, window, cx| t.diagram.update(cx, |d, cx| d.open_search(window, cx)),
            )));
        let (inferred, groups) = {
            let d = self.diagram.read(cx);
            (d.show_inferred(), d.color_groups())
        };
        bar = bar
            .child(div().w(px(12.)))
            .child(
                btn("inferred", "Inferred".into())
                    .selected(inferred)
                    .on_click(cx.listener(move |t, _, _, cx| {
                        t.diagram
                            .update(cx, |d, cx| d.set_show_inferred(!inferred, cx));
                        cx.notify();
                    })),
            )
            .child(
                btn("groups", "Groups".into())
                    .selected(groups)
                    .on_click(cx.listener(move |t, _, _, cx| {
                        t.diagram
                            .update(cx, |d, cx| d.set_color_groups(!groups, cx));
                        cx.notify();
                    })),
            )
            .child(
                btn("join", "JOIN path".into()).on_click(cx.listener(|t, _, _, cx| {
                    let r = t
                        .diagram
                        .update(cx, |d, cx| d.join_path(|s| s.to_string(), cx));
                    t.status = format!("JOIN path: {r:?}");
                    cx.notify();
                })),
            )
            .child(
                btn("svg", "SVG".into()).on_click(cx.listener(|t, _, _, cx| t.export(false, cx))),
            )
            .child(
                btn("png", "PNG".into()).on_click(cx.listener(|t, _, _, cx| t.export(true, cx))),
            );
        if nhidden > 0 {
            bar = bar.child(btn("unhide", format!("Show hidden ({nhidden})")).on_click(
                cx.listener(|t, _, _, cx| {
                    t.diagram.update(cx, |d, cx| d.show_hidden_tables(cx));
                    cx.notify();
                }),
            ));
        }
        bar.child(
            div()
                .ml_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(format!("{nvis} tables · {nrel} FK · {}", self.status)),
        )
    }

    fn on_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self.frame == self.warmup {
            println!("start");
            self.t_start = Some(now);
        } else if self.frame > self.warmup {
            self.samples
                .push((now - self.last.unwrap()).as_secs_f64() * 1000.0);
        }
        self.last = Some(now);
        if self.samples.len() >= self.frames {
            self.finish();
            if std::env::var("ER_NO_QUIT").is_err() {
                cx.quit();
            }
            return;
        }
        // one step per frame; a slow triangle wave so the motion never leaves the screen
        let phase = (self.frame % 200) as f32;
        let dir = if phase < 100.0 { 1.0 } else { -1.0 };
        let (dx, dy) = (dir * 3.0, dir * 1.5);
        let target = self.drag_target;
        self.diagram.update(cx, |d, cx| match self.mode {
            Mode::Drag => d.nudge_table(target, dx, dy, cx),
            Mode::Pan => d.pan_by(dx * 2.0, dy * 2.0, cx),
            Mode::Show => {}
        });
        self.frame += 1;
        cx.notify();
        let me = cx.entity();
        window.on_next_frame(move |w, cx| me.update(cx, |b, cx| b.on_frame(w, cx)));
    }

    fn finish(&mut self) {
        let wall = self.t_start.unwrap().elapsed().as_secs_f64() * 1000.0;
        let mut s = self.samples.clone();
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let q = |p: f64| s[((s.len() as f64 - 1.0) * p).round() as usize];
        println!(
            r#"done {{"frames":{},"wall_ms":{wall:.3},"p50_ms":{:.3},"p95_ms":{:.3},"p99_ms":{:.3},"max_ms":{:.3}}}"#,
            s.len(),
            q(0.5),
            q(0.95),
            q(0.99),
            s[s.len() - 1]
        );
    }
}

impl Render for Demo {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.started {
            self.started = true;
            let me = cx.entity();
            let (n, mode, note) = (self.n, self.mode, self.note.clone());
            let diagram = self.diagram.clone();
            window.on_next_frame(move |w, cx| {
                diagram.update(cx, |d, cx| {
                    if std::env::var("BENCH_VIEW").as_deref() == Ok("zoom") {
                        d.view_centered(1.0, cx);
                    }
                });
                println!(
                    r#"ready {{"scene":"er-{}","tables":{n}{note}}}"#,
                    if mode == Mode::Pan { "pan" } else { "drag" }
                );
                if mode != Mode::Show {
                    me.update(cx, |b, cx| b.on_frame(w, cx));
                } else {
                    me.update(cx, |b, cx| b.apply_scope(cx));
                    // ER_FOCUS=<table> flies to a table (as Enter in the search does)
                    if let Ok(t) = std::env::var("ER_FOCUS") {
                        diagram.update(cx, |d, cx| d.focus_table(&format!("demo.{t}"), cx));
                    }
                    // ER_SEARCH=<text> opens the search with it typed in
                    if let Ok(q) = std::env::var("ER_SEARCH") {
                        diagram.update(cx, |d, cx| d.search_for(&q, w, cx));
                    }
                    // ER_SELECT=a,b selects tables; ER_JOIN=1 then runs "JOIN path";
                    // ER_GROUPS=1 tints by prefix; ER_EXPORT=svg|png saves an export
                    if let Ok(list) = std::env::var("ER_SELECT") {
                        let keys: Vec<String> =
                            list.split(',').map(|t| format!("demo.{t}")).collect();
                        diagram.update(cx, |d, cx| d.set_selection(&keys, cx));
                    }
                    if std::env::var("ER_GROUPS").is_ok() {
                        diagram.update(cx, |d, cx| d.set_color_groups(true, cx));
                    }
                    if std::env::var("ER_JOIN").is_ok() {
                        me.update(cx, |b, cx| {
                            let r = b
                                .diagram
                                .update(cx, |d, cx| d.join_path(|s| s.to_string(), cx));
                            b.status = format!("JOIN path: {r:?}");
                        });
                    }
                    match std::env::var("ER_EXPORT").as_deref() {
                        Ok("svg") => me.update(cx, |b, cx| b.export(false, cx)),
                        Ok("png") => me.update(cx, |b, cx| b.export(true, cx)),
                        _ => {}
                    }
                }
            });
        }
        if self.mode != Mode::Show {
            return div()
                .size_full()
                .child(self.diagram.clone())
                .into_any_element();
        }
        let bar = self.toolbar(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(bar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(self.diagram.clone()),
            )
            .into_any_element()
    }
}

#[cfg(feature = "live")]
fn load_live() -> Result<DiagramModel, String> {
    use db_core::{ConnectOptions, Driver, TlsMode};
    let raw = std::env::var("BENCH_DSN").map_err(|_| "BENCH_DSN not set".to_string())?;
    let rest = raw.strip_prefix("mysql://").ok_or("bad BENCH_DSN")?;
    let (creds, addr) = rest.rsplit_once('@').ok_or("bad BENCH_DSN")?;
    let (user, password) = creds.split_once(':').ok_or("bad BENCH_DSN")?;
    let (hostport, db) = addr.split_once('/').unwrap_or((addr, "bench"));
    let (host, port) = hostport.split_once(':').unwrap_or((hostport, "3306"));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let opts = ConnectOptions {
        host: host.into(),
        port: port.parse().map_err(|_| "bad port")?,
        user: user.into(),
        password: password.into(),
        database: Some(db.into()),
        tls: TlsMode::RequireUnverified,
    };
    let handle = rt.handle().clone();
    let drv = rt
        .block_on(db_mysql::MysqlDriver::connect(&handle, opts))
        .map_err(|e| e.to_string())?;
    let cat = drv
        .catalog()
        .ok_or("this driver has no Catalog yet (Driver::catalog() returned None)")?;
    let schema = db.to_string();
    rt.block_on(async {
        let objs = cat.objects(&schema).await.map_err(|e| e.to_string())?;
        let mut tables = vec![];
        for o in objs
            .into_iter()
            .filter(|o| o.kind == db_core::ObjectKind::Table)
        {
            let cols = cat
                .columns(&schema, &o.name)
                .await
                .map_err(|e| e.to_string())?;
            tables.push((o.name, cols));
        }
        let fks = cat.foreign_keys(&schema).await.map_err(|e| e.to_string())?;
        Ok(DiagramModel::from_catalog(&schema, &tables, &fks))
    })
}

fn main() {
    // SAFETY: `AttachConsole` takes no pointers; failing (no parent console) is harmless.
    unsafe {
        windows_sys::Win32::System::Console::AttachConsole(
            windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
        );
    }
    let args: Vec<String> = std::env::args().collect();
    let a1 = args.get(1).cloned().unwrap_or_default();
    let (mode, n_arg) = match a1.as_str() {
        "bench-drag" => (Mode::Drag, args.get(2)),
        "bench-pan" => (Mode::Pan, args.get(2)),
        _ => (Mode::Show, args.get(1)),
    };
    let (model, note) = if a1 == "live" {
        match load_live() {
            Ok(m) => (m, String::new()),
            Err(e) => {
                eprintln!("live mode skipped: {e}");
                std::process::exit(2);
            }
        }
    } else {
        let n: usize = n_arg.and_then(|s| s.parse().ok()).unwrap_or(30);
        let m = sample::generate(n);
        let note = format!(r#","relations":{}"#, m.relations.len());
        (m, note)
    };
    let n = model.tables.len();
    let dark = std::env::var("ER_THEME").as_deref() == Ok("dark");
    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        Theme::change(
            if dark {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            },
            None,
            cx,
        );
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1280.), px(800.)), cx);
        let opts = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("er-demo".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let labels = Labels {
            empty: "No tables".into(),
            fit: "Fit".into(),
            reset_zoom: "100%".into(),
            relayout: "Re-layout".into(),
            search: "Find a table".into(),
            inferred: "inferred, no FK".into(),
            fk_no_index: "No index starts with this foreign key".into(),
            columns: "Columns".into(),
            indexes: "Indexes".into(),
            rows: "rows".into(),
            open_data: "Data".into(),
            structure: "Structure".into(),
            select_star: "SELECT".into(),
            copy_name: "Copy".into(),
            choose_path: "Several JOIN paths of the same length".into(),
        };
        gpui_kit::open_window(opts, cx, move |_, cx| {
            let mut hub = String::new();
            let mut list = vec![];
            {
                // hub = table touched by most foreign keys; list = a handful of tables
                let mut deg: std::collections::HashMap<&str, usize> = Default::default();
                for r in &model.relations {
                    *deg.entry(r.to_table.as_str()).or_default() += 1;
                    *deg.entry(r.from_table.as_str()).or_default() += 1;
                }
                if let Some((k, _)) = model
                    .tables
                    .iter()
                    .map(|t| t.key())
                    .map(|k| (deg.get(k.as_str()).copied().unwrap_or(0), k))
                    .max_by_key(|(d, _)| *d)
                    .map(|(d, k)| (k, d))
                {
                    hub = k;
                }
                list.extend(model.tables.iter().take(8).map(|t| t.key()));
            }
            // ER_ROWS=1 shows made-up row counts in the headers
            let counts: std::collections::HashMap<String, u64> =
                if std::env::var("ER_ROWS").is_ok() || mode == Mode::Show {
                    model
                        .tables
                        .iter()
                        .enumerate()
                        .map(|(i, t)| {
                            (
                                t.key(),
                                ((i as u64 * 7919 + 13) % 1000) * if i % 5 == 0 { 1500 } else { 1 },
                            )
                        })
                        .collect()
                } else {
                    Default::default()
                };
            let diagram = cx.new(|cx| ErDiagram::new(model, labels, None, cx));
            diagram.update(cx, |d, cx| {
                d.set_row_counts(counts, cx);
                // ER_EDGE=smooth|bezier, ER_DENSITY=keys|header, ER_MINIMAP=0
                match std::env::var("ER_EDGE").as_deref() {
                    Ok("smooth") => d.set_edge_style(EdgeStyle::Smooth, cx),
                    Ok("bezier") => d.set_edge_style(EdgeStyle::Bezier, cx),
                    _ => {}
                }
                match std::env::var("ER_DENSITY").as_deref() {
                    Ok("keys") => d.set_density(Density::Keys, cx),
                    Ok("header") => d.set_density(Density::Header, cx),
                    _ => {}
                }
                if std::env::var("ER_MINIMAP").as_deref() == Ok("0") {
                    d.set_minimap(false, cx);
                }
            });
            let scope_mode = match std::env::var("ER_SCOPE").as_deref() {
                Ok("centre") => ScopeMode::Centre,
                Ok("list") => ScopeMode::List,
                _ => ScopeMode::All,
            };
            let depth = match std::env::var("ER_DEPTH").as_deref() {
                Ok("2") => Depth::Hops(2),
                Ok("all") => Depth::All,
                _ => Depth::Hops(1),
            };

            cx.new(|cx| {
                cx.subscribe(&diagram, |t: &mut Demo, _, ev: &ErDiagramEvent, cx| {
                    t.on_event(ev, cx)
                })
                .detach();
                Demo {
                    scope: scope_mode,
                    depth,
                    zoom_pct: 100,
                    status: "click a line: JOIN to clipboard · right-click a table: hide".into(),
                    hub,
                    list,
                    diagram: diagram.clone(),
                    mode,
                    warmup: env_n("BENCH_WARMUP", 120),
                    frames: env_n("BENCH_FRAMES", 900),
                    frame: 0,
                    last: None,
                    samples: vec![],
                    t_start: None,
                    started: false,
                    n,
                    drag_target: n / 2,
                    note,
                }
            })
        })
        .expect("open window");
    });
}

#[cfg(not(feature = "live"))]
fn load_live() -> Result<DiagramModel, String> {
    Err("built without the `live` feature (cargo build --features live)".into())
}

//! Export of a result: the dialog (format list, source, options) and the two
//! ways to write it. "Loaded rows" reads the grid on the UI thread in chunks
//! (respecting filter and sort); "Full query result" runs the statement again
//! on a session of its own and streams it to the file (`export_stream`),
//! without touching the grid.
use crate::query::tab::QueryTab;
use crate::results::ui::dialect_of;
use crate::shell::PopDialog;
use crate::shell::scale::*;
use db_core::Driver as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use locus_core::driver::AnyDriver;
use locus_core::export::{self, ColKind, Format, Spec};
use locus_core::export_stream::{Cancel, Progress};
use rust_i18n::t;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// What the dialog is opened for.
#[derive(Clone)]
pub struct ExportSource {
    /// Statement to run again for "Full query result"; `Err` says why there is none.
    pub sql: Result<String, String>,
    /// Session source for the full export (the tab's connection).
    pub driver: Option<Arc<AnyDriver>>,
    /// Rows loaded in the grid (after filter) and the note about the filter; `None` = no grid result.
    pub loaded: Option<(usize, Option<String>)>,
    /// Default INSERT target.
    pub table: String,
    /// Opened from the object tree: only the server export makes sense.
    pub full_only: bool,
}

#[derive(Clone)]
struct Request {
    format: Format,
    full: bool,
    table: String,
    batch: usize,
    header: bool,
    open_folder: bool,
}

pub struct ServerJob {
    pub progress: Arc<Progress>,
    pub cancel: Arc<Cancel>,
    pub started: Instant,
}

pub struct ExportJob {
    pub done: u64,
    pub total: u64,
    pub cancel: Arc<AtomicBool>,
    pub server: Option<ServerJob>,
    _task: Task<()>,
}

struct ExportForm {
    tab: WeakEntity<QueryTab>,
    src: ExportSource,
    format: Format,
    full: bool,
    table: Entity<InputState>,
    batch: Entity<InputState>,
    header: bool,
    open_folder: bool,
}

fn format_name(f: Format) -> String {
    let key = format!("exp2.fmt_{}", f.id());
    t!(key.as_str()).into_owned()
}

fn format_desc(f: Format) -> String {
    let key = format!("exp2.desc_{}", f.id());
    t!(key.as_str()).into_owned()
}

impl ExportForm {
    fn request(&self, cx: &App) -> Request {
        let table = self.table.read(cx).value().trim().to_string();
        let batch = self
            .batch
            .read(cx)
            .value()
            .trim()
            .parse::<usize>()
            .unwrap_or(100)
            .clamp(1, 100_000);
        Request {
            format: self.format,
            full: self.full,
            table: if table.is_empty() {
                self.src.table.clone()
            } else {
                table
            },
            batch,
            header: self.header,
            open_folder: self.open_folder,
        }
    }
}

impl Render for ExportForm {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let list = self.format_list(cx);
        let source = self.source_section(cx);
        let options = self.options_section(cx);
        let loaded_n = self.src.loaded.clone().map(|l| l.0);
        let full_ok = self.src.sql.is_ok() && self.src.driver.is_some();
        let can_go = if self.full {
            full_ok
        } else {
            loaded_n.is_some_and(|n| n > 0)
        };
        v_flex()
            .p(px(S6))
            .gap(px(S5))
            .child(list)
            .child(source)
            .child(options)
            .child(
                Checkbox::new("exp-open")
                    .label(t!("exp2.open_folder").to_string())
                    .checked(self.open_folder)
                    .on_click(cx.listener(|this, c: &bool, _, cx| {
                        this.open_folder = *c;
                        cx.notify();
                    })),
            )
            .child(
                h_flex()
                    .gap(px(S4))
                    .justify_end()
                    .child(
                        Button::new("exp-cancel")
                            .h(px(H_ROW))
                            .label(t!("exp2.cancel_btn").to_string())
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("exp-go")
                            .h(px(H_ROW))
                            .primary()
                            .label(t!("exp2.export_btn").to_string())
                            .disabled(!can_go)
                            .on_click(cx.listener(|this, _, window, cx| {
                                let req = this.request(cx);
                                let (tab, src) = (this.tab.clone(), this.src.clone());
                                window.close_dialog(cx);
                                let _ =
                                    tab.update(cx, |t, cx| t.start_export(req, src, window, cx));
                            })),
                    ),
            )
    }
}

impl ExportForm {
    /// One card per export format.
    fn format_list(&self, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let theme = cx.theme().clone();
        let mut list = h_flex().flex_wrap().gap(px(S3));
        for (i, f) in Format::ALL.into_iter().enumerate() {
            let on = f == self.format;
            list = list.child(
                v_flex()
                    .id(("exp-fmt", i))
                    .w(px(212.))
                    .px(px(S4))
                    .py(px(S3))
                    .gap(px(2.))
                    .rounded(px(6.))
                    .border_1()
                    .border_color(if on { theme.primary } else { theme.border })
                    .when(on, |d| d.bg(theme.muted))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.format = f;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .text_size(px(TX_SM))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format_name(f)),
                    )
                    .child(
                        div()
                            .text_size(px(TX_XS))
                            .text_color(t.tx3)
                            .whitespace_normal()
                            .child(format_desc(f)),
                    ),
            );
        }
        list
    }

    /// Which rows to export (loaded or all) and the notes that apply.
    fn source_section(&self, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let (loaded_n, filter_note) = self.src.loaded.clone().unzip();
        let full_ok = self.src.sql.is_ok() && self.src.driver.is_some();
        let mut source = v_flex().gap(px(S3));
        if !self.src.full_only {
            let loaded_label = match loaded_n {
                Some(n) => t!("exp2.src_loaded", n = locus_core::fmt::count(n as u64)).into_owned(),
                None => t!("exp2.src_loaded_none").into_owned(),
            };
            source = source.child(
                RadioGroup::vertical("exp-src")
                    .selected_index(Some(if self.full { 1 } else { 0 }))
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.full = *ix == 1;
                        cx.notify();
                    }))
                    .child(
                        Radio::new("exp-src-loaded")
                            .label(loaded_label)
                            .disabled(loaded_n.is_none()),
                    )
                    .child(
                        Radio::new("exp-src-full")
                            .label(t!("exp2.src_full").to_string())
                            .disabled(!full_ok),
                    ),
            );
        }
        let note = |s: String| {
            div()
                .text_size(px(TX_SM))
                .text_color(t.tx3)
                .whitespace_normal()
                .child(s)
        };
        if self.full {
            source = source.child(note(t!("exp2.full_note").into_owned()));
        } else if let Some(n) = filter_note.flatten() {
            source = source.child(note(n));
        }
        if let Err(why) = &self.src.sql
            && !self.src.full_only
        {
            source = source.child(note(
                t!("exp2.full_refused", why = why.clone()).into_owned(),
            ));
        }
        source
    }

    /// Format options: SQL table name and batch size, CSV header, XLSX note.
    fn options_section(&self, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let note = |s: String| {
            div()
                .text_size(px(TX_SM))
                .text_color(t.tx3)
                .whitespace_normal()
                .child(s)
        };
        let mut options = v_flex().gap(px(S3));
        match self.format {
            Format::Sql => {
                options = options.child(
                    h_flex()
                        .gap(px(S4))
                        .items_end()
                        .child(
                            v_flex()
                                .flex_1()
                                .gap(px(S2))
                                .child(
                                    div()
                                        .text_size(px(TX_SM))
                                        .text_color(t.tx2)
                                        .child(t!("exp2.table_name").into_owned()),
                                )
                                .child(Input::new(&self.table)),
                        )
                        .child(
                            v_flex()
                                .w(px(110.))
                                .gap(px(S2))
                                .child(
                                    div()
                                        .text_size(px(TX_SM))
                                        .text_color(t.tx2)
                                        .child(t!("exp2.batch_rows").into_owned()),
                                )
                                .child(Input::new(&self.batch)),
                        ),
                );
            }
            Format::Csv | Format::CsvExcel | Format::Tsv => {
                options = options.child(
                    Checkbox::new("exp-header")
                        .label(t!("exp2.header").to_string())
                        .checked(self.header)
                        .on_click(cx.listener(|this, c: &bool, _, cx| {
                            this.header = *c;
                            cx.notify();
                        })),
                );
            }
            Format::Xlsx => options = options.child(note(t!("exp2.xlsx_note").into_owned())),
            _ => {}
        }
        options
    }
}

impl QueryTab {
    /// The statement the full export runs again: the one that made the result in front.
    fn export_statement(&self, cx: &App) -> Result<String, String> {
        let why = |k: &str| t!(k).into_owned();
        let stmts: Vec<String> = if self.last_sql.trim().is_empty() {
            Vec::new()
        } else {
            let ranges = db_core::sql::split_statements(&self.last_sql);
            ranges
                .into_iter()
                .map(|r| self.last_sql[r].trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        let selects: Vec<&String> = stmts
            .iter()
            .filter(|s| {
                db_core::classify::classify(s).kind == db_core::classify::StatementKind::Select
            })
            .collect();
        let stmt = match (stmts.len(), selects.len()) {
            (0, _) => return Err(why("exp2.full_none")),
            (1, _) => stmts[0].clone(),
            (_, n) if n == self.out.results.len() && self.out.active < n => {
                selects[self.out.active].clone()
            }
            _ => return Err(why("exp2.full_multi")),
        };
        let c = db_core::classify::classify(&stmt);
        if c.kind != db_core::classify::StatementKind::Select || c.writes {
            return Err(why("exp2.full_not_select"));
        }
        let _ = cx;
        Ok(stmt
            .trim_end_matches(|c: char| c == ';' || c.is_whitespace())
            .to_string())
    }

    /// Opens the export dialog for the result in front.
    pub fn export_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.export.is_some() {
            crate::shell::toast::show(t!("export.nothing").to_string(), cx);
            return;
        }
        let loaded = self.current_table().map(|t| {
            let g = t.read(cx).delegate();
            let note = g.filter_active().then(|| {
                t!(
                    "export_filter.note",
                    n = locus_core::fmt::count(g.matched() as u64),
                    total = locus_core::fmt::count(g.fetched() as u64)
                )
                .into_owned()
            });
            (g.matched(), note)
        });
        let table = self
            .current_table()
            .and_then(|t| t.read(cx).delegate().origin_table())
            .unwrap_or_else(|| "table".into());
        let src = ExportSource {
            sql: self.export_statement(cx),
            driver: self.driver.clone(),
            loaded,
            table,
            full_only: false,
        };
        if src.loaded.is_none() && src.sql.is_err() {
            crate::shell::toast::show(t!("export.nothing").to_string(), cx);
            return;
        }
        self.open_export_dialog(src, window, cx);
    }

    /// Opens the dialog for a statement that is not in the grid (table export from the object tree).
    pub fn export_statement_dialog(
        &mut self,
        src: ExportSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.export.is_some() {
            return;
        }
        self.open_export_dialog(src, window, cx);
    }

    fn open_export_dialog(
        &mut self,
        src: ExportSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let full = src.full_only || src.loaded.is_none();
        let table = cx.new(|cx| InputState::new(window, cx).default_value(src.table.clone()));
        let batch = cx.new(|cx| InputState::new(window, cx).default_value("100"));
        let tab = cx.entity().downgrade();
        let form = cx.new(|_| ExportForm {
            tab,
            src,
            format: Format::Csv,
            full,
            table,
            batch,
            header: true,
            open_folder: false,
        });
        window.open_pop_dialog(cx, move |dialog, _, _| {
            dialog
                .title(t!("exp2.title").to_string())
                .w(px(W_DLG_M + 80.))
                .child(form.clone())
        });
    }

    fn start_export(
        &mut self,
        req: Request,
        src: ExportSource,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dir = std::env::var_os("USERPROFILE")
            .map(|h| std::path::PathBuf::from(h).join("Documents"))
            .unwrap_or_default();
        let name = format!(
            "{}.{}",
            if req.full && src.full_only {
                req.table.rsplit('.').next().unwrap_or("result").to_string()
            } else {
                "result".to_string()
            },
            req.format.extension()
        );
        let rx = cx.prompt_for_new_path(&dir, Some(&name));
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel2 = cancel.clone();
        let server = req.full.then(|| ServerJob {
            progress: Arc::new(Progress::default()),
            cancel: Arc::new(Cancel::default()),
            started: Instant::now(),
        });
        let srv = server
            .as_ref()
            .map(|s| (s.progress.clone(), s.cancel.clone()));
        let dialect = dialect_of(src.driver.as_ref().or(self.driver.as_ref()));
        let table = self.current_table().cloned();
        let task = cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = rx.await else {
                let _ = this.update(cx, |t, cx| {
                    t.export = None;
                    cx.notify();
                });
                return;
            };
            let mut spec = Spec::new(Vec::new());
            spec.table = req.table.clone();
            spec.dialect = dialect;
            spec.batch = req.batch;
            spec.header = req.header;
            let (res, filtered_of): (Result<Option<u64>, String>, Option<u64>) =
                if let Some((progress, cancel_srv)) = srv {
                    let Some(out) =
                        export_server(&this, cx, &src, &req, spec, &path, (progress, cancel_srv))
                            .await
                    else {
                        return;
                    };
                    out
                } else {
                    let Some(table) = table else { return };
                    export_loaded(&this, cx, table, cancel2, &req, spec, &path).await
                };
            let _ = this.update(cx, |t, cx| {
                t.finish_export(
                    res,
                    filtered_of,
                    req.full,
                    path.clone(),
                    req.open_folder,
                    cx,
                )
            });
        });
        self.export = Some(ExportJob {
            done: 0,
            total: 0,
            cancel,
            server,
            _task: task,
        });
        cx.notify();
    }

    fn finish_export(
        &mut self,
        res: Result<Option<u64>, String>,
        filtered_of: Option<u64>,
        _full: bool,
        path: std::path::PathBuf,
        open_folder: bool,
        cx: &mut Context<Self>,
    ) {
        self.export = None;
        let msg = match res {
            Ok(None) => t!("export.cancelled").into_owned(),
            Ok(Some(n)) => {
                if open_folder {
                    cx.reveal_path(&path);
                }
                match filtered_of {
                    Some(all) => t!(
                        "export_filter.done",
                        n = locus_core::fmt::count(n),
                        total = locus_core::fmt::count(all),
                        path = path.display().to_string()
                    )
                    .into_owned(),
                    None => t!(
                        "export.done",
                        n = locus_core::fmt::rows(n),
                        path = path.display().to_string()
                    )
                    .into_owned(),
                }
            }
            Err(e) => t!("export.failed", msg = e).into_owned(),
        };
        crate::shell::toast::show(msg, cx);
        cx.notify();
    }

    pub(crate) fn render_export_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let j = self.export.as_ref()?;
        let theme = cx.theme().clone();
        let label = match &j.server {
            Some(s) => {
                let rows = s.progress.rows.load(Ordering::Relaxed);
                let mb = s.progress.bytes.load(Ordering::Relaxed) as f64 / (1024.0 * 1024.0);
                let rate = rows as f64 / s.started.elapsed().as_secs_f64().max(0.001);
                t!(
                    "exp2.progress_full",
                    rows = locus_core::fmt::count(rows),
                    mb = format!("{mb:.1}"),
                    rate = locus_core::fmt::count(rate as u64)
                )
                .into_owned()
            }
            None => t!(
                "export.progress",
                done = locus_core::fmt::count(j.done),
                total = locus_core::fmt::count(j.total)
            )
            .into_owned(),
        };
        let pct = if j.total == 0 {
            0.0
        } else {
            j.done as f32 / j.total as f32
        };
        Some(
            h_flex()
                .id("export-bar")
                .flex_none()
                .gap(px(S4))
                .px(px(S5))
                .py(px(S2))
                .items_center()
                .text_size(px(TX_SM))
                .border_b_1()
                .border_color(theme.border)
                .child(div().flex_none().child(label))
                .child(
                    div()
                        .flex_1()
                        .h(px(4.))
                        .rounded_full()
                        .bg(theme.muted)
                        .when(j.server.is_none(), |d| {
                            d.child(
                                div()
                                    .h_full()
                                    .rounded_full()
                                    .bg(theme.primary)
                                    .w(relative(pct)),
                            )
                        }),
                )
                .child(
                    Button::new("export-cancel")
                        .small()
                        .label(t!("export.cancel").to_string())
                        .on_click(cx.listener(|t, _, _, cx| {
                            if let Some(j) = &t.export {
                                j.cancel.store(true, Ordering::SeqCst);
                                if let Some(s) = &j.server {
                                    s.cancel.cancel();
                                }
                            }
                            cx.notify();
                        })),
                )
                .into_any_element(),
        )
    }
}

impl crate::shell::Locus {
    /// "Export table…" in the object tree: a new tab on the connection, then the export dialog for `SELECT * FROM schema.table`.
    pub(crate) fn export_table(
        &mut self,
        conn_id: String,
        schema: String,
        table: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(driver) = self.pool.open_driver(&conn_id) else {
            crate::shell::toast::show(t!("error.no_connection").to_string(), cx);
            return;
        };
        let sql = format!(
            "SELECT * FROM {}.{}",
            driver.quote_identifier(&schema),
            driver.quote_identifier(&table)
        );
        self.open_sql_tab(conn_id, sql.clone(), false, window, cx);
        let tab = self.active_tab(cx);
        let src = ExportSource {
            sql: Ok(sql),
            driver: Some(driver),
            loaded: None,
            table: format!("{schema}.{table}"),
            full_only: true,
        };
        tab.update(cx, |t, cx| t.export_statement_dialog(src, window, cx));
    }
}

/// Full result: a thread of its own reads the stream and writes the file. `None` when the run
/// ended early (no driver, no session: already reported).
async fn export_server(
    this: &WeakEntity<QueryTab>,
    cx: &mut AsyncWindowContext,
    src: &ExportSource,
    req: &Request,
    spec: Spec,
    path: &std::path::Path,
    (progress, cancel_srv): (Arc<Progress>, Arc<Cancel>),
) -> Option<(Result<Option<u64>, String>, Option<u64>)> {
    let path = path.to_path_buf();
    let (Some(driver), Ok(sql)) = (src.driver.clone(), src.sql.clone()) else {
        return None;
    };
    let session = match driver.session().await {
        Ok(s) => s,
        Err(e) => {
            let _ = this.update(cx, |t, cx| {
                t.finish_export(
                    Err(e.message),
                    None,
                    false,
                    path.clone(),
                    req.open_folder,
                    cx,
                )
            });
            return None;
        }
    };
    let finished = Arc::new(AtomicBool::new(false));
    let (fin, p2, c2, path2, fmt) = (
        finished.clone(),
        progress.clone(),
        cancel_srv.clone(),
        path.clone(),
        req.format,
    );
    let worker = std::thread::Builder::new()
        .name("export-server".into())
        .spawn(move || {
            let r = locus_core::export_stream::export_query(
                &*session, sql, fmt, spec, &path2, &p2, &c2,
            );
            fin.store(true, Ordering::SeqCst);
            r
        })
        .expect("export thread");
    while !finished.load(Ordering::SeqCst) {
        cx.background_executor()
            .timer(Duration::from_millis(200))
            .await;
        let rows = progress.rows.load(Ordering::Relaxed);
        let _ = this.update(cx, |t, cx| {
            if let Some(j) = &mut t.export {
                j.done = rows;
            }
            cx.notify();
        });
    }
    let r = cx
        .background_executor()
        .spawn(async move { worker.join() })
        .await;
    let res = match r {
        Ok(Ok(locus_core::export_stream::End::Done(n))) => Ok(Some(n)),
        Ok(Ok(locus_core::export_stream::End::Cancelled)) => Ok(None),
        Ok(Err(e)) => Err(e),
        Err(_) => Err("export thread stopped".into()),
    };
    Some((res, None))
}

/// The loaded rows: read the grid on the UI thread in chunks, written on the export thread.
async fn export_loaded(
    this: &WeakEntity<QueryTab>,
    cx: &mut AsyncWindowContext,
    table: Entity<gpui_kit::component::table::TableState<crate::results::grid::ResultGrid>>,
    cancel2: Arc<AtomicBool>,
    req: &Request,
    mut spec: Spec,
    path: &std::path::Path,
) -> (Result<Option<u64>, String>, Option<u64>) {
    let path = path.to_path_buf();
    // The loaded rows: read the grid on the UI thread in chunks.
    let (columns, kinds, total, snapshot, filtered_of) = table.read_with(cx, |t, _| {
        let g = t.delegate();
        let view = g.export_view().map(|v| v.to_vec());
        let metas = g.rs.columns();
        (
            metas.iter().map(|c| c.name.clone()).collect::<Vec<_>>(),
            metas
                .iter()
                .map(|c| export::col_kind(&c.type_name))
                .collect::<Vec<ColKind>>(),
            export::visible_len(g.rs.rows(), view.as_deref()),
            view,
            g.filter_active().then(|| g.fetched() as u64),
        )
    });
    spec.columns = columns;
    spec.kinds = kinds;
    let ncols = spec.columns.len();
    let _ = this.update(cx, |t, cx| {
        if let Some(j) = &mut t.export {
            j.total = total as u64;
        }
        cx.notify();
    });
    let (tx, done) = export::spawn_with(path.clone(), req.format, spec);
    const CHUNK: usize = 2000;
    let mut start = 0;
    let mut cancelled = false;
    while start < total {
        if cancel2.load(Ordering::SeqCst) {
            cancelled = true;
            break;
        }
        let end = (start + CHUNK).min(total);
        // A chunk is read on the UI thread (cells are borrowed from the
        // result set); writing happens on the export thread.
        let chunk = table.read_with(cx, |t, _| {
            let g = t.delegate();
            export::read_chunk(snapshot.as_deref(), start, end, ncols, &|r, c| {
                g.export_text(r, c)
            })
        });
        let mut chunk = Some(chunk);
        loop {
            match tx.try_send(chunk.take().unwrap()) {
                Ok(()) => break,
                Err(std::sync::mpsc::TrySendError::Full(c)) => {
                    chunk = Some(c);
                    cx.background_executor()
                        .timer(Duration::from_millis(4))
                        .await;
                }
                Err(std::sync::mpsc::TrySendError::Disconnected(_)) => break,
            }
        }
        start = end;
        let _ = this.update(cx, |t, cx| {
            if let Some(j) = &mut t.export {
                j.done = start as u64;
            }
            cx.notify();
        });
    }
    drop(tx);
    let r = cx
        .background_executor()
        .spawn(async move {
            done.recv()
                .unwrap_or_else(|_| Err(std::io::Error::other("export thread stopped")))
        })
        .await;
    if cancelled {
        let _ = std::fs::remove_file(&path);
    }
    (
        if cancelled {
            Ok(None)
        } else {
            r.map(Some).map_err(|e| e.to_string())
        },
        filtered_of,
    )
}

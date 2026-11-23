//! Import wizard: a CSV / TSV / spreadsheet file into an existing table or a
//! new one. Steps: file (format, preview) → mapping → options → run.
//! The logic (reading, conversion, SQL, the run itself) is
//! `locus_core::import`; this is the dialog around it. The write goes
//! through the Production gate (`Locus::gate`) before it starts.
use crate::shell::scale::*;
use crate::shell::seg::{SegItem, seg};
use crate::shell::*;
use db_core::Driver as _;
use gpui_kit::component::Disableable as _;
use locus_core::driver::AnyDriver;
use locus_core::import::infer::infer_columns;
use locus_core::import::source::{self, CsvOptions, Preview, SourceSpec};
use locus_core::import::{
    self, ColumnMap, ConvertOptions, DateFormat, ImportJob, ImportOptions, OnError, Progress,
    Report, TargetColumn, TxScope,
};
use locus_core::txn::Engine;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    File,
    Mapping,
    Options,
    Run,
}

type Shared = Arc<Mutex<Progress>>;

pub(crate) struct ImportWizard {
    wb: WeakEntity<Locus>,
    conn: SavedConnection,
    driver: Arc<AnyDriver>,
    engine: Engine,
    schema: String,
    rt: tokio::runtime::Handle,
    step: Step,
    /// `Some`: import into this table; `None`: create a new one.
    existing: Option<String>,
    table_in: Entity<InputState>,
    target_cols: Vec<TargetColumn>,
    path: Option<PathBuf>,
    sheets: Vec<String>,
    sheet: Option<String>,
    csv: CsvOptions,
    pv: Option<Preview>,
    names: Vec<String>,
    err: Option<String>,
    map_to: Vec<Option<usize>>,
    new_name: Vec<Entity<InputState>>,
    new_type: Vec<Entity<InputState>>,
    new_skip: Vec<bool>,
    empty_null: bool,
    trim: bool,
    skip_errors: bool,
    per_batch: bool,
    clear_first: bool,
    save_errors: bool,
    confirm_clear: bool,
    date_in: Entity<InputState>,
    batch_in: Entity<InputState>,
    shared: Shared,
    cancel: Arc<AtomicBool>,
    running: bool,
    progress: Progress,
    report: Option<Report>,
}

impl Locus {
    /// Opens the wizard. `table`: import into it; `None`: into a new table of `schema`.
    pub(crate) fn open_import(
        &mut self,
        conn_id: String,
        schema: String,
        table: Option<String>,
        file: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(conn) = self.conns.iter().find(|c| c.id == conn_id).cloned() else {
            return;
        };
        if conn.kind == locus_core::store::DbKind::Redis {
            return;
        }
        let Some(driver) = self.pool.open_driver(&conn_id) else {
            crate::shell::toast::show(t!("error.no_connection").to_string(), cx);
            return;
        };
        let schema = if schema.is_empty() {
            self.default_schema(&conn_id).unwrap_or_default()
        } else {
            schema
        };
        let wb = cx.entity().downgrade();
        let rt = self.pool.rt().clone();
        let wiz = cx.new(|cx| ImportWizard::new(wb, conn, driver, schema, table, rt, window, cx));
        if let Some(f) = file {
            wiz.update(cx, |w, cx| w.set_file(f, window, cx));
        }
        window.open_pop_dialog(cx, move |dialog, _, _| {
            dialog
                .title(t!("imp.title").to_string())
                .w(px(W_DLG_L + 80.))
                .child(wiz.clone())
        });
    }
}

impl ImportWizard {
    #[allow(clippy::too_many_arguments)]
    fn new(
        wb: WeakEntity<Locus>,
        conn: SavedConnection,
        driver: Arc<AnyDriver>,
        schema: String,
        existing: Option<String>,
        rt: tokio::runtime::Handle,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let engine = Engine::of(&driver);
        let table_in = cx
            .new(|cx| InputState::new(window, cx).placeholder(t!("imp.new_table_ph").to_string()));
        let date_in =
            cx.new(|cx| InputState::new(window, cx).placeholder(t!("imp.date_ph").to_string()));
        let batch_in = cx.new(|cx| InputState::new(window, cx));
        batch_in.update(cx, |s, cx| s.set_value("1000".to_string(), window, cx));
        let mut me = Self {
            wb,
            conn,
            driver,
            engine,
            schema,
            rt,
            step: Step::File,
            existing,
            table_in,
            target_cols: Vec::new(),
            path: None,
            sheets: Vec::new(),
            sheet: None,
            csv: CsvOptions::default(),
            pv: None,
            names: Vec::new(),
            err: None,
            map_to: Vec::new(),
            new_name: Vec::new(),
            new_type: Vec::new(),
            new_skip: Vec::new(),
            empty_null: true,
            trim: true,
            skip_errors: false,
            per_batch: false,
            clear_first: false,
            save_errors: true,
            confirm_clear: false,
            date_in,
            batch_in,
            shared: Default::default(),
            cancel: Arc::new(AtomicBool::new(false)),
            running: false,
            progress: Progress::default(),
            report: None,
        };
        me.load_target(cx);
        me
    }

    /// Columns of the existing target table.
    fn load_target(&mut self, cx: &mut Context<Self>) {
        let Some(table) = self.existing.clone() else {
            return;
        };
        let (driver, schema) = (self.driver.clone(), self.schema.clone());
        cx.spawn(async move |this, cx| {
            let cols = match driver.catalog() {
                Some(c) => c.columns(&schema, &table).await.map_err(|e| e.message),
                None => Err("no catalog".to_string()),
            };
            let _ = this.update(cx, |w, cx| {
                match cols {
                    Ok(c) => {
                        w.target_cols = c
                            .into_iter()
                            .map(|c| TargetColumn {
                                name: c.name,
                                type_name: c.type_name,
                            })
                            .collect()
                    }
                    Err(e) => w.err = Some(e),
                }
                w.remap();
                cx.notify();
            });
        })
        .detach();
    }

    fn pick_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(p) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |w, window, cx| w.set_file(p, window, cx));
        })
        .detach();
    }

    fn set_file(&mut self, p: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.err = None;
        self.sheets.clear();
        self.sheet = None;
        if source::is_spreadsheet(&p) {
            match source::sheet_names(&p) {
                Ok(s) => {
                    self.sheet = s.first().cloned();
                    self.sheets = s;
                }
                Err(e) => self.err = Some(e),
            }
        } else {
            match source::detect(&p) {
                Ok(d) => {
                    self.csv.encoding = d.encoding;
                    self.csv.delimiter = d.delimiter;
                }
                Err(e) => self.err = Some(e.to_string()),
            }
        }
        if self.existing.is_none() && self.table_in.read(cx).value().is_empty() {
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            let name: String = stem
                .chars()
                .map(|c| {
                    if c.is_alphanumeric() || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            self.table_in
                .update(cx, |s, cx| s.set_value(name, window, cx));
        }
        self.path = Some(p);
        self.reload(window, cx);
    }

    fn spec(&self) -> Option<SourceSpec> {
        Some(SourceSpec {
            path: self.path.clone()?,
            sheet: self.sheet.clone(),
            csv: self.csv.clone(),
        })
    }

    fn convert_opts(&self, cx: &App) -> ConvertOptions {
        let f = self.date_in.read(cx).value().trim().to_string();
        ConvertOptions {
            empty_as_null: self.empty_null,
            trim: self.trim,
            date_format: if f.is_empty() {
                DateFormat::Auto
            } else {
                DateFormat::Custom(f)
            },
        }
    }

    /// Reads the preview again after the file, sheet or format changed.
    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(spec) = self.spec() else { return };
        match source::preview(&spec, 100) {
            Ok(pv) => {
                self.err = None;
                self.names = source::column_names(&pv.header, pv.ncols);
                let types = infer_columns(&pv.rows, pv.ncols, &self.convert_opts(cx));
                self.new_name = self
                    .names
                    .iter()
                    .map(|n| cx.new(|cx| InputState::new(window, cx).default_value(n.clone())))
                    .collect();
                self.new_type = types
                    .iter()
                    .map(|t| t.sql(self.engine))
                    .map(|s| cx.new(|cx| InputState::new(window, cx).default_value(s)))
                    .collect();
                self.new_skip = vec![false; pv.ncols];
                self.pv = Some(pv);
                self.remap();
            }
            Err(e) => {
                self.err = Some(e);
                self.pv = None;
            }
        }
        cx.notify();
    }

    fn remap(&mut self) {
        if self.existing.is_some() {
            self.map_to =
                import::auto_map_sources(&self.names, self.csv.has_header, &self.target_cols);
        }
    }

    fn cycle_target(&mut self, i: usize) {
        let n = self.target_cols.len();
        let cur = self.map_to.get(i).copied().flatten();
        let used: Vec<usize> = self
            .map_to
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .filter_map(|(_, t)| *t)
            .collect();
        let mut next = cur.map(|c| c + 1).unwrap_or(0);
        while next < n && used.contains(&next) {
            next += 1;
        }
        if let Some(slot) = self.map_to.get_mut(i) {
            *slot = (next < n).then_some(next);
        }
    }

    fn table_name(&self, cx: &App) -> String {
        match &self.existing {
            Some(t) => t.clone(),
            None => self.table_in.read(cx).value().trim().to_string(),
        }
    }

    fn mapping(&self, cx: &App) -> (Option<Vec<import::sqlgen::NewColumn>>, Vec<ColumnMap>) {
        if self.existing.is_some() {
            let maps = self
                .map_to
                .iter()
                .enumerate()
                .filter_map(|(i, t)| {
                    let c = self.target_cols.get((*t)?)?;
                    Some(ColumnMap {
                        source: i,
                        target: c.name.clone(),
                        kind: import::kind_of(self.engine, &c.type_name),
                    })
                })
                .collect();
            (None, maps)
        } else {
            let names: Vec<String> = self
                .new_name
                .iter()
                .map(|n| n.read(cx).value().trim().to_string())
                .collect();
            let types: Vec<String> = self
                .new_type
                .iter()
                .map(|n| n.read(cx).value().trim().to_string())
                .collect();
            let (cols, maps) = import::plan_new_table(&names, &types, &self.new_skip, self.engine);
            (Some(cols), maps)
        }
    }

    fn job(&self, cx: &App) -> Option<ImportJob> {
        let source = self.spec()?;
        let (create, maps) = self.mapping(cx);
        let log = self.save_errors.then(|| {
            let p = &source.path;
            let stem = p
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            p.with_file_name(format!("{stem}.import-errors.csv"))
        });
        let batch = self
            .batch_in
            .read(cx)
            .value()
            .trim()
            .parse::<usize>()
            .unwrap_or(1000)
            .clamp(1, 100_000);
        let (schema, table) = if self.existing.is_some() {
            (
                (!self.schema.is_empty()).then(|| self.schema.clone()),
                self.table_name(cx),
            )
        } else {
            import::split_target(
                (!self.schema.is_empty()).then_some(self.schema.as_str()),
                &self.table_name(cx),
            )
        };
        Some(ImportJob {
            source,
            engine: self.engine,
            schema,
            table,
            create,
            maps,
            options: ImportOptions {
                convert: self.convert_opts(cx),
                on_error: if self.skip_errors {
                    OnError::Skip
                } else {
                    OnError::Stop
                },
                clear_first: self.clear_first && self.existing.is_some(),
                batch_size: batch,
                scope: if self.per_batch {
                    TxScope::PerBatch
                } else {
                    TxScope::Single
                },
                error_log: log,
            },
        })
    }

    fn problem(&self, cx: &App) -> Option<String> {
        if self.table_name(cx).is_empty() {
            return Some(t!("imp.err_table").to_string());
        }
        let (create, maps) = self.mapping(cx);
        if maps.is_empty() {
            return Some(t!("imp.err_no_map").to_string());
        }
        if let Some(c) = create {
            let mut seen = std::collections::HashSet::new();
            for col in &c {
                if col.name.is_empty()
                    || col.sql_type.is_empty()
                    || !seen.insert(col.name.to_lowercase())
                {
                    return Some(t!("imp.err_cols").to_string());
                }
            }
        }
        None
    }

    /// Import pressed: confirm clearing, then the Production gate, then run.
    fn request(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.problem(cx) {
            self.err = Some(p);
            return cx.notify();
        }
        if self.clear_first && self.existing.is_some() && !self.confirm_clear {
            self.confirm_clear = true;
            return cx.notify();
        }
        let Some(job) = self.job(cx) else { return };
        let q = import::sqlgen::qualified(self.engine, job.schema.as_deref(), &job.table);
        let mut sql = String::new();
        if job.create.is_some() {
            sql.push_str(&format!("CREATE TABLE {q} (...);\n"));
        }
        if job.options.clear_first {
            sql.push_str(&format!("DELETE FROM {q};\n"));
        }
        sql.push_str(&format!(
            "INSERT INTO {q} (...) VALUES (...) /* {} */",
            job.source.path.display()
        ));
        let classified = Locus::classify_run(&sql);
        let wiz = cx.entity();
        let go: crate::prod::Cont =
            Rc::new(move |_, window, cx| wiz.update(cx, |w, cx| w.begin(window, cx)));
        let g = crate::prod::Gated {
            tab: None,
            conn: self.conn.clone(),
            classified,
            preview: sql,
            go,
        };
        let _ = self.wb.update(cx, |wb, cx| wb.gate(g, window, cx));
    }

    fn begin(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(job) = self.job(cx) else { return };
        self.step = Step::Run;
        self.running = true;
        self.report = None;
        self.err = None;
        self.cancel.store(false, Ordering::Relaxed);
        *self.shared.lock() = Progress::default();
        let (driver, cancel, shared) = (
            self.driver.clone(),
            self.cancel.clone(),
            self.shared.clone(),
        );
        let sink = shared.clone();
        let handle = self.rt.spawn(async move {
            import::run(driver, job, cancel, move |p| *sink.lock() = p).await
        });
        cx.spawn_in(window, async move |this, cx| {
            let mut handle = handle;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let snap = shared.lock().clone();
                let done = handle.is_finished();
                if this
                    .update(cx, |w, cx| {
                        w.progress = snap;
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
                if done {
                    break;
                }
            }
            let rep = (&mut handle).await.unwrap_or_else(|e| Report {
                fatal: Some(e.to_string()),
                ..Report::default()
            });
            let _ = this.update_in(cx, |w, _, cx| {
                w.running = false;
                w.report = Some(rep);
                cx.notify();
            });
        })
        .detach();
    }

    fn sg(
        &self,
        cx: &Context<Self>,
        id: &str,
        label: String,
        on: bool,
        f: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> SegItem {
        let me = cx.entity().downgrade();
        SegItem::new(
            SharedString::from(id.to_string()),
            label,
            on,
            Rc::new(move |_, w, cx| {
                let _ = me.update(cx, |this, cx| {
                    f(this, w, cx);
                    cx.notify();
                });
            }),
        )
    }

    fn field(&self, label: String, body: impl IntoElement, cx: &App) -> Div {
        let t = crate::shell::theme::tokens(cx);
        h_flex()
            .gap(px(S5))
            .items_center()
            .child(
                div()
                    .w(px(130.))
                    .flex_none()
                    .text_size(px(TX_SM))
                    .text_color(t.tx3)
                    .child(label),
            )
            .child(body)
    }

    fn file_step(&mut self, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let mut body = v_flex().gap(px(S5));
        let shown = self
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| t!("imp.no_file").to_string());
        body = body.child(
            h_flex()
                .gap(px(S4))
                .items_center()
                .child(
                    Button::new("imp-pick")
                        .label(t!("imp.choose").to_string())
                        .on_click(cx.listener(|this, _, window, cx| this.pick_file(window, cx))),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(TX_SM))
                        .text_color(t.tx2)
                        .child(shown),
                ),
        );
        if self.existing.is_none() {
            body = body.child(self.field(
                t!("imp.new_table").to_string(),
                div().w(px(260.)).child(Input::new(&self.table_in)),
                cx,
            ));
        } else if let Some(tb) = &self.existing {
            body = body.child(
                self.field(
                    t!("imp.target").to_string(),
                    div()
                        .text_size(px(TX_BASE))
                        .child(format!("{}.{}", self.schema, tb)),
                    cx,
                ),
            );
        }
        if self
            .path
            .as_ref()
            .is_some_and(|p| source::is_spreadsheet(p))
        {
            let items: Vec<SegItem> = self
                .sheets
                .clone()
                .into_iter()
                .enumerate()
                .map(|(i, s)| {
                    let on = self.sheet.as_deref() == Some(s.as_str());
                    self.sg(
                        cx,
                        &format!("imp-sheet-{i}"),
                        s.clone(),
                        on,
                        move |w, win, cx| {
                            w.sheet = Some(s.clone());
                            w.reload(win, cx);
                        },
                    )
                })
                .collect();
            body = body.child(self.field(t!("imp.sheet").to_string(), seg(items, cx), cx));
        } else if self.path.is_some() {
            body = self.csv_options(body, cx);
        }
        if let Some(pv) = &self.pv {
            body = body.child(self.preview_table(pv, cx));
        }
        body
    }

    /// CSV options: delimiter, quote, header row and encoding.
    fn csv_options(&self, mut body: Div, cx: &mut Context<Self>) -> Div {
        let delims: [(u8, &str); 4] = [(b',', ","), (b';', ";"), (b'\t', "Tab"), (b'|', "|")];
        let items = delims.iter().map(|(d, l)| {
            let d = *d;
            self.sg(
                cx,
                &format!("imp-d-{d}"),
                l.to_string(),
                self.csv.delimiter == d,
                move |w, win, cx| {
                    w.csv.delimiter = d;
                    w.reload(win, cx);
                },
            )
        });
        body = body.child(self.field(
            t!("imp.delimiter").to_string(),
            seg(items.collect(), cx),
            cx,
        ));
        let quotes: [(u8, &str); 3] = [(b'"', "\""), (b'\'', "'"), (0, "—")];
        let items = quotes.iter().map(|(q, l)| {
            let q = *q;
            self.sg(
                cx,
                &format!("imp-q-{q}"),
                l.to_string(),
                self.csv.quote == q,
                move |w, win, cx| {
                    w.csv.quote = q;
                    w.reload(win, cx);
                },
            )
        });
        body = body.child(self.field(t!("imp.quote").to_string(), seg(items.collect(), cx), cx));
        let items = vec![
            self.sg(
                cx,
                "imp-h-yes",
                t!("imp.yes").to_string(),
                self.csv.has_header,
                |w, win, cx| {
                    w.csv.has_header = true;
                    w.reload(win, cx);
                },
            ),
            self.sg(
                cx,
                "imp-h-no",
                t!("imp.no").to_string(),
                !self.csv.has_header,
                |w, win, cx| {
                    w.csv.has_header = false;
                    w.reload(win, cx);
                },
            ),
        ];
        body = body.child(self.field(t!("imp.header").to_string(), seg(items, cx), cx));
        let items: Vec<SegItem> = source::ENCODINGS
            .iter()
            .map(|e| {
                let e = e.to_string();
                let on = self.csv.encoding.eq_ignore_ascii_case(&e);
                self.sg(
                    cx,
                    &format!("imp-e-{e}"),
                    e.clone(),
                    on,
                    move |w, win, cx| {
                        w.csv.encoding = e.clone();
                        w.reload(win, cx);
                    },
                )
            })
            .collect();
        body = body.child(self.field(t!("imp.encoding").to_string(), seg(items, cx), cx));
        body
    }

    fn preview_table(&self, pv: &Preview, cx: &App) -> impl IntoElement + use<> {
        let t = crate::shell::theme::tokens(cx);
        let cell = |s: String, head: bool| {
            div()
                .w(px(120.))
                .flex_none()
                .px(px(S3))
                .h(px(H_COMPACT))
                .flex()
                .items_center()
                .text_size(px(TX_SM))
                .overflow_hidden()
                .whitespace_nowrap()
                .when(head, |d| {
                    d.font_weight(FontWeight::SEMIBOLD).text_color(t.tx)
                })
                .when(!head, |d| d.text_color(t.tx2))
                .child(s)
        };
        let mut rows = v_flex().min_w_0();
        rows = rows.child(
            h_flex()
                .bg(t.hv)
                .children(self.names.iter().map(|n| cell(n.clone(), true))),
        );
        for r in pv.rows.iter() {
            rows = rows.child(h_flex().children(
                (0..pv.ncols).map(|i| cell(r.get(i).cloned().unwrap_or_default(), false)),
            ));
        }
        v_flex()
            .gap(px(S2))
            .child(
                div()
                    .text_size(px(TX_XS))
                    .text_color(t.tx3)
                    .child(t!("imp.preview", n = pv.rows.len().to_string()).to_string()),
            )
            .child(
                div()
                    .id("imp-preview")
                    .h(px(220.))
                    .overflow_scroll()
                    .rounded(px(R_SM))
                    .border_1()
                    .border_color(t.hv)
                    .child(rows),
            )
    }

    fn mapping_step(&mut self, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let mut list = v_flex().gap(px(S2));
        let ncols = self.names.len();
        let sample = |pv: &Option<Preview>, i: usize| {
            pv.as_ref()
                .and_then(|p| p.rows.first())
                .and_then(|r| r.get(i))
                .cloned()
                .unwrap_or_default()
        };
        if self.existing.is_some() {
            for i in 0..ncols {
                let tgt = self
                    .map_to
                    .get(i)
                    .copied()
                    .flatten()
                    .and_then(|j| self.target_cols.get(j));
                let label = match tgt {
                    Some(c) => format!("{}  ({})", c.name, c.type_name),
                    None => t!("imp.skip").to_string(),
                };
                list = list.child(
                    h_flex()
                        .gap(px(S5))
                        .items_center()
                        .child(
                            div()
                                .w(px(180.))
                                .truncate()
                                .text_size(px(TX_BASE))
                                .child(self.names[i].clone()),
                        )
                        .child(
                            div()
                                .w(px(140.))
                                .truncate()
                                .text_size(px(TX_SM))
                                .text_color(t.tx3)
                                .child(sample(&self.pv, i)),
                        )
                        .child(div().text_color(t.tx3).child("→"))
                        .child(
                            Button::new(("imp-map", i))
                                .w(px(240.))
                                .label(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.cycle_target(i);
                                    cx.notify();
                                })),
                        ),
                );
            }
        } else {
            for i in 0..ncols {
                let skip = self.new_skip.get(i).copied().unwrap_or(false);
                list = list.child(
                    h_flex()
                        .gap(px(S4))
                        .items_center()
                        .child(
                            div()
                                .w(px(150.))
                                .truncate()
                                .text_size(px(TX_SM))
                                .text_color(t.tx3)
                                .child(format!("{} · {}", self.names[i], sample(&self.pv, i))),
                        )
                        .child(div().text_color(t.tx3).child("→"))
                        .child(
                            div()
                                .w(px(200.))
                                .child(Input::new(&self.new_name[i]).disabled(skip)),
                        )
                        .child(
                            div()
                                .w(px(170.))
                                .child(Input::new(&self.new_type[i]).disabled(skip)),
                        )
                        .child(
                            Button::new(("imp-skip", i))
                                .label(if skip {
                                    t!("imp.use").to_string()
                                } else {
                                    t!("imp.skip").to_string()
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    if let Some(s) = this.new_skip.get_mut(i) {
                                        *s = !*s;
                                    }
                                    cx.notify();
                                })),
                        ),
                );
            }
        }
        let hint = if self.existing.is_some() {
            t!("imp.map_hint").to_string()
        } else {
            t!("imp.new_hint").to_string()
        };
        v_flex()
            .gap(px(S5))
            .child(div().text_size(px(TX_SM)).text_color(t.tx3).child(hint))
            .child(
                div()
                    .id("imp-map-scroll")
                    .max_h(px(380.))
                    .overflow_y_scroll()
                    .child(list),
            )
    }

    fn options_step(&mut self, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let yn = |this: &Self,
                  cx: &Context<Self>,
                  id: &'static str,
                  on: bool,
                  f: fn(&mut Self, bool)| {
            seg(
                vec![
                    this.sg(
                        cx,
                        &format!("{id}-y"),
                        t!("imp.yes").to_string(),
                        on,
                        move |w, _, _| f(w, true),
                    ),
                    this.sg(
                        cx,
                        &format!("{id}-n"),
                        t!("imp.no").to_string(),
                        !on,
                        move |w, _, _| f(w, false),
                    ),
                ],
                cx,
            )
        };
        let mut b = v_flex().gap(px(S5));
        b = b.child(self.field(
            t!("imp.empty_null").to_string(),
            yn(self, cx, "imp-en", self.empty_null, |w, v| w.empty_null = v),
            cx,
        ));
        b = b.child(self.field(
            t!("imp.trim").to_string(),
            yn(self, cx, "imp-tr", self.trim, |w, v| w.trim = v),
            cx,
        ));
        b = b.child(self.field(
            t!("imp.date_format").to_string(),
            div().w(px(220.)).child(Input::new(&self.date_in)),
            cx,
        ));
        let on_err = seg(
            vec![
                self.sg(
                    cx,
                    "imp-oe-stop",
                    t!("imp.on_stop").to_string(),
                    !self.skip_errors,
                    |w, _, _| w.skip_errors = false,
                ),
                self.sg(
                    cx,
                    "imp-oe-skip",
                    t!("imp.on_skip").to_string(),
                    self.skip_errors,
                    |w, _, _| w.skip_errors = true,
                ),
            ],
            cx,
        );
        b = b.child(self.field(t!("imp.on_error").to_string(), on_err, cx));
        if self.skip_errors {
            b = b.child(self.field(
                t!("imp.save_errors").to_string(),
                yn(self, cx, "imp-se", self.save_errors, |w, v| {
                    w.save_errors = v
                }),
                cx,
            ));
        }
        let scope = seg(
            vec![
                self.sg(
                    cx,
                    "imp-tx-one",
                    t!("imp.tx_single").to_string(),
                    !self.per_batch,
                    |w, _, _| w.per_batch = false,
                ),
                self.sg(
                    cx,
                    "imp-tx-batch",
                    t!("imp.tx_batch").to_string(),
                    self.per_batch,
                    |w, _, _| w.per_batch = true,
                ),
            ],
            cx,
        );
        b = b.child(self.field(t!("imp.tx").to_string(), scope, cx));
        b = b.child(self.field(
            t!("imp.batch").to_string(),
            div().w(px(120.)).child(Input::new(&self.batch_in)),
            cx,
        ));
        if self.existing.is_some() {
            b = b.child(self.field(
                t!("imp.clear").to_string(),
                yn(self, cx, "imp-cl", self.clear_first, |w, v| {
                    w.clear_first = v;
                    w.confirm_clear = false;
                }),
                cx,
            ));
        }
        if self.confirm_clear {
            b = b.child(
                div()
                    .text_size(px(TX_SM))
                    .text_color(t.red_text)
                    .child(t!("imp.clear_confirm", table = self.table_name(cx)).to_string()),
            );
        }
        b
    }

    fn run_step(&mut self, cx: &mut Context<Self>) -> Div {
        let t = crate::shell::theme::tokens(cx);
        let p = &self.progress;
        let mut b = v_flex().gap(px(S5));
        let eta = p
            .eta
            .map(|d| format!("  ·  ETA {}s", d.as_secs()))
            .unwrap_or_default();
        b = b.child(
            div().text_size(px(TX_BASE)).child(
                t!(
                    "imp.progress",
                    read = p.read.to_string(),
                    ok = p.inserted.to_string(),
                    skipped = p.skipped.to_string(),
                    rate = format!("{:.0}", p.rows_per_sec)
                )
                .to_string()
                    + &eta,
            ),
        );
        let w = (p.fraction.clamp(0.0, 1.0) as f32) * 100.0;
        b = b.child(
            div().h(px(6.)).w_full().rounded_full().bg(t.hv).child(
                div()
                    .h_full()
                    .w(relative(w / 100.0))
                    .rounded_full()
                    .bg(t.tx2),
            ),
        );
        if let Some(r) = &self.report {
            let line = if let Some(e) = &r.fatal {
                t!("imp.failed", msg = e.clone()).to_string()
                    + &if r.rolled_back {
                        format!("  {}", t!("imp.rolled_back"))
                    } else {
                        String::new()
                    }
            } else if r.cancelled {
                if r.table_dropped {
                    t!("imp.cancelled_dropped").to_string()
                } else if r.rolled_back {
                    t!("imp.cancelled_rolled").to_string()
                } else {
                    t!("imp.cancelled_kept", n = r.inserted.to_string()).to_string()
                }
            } else {
                t!(
                    "imp.done",
                    n = r.inserted.to_string(),
                    skipped = r.skipped.to_string(),
                    secs = format!("{:.1}", r.elapsed.as_secs_f64())
                )
                .to_string()
            };
            let bad = r.fatal.is_some();
            b = b.child(
                div()
                    .text_size(px(TX_BASE))
                    .font_weight(FontWeight::MEDIUM)
                    .when(bad, |d| d.text_color(t.red_text))
                    .child(line),
            );
            if let Some(p) = &r.error_log {
                b = b.child(
                    div()
                        .text_size(px(TX_SM))
                        .text_color(t.tx3)
                        .child(t!("imp.errors_saved", path = p.display().to_string()).to_string()),
                );
            }
            for e in r.errors.iter().take(8) {
                b = b.child(
                    div()
                        .text_size(px(TX_SM))
                        .text_color(t.tx2)
                        .truncate()
                        .child(format!("{}: {}", e.line, e.message)),
                );
            }
        }
        b
    }
}

impl Render for ImportWizard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = crate::shell::theme::tokens(cx);
        let body = match self.step {
            Step::File => self.file_step(cx),
            Step::Mapping => self.mapping_step(cx),
            Step::Options => self.options_step(cx),
            Step::Run => self.run_step(cx),
        };
        let titles = [
            (Step::File, "imp.s_file"),
            (Step::Mapping, "imp.s_map"),
            (Step::Options, "imp.s_opts"),
            (Step::Run, "imp.s_run"),
        ];
        let crumbs = h_flex().gap(px(S5)).children(titles.iter().map(|(s, k)| {
            div()
                .text_size(px(TX_SM))
                .text_color(if *s == self.step { t.tx } else { t.tx4 })
                .when(*s == self.step, |d| d.font_weight(FontWeight::SEMIBOLD))
                .child(t!(*k).to_string())
        }));
        let mut foot = h_flex().gap(px(S4)).child(div().flex_1());
        let step = self.step;
        if self.running {
            foot = foot.child(
                Button::new("imp-cancel")
                    .label(t!("common.cancel").to_string())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.cancel.store(true, Ordering::Relaxed);
                        cx.notify();
                    })),
            );
        } else if step == Step::Run {
            foot = foot.child(
                Button::new("imp-close")
                    .primary()
                    .label(t!("imp.close").to_string())
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            );
        } else {
            foot = foot.child(
                Button::new("imp-x")
                    .label(t!("common.cancel").to_string())
                    .on_click(|_, window, cx| window.close_dialog(cx)),
            );
            if step != Step::File {
                foot = foot.child(
                    Button::new("imp-back")
                        .label(t!("imp.back").to_string())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.step = if step == Step::Options {
                                Step::Mapping
                            } else {
                                Step::File
                            };
                            this.confirm_clear = false;
                            cx.notify();
                        })),
                );
            }
            let next_ok = self.pv.is_some()
                && (step != Step::File
                    || self.existing.is_some()
                    || !self.table_in.read(cx).value().trim().is_empty());
            if step == Step::Options {
                let label = if self.confirm_clear {
                    t!("imp.run_confirm").to_string()
                } else {
                    t!("imp.run").to_string()
                };
                foot = foot.child(
                    Button::new("imp-run")
                        .primary()
                        .label(label)
                        .on_click(cx.listener(|this, _, window, cx| this.request(window, cx))),
                );
            } else {
                foot = foot.child(
                    Button::new("imp-next")
                        .primary()
                        .label(t!("imp.next").to_string())
                        .disabled(!next_ok)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.step = if step == Step::File {
                                Step::Mapping
                            } else {
                                Step::Options
                            };
                            cx.notify();
                        })),
                );
            }
        }
        let mut root = v_flex().gap(px(S6)).child(crumbs);
        if let Some(e) = &self.err {
            root = root.child(
                div()
                    .text_size(px(TX_SM))
                    .text_color(t.red_text)
                    .child(e.clone()),
            );
        }
        root.child(body).child(foot)
    }
}

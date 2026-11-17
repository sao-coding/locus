//! Bench mode for the local regression harness. The real product
//! shell runs and drives itself; output lines match the old POC format:
//! `ready {json}`, `info {json}`, `start`, `done {json}`.

use crate::shell::Locus;
use db_core::{Cell, Driver as _, QueryEvent, QueryOptions};
use gpui_kit::component::input::Position;
use gpui_kit::*;
use locus_core::status::Phase;
use locus_core::store;
use std::time::Instant;

pub const SCENES: &[&str] = &[
    "idle",
    "grid",
    "editor-scroll",
    "editor-type",
    "stream",
    "demo",
    "history-type",
];

const GRID_SQL: &str = "SELECT * FROM student_real";
/// 40,450 x 10 = 404,500 rows.
const STREAM_SQL: &str = "SELECT s.* FROM student_real s CROSS JOIN (SELECT 1 n UNION ALL SELECT 2 UNION ALL SELECT 3 UNION ALL SELECT 4 UNION ALL SELECT 5 UNION ALL SELECT 6 UNION ALL SELECT 7 UNION ALL SELECT 8 UNION ALL SELECT 9 UNION ALL SELECT 10) k";
const STREAM_TOTAL: u64 = 404_500;

fn env_n(k: &str, d: usize) -> usize {
    std::env::var(k)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

#[derive(PartialEq)]
enum Stage {
    /// Waiting for the shell's first frame.
    Boot,
    /// Waiting for a preparatory query (grid: the data; stream: SELECT 1 to connect).
    Loading,
    Frames,
    Finished,
}

pub struct Bench {
    scene: String,
    wb: Entity<Locus>,
    stage: Stage,
    t0: Instant,
    load_ms: f64,
    extra: String,
    warmup: usize,
    frames: usize,
    frame: usize,
    last: Option<Instant>,
    samples: Vec<f64>,
    t_start: Option<Instant>,
    row_h: f32,
    target_y: f32,
    wraps: usize,
    max_y: f32,
    // stream scene
    stream_samples: usize,
    cancel_at: Option<Instant>,
    rows_at_cancel: u64,
    cancel_ms: Option<f64>,
    stream_outcome: Option<&'static str>,
    first_row_ms: Option<f64>,
    gone_check: Option<String>,
    _check: Option<Task<()>>,
}

impl Bench {
    pub fn new(scene: String, wb: Entity<Locus>, t0: Instant, extra: String, load_ms: f64) -> Self {
        Self {
            scene,
            wb,
            stage: Stage::Boot,
            t0,
            load_ms,
            extra,
            warmup: env_n("BENCH_WARMUP", 120),
            frames: env_n("BENCH_FRAMES", 900),
            frame: 0,
            last: None,
            samples: Vec::new(),
            t_start: None,
            row_h: 0.,
            target_y: 0.,
            wraps: 0,
            max_y: 0.,
            stream_samples: 0,
            cancel_at: None,
            rows_at_cancel: 0,
            cancel_ms: None,
            stream_outcome: None,
            first_row_ms: None,
            gone_check: None,
            _check: None,
        }
    }

    /// Called from the first rendered frame of the shell.
    pub fn boot(me: Entity<Self>, window: &mut Window, cx: &mut App) {
        me.update(cx, |b, cx| {
            let ms = b.t0.elapsed().as_secs_f64() * 1000.0;
            println!(
                r#"ready {{"framework":"gpui","app":"locus","scene":"{}","ready_since_main_ms":{ms:.3},"load_ms":{:.3},"ready_signal":"window.on_next_frame from first render"}}"#,
                b.scene, b.load_ms
            );
            match b.scene.as_str() {
                "idle" => {
                    println!(r#"info {{"scene":"idle"{}}}"#, b.extra);
                    println!("start");
                    println!(r#"done {{"frames":0,"wall_ms":0,"p50_ms":null,"p95_ms":null,"p99_ms":null,"max_ms":null,"idle":true}}"#);
                    b.stage = Stage::Finished;
                }
                "history-type" => {
                    // History tab with the generated entries (LOCUS_DATA_DIR/history.json); each measured frame sets the search text.
                    b.wb.update(cx, |wb, cx| wb.bench_open_history(window, cx));
                    println!(r#"info {{"scene":"history-type"{}}}"#, b.extra);
                    b.stage = Stage::Frames;
                }
                "grid" | "stream" | "demo" => {
                    // stream: the editor already holds the stream SQL; connect with
                    // a separate SELECT 1 so the measured frames start at Run.
                    let sql = if b.scene == "stream" { "SELECT 1" } else { GRID_SQL };
                    // stream: 404,500 rows must flow past the default row
                    // limit (100,000, where the query would pause), so the
                    // scene can cancel at half way as it did before paging.
                    if b.scene == "stream" {
                        let tab = b.wb.read(cx).active_tab(cx);
                        tab.update(cx, |t, cx| t.set_limit(None, cx));
                    }
                    let sql = sql.to_string();
                    b.wb.update(cx, |wb, cx| wb.run_text(sql, window, cx));
                    b.stage = Stage::Loading;
                }
                _ => {
                    println!(r#"info {{"scene":"{}"{}}}"#, b.scene, b.extra);
                    b.stage = Stage::Frames;
                }
            }
        });
        Self::schedule(me, window);
    }

    fn schedule(me: Entity<Self>, window: &mut Window) {
        window.on_next_frame(move |w, cx| Self::tick(me, w, cx));
    }

    fn tick(me: Entity<Self>, window: &mut Window, cx: &mut App) {
        let again = me.update(cx, |b, cx| b.on_frame(window, cx));
        if again {
            Self::schedule(me, window);
        }
    }

    /// Returns whether to keep ticking.
    fn on_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match self.stage {
            Stage::Boot | Stage::Finished => false,
            Stage::Loading => {
                let st = self.wb.read(cx).tab_status(cx).read(cx);
                match st.phase {
                    Phase::Done => {
                        let rows = st.rows;
                        let ms = st.elapsed.as_secs_f64() * 1000.0;
                        let sid = st.session_id.unwrap_or(0);
                        if self.scene == "demo" {
                            self.stage = Stage::Finished;
                            return false;
                        }
                        if self.scene == "grid" {
                            self.load_ms = ms;
                            let cols = self
                                .wb
                                .read(cx)
                                .tab_table(cx)
                                .map(|t| t.read(cx).delegate().rs.columns().len())
                                .unwrap_or(0);
                            println!(
                                r#"info {{"scene":"grid","rows":{rows},"cols":{cols},"query_ms":{ms:.3},"session_id":{sid}{}}}"#,
                                self.extra
                            );
                        } else {
                            println!(
                                r#"info {{"scene":"stream","connect_and_select1_ms":{ms:.3},"session_id":{sid},"stream_rows":{STREAM_TOTAL}{}}}"#,
                                self.extra
                            );
                        }
                        self.stage = Stage::Frames;
                        true
                    }
                    Phase::Failed | Phase::Cancelled => {
                        let msg = st.error.as_ref().map(|e| e.to_string()).unwrap_or_default();
                        println!(
                            r#"done {{"frames":0,"error":{}}}"#,
                            serde_json::to_string(&msg).unwrap()
                        );
                        self.stage = Stage::Finished;
                        false
                    }
                    _ => true,
                }
            }
            Stage::Frames => {
                let now = Instant::now();
                if self.frame == self.warmup {
                    println!("start");
                    self.t_start = Some(now);
                    if self.scene == "stream" {
                        let t = Instant::now();
                        self.wb.update(cx, |wb, cx| wb.run(window, cx));
                        if std::env::var_os("BENCH_TRACE").is_some() {
                            eprintln!("trace run {:.3} ms", t.elapsed().as_secs_f64() * 1000.0);
                        }
                    }
                } else if self.frame > self.warmup {
                    self.samples
                        .push((now - self.last.unwrap()).as_secs_f64() * 1000.0);
                }
                self.last = Some(now);
                if self.scene == "stream" && self.frame >= self.warmup {
                    self.stream_bookkeeping(now, cx);
                }
                if self.samples.len() >= self.frames
                    && (self.scene != "stream" || self.stream_settled())
                {
                    self.finish(cx);
                    self.stage = Stage::Finished;
                    return false;
                }
                self.step(self.frame.saturating_sub(self.warmup), window, cx);
                self.frame += 1;
                true
            }
        }
    }

    fn stream_settled(&self) -> bool {
        self.stream_outcome.is_some() && (self.cancel_ms.is_none() || self.gone_check.is_some())
    }

    fn stream_bookkeeping(&mut self, now: Instant, cx: &mut Context<Self>) {
        let (phase, rows, finished_at) = {
            let st = self.wb.read(cx).tab_status(cx).read(cx);
            (st.phase, st.rows, st.finished_at)
        };
        if self.stream_outcome.is_some() {
            return;
        }
        if self.first_row_ms.is_none() && rows > 0 {
            self.first_row_ms = Some((now - self.t_start.unwrap()).as_secs_f64() * 1000.0);
        }
        if self.cancel_at.is_none() {
            self.stream_samples = self.samples.len();
        }
        match phase {
            Phase::Running if self.cancel_at.is_none() && rows >= STREAM_TOTAL / 2 => {
                self.cancel_at = Some(Instant::now());
                self.rows_at_cancel = rows;
                self.wb.update(cx, |wb, cx| wb.cancel(cx));
            }
            Phase::Cancelled | Phase::Done | Phase::Failed => {
                self.stream_outcome = Some(match phase {
                    Phase::Cancelled => "cancelled",
                    Phase::Done => "done",
                    _ => "failed",
                });
                if let (Some(c), Some(f)) = (self.cancel_at, finished_at) {
                    self.cancel_ms = Some((f - c).as_secs_f64() * 1000.0);
                }
                if phase == Phase::Cancelled {
                    self.check_gone(cx);
                }
            }
            _ => {}
        }
    }

    /// Asks the server whether the cancelled statement is still running.
    fn check_gone(&mut self, cx: &mut Context<Self>) {
        let Some(driver) = self.wb.read(cx).tab_driver(cx) else {
            return;
        };
        let sql = "SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE INFO LIKE '%CROSS JOIN (SELECT 1 n%' AND ID <> CONNECTION_ID()".to_string();
        let t = Instant::now();
        self._check = Some(cx.spawn(async move |this, cx| {
            let mut h = driver.query(sql, QueryOptions::default());
            let mut count = None;
            while let Some(ev) = h.next().await {
                match ev {
                    QueryEvent::Batch(b) if b.rows() > 0 => {
                        if let Cell::Text(s) = b.cell(0, 0) {
                            count = s.parse::<u64>().ok();
                        }
                    }
                    QueryEvent::Finished(_) => break,
                    _ => {}
                }
            }
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            let _ = this.update(cx, |b, _| {
                b.gone_check = Some(match count {
                    Some(n) => format!(
                        r#","processlist_matches_after_cancel":{n},"processlist_check_ms":{ms:.3}"#
                    ),
                    None => r#","processlist_matches_after_cancel":null"#.to_string(),
                });
            });
        }));
    }

    fn step(&mut self, measured_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        match self.scene.as_str() {
            "grid" | "stream" => {
                let Some(table) = self.wb.read(cx).tab_table(cx).cloned() else {
                    return;
                };
                let n = table.read(cx).delegate().rs.rows() as f32;
                if n < 2. {
                    return;
                }
                let (h, ch) = {
                    let v = table.read(cx).vertical_scroll_handle.0.borrow();
                    (
                        v.base_handle.clone(),
                        v.last_item_size
                            .map(|s| s.contents.height.as_f32())
                            .unwrap_or(0.),
                    )
                };
                if ch <= 0. {
                    return;
                }
                self.row_h = ch / n;
                let max = h.max_offset().y.as_f32();
                self.max_y = max;
                self.target_y += self.row_h;
                if self.target_y > max {
                    self.target_y = 0.;
                    self.wraps += 1;
                }
                h.set_offset(point(px(0.), px(-self.target_y)));
                table.update(cx, |_, cx| cx.notify());
            }
            "history-type" => self
                .wb
                .update(cx, |wb, cx| wb.bench_type_history(measured_ix, window, cx)),
            "editor-scroll" | "editor-type" => {
                let st = self.wb.read(cx).tab_editor(cx).read(cx).state.clone();
                if self.scene == "editor-type" {
                    st.update(cx, |s, cx| {
                        if measured_ix == 0 {
                            s.set_cursor_position(Position::new(49_999, 0), window, cx);
                        }
                        s.insert("x", window, cx);
                    });
                } else {
                    let cur = st.read(cx).scroll_offset().y.as_f32();
                    if self.target_y > 0. && -cur + 1. < self.target_y {
                        self.max_y = -cur;
                        self.target_y = 0.;
                        self.wraps += 1;
                    } else {
                        self.target_y += 24.;
                    }
                    let y = self.target_y;
                    st.update(cx, |s, cx| s.set_scroll_offset(point(px(0.), px(-y)), cx));
                }
            }
            _ => {}
        }
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        let wall = self.t_start.unwrap().elapsed().as_secs_f64() * 1000.0;
        if let Ok(p) = std::env::var("BENCH_DUMP") {
            let _ = std::fs::write(
                p,
                self.samples
                    .iter()
                    .map(|v| format!("{v:.3}\n"))
                    .collect::<String>(),
            );
        }
        let q = |v: &[f64], p: f64| -> f64 {
            let mut s = v.to_vec();
            s.sort_by(|a, b| a.partial_cmp(b).unwrap());
            if s.is_empty() {
                0.0
            } else {
                s[((s.len() as f64 - 1.0) * p).round() as usize]
            }
        };
        let all = &self.samples;
        let extra = match self.scene.as_str() {
            "grid" => format!(
                r#","row_height_px":{:.2},"max_offset_y":{:.1},"target_y":{:.1},"wraps":{}"#,
                self.row_h, self.max_y, self.target_y, self.wraps
            ),
            "stream" => {
                let s = &all[..self.stream_samples.min(all.len())];
                let st = self.wb.read(cx).tab_status(cx).read(cx);
                format!(
                    r#","stream_frames":{},"stream_p50_ms":{:.3},"stream_p95_ms":{:.3},"stream_p99_ms":{:.3},"stream_max_ms":{:.3},"first_row_ms":{},"rows_at_cancel":{},"rows_final":{},"outcome":"{}","cancel_to_finished_ms":{}{}"#,
                    s.len(),
                    q(s, 0.5),
                    q(s, 0.95),
                    q(s, 0.99),
                    s.iter().cloned().fold(0.0, f64::max),
                    self.first_row_ms
                        .map(|v| format!("{v:.3}"))
                        .unwrap_or("null".into()),
                    self.rows_at_cancel,
                    st.rows,
                    self.stream_outcome.unwrap_or("none"),
                    self.cancel_ms
                        .map(|v| format!("{v:.3}"))
                        .unwrap_or("null".into()),
                    self.gone_check.clone().unwrap_or_default()
                )
            }
            _ => {
                let e = self.wb.read(cx).tab_editor(cx).read(cx);
                let s = e.state.read(cx);
                let off = -s.scroll_offset().y.as_f32();
                let p = s.cursor_position();
                format!(
                    r#","syntax":"{}","text_bytes":{},"cursor_line":{},"scroll_offset_y":{off:.1},"max_offset_y_seen":{:.1},"wraps":{}"#,
                    if e.plain { "plain" } else { "sql" },
                    s.text().len(),
                    p.line,
                    self.max_y,
                    self.wraps
                )
            }
        };
        println!(
            r#"done {{"frames":{},"wall_ms":{wall:.3},"p50_ms":{:.3},"p95_ms":{:.3},"p99_ms":{:.3},"max_ms":{:.3}{extra}}}"#,
            all.len(),
            q(all, 0.5),
            q(all, 0.95),
            q(all, 0.99),
            all.iter().cloned().fold(0.0, f64::max)
        );
    }
}

/// Bench setup before the window exists: fixture loading for editor scenes.
pub fn initial_sql(scene: &str, dir: &str) -> (String, String, f64) {
    let t = Instant::now();
    match scene {
        "editor-scroll" | "editor-type" => {
            let text = std::fs::read_to_string(format!("{dir}/big.sql")).expect("read big.sql");
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            let lines = text.lines().count();
            let extra = format!(r#","fixture_bytes":{},"fixture_lines":{lines}"#, text.len());
            (text, extra, ms)
        }
        "stream" => (STREAM_SQL.to_string(), String::new(), 0.0),
        _ => (GRID_SQL.to_string(), String::new(), 0.0),
    }
}

/// The bench connection from BENCH_DSN, if set.
pub fn bench_connection() -> Option<(store::SavedConnection, String)> {
    std::env::var("BENCH_DSN")
        .ok()
        .and_then(|d| store::parse_dsn(&d))
}

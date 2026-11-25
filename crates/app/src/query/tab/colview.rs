use super::*;
use crate::results::colmenu::ColAct;
use locus_core::colfilter::{ColView, quote_ident};
use locus_core::copyfmt::Dialect;
use locus_core::store::DbKind;

// ---- server-side column filters and orders (grid header menu) -------------------

/// The filters on the tab's result and the query they wrap.
#[derive(Default)]
pub(crate) struct ColState {
    pub view: ColView,
    /// The plain SELECT the filters wrap (`None` = the menu is off).
    pub body: Option<String>,
    /// The SQL the last filter run sent; a run of this text keeps the filters.
    wrapped: Option<String>,
    dialect: Option<(db_core::sql::SqlDialect, Dialect)>,
    conn_id: Option<String>,
    pub saved: bool,
}

fn dialects(kind: DbKind) -> (db_core::sql::SqlDialect, Dialect) {
    use db_core::sql::SqlDialect as S;
    match kind {
        DbKind::Postgres => (S::Postgres, Dialect::Postgres),
        DbKind::Oracle => (S::Oracle, Dialect::Oracle),
        DbKind::Sqlite => (S::Sqlite, Dialect::Sqlite),
        // T-SQL lexes like SQLite here: no `#` comments, no `` escapes.
        DbKind::SqlServer => (S::Sqlite, Dialect::SqlServer),
        _ => (S::Mysql, Dialect::Mysql),
    }
}

impl QueryTab {
    /// A run starts with `sql`: a statement that is not the last filter run is a new query.
    pub(super) fn cv_begin(&mut self, sql: &str, conn: &SavedConnection) {
        let d = dialects(conn.kind);
        let c = &mut self.cv;
        c.dialect = Some(d);
        c.conn_id = Some(conn.id.clone());
        if c.wrapped.as_deref() == Some(sql) {
            return;
        }
        c.view = ColView::default();
        c.wrapped = None;
        c.body = (conn.kind != DbKind::Redis)
            .then(|| db_core::sql::select_body(sql, d.0))
            .flatten();
        c.saved = c
            .body
            .as_deref()
            .is_some_and(|b| locus_core::colfilter_store::find(&conn.id, b).is_some());
    }

    /// The shell asks before a run: filters saved for this query turn it into the filtered one.
    pub fn saved_view_sql(&mut self, sql: &str, conn: &SavedConnection) -> Option<String> {
        if self.cv.wrapped.as_deref() == Some(sql) || conn.kind == DbKind::Redis {
            return None;
        }
        let d = dialects(conn.kind);
        let body = db_core::sql::select_body(sql, d.0)?;
        let view = locus_core::colfilter_store::find(&conn.id, &body)?;
        let quote = |s: &str| quote_ident(d.1, s);
        let wrapped = view.wrap(&body, d.1, &quote);
        self.cv.view = view;
        self.cv.wrapped = Some(wrapped.clone());
        Some(wrapped)
    }

    /// The SQL of the current filters; remembered so the run keeps them.
    pub fn view_sql(&mut self) -> Option<String> {
        let (_, d) = self.cv.dialect?;
        let body = self.cv.body.clone()?;
        let quote = |s: &str| quote_ident(d, s);
        let sql = if self.cv.view.is_empty() {
            body
        } else {
            self.cv.view.wrap(&body, d, &quote)
        };
        self.cv.wrapped = Some(sql.clone());
        Some(sql)
    }

    /// The query as the user wrote it, for table lookup (no filters around it).
    pub(super) fn base_sql(&self) -> &str {
        self.cv.body.as_deref().unwrap_or(&self.last_sql)
    }

    /// Gives a new result grid its menu state.
    pub(super) fn cv_grid(&self, g: &mut ResultGrid) {
        g.colview = self.cv.body.is_some().then(|| self.cv.view.clone());
        g.colsaved = self.cv.saved;
    }

    /// A header menu entry or a chip: change the filters and run again.
    pub(super) fn col_act(&mut self, a: ColAct, cx: &mut Context<Self>) {
        let before = self.cv.view.clone();
        match a {
            ColAct::Order { col, desc } => self.cv.view.set_order(&col, desc),
            ColAct::Add(c) => self.cv.view.add_cond(c),
            ColAct::RemoveCond(i) if i < self.cv.view.conds.len() => {
                self.cv.view.conds.remove(i);
            }
            ColAct::RemoveOrder(i) if i < self.cv.view.orders.len() => {
                self.cv.view.orders.remove(i);
            }
            ColAct::Clear => self.cv.view = ColView::default(),
            ColAct::Save => return self.col_save(cx),
            ColAct::ResetSaved => return self.col_reset(cx),
            _ => {}
        }
        if self.cv.view != before {
            cx.emit(TabEvent::RerunView);
        }
    }

    fn col_save(&mut self, cx: &mut Context<Self>) {
        let (Some(conn), Some(body)) = (self.cv.conn_id.clone(), self.cv.body.clone()) else {
            return;
        };
        let msg = match locus_core::colfilter_store::put(&conn, &body, &self.cv.view) {
            Ok(()) => {
                self.cv.saved = !self.cv.view.is_empty();
                t!("colmenu.saved").to_string()
            }
            Err(e) => t!("colmenu.save_failed", msg = e.to_string()).to_string(),
        };
        self.cv_refresh(cx);
        crate::shell::toast::show(msg, cx);
    }

    fn col_reset(&mut self, cx: &mut Context<Self>) {
        let (Some(conn), Some(body)) = (self.cv.conn_id.clone(), self.cv.body.clone()) else {
            return;
        };
        if locus_core::colfilter_store::put(&conn, &body, &ColView::default()).is_ok() {
            self.cv.saved = false;
        }
        self.cv_refresh(cx);
    }

    /// Pushes the saved flag to the open grid.
    fn cv_refresh(&mut self, cx: &mut Context<Self>) {
        let saved = self.cv.saved;
        if let Some(t) = self.current_table() {
            t.update(cx, |t, cx| {
                t.delegate_mut().colsaved = saved;
                cx.notify();
            });
        }
        cx.notify();
    }
}

impl QueryTab {
    /// The active filters and orders as chips above the grid, each with an x.
    pub(super) fn render_chips(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let v = &self.cv.view;
        if self.cv.body.is_none() || v.is_empty() {
            return None;
        }
        let theme = cx.theme().clone();
        let chip = |id: (&'static str, usize),
                    text: String,
                    act: ColAct,
                    cx: &mut Context<Self>| {
            h_flex()
                .flex_none()
                .h(px(24.))
                .gap(px(sc::S2))
                .pl(px(8.))
                .pr(px(2.))
                .items_center()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .bg(theme.muted)
                .text_size(px(sc::TX_SM))
                .child(div().max_w(px(360.)).truncate().child(text))
                .child(
                    Button::new(id)
                        .ghost()
                        .xsmall()
                        .size(px(18.))
                        .icon(crate::shell::icons::icon("x").size(px(12.)))
                        .tooltip(t!("colmenu.remove").to_string())
                        .on_click(cx.listener(move |this, _, _, cx| this.col_act(act.clone(), cx))),
                )
        };
        let mut row = h_flex()
            .id("colchips")
            .flex_none()
            .flex_wrap()
            .gap(px(sc::S3))
            .px(px(24.))
            .py(px(4.))
            .items_center()
            .border_b_1()
            .border_color(theme.border);
        for (i, c) in v.conds.iter().enumerate() {
            let text = crate::results::colmenu::chip_text(c);
            row = row.child(chip(("cc", i), text, ColAct::RemoveCond(i), cx));
        }
        for (i, o) in v.orders.iter().enumerate() {
            let text = format!("{} {}", o.col, if o.desc { "DESC" } else { "ASC" });
            row = row.child(chip(("co", i), text, ColAct::RemoveOrder(i), cx));
        }
        Some(row.into_any_element())
    }
}

impl QueryTab {
    /// Shows a grid menu (right-click or header arrow) at `pos`, above everything else.
    pub(super) fn show_grid_menu(
        &mut self,
        menu: Entity<PopupMenu>,
        pos: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let sub = cx.subscribe(&menu, |this: &mut QueryTab, _, _: &DismissEvent, cx| {
            this.grid_menu = None;
            cx.notify();
        });
        self.grid_menu = Some((menu, pos, sub));
        cx.notify();
    }
}

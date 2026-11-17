use super::*;

// ---- paging --------------------------------------------------------------------

impl QueryTab {
    pub(super) fn page_size(&self) -> Option<usize> {
        if self.pg.show_all {
            None
        } else {
            self.limit.map(|l| l.max(1) as usize)
        }
    }

    /// Rows of the result in front shown on the current page.
    pub(super) fn page_window(&self) -> Option<(usize, usize)> {
        self.page_size().map(|n| (self.pg.page * n, n))
    }

    pub(crate) fn apply_page(&mut self, cx: &mut Context<Self>) {
        let w = self.page_window();
        for r in &self.out.results {
            r.table.update(cx, |t, cx| {
                t.delegate_mut().page = w;
                t.delegate_mut().range = None;
                t.delegate_mut().range_changed(cx);
                t.refresh(cx);
                t.scroll_to_row(0, cx);
                cx.notify();
            });
        }
        cx.notify();
    }

    /// Called after each event: at the end of the page, pause and hand the
    /// stream loop a receiver to wait on (true = go on, false = give up).
    pub(super) fn maybe_pause(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<bool>> {
        if self.trace.open_stmt.is_none()
            || self.out.truncated.is_some()
            || self.out.rows_total < self.pg.fetch_target
        {
            return None;
        }
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let idle = crate::settings::get(cx).page_idle();
        let tx2 = tx.clone();
        self.pg.idle_task = Some(cx.spawn(async move |_, cx| {
            cx.background_executor().timer(idle).await;
            let _ = tx2.send(false);
        }));
        self.pg.pager_tx = Some(tx);
        self.start_count(cx);
        self.pg.paused = true;
        self.sync_status(cx);
        self.status.update(cx, |s, cx| {
            s.phase = Phase::Paused;
            cx.notify();
        });
        cx.notify();
        Some(rx)
    }

    pub(super) fn resume(&mut self, cx: &mut Context<Self>) {
        if !self.pg.paused {
            return;
        }
        self.pg.paused = false;
        self.pg.idle_task = None;
        if let Some(tx) = self.pg.pager_tx.take() {
            let _ = tx.send(true);
        }
        self.status.update(cx, |s, cx| {
            s.phase = Phase::Running;
            cx.notify();
        });
    }

    /// The paused query is given up (idle timeout or cancel): the server
    /// cursor is released and the rows fetched so far stay.
    pub(super) fn expire(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.pg.paused = false;
        self.pg.expired = true;
        self.pg.pager_tx = None;
        self.pg.idle_task = None;
        let rows = self.out.rows_total;
        self.finish(Outcome::Done { rows }, window, cx);
    }

    pub fn next_page(&mut self, cx: &mut Context<Self>) {
        let Some(n) = self.page_size() else { return };
        // With a filter on, pages run over the rows it lets through.
        let fetched = match self.current_table().map(|t| t.read(cx).delegate()) {
            Some(g) if g.filter_active() => g.matched(),
            _ => self
                .out
                .results
                .get(self.out.active)
                .map(|r| r.rows as usize)
                .unwrap_or(0),
        };
        if (self.pg.page + 1) * n >= fetched && !self.pg.paused {
            return;
        }
        self.pg.page += 1;
        if (self.pg.page + 1) * n > fetched && self.pg.paused {
            self.pg.fetch_target = self.out.rows_total + n as u64;
            self.resume(cx);
        }
        self.apply_page(cx);
    }

    /// "Load more": fetch one more batch (a page) from the paused query and show it.
    pub fn load_more(&mut self, cx: &mut Context<Self>) {
        let Some(n) = self.page_size() else { return };
        if !self.pg.paused {
            return;
        }
        self.pg.fetch_target = self.out.rows_total + n as u64;
        self.resume(cx);
        self.pg.page += 1;
        self.apply_page(cx);
    }

    pub fn prev_page(&mut self, cx: &mut Context<Self>) {
        if self.pg.page > 0 {
            self.pg.page -= 1;
            self.apply_page(cx);
        }
    }

    pub fn first_page(&mut self, cx: &mut Context<Self>) {
        self.pg.page = 0;
        self.apply_page(cx);
    }

    pub fn load_all(&mut self, cx: &mut Context<Self>) {
        self.pg.show_all = true;
        self.pg.page = 0;
        self.pg.fetch_target = u64::MAX;
        self.resume(cx);
        self.apply_page(cx);
    }

    pub(super) fn render_pager(&self, fetched: u64, cx: &mut Context<Self>) -> Option<AnyElement> {
        // The footer always shows the range and the total; without a row
        // limit the whole result is one page. With a filter on, the pages
        // run over the rows the filter lets through.
        let (matched, filtered) = self
            .current_table()
            .map(|t| {
                let g = t.read(cx).delegate();
                (g.matched() as u64, g.filter_active())
            })
            .unwrap_or((fetched, false));
        let rows = if filtered { matched } else { fetched };
        let n = self.page_size().map(|n| n as u64).unwrap_or(rows.max(1));
        if fetched == 0 && !self.pg.paused && !self.pg.expired {
            return None;
        }
        let theme = cx.theme().clone();
        let a = if rows == 0 {
            0
        } else {
            (self.pg.page as u64 * n + 1).min(rows)
        };
        let b = ((self.pg.page as u64 + 1) * n).min(rows);
        let can_next = b < rows || self.pg.paused;
        // The footer text and the offered actions come from the one run-state mapping.
        use locus_core::runview::State as Rs;
        let rv = self.status.read(cx).view();
        let color = match rv.state {
            Rs::PausedMore => theme.primary,
            Rs::Failed => theme.danger,
            Rs::Cancelled | Rs::Released | Rs::Capped => theme.warning,
            _ => theme.muted_foreground,
        };
        let state = (!rv.footer.is_empty()).then(|| (rv.footer.clone(), color));
        let actions = rv.actions;
        Some(
            h_flex()
                .id("pager")
                .flex_none()
                .gap(px(sc::S2))
                .px(px(sc::S4))
                .py(px(sc::S2))
                .items_center()
                .text_size(px(sc::TX_SM))
                .border_t_1()
                .border_color(theme.border)
                .child(
                    Button::new("page-first")
                        .small()
                        .ghost()
                        .icon(IconName::ArrowLeft)
                        .tooltip(t!("pager.first").to_string())
                        .disabled(self.pg.page == 0)
                        .on_click(cx.listener(|t, _, _, cx| t.first_page(cx))),
                )
                .child(
                    Button::new("page-prev")
                        .small()
                        .ghost()
                        .icon(IconName::ChevronLeft)
                        .tooltip(t!("pager.prev").to_string())
                        .disabled(self.pg.page == 0)
                        .on_click(cx.listener(|t, _, _, cx| t.prev_page(cx))),
                )
                .child(
                    div()
                        .flex_none()
                        .child(self.range_text(a, b, rows, fetched, filtered).to_string()),
                )
                .child(
                    Button::new("page-next")
                        .small()
                        .ghost()
                        .icon(IconName::ChevronRight)
                        .tooltip(t!("pager.next").to_string())
                        .disabled(!can_next)
                        .on_click(cx.listener(|t, _, _, cx| t.next_page(cx))),
                )
                .when_some(state, |h, (s, color)| {
                    h.child(div().min_w_0().truncate().text_color(color).child(s))
                })
                .child(div().flex_1())
                .children(self.sel_stats(cx))
                .when(actions.load_more, |h| {
                    h.child(
                        Button::new("page-more")
                            .small()
                            .ghost()
                            .label(t!("runview.load_more").to_string())
                            .tooltip(t!("runview.load_more_hint").to_string())
                            .on_click(cx.listener(|t, _, _, cx| t.load_more(cx))),
                    )
                })
                .when(actions.load_all, |h| {
                    h.child(
                        Button::new("page-all")
                            .small()
                            .ghost()
                            .label(t!("runview.load_all").to_string())
                            .tooltip(t!("runview.load_all_hint").to_string())
                            .on_click(cx.listener(|t, _, _, cx| t.load_all(cx))),
                    )
                })
                .when(actions.release, |h| {
                    h.child(
                        Button::new("page-release")
                            .small()
                            .ghost()
                            .label(t!("runview.release").to_string())
                            .tooltip(t!("runview.release_hint").to_string())
                            .on_click(cx.listener(|t, _, _, cx| t.cancel(cx))),
                    )
                })
                .into_any_element(),
        )
    }

    /// Count / sum / average of the selected cells; click copies them.
    fn sel_stats(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let theme = cx.theme().clone();
        let s = self
            .current_table()
            .and_then(|t| t.read(cx).delegate().sel_summary.clone())?;
        let text = match (&s.sum, &s.avg, &s.min, &s.max) {
            (Some(sum), Some(avg), Some(min), Some(max)) => t!(
                "selstats.numeric",
                count = fmt::count(s.count),
                sum = sum.clone(),
                avg = avg.clone(),
                min = min.clone(),
                max = max.clone()
            )
            .into_owned(),
            _ => t!("selstats.count", count = fmt::count(s.count)).into_owned(),
        };
        let tip = t!(
            "selstats.tip",
            nulls = fmt::count(s.nulls),
            numeric = fmt::count(s.numeric)
        )
        .into_owned();
        let copy = s.clipboard();
        Some(
            div()
                .id("sel-stats")
                .min_w_0()
                .truncate()
                .px(px(sc::S3))
                .rounded(px(sc::R_SM))
                .cursor_pointer()
                .text_color(theme.muted_foreground)
                .hover(|d| d.bg(theme.muted))
                .tooltip(move |w, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)
                })
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                })
                .child(text),
        )
    }
}

// ---- total row count ---------------------------------------------------------------

impl QueryTab {
    /// "1–1,000 of 40,450": exact when the query ended, the background count
    /// while paused, "many" until it arrives.
    pub(super) fn range_text(
        &self,
        a: u64,
        b: u64,
        rows: u64,
        fetched: u64,
        filtered: bool,
    ) -> String {
        if filtered {
            // The filter works on the rows loaded so far: "a–b of N (filtered from M)".
            let mut s = crate::results::ui::data_range_text(
                a,
                b,
                rows,
                fetched,
                true,
                self.filter_scanning(),
            );
            if self.pg.paused || self.pg.expired {
                s.push_str(&t!("pager.loaded", n = fmt::count(fetched)));
            }
            return s;
        }
        let total = if self.pg.paused || self.pg.expired {
            match self.pg.total_count {
                Some(n) => fmt::count(n),
                None => t!("pager.many").into_owned(),
            }
        } else {
            fmt::count(rows)
        };
        // How many rows are loaded is said by the footer text (`runview`).
        t!(
            "pager.range",
            a = fmt::count(a),
            b = fmt::count(b),
            total = total
        )
        .into_owned()
    }

    /// Paused single SELECT: count its rows on another pool connection
    /// (10 s timeout; dropped, so cancelled, when the tab runs something else).
    pub(super) fn start_count(&mut self, cx: &mut Context<Self>) {
        if self.pg.total_count.is_some()
            || self.pg.count_task.is_some()
            || crate::settings::get(cx).no_count
        {
            return;
        }
        let Some(driver) = self.driver.clone() else {
            return;
        };
        // The count runs the query a second time: not on its own on Production.
        let Some((dialect, prod)) = self.trace.count_ctx else {
            return;
        };
        if prod {
            return;
        }
        let Some(count_sql) = db_core::sql::count_wrapper(&self.last_sql, dialect) else {
            return;
        };
        self.pg.count_task = Some(cx.spawn(async move |this, cx| {
            let opts = QueryOptions {
                timeout: Some(Duration::from_secs(10)),
                ..Default::default()
            };
            let mut h = driver.query(count_sql, opts);
            let mut n = None;
            while let Some(ev) = h.next().await {
                match ev {
                    QueryEvent::Batch(b) if b.rows() > 0 => {
                        if let db_core::Cell::Text(v) = b.cell(0, 0) {
                            n = v.trim().parse::<u64>().ok();
                        }
                    }
                    QueryEvent::Finished(_) => break,
                    _ => {}
                }
            }
            let _ = this.update(cx, |t, cx| {
                t.pg.total_count = n;
                t.sync_status(cx);
                cx.notify();
            });
        }));
    }
}

impl QueryTab {
    /// Row limit picked from the status bar.
    pub fn set_limit(&mut self, v: Option<u64>, cx: &mut Context<Self>) {
        self.limit = v;
        self.sync_status(cx);
        cx.emit(TabEvent::Changed);
        cx.notify();
    }
}

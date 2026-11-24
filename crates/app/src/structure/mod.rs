//! Structure tabs: the read-only structure of one table or view (columns,
//! indexes, foreign keys both ways, triggers, checks, DDL), like TablePlus /
//! DataGrip. Data comes from `Catalog::structure` and `Catalog::ddl`
//! (`db_core::struct_sql`), cached per connection and table until the tab's
//! Refresh. The tab is a `QueryTab` whose `structure` is set; its editor is
//! the read-only DDL view.
//!
//! Editing the structure (ALTER) is not built: the place for it is the
//! header (`ui::render_structure_header`, next to Refresh) and a statement
//! builder that would go through the Production gate like `routines`.
use crate::shell::*;
use db_core::structure::TableStructure;
use db_core::{DbObjectKind, Driver as _};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

mod ui;

/// Rows the catalog may return per section.
const CAP: u32 = 500;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StructureRef {
    pub conn_id: String,
    pub schema: String,
    pub table: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    Columns,
    Indexes,
    ForeignKeys,
    Triggers,
    Checks,
    Ddl,
}

pub enum Load<T> {
    Loading,
    Ready(T),
    Failed(String),
}

pub struct Structure {
    pub r: StructureRef,
    pub section: Section,
    pub data: Load<Arc<TableStructure>>,
    pub ddl: Load<String>,
}

/// The section last chosen per table, so a reopened tab comes back to it (this run only).
fn last_sections() -> &'static Mutex<HashMap<StructureRef, Section>> {
    static C: OnceLock<Mutex<HashMap<StructureRef, Section>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

pub(crate) fn remember_section(r: &StructureRef, s: Section) {
    last_sections().lock().insert(r.clone(), s);
}

pub(crate) fn last_section(r: &StructureRef) -> Section {
    last_sections()
        .lock()
        .get(r)
        .copied()
        .unwrap_or(Section::Columns)
}

type Entry = (Arc<TableStructure>, Result<String, String>);

fn cache() -> &'static Mutex<HashMap<StructureRef, Entry>> {
    static C: OnceLock<Mutex<HashMap<StructureRef, Entry>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Reads the structure and the DDL (table first, then view).
async fn read(
    driver: Arc<locus_core::driver::AnyDriver>,
    r: StructureRef,
) -> Result<Entry, db_core::DbError> {
    let cat = driver
        .catalog()
        .ok_or_else(|| db_core::DbError::new(db_core::ErrorKind::Internal, "no catalog"))?;
    let st = cat.structure(&r.schema, &r.table, CAP).await?;
    let ddl = match cat.ddl(&r.schema, DbObjectKind::Table, &r.table).await {
        Ok(d) => Ok(d),
        Err(_) => cat
            .ddl(&r.schema, DbObjectKind::View, &r.table)
            .await
            .map_err(|e| e.message),
    };
    Ok((Arc::new(st), ddl))
}

impl Locus {
    /// Opens (or shows) the Structure tab of a table or view.
    pub(crate) fn open_structure(
        &mut self,
        conn_id: String,
        schema: String,
        table: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let schema = if schema.is_empty() {
            self.default_schema(&conn_id).unwrap_or_default()
        } else {
            schema
        };
        let r = StructureRef {
            conn_id: conn_id.clone(),
            schema,
            table,
        };
        let found = self
            .space()
            .tabs
            .iter()
            .find(|t| t.read(cx).structure.as_ref().is_some_and(|s| s.r == r))
            .cloned();
        if let Some(t) = found {
            let id = t.read(cx).id.clone();
            return self.show_tab(&id, window, cx);
        }
        self.ensure_conn_in_space(&conn_id, cx);
        let mut data = self.blank_tab(Some(conn_id), cx);
        data.title = Some(r.table.clone());
        let tab = self.make_tab(&data, window, cx);
        let state = tab.read(cx).editor.read(cx).state.clone();
        state.update(cx, |s, cx| s.set_readonly(true, cx));
        tab.update(cx, |t, _| {
            t.structure = Some(Structure {
                section: last_section(&r),
                r,
                data: Load::Loading,
                ddl: Load::Loading,
            })
        });
        let s = self.space_mut();
        s.tabs.push(tab.clone());
        s.show_alone(&data.id, cx);
        self.sync_space_conns(self.active_space, cx);
        self.schedule_save(cx);
        self.load_structure(tab, false, window, cx);
        cx.notify();
    }

    /// Fills the tab from the cache, or from the server (`force` = Refresh).
    pub(crate) fn load_structure(
        &mut self,
        tab: Entity<QueryTab>,
        force: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(r) = tab.read(cx).structure.as_ref().map(|s| s.r.clone()) else {
            return;
        };
        if !force && let Some(hit) = cache().lock().get(&r).cloned() {
            tab.update(cx, |t, cx| t.set_structure(Ok(hit), window, cx));
            return;
        }
        let Some(driver) = self.pool.open_driver(&r.conn_id) else {
            // A tab restored at startup: the connection opens later (first run or
            // tree click). Wait for it instead of failing for good.
            if !force && !r.conn_id.is_empty() {
                tab.update(cx, |t, cx| {
                    if let Some(s) = t.structure.as_mut() {
                        s.data = Load::Loading;
                        s.ddl = Load::Loading;
                    }
                    cx.notify();
                });
                cx.spawn_in(window, async move |this, cx| {
                    for _ in 0..600 {
                        cx.background_executor()
                            .timer(std::time::Duration::from_secs(1))
                            .await;
                        let Ok(ready) =
                            this.update(cx, |this, _| this.pool.open_driver(&r.conn_id).is_some())
                        else {
                            return;
                        };
                        if ready {
                            let _ = this.update_in(cx, |this, window, cx| {
                                this.load_structure(tab.clone(), false, window, cx)
                            });
                            return;
                        }
                        if tab.update(cx, |t, _| t.structure.is_none()) {
                            return;
                        }
                    }
                    // Gave up (the connection was never opened): say so, Refresh tries again.
                    let _ = tab.update_in(cx, |t, window, cx| {
                        t.set_structure(Err(t!("error.no_connection").into_owned()), window, cx)
                    });
                })
                .detach();
                return;
            }
            tab.update(cx, |t, cx| {
                t.set_structure(Err(t!("error.no_connection").into_owned()), window, cx)
            });
            return;
        };
        tab.update(cx, |t, cx| {
            if let Some(s) = t.structure.as_mut() {
                s.data = Load::Loading;
                s.ddl = Load::Loading;
            }
            cx.notify();
        });
        cx.spawn_in(window, async move |_, cx| {
            let res = read(driver, r.clone()).await;
            if let Ok(e) = &res {
                cache().lock().insert(r, e.clone());
            }
            let _ = tab.update_in(cx, |t, window, cx| {
                t.set_structure(res.map_err(|e| e.message), window, cx)
            });
        })
        .detach();
    }
}

impl QueryTab {
    fn set_structure(
        &mut self,
        res: Result<Entry, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.editor.read(cx).state.clone();
        let Some(s) = self.structure.as_mut() else {
            return;
        };
        let mut ddl_text = None;
        match res {
            Err(e) => {
                s.data = Load::Failed(e.clone());
                s.ddl = Load::Failed(e);
            }
            Ok((st, ddl)) => {
                s.data = Load::Ready(st);
                s.ddl = match ddl {
                    Ok(d) => {
                        ddl_text = Some(d.clone());
                        Load::Ready(d)
                    }
                    Err(e) => Load::Failed(e),
                };
            }
        }
        if let Some(d) = ddl_text {
            state.update(cx, |st, cx| {
                st.set_readonly(false, cx);
                st.set_value(d, window, cx);
                st.set_readonly(true, cx);
            });
        }
        cx.notify();
    }
}

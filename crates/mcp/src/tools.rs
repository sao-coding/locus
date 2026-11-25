//! The tools. Each one takes the connection by id or unique name, returns
//! compact text, and reports short, actionable errors.

use crate::access::{self, Verdict};
use crate::exec::{self, StmtOut};
use crate::format;
use crate::host::{ConfirmRequest, Event, Host};
use db_core::classify::classify;
use db_core::{Catalog, DbObjectKind, Driver, ErrorKind, ObjectKind};
use locus_core::audit;
use locus_core::driver::AnyDriver;
use locus_core::store::{DbKind, Env, McpAccess, SavedConnection};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// How long a Production write waits for Allow / Deny in Locus.
pub const CONFIRM_TIMEOUT: Duration = Duration::from_secs(60);
/// Catalog calls (list_objects, describe_table, get_ddl).
const CATALOG_TIMEOUT: Duration = Duration::from_secs(30);
/// Lines `list_objects` returns at most.
const LIST_CAP: u32 = 2000;

pub type ToolResult = Result<String, String>;

/// Error of a call the client cancelled (or disconnected from).
const CANCELLED: &str = "cancelled by the client";

/// One open driver per connection and access level, separate from the
/// app's own pool so MCP never shares a session (or a manual transaction)
/// with the user's tabs.
struct Open {
    /// The saved settings it was opened with; an edit opens a new one.
    fingerprint: String,
    driver: Arc<AnyDriver>,
}

pub struct Core {
    pub host: Arc<dyn Host>,
    pub rt: tokio::runtime::Handle,
    drivers: tokio::sync::Mutex<HashMap<String, Open>>,
    next_id: AtomicU64,
    pub confirm_timeout: Duration,
}

/// Everything about the call that is not an argument.
pub struct Call<'a> {
    pub client: &'a str,
    pub ct: &'a tokio_util::sync::CancellationToken,
}

fn env_str(env: Env) -> &'static str {
    match env {
        Env::Local => "local",
        Env::Dev => "dev",
        Env::Staging => "staging",
        Env::Team => "team",
        Env::Prod => "prod",
    }
}

fn kind_str(k: DbKind) -> &'static str {
    match k {
        DbKind::Mysql => "mysql",
        DbKind::Postgres => "postgres",
        DbKind::Oracle => "oracle",
        DbKind::Sqlite => "sqlite",
        DbKind::SqlServer => "sqlserver",
        DbKind::Redis => "redis",
    }
}

fn str_arg<'a>(args: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn need<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str, String> {
    str_arg(args, key).ok_or_else(|| format!("missing argument '{key}'"))
}

/// Waits for `fut` unless the call is cancelled (client gone) first.
async fn or_cancel<T>(
    ct: &tokio_util::sync::CancellationToken,
    fut: impl std::future::Future<Output = T>,
) -> Result<T, String> {
    tokio::select! {
        r = fut => Ok(r),
        _ = ct.cancelled() => Err(CANCELLED.into()),
    }
}

/// The answer to a Production confirmation.
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    Allowed,
    Denied,
    TimedOut,
}

impl Core {
    pub fn new(host: Arc<dyn Host>, rt: tokio::runtime::Handle) -> Self {
        Core {
            host,
            rt,
            drivers: Default::default(),
            next_id: AtomicU64::new(1),
            confirm_timeout: CONFIRM_TIMEOUT,
        }
    }

    fn id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Connections MCP may see (access is not Off).
    fn visible(&self) -> Vec<SavedConnection> {
        self.host
            .connections()
            .into_iter()
            .filter(|c| c.mcp_access() != McpAccess::Off)
            .collect()
    }

    /// The connection named by `arg`: its id, else its exact name, else a
    /// case-insensitive name. Off connections do not exist here.
    pub fn resolve(&self, arg: &str) -> Result<SavedConnection, String> {
        let all = self.visible();
        if let Some(c) = all.iter().find(|c| c.id == arg) {
            return Ok(c.clone());
        }
        for exact in [true, false] {
            let hits: Vec<&SavedConnection> = all
                .iter()
                .filter(|c| {
                    if exact {
                        c.name == arg
                    } else {
                        c.name.eq_ignore_ascii_case(arg)
                    }
                })
                .collect();
            match hits.len() {
                0 => continue,
                1 => return Ok(hits[0].clone()),
                n => {
                    return Err(format!(
                        "{n} connections are named '{arg}'; pass the id (see list_connections)"
                    ));
                }
            }
        }
        Err(format!(
            "unknown connection '{arg}'; call list_connections for ids and names"
        ))
    }

    async fn driver(&self, c: &SavedConnection) -> Result<Arc<AnyDriver>, String> {
        let ro = c.mcp_access() == McpAccess::ReadOnly;
        let key = format!("{}#{}", c.id, if ro { "ro" } else { "rw" });
        let mut probe = c.clone();
        probe.mcp = None;
        let fingerprint = serde_json::to_string(&probe).unwrap_or_default();
        let mut map = self.drivers.lock().await;
        if let Some(o) = map.get(&key)
            && o.fingerprint == fingerprint
        {
            return Ok(o.driver.clone());
        }
        map.remove(&key);
        let password = self
            .host
            .password(&c.id)
            .map_err(|e| format!("saved password could not be read: {e}"))?
            .unwrap_or_default();
        let flag = Arc::new(AtomicBool::new(ro));
        let d = AnyDriver::connect(&self.rt, c, password, flag)
            .await
            .map_err(|e| format!("could not connect to '{}': {}", c.name, exec::db_error(&e)))?;
        let d = Arc::new(d);
        map.insert(
            key,
            Open {
                fingerprint,
                driver: d.clone(),
            },
        );
        Ok(d)
    }

    /// A connection-level failure: the next call connects again.
    async fn forget(&self, c: &SavedConnection) {
        let mut map = self.drivers.lock().await;
        map.retain(|k, _| !k.starts_with(&format!("{}#", c.id)));
    }

    /// Drops every open driver (server stopped).
    pub async fn close_all(&self) {
        self.drivers.lock().await.clear();
    }

    fn busy(&self, call: &Call<'_>, c: &SavedConnection, tool: &'static str) -> Busy {
        let id = self.id();
        self.host.event(Event::Busy {
            id,
            client: call.client.to_string(),
            conn: c.name.clone(),
            tool,
        });
        Busy {
            id,
            host: self.host.clone(),
        }
    }

    fn audit(&self, c: &SavedConnection, event: &str, call: &Call<'_>, sql: &str, kw: Vec<String>) {
        let mut e = audit::Entry::new(&c.name, env_str(c.env), event);
        e.sql = Some(sql);
        e.keywords = kw;
        e.source = Some("mcp");
        e.client = Some(call.client);
        audit::append(&e);
    }

    /// Asks the user in Locus; `TimedOut` after `confirm_timeout`.
    pub async fn ask(
        &self,
        c: &SavedConnection,
        client: &str,
        sql: &str,
        info: locus_core::guard::WriteInfo,
    ) -> Answer {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let id = self.id();
        self.host.confirm(ConfirmRequest {
            id,
            conn: c.clone(),
            client: client.to_string(),
            sql: sql.to_string(),
            info,
            reply: tx,
        });
        match tokio::time::timeout(self.confirm_timeout, rx).await {
            Ok(Ok(true)) => Answer::Allowed,
            Ok(_) => Answer::Denied,
            Err(_) => {
                self.host.event(Event::ConfirmExpired { id });
                Answer::TimedOut
            }
        }
    }

    /// Access decision + Production confirmation + audit. `Ok(read_only)` = send.
    async fn admit(
        &self,
        c: &SavedConnection,
        call: &Call<'_>,
        sql: &str,
        stmts: &[String],
    ) -> Result<bool, String> {
        let classified: Vec<_> = stmts.iter().map(|s| classify(s)).collect();
        let kw = locus_core::guard::WriteInfo::of(&classified)
            .map(|w| w.keywords)
            .unwrap_or_default();
        match access::decide(c, &classified) {
            Verdict::Send { read_only } => {
                self.audit(c, "mcp_sent", call, sql, kw);
                Ok(read_only)
            }
            Verdict::Refuse(m) => {
                self.audit(c, "blocked", call, sql, kw);
                Err(m)
            }
            Verdict::Confirm(info) => {
                let answer = or_cancel(call.ct, self.ask(c, call.client, sql, info)).await;
                match answer {
                    Ok(Answer::Allowed) => {
                        self.audit(c, "confirmed_sent", call, sql, kw);
                        Ok(false)
                    }
                    Ok(Answer::Denied) => {
                        self.audit(c, "mcp_denied", call, sql, kw);
                        Err(format!(
                            "the user denied this write on Production connection '{}'; nothing was sent",
                            c.name
                        ))
                    }
                    Ok(Answer::TimedOut) => {
                        self.audit(c, "mcp_timeout", call, sql, kw);
                        Err(format!(
                            "no answer in Locus within {} s for this write on Production connection '{}'; nothing was sent",
                            self.confirm_timeout.as_secs(),
                            c.name
                        ))
                    }
                    Err(e) => Err(e),
                }
            }
        }
    }

    fn history(
        &self,
        c: &SavedConnection,
        call: &Call<'_>,
        sql: &str,
        ms: u64,
        rows: u64,
        error: Option<String>,
    ) {
        self.host.event(Event::History(locus_core::history::Entry {
            conn_id: c.id.clone(),
            sql: sql.to_string(),
            at: locus_core::history::now_secs().saturating_sub(ms / 1000),
            ms,
            rows: error.is_none().then_some(rows),
            error,
            params: Vec::new(),
            mcp_client: Some(call.client.to_string()),
        }));
    }

    // ---- tools ----------------------------------------------------------------

    pub fn list_connections(&self) -> ToolResult {
        let mut out = String::from("id\tname\tkind\tenv\taccess\tdatabase\n");
        let all = self.visible();
        for c in &all {
            let db = match c.kind {
                DbKind::Sqlite => c.database.clone().unwrap_or_default(),
                _ => {
                    let mut s = format!("{}:{}", c.host, c.port);
                    if let Some(d) = c.database.as_deref().filter(|d| !d.is_empty()) {
                        s.push('/');
                        s.push_str(d);
                    }
                    s
                }
            };
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\n",
                c.id,
                format::cut(&c.name),
                kind_str(c.kind),
                env_str(c.env),
                c.mcp_access().label(),
                format::cut(&db)
            ));
        }
        out.push_str(&format!("({} connections)", all.len()));
        Ok(out)
    }

    async fn catalog_call<T>(
        &self,
        c: &SavedConnection,
        call: &Call<'_>,
        tool: &'static str,
        f: impl FnOnce(&dyn Catalog) -> db_core::CatalogFuture<T>,
    ) -> Result<T, String> {
        let d = or_cancel(call.ct, self.driver(c)).await??;
        let cat = d
            .catalog()
            .ok_or_else(|| format!("{} has no catalog support", kind_str(c.kind)))?;
        let _busy = self.busy(call, c, tool);
        let fut = f(cat);
        match or_cancel(call.ct, tokio::time::timeout(CATALOG_TIMEOUT, fut)).await? {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => {
                if matches!(e.kind, ErrorKind::Connect | ErrorKind::Internal) {
                    self.forget(c).await;
                }
                Err(exec::db_error(&e))
            }
            Err(_) => Err(format!("timed out after {} s", CATALOG_TIMEOUT.as_secs())),
        }
    }

    /// The schema to use: the argument, the `schema.` prefix of the name, or the current one.
    async fn schema_for(
        &self,
        c: &SavedConnection,
        call: &Call<'_>,
        schema: Option<&str>,
        name: &str,
    ) -> Result<(String, String), String> {
        if let Some(s) = schema {
            return Ok((s.to_string(), name.to_string()));
        }
        if let Some((s, n)) = name.split_once('.')
            && !s.is_empty()
            && !n.is_empty()
        {
            return Ok((s.to_string(), n.to_string()));
        }
        let cur = self
            .catalog_call(c, call, "list_objects", |cat| cat.current_schema())
            .await?;
        match cur {
            Some(s) => Ok((s, name.to_string())),
            None => Err("no current schema; pass 'schema' (list_objects lists them)".into()),
        }
    }

    pub async fn list_objects(&self, args: &Map<String, Value>, call: &Call<'_>) -> ToolResult {
        let c = self.resolve(need(args, "connection")?)?;
        let Some(schema) = str_arg(args, "schema") else {
            let schemas = self
                .catalog_call(&c, call, "list_objects", |cat| cat.schemas())
                .await?;
            let cur = self
                .catalog_call(&c, call, "list_objects", |cat| cat.current_schema())
                .await
                .ok()
                .flatten();
            let mut out = String::from("schema\n");
            for s in &schemas {
                out.push_str(s);
                if cur.as_deref() == Some(s.as_str()) {
                    out.push_str("\t(current)");
                }
                out.push('\n');
            }
            out.push_str(&format!("({} schemas)", schemas.len()));
            return Ok(out);
        };
        let kind = str_arg(args, "kind").unwrap_or("all").to_ascii_lowercase();
        let (tables, routines) = match kind.as_str() {
            "all" => (true, true),
            "table" | "tables" | "view" | "views" => (true, false),
            "routine" | "routines" => (false, true),
            other => {
                return Err(format!(
                    "unknown kind '{other}'; use table, view, routine or all"
                ));
            }
        };
        let mut lines: Vec<(String, &'static str)> = Vec::new();
        if tables {
            let s = schema.to_string();
            let objs = self
                .catalog_call(&c, call, "list_objects", move |cat| cat.objects(&s))
                .await?;
            for o in objs {
                let k = match o.kind {
                    ObjectKind::Table => "table",
                    ObjectKind::View => "view",
                };
                let keep = match kind.as_str() {
                    "table" | "tables" => k == "table",
                    "view" | "views" => k == "view",
                    _ => true,
                };
                if keep {
                    lines.push((o.name, k));
                }
            }
        }
        if routines {
            for (k, label) in [
                (DbObjectKind::Procedure, "procedure"),
                (DbObjectKind::Function, "function"),
            ] {
                let s = schema.to_string();
                let got = self
                    .catalog_call(&c, call, "list_objects", move |cat| {
                        if cat.supported_kinds().contains(&k) {
                            cat.list_objects(&s, k, LIST_CAP)
                        } else {
                            Box::pin(async { Ok(Vec::new()) })
                        }
                    })
                    .await?;
                lines.extend(got.into_iter().map(|e| (e.name, label)));
            }
        }
        let total = lines.len();
        let mut out = String::from("name\tkind\n");
        for (n, k) in lines.iter().take(LIST_CAP as usize) {
            out.push_str(&format!("{}\t{k}\n", format::cut(n)));
        }
        if total > LIST_CAP as usize {
            out.push_str(&format!(
                "({} of {} objects shown, truncated)",
                format::thousands(LIST_CAP as u64),
                format::thousands(total as u64)
            ));
        } else {
            out.push_str(&format!("({total} objects)"));
        }
        Ok(out)
    }

    pub async fn describe_table(&self, args: &Map<String, Value>, call: &Call<'_>) -> ToolResult {
        let c = self.resolve(need(args, "connection")?)?;
        let (schema, table) = self
            .schema_for(&c, call, str_arg(args, "schema"), need(args, "table")?)
            .await?;
        let (s, t) = (schema.clone(), table.clone());
        let st = self
            .catalog_call(&c, call, "describe_table", move |cat| {
                cat.structure(&s, &t, 500)
            })
            .await?;
        if st.columns.is_empty() {
            return Err(format!("no table or view '{table}' in schema '{schema}'"));
        }
        Ok(render_structure(&schema, &table, &st))
    }

    pub async fn get_ddl(&self, args: &Map<String, Value>, call: &Call<'_>) -> ToolResult {
        let c = self.resolve(need(args, "connection")?)?;
        let (schema, name) = self
            .schema_for(&c, call, str_arg(args, "schema"), need(args, "object")?)
            .await?;
        let d = or_cancel(call.ct, self.driver(&c)).await??;
        let cat = d
            .catalog()
            .ok_or_else(|| format!("{} has no catalog support", kind_str(c.kind)))?;
        let _busy = self.busy(call, &c, "get_ddl");
        let mut last = String::new();
        for k in [
            DbObjectKind::Table,
            DbObjectKind::View,
            DbObjectKind::MaterializedView,
            DbObjectKind::Function,
            DbObjectKind::Procedure,
            DbObjectKind::Package,
            DbObjectKind::Trigger,
            DbObjectKind::Sequence,
        ] {
            let fut = cat.ddl(&schema, k, &name);
            match or_cancel(call.ct, tokio::time::timeout(CATALOG_TIMEOUT, fut)).await? {
                Ok(Ok(text)) if !text.trim().is_empty() => return Ok(text),
                Ok(Ok(_)) => {}
                Ok(Err(e)) => last = exec::db_error(&e),
                Err(_) => return Err(format!("timed out after {} s", CATALOG_TIMEOUT.as_secs())),
            }
        }
        let _ = last;
        Err(format!(
            "no table, view or routine '{name}' in schema '{schema}' (list_objects lists them)"
        ))
    }

    pub async fn run_sql(&self, args: &Map<String, Value>, call: &Call<'_>) -> ToolResult {
        let c = self.resolve(need(args, "connection")?)?;
        let sql = need(args, "sql")?;
        let cap = format::row_cap(args.get("max_rows").and_then(Value::as_u64));
        let stmts: Vec<String> = db_core::sql::split_statements(sql)
            .into_iter()
            .map(|r| sql[r].to_string())
            .collect();
        if stmts.is_empty() {
            return Err("no statement in 'sql'".into());
        }
        let read_only = self.admit(&c, call, sql, &stmts).await?;
        let started = std::time::Instant::now();
        let r = self.run_admitted(&c, call, &stmts, cap, read_only).await;
        let ms = started.elapsed().as_millis() as u64;
        match r {
            Ok(out) => {
                let failed = out.iter().find_map(|o| match o {
                    StmtOut::Failed(m) => Some(m.clone()),
                    _ => None,
                });
                let rows = out.iter().map(StmtOut::rows).sum();
                let text = exec::render_script(&stmts, &out);
                let ok = failed.is_none();
                self.history(&c, call, sql, ms, rows, failed);
                // A failed statement makes the call an error (isError), with the same text.
                if ok { Ok(text) } else { Err(text) }
            }
            Err(e) => {
                // History shows a cancelled run as Cancelled, not as an error.
                let shown = if e == CANCELLED {
                    locus_core::history::cancelled_text()
                } else {
                    e.clone()
                };
                self.history(&c, call, sql, ms, 0, Some(shown));
                Err(e)
            }
        }
    }

    /// Runs statements that passed `admit` on a session of their own.
    async fn run_admitted(
        &self,
        c: &SavedConnection,
        call: &Call<'_>,
        stmts: &[String],
        cap: usize,
        read_only: bool,
    ) -> Result<Vec<StmtOut>, String> {
        let d = or_cancel(call.ct, self.driver(c)).await??;
        let _busy = self.busy(call, c, "run_sql");
        let session = match or_cancel(call.ct, d.session()).await? {
            Ok(s) => s,
            Err(e) => {
                self.forget(c).await;
                return Err(format!("could not open a session: {}", exec::db_error(&e)));
            }
        };
        let txn = if read_only {
            access::read_only_txn(c.kind)
        } else {
            None
        };
        or_cancel(call.ct, exec::run_script(session.as_ref(), stmts, cap, txn)).await
    }

    pub async fn explain(&self, args: &Map<String, Value>, call: &Call<'_>) -> ToolResult {
        use locus_core::plan::{self, Engine};
        let c = self.resolve(need(args, "connection")?)?;
        let sql = need(args, "sql")?;
        let stmts: Vec<String> = db_core::sql::split_statements(sql)
            .into_iter()
            .map(|r| sql[r].to_string())
            .collect();
        if stmts.len() != 1 {
            return Err("explain takes exactly one statement".into());
        }
        let engine = match c.kind {
            DbKind::Mysql => Engine::Mysql,
            DbKind::Postgres => Engine::Postgres,
            DbKind::Sqlite => Engine::Sqlite,
            k => return Err(format!("explain is not supported for {} yet", kind_str(k))),
        };
        let variants = plan::explain_sql(engine, &stmts[0], false);
        // What is sent is the EXPLAIN text: it goes through the same decision.
        let read_only = self.admit(&c, call, &variants[0], &variants[..1]).await?;
        let d = or_cancel(call.ct, self.driver(&c)).await??;
        let _busy = self.busy(call, &c, "explain");
        let session = or_cancel(call.ct, d.session())
            .await?
            .map_err(|e| format!("could not open a session: {}", exec::db_error(&e)))?;
        let txn = if read_only {
            access::read_only_txn(c.kind)
        } else {
            None
        };
        let s = session.as_ref();
        let work = async {
            if let Some((begin, _)) = txn {
                exec::answer(s, begin).await?;
            }
            let mut last = String::new();
            let mut got = None;
            for v in &variants {
                match exec::answer(s, v).await {
                    Ok(a) => {
                        got = Some(a);
                        break;
                    }
                    Err(e) => last = e,
                }
            }
            if let Some((_, end)) = txn {
                let _ = exec::answer(s, end).await;
            }
            got.ok_or(last)
        };
        let a = or_cancel(call.ct, work).await??;
        let p = plan::parse(engine, false, &a)?;
        Ok(render_plan(&p))
    }
}

/// Reports Idle when the work ends, however it ends.
struct Busy {
    id: u64,
    host: Arc<dyn Host>,
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.host.event(Event::Idle { id: self.id });
    }
}

/// Columns as TSV, then PK / indexes / FKs one per line.
pub fn render_structure(
    schema: &str,
    table: &str,
    st: &db_core::structure::TableStructure,
) -> String {
    let mut out = format!("{schema}.{table}\ncolumn\ttype\tnull\tdefault\textra\n");
    for col in &st.columns {
        let mut extra = Vec::new();
        if col.primary_key {
            extra.push("pk".to_string());
        }
        if col.auto_increment {
            extra.push("auto".into());
        }
        if let Some(cm) = col.comment.as_deref().filter(|c| !c.is_empty()) {
            extra.push(format!("-- {cm}"));
        }
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            col.name,
            col.type_name,
            if col.nullable { "yes" } else { "no" },
            col.default.as_deref().map(format::cut).unwrap_or_default(),
            format::cut(&extra.join(" "))
        ));
    }
    let pk: Vec<&str> = st
        .indexes
        .iter()
        .find(|i| i.primary)
        .map(|i| i.columns.iter().map(String::as_str).collect())
        .unwrap_or_else(|| {
            st.columns
                .iter()
                .filter(|c| c.primary_key)
                .map(|c| c.name.as_str())
                .collect()
        });
    if !pk.is_empty() {
        out.push_str(&format!("pk: ({})\n", pk.join(", ")));
    }
    for i in st.indexes.iter().filter(|i| !i.primary) {
        out.push_str(&format!(
            "index {}{}: ({})\n",
            i.name,
            if i.unique { " unique" } else { "" },
            i.columns.join(", ")
        ));
    }
    for f in &st.foreign_keys {
        out.push_str(&format!(
            "fk {}: ({}) -> {}.{}({}){}{}\n",
            f.name,
            f.columns.join(", "),
            f.ref_schema,
            f.ref_table,
            f.ref_columns.join(", "),
            rule(" on delete", &f.on_delete),
            rule(" on update", &f.on_update),
        ));
    }
    for f in &st.referenced_by {
        out.push_str(&format!(
            "referenced by {}.{}({}) via {}\n",
            f.schema,
            f.table,
            f.columns.join(", "),
            f.name
        ));
    }
    out.trim_end().to_string()
}

/// ` on delete CASCADE`, or nothing for the default rule.
fn rule(label: &str, r: &str) -> String {
    let r = r.trim();
    if r.is_empty() || r.eq_ignore_ascii_case("NO ACTION") || r.eq_ignore_ascii_case("RESTRICT") {
        String::new()
    } else {
        format!("{label} {r}")
    }
}

/// One plan step per line, indented by depth, with its share when known.
pub fn render_plan(p: &locus_core::plan::Plan) -> String {
    let mut out = String::new();
    if let Some(c) = p.total_cost {
        out.push_str(&format!("total cost {c:.2}\n"));
    }
    for n in &p.nodes {
        out.push_str(&"  ".repeat(n.depth));
        out.push_str(&n.op);
        if !n.detail.is_empty() {
            out.push(' ');
            out.push_str(&format::cut(&n.detail));
        }
        if let Some(s) = n.share {
            out.push_str(&format!(" [{s:.0}%]"));
        }
        out.push('\n');
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use db_core::structure::{StructColumn, StructForeignKey, StructIndex, TableStructure};

    #[test]
    fn structure_is_compact() {
        let st = TableStructure {
            columns: vec![
                StructColumn {
                    name: "id".into(),
                    type_name: "int(11)".into(),
                    nullable: false,
                    default: None,
                    primary_key: true,
                    auto_increment: true,
                    comment: None,
                },
                StructColumn {
                    name: "user_id".into(),
                    type_name: "int(11)".into(),
                    nullable: true,
                    default: Some("NULL".into()),
                    primary_key: false,
                    auto_increment: false,
                    comment: Some("owner".into()),
                },
            ],
            indexes: vec![
                StructIndex {
                    name: "PRIMARY".into(),
                    columns: vec!["id".into()],
                    unique: true,
                    primary: true,
                    kind: "BTREE".into(),
                },
                StructIndex {
                    name: "ix_user".into(),
                    columns: vec!["user_id".into()],
                    unique: false,
                    primary: false,
                    kind: "BTREE".into(),
                },
            ],
            foreign_keys: vec![StructForeignKey {
                name: "fk_user".into(),
                schema: "app".into(),
                table: "orders".into(),
                columns: vec!["user_id".into()],
                ref_schema: "app".into(),
                ref_table: "users".into(),
                ref_columns: vec!["id".into()],
                on_delete: "CASCADE".into(),
                on_update: "NO ACTION".into(),
            }],
            ..Default::default()
        };
        assert_eq!(
            render_structure("app", "orders", &st),
            "app.orders\n\
             column\ttype\tnull\tdefault\textra\n\
             id\tint(11)\tno\t\tpk auto\n\
             user_id\tint(11)\tyes\tNULL\t-- owner\n\
             pk: (id)\n\
             index ix_user: (user_id)\n\
             fk fk_user: (user_id) -> app.users(id) on delete CASCADE"
        );
    }

    #[test]
    fn plan_lines_are_indented() {
        use locus_core::plan::{Basis, Plan, PlanNode};
        let p = Plan {
            nodes: vec![
                PlanNode {
                    depth: 0,
                    op: "Nested loop".into(),
                    detail: String::new(),
                    share: Some(100.0),
                },
                PlanNode {
                    depth: 1,
                    op: "Full scan".into(),
                    detail: "t".into(),
                    share: Some(62.4),
                },
            ],
            basis: Basis::Cost,
            total_cost: Some(12.5),
            exec_ms: None,
            plan_ms: None,
            analyze: false,
        };
        assert_eq!(
            render_plan(&p),
            "total cost 12.50\nNested loop [100%]\n  Full scan t [62%]"
        );
    }
}

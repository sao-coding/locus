//! SQL autocomplete for the editor: keywords, plus schemas / tables / views /
//! columns from `Driver::catalog()`, cached per connection and loaded lazily.
//! `alias.` resolves aliases in simple FROM / JOIN clauses of the statement
//! around the cursor.
//!
//! Typing is never blocked: the provider only looks at a window of text
//! around the cursor (not the whole buffer), answers from the cache, and
//! anything missing is fetched inside the returned task (the catalog runs on
//! the driver's runtime).

use anyhow::Result;
use db_core::{ColumnInfo, Driver as _, ObjectKind, SchemaObject};
use gpui_kit::component::input::{CompletionProvider, Rope, RopeExt as _};
use gpui_kit::*;
use locus_core::driver::AnyDriver;
use locus_core::joinsug::{self, Ctx, Rel};
use locus_core::saved::{self, Snippet};
use locus_core::sqlrefs::{is_word, table_refs, unquote};
use lsp_types::{CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse};
use parking_lot::Mutex;
use rust_i18n::t;
use std::collections::HashMap;
use std::sync::Arc;

const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "IS",
    "IN",
    "LIKE",
    "BETWEEN",
    "EXISTS",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "CROSS",
    "ON",
    "USING",
    "GROUP BY",
    "ORDER BY",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "AS",
    "DISTINCT",
    "UNION",
    "ALL",
    "INSERT INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE FROM",
    "CREATE TABLE",
    "ALTER TABLE",
    "DROP TABLE",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "ASC",
    "DESC",
    "COUNT",
    "SUM",
    "AVG",
    "MIN",
    "MAX",
    "COALESCE",
    "IFNULL",
    "CAST",
    "CONCAT",
    "NOW",
    "SHOW",
    "DESCRIBE",
    "EXPLAIN",
    "WITH",
    "START TRANSACTION",
    "COMMIT",
    "ROLLBACK",
    "PRIMARY KEY",
    "FOREIGN KEY",
    "REFERENCES",
    "INDEX",
    "DEFAULT",
    "AUTO_INCREMENT",
    "TRUE",
    "FALSE",
];

/// Bytes of text before / after the cursor the provider looks at.
const WINDOW_BEFORE: usize = 16 * 1024;
const WINDOW_AFTER: usize = 4 * 1024;

#[derive(Default)]
pub struct ConnCache {
    pub schemas: Option<Vec<String>>,
    pub current: Option<Option<String>>,
    pub objects: HashMap<String, Vec<SchemaObject>>,
    pub columns: HashMap<(String, String), Vec<ColumnInfo>>,
    /// Foreign keys per schema (for JOIN suggestions).
    pub fks: HashMap<String, Vec<db_core::ForeignKey>>,
    /// Identifier quote character of the driver.
    pub quote: Option<char>,
}

/// Built-in plus user snippets, shared by every editor. The Saved page
/// replaces the user part when it changes.
fn snippet_store() -> &'static Mutex<Vec<Snippet>> {
    static S: std::sync::OnceLock<Mutex<Vec<Snippet>>> = std::sync::OnceLock::new();
    S.get_or_init(|| Mutex::new(saved::all_snippets(&saved::load_snippets())))
}

pub fn set_user_snippets(user: &[Snippet]) {
    *snippet_store().lock() = saved::all_snippets(user);
}

/// Catalog cache of every connection, shared by all tabs. Cleared by the
/// object tree's refresh.
pub type Cache = Arc<Mutex<HashMap<String, ConnCache>>>;

/// What the provider of one tab completes against (set by the tab).
#[derive(Clone, Default)]
pub struct Target {
    pub conn_id: Option<String>,
    /// The shared connection slot; empty until the connection is opened.
    pub slot: Option<locus_core::pool::Slot>,
}

pub struct SqlCompletions {
    pub cache: Cache,
    pub target: Arc<Mutex<Target>>,
}

fn item(label: String, kind: CompletionItemKind, detail: Option<String>) -> CompletionItem {
    CompletionItem {
        label,
        kind: Some(kind),
        detail,
        ..Default::default()
    }
}

impl SqlCompletions {
    /// Builds the item list for `prefix` (after `qualifier.` if any) from the
    /// cache, and says which catalog pieces are missing.
    fn answer(
        &self,
        conn: &str,
        qualifier: Option<&str>,
        prefix: &str,
        stmt: &str,
        before: &str,
    ) -> (Vec<CompletionItem>, Vec<Need>) {
        let cache = self.cache.lock();
        let c = cache.get(conn);
        let mut items = Vec::new();
        let mut needs = Vec::new();
        let p = prefix.to_lowercase();
        let ok = |s: &str| p.is_empty() || s.to_lowercase().starts_with(&p);
        let current = c.and_then(|c| c.current.clone()).flatten();
        if c.is_none_or(|c| c.current.is_none()) {
            needs.push(Need::Current);
        }
        let refs = table_refs(stmt);
        let resolve = |name: &str| -> Option<(String, String)> {
            refs.iter()
                .find(|(a, _, t)| a.eq_ignore_ascii_case(name) || t.eq_ignore_ascii_case(name))
                .and_then(|(_, s, t)| {
                    s.clone()
                        .or_else(|| current.clone())
                        .map(|s| (s, t.clone()))
                })
        };
        match qualifier {
            Some(q) => {
                // alias / table -> its columns; schema -> its tables.
                if let Some(key) = resolve(q) {
                    match c.and_then(|c| c.columns.get(&key)) {
                        Some(cols) => items.extend(cols.iter().filter(|x| ok(&x.name)).map(|x| {
                            item(
                                x.name.clone(),
                                CompletionItemKind::FIELD,
                                Some(x.type_name.clone()),
                            )
                        })),
                        None => needs.push(Need::Columns(key.0, key.1)),
                    }
                } else {
                    let s = unquote(q);
                    match c.and_then(|c| c.objects.get(&s)) {
                        Some(objs) => {
                            items.extend(objs.iter().filter(|o| ok(&o.name)).map(obj_item))
                        }
                        None => needs.push(Need::Objects(s)),
                    }
                }
            }
            None => {
                // After JOIN / ON: tables related by a foreign key, first.
                if let Some(ctx) = joinsug::context(before, prefix) {
                    let head = before.strip_suffix(prefix).unwrap_or(before);
                    let jrefs = table_refs(head);
                    let mut schemas: Vec<String> =
                        jrefs.iter().filter_map(|(_, s, _)| s.clone()).collect();
                    schemas.extend(current.clone());
                    schemas.sort();
                    schemas.dedup();
                    let mut rels: Vec<Rel> = Vec::new();
                    for s in schemas {
                        match c.and_then(|c| c.fks.get(&s)) {
                            Some(v) => rels.extend(v.iter().cloned().map(|fk| Rel {
                                schema: s.clone(),
                                fk,
                            })),
                            None => needs.push(Need::Fks(s)),
                        }
                    }
                    let q = c.and_then(|c| c.quote).unwrap_or('"');
                    match ctx {
                        Ctx::AfterJoin => {
                            for s in joinsug::suggest_tables(&rels, &jrefs, current.as_deref(), q) {
                                let text = s.insert_with_on(q);
                                if ok(&text) {
                                    items.push(item(
                                        text,
                                        CompletionItemKind::CLASS,
                                        Some(format!("FK {}", s.key)),
                                    ));
                                }
                            }
                        }
                        Ctx::AfterOn => {
                            for on in joinsug::suggest_on(&rels, &jrefs, current.as_deref(), q) {
                                if ok(&on) {
                                    items.push(item(
                                        on,
                                        CompletionItemKind::SNIPPET,
                                        Some(t!("sq.fk_on").into_owned()),
                                    ));
                                }
                            }
                        }
                    }
                }
                for k in KEYWORDS {
                    if ok(k) {
                        items.push(item((*k).to_string(), CompletionItemKind::KEYWORD, None));
                    }
                }
                match c.and_then(|c| c.schemas.as_ref()) {
                    Some(s) => items.extend(
                        s.iter()
                            .filter(|x| ok(x))
                            .map(|x| item(x.clone(), CompletionItemKind::MODULE, None)),
                    ),
                    None => needs.push(Need::Schemas),
                }
                if let Some(cur) = &current {
                    match c.and_then(|c| c.objects.get(cur)) {
                        Some(objs) => {
                            items.extend(objs.iter().filter(|o| ok(&o.name)).map(obj_item))
                        }
                        None => needs.push(Need::Objects(cur.clone())),
                    }
                }
                // Columns of the tables named in this statement.
                for (alias, _, _) in &refs {
                    if let Some(key) = resolve(alias) {
                        match c.and_then(|c| c.columns.get(&key)) {
                            Some(cols) => {
                                items.extend(cols.iter().filter(|x| ok(&x.name)).map(|x| {
                                    item(
                                        x.name.clone(),
                                        CompletionItemKind::FIELD,
                                        Some(format!("{} · {}", key.1, x.type_name)),
                                    )
                                }))
                            }
                            None => needs.push(Need::Columns(key.0, key.1)),
                        }
                    }
                }
            }
        }
        items.dedup_by(|a, b| a.label == b.label && a.kind == b.kind);
        items.truncate(200);
        (items, needs)
    }
}

fn obj_item(o: &SchemaObject) -> CompletionItem {
    let kind = if o.kind == ObjectKind::View {
        CompletionItemKind::INTERFACE
    } else {
        CompletionItemKind::CLASS
    };
    item(o.name.clone(), kind, None)
}

#[derive(Clone, Debug)]
enum Need {
    Current,
    Schemas,
    Objects(String),
    Columns(String, String),
    Fks(String),
}

/// Fetches what `needs` lists into the cache.
/// Logs a failed catalog fetch: connection id and error kind only (the message can echo SQL).
fn warn_catalog(conn: &str, what: &str, e: &db_core::DbError) {
    log::warn!(
        "completion catalog {what} failed on connection {conn}: {:?} code={:?}",
        e.kind,
        e.code
    );
}

async fn fill(cache: Cache, conn: String, driver: Arc<AnyDriver>, needs: Vec<Need>) {
    let Some(cat) = driver.catalog() else { return };
    for n in needs {
        match n {
            Need::Current => {
                if let Ok(cur) = cat
                    .current_schema()
                    .await
                    .inspect_err(|e| warn_catalog(&conn, "current_schema", e))
                {
                    cache.lock().entry(conn.clone()).or_default().current = Some(cur);
                }
            }
            Need::Schemas => {
                if let Ok(s) = cat
                    .schemas()
                    .await
                    .inspect_err(|e| warn_catalog(&conn, "schemas", e))
                {
                    cache.lock().entry(conn.clone()).or_default().schemas = Some(s);
                }
            }
            Need::Objects(s) => {
                if let Ok(o) = cat
                    .objects(&s)
                    .await
                    .inspect_err(|e| warn_catalog(&conn, "objects", e))
                {
                    cache
                        .lock()
                        .entry(conn.clone())
                        .or_default()
                        .objects
                        .insert(s, o);
                }
            }
            Need::Fks(s) => {
                let q = driver
                    .quote_identifier("a")
                    .chars()
                    .next()
                    .filter(|c| !c.is_alphanumeric());
                let fks = cat
                    .foreign_keys(&s)
                    .await
                    .inspect_err(|e| warn_catalog(&conn, "foreign_keys", e))
                    .unwrap_or_default();
                let mut g = cache.lock();
                let e = g.entry(conn.clone()).or_default();
                e.fks.insert(s, fks);
                e.quote = q.or(Some('"'));
            }
            Need::Columns(s, t) => {
                if let Ok(c) = cat
                    .columns(&s, &t)
                    .await
                    .inspect_err(|e| warn_catalog(&conn, "columns", e))
                {
                    cache
                        .lock()
                        .entry(conn.clone())
                        .or_default()
                        .columns
                        .insert((s, t), c);
                }
            }
        }
    }
}

impl CompletionProvider for SqlCompletions {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let target = self.target.lock().clone();
        let driver = target
            .slot
            .as_ref()
            .and_then(|s| s.try_lock().ok().and_then(|g| g.clone()));
        // Only a window around the cursor, on char boundaries.
        let floor =
            |x: usize| text.char_index_to_offset(text.offset_to_char_index(x.min(text.len())));
        let start = floor(offset.saturating_sub(WINDOW_BEFORE));
        let end = floor(offset + WINDOW_AFTER);
        let window = text.slice(start..end).to_string();
        let cur = offset - start;
        let range = db_core::sql::statement_at(&window, cur);
        let stmt = range
            .clone()
            .map(|r| window[r].to_string())
            .unwrap_or_default();
        let before = &window[..cur];
        let before_stmt = range
            .map(|r| &window[r.start.min(cur)..cur])
            .unwrap_or(before)
            .to_string();
        let prefix: String = before
            .chars()
            .rev()
            .take_while(|c| is_word(*c))
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let rest = &before[..before.len() - prefix.len()];
        let qualifier = rest.strip_suffix('.').map(|r| {
            r.chars()
                .rev()
                .take_while(|c| is_word(*c) || *c == '`')
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<String>()
        });
        // Snippets by prefix (not after `alias.`), replacing the typed word.
        let snippets: Vec<CompletionItem> = if qualifier.is_some() {
            Vec::new()
        } else {
            let all = snippet_store().lock().clone();
            let range = lsp_types::Range {
                start: text.offset_to_position(offset - prefix.len()),
                end: text.offset_to_position(offset),
            };
            saved::matching(&all, &prefix)
                .into_iter()
                .map(|s| CompletionItem {
                    label: s.prefix.clone(),
                    kind: Some(CompletionItemKind::SNIPPET),
                    detail: Some(s.description.clone()),
                    text_edit: Some(lsp_types::CompletionTextEdit::Edit(lsp_types::TextEdit {
                        range,
                        new_text: saved::expand(&s.body).0,
                    })),
                    ..Default::default()
                })
                .collect()
        };
        let with_snippets = move |mut items: Vec<CompletionItem>| {
            // Foreign-key suggestions after JOIN / ON stay first: that is what was asked for.
            let fk_on = t!("sq.fk_on").into_owned();
            let lead = items
                .iter()
                .take_while(|i| {
                    i.detail
                        .as_deref()
                        .is_some_and(|d| d.starts_with("FK ") || d == fk_on)
                })
                .count();
            let mut v: Vec<CompletionItem> = items.drain(..lead).collect();
            v.extend(snippets.iter().cloned());
            v.append(&mut items);
            v
        };
        let Some(conn) = target.conn_id else {
            return Task::ready(Ok(CompletionResponse::Array(with_snippets(Vec::new()))));
        };
        let (items, needs) = self.answer(&conn, qualifier.as_deref(), &prefix, &stmt, &before_stmt);
        let (Some(driver), false) = (driver, needs.is_empty()) else {
            return Task::ready(Ok(CompletionResponse::Array(with_snippets(items))));
        };
        // Missing catalog pieces are fetched in the task, never on the UI thread.
        let cache = self.cache.clone();
        let me = SqlCompletions {
            cache: cache.clone(),
            target: self.target.clone(),
        };
        cx.spawn(async move |_| {
            fill(cache, conn.clone(), driver, needs).await;
            let (items, _) = me.answer(&conn, qualifier.as_deref(), &prefix, &stmt, &before_stmt);
            Ok(CompletionResponse::Array(with_snippets(items)))
        })
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, _: &mut App) -> bool {
        new_text
            .chars()
            .last()
            .is_some_and(|c| is_word(c) || c == '.')
    }
}

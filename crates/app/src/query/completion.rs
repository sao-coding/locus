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

/// The tab stops of the snippet whose expanded text is exactly `inserted`
/// (the editor calls this for each insertion to know a snippet was accepted).
pub fn snippet_stops_for(inserted: &str) -> Option<Vec<(u32, std::ops::Range<usize>)>> {
    if inserted.len() < 2 {
        return None;
    }
    snippet_store().lock().iter().find_map(|s| {
        let (text, stops) = saved::expand_numbered(&s.body);
        (text == inserted && stops.iter().any(|(n, _)| *n > 0)).then_some(stops)
    })
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
        let expect = expect_of(before, prefix);
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
                    if (!p.is_empty() || expect == Expect::Any) && ok(k) {
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
        // What the position asks for goes first (stable: FK suggestions already lead).
        if expect != Expect::Any {
            items.sort_by_key(|i| rank(expect, i));
        }
        items.dedup_by(|a, b| a.label == b.label && a.kind == b.kind);
        items.truncate(200);
        (items, needs)
    }
}

fn is_quote(c: char) -> bool {
    matches!(c, '"' | '`')
}

/// The text an accepted completion replaces, as byte offsets in the text it was
/// computed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span {
    /// Start of the replaced range: the opening quote if there is one, else the word.
    start: usize,
    /// Start of the word (after the opening quote); the typed prefix is `word_start..cursor`.
    word_start: usize,
    /// End of the word (rest of the identifier right of the cursor).
    word_end: usize,
    /// End of the replaced range: the closing quote too, if it follows the word.
    end: usize,
    /// The identifier quote the user opened (`"` or a backtick).
    quote: Option<char>,
}

/// The word being completed around `cursor` (a char boundary in `text`): from the
/// start of the identifier to the cursor, and on to the end of the identifier if
/// the cursor is mid-word. A quote just before the word that opens a quoted name
/// (an even number of the same quote earlier on the line) belongs to the range,
/// and so does the closing quote right after it.
fn replace_span(text: &str, cursor: usize) -> Span {
    let before = &text[..cursor];
    let word_start = before
        .char_indices()
        .rev()
        .take_while(|(_, c)| is_word(*c))
        .last()
        .map_or(cursor, |(i, _)| i);
    let word_end = cursor
        + text[cursor..]
            .char_indices()
            .find(|(_, c)| !is_word(*c))
            .map_or(text.len() - cursor, |(i, _)| i);
    let quote = before[..word_start].chars().next_back().filter(|q| {
        let line = &before[..word_start - q.len_utf8()];
        let line = line.rsplit('\n').next().unwrap_or("");
        is_quote(*q) && line.matches(*q).count() % 2 == 0
    });
    match quote {
        Some(q) => Span {
            start: word_start - q.len_utf8(),
            word_start,
            word_end,
            end: word_end + text[word_end..].starts_with(q) as usize * q.len_utf8(),
            quote,
        },
        None => Span {
            start: word_start,
            word_start,
            word_end,
            end: word_end,
            quote,
        },
    }
}

/// The text inserted for `label`: wrapped in the user's quote when `wrap` is set.
fn replacement(label: &str, wrap: Option<char>) -> String {
    match wrap {
        Some(q) => format!("{q}{label}{q}"),
        None => label.to_string(),
    }
}

/// Whether an item is a plain name that takes the quotes the user opened (not a
/// keyword, a snippet, or an FK suggestion that brings its own quotes and ON).
fn wraps_in_quote(i: &CompletionItem, fk_on: &str) -> bool {
    !matches!(
        i.kind,
        Some(CompletionItemKind::KEYWORD | CompletionItemKind::SNIPPET)
    ) && !i
        .detail
        .as_deref()
        .is_some_and(|d| d.starts_with("FK ") || d == fk_on)
        && !i.label.contains(is_quote)
}

/// What the cursor position expects when nothing is typed yet.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Expect {
    Any,
    /// After JOIN / FROM / UPDATE / INTO: a table.
    Tables,
    /// After ON: a column.
    Columns,
}

fn expect_of(before: &str, prefix: &str) -> Expect {
    let head = before.strip_suffix(prefix).unwrap_or(before);
    if !head.ends_with(|c: char| c.is_whitespace()) {
        return Expect::Any;
    }
    match head
        .split_whitespace()
        .next_back()
        .map(str::to_ascii_uppercase)
        .as_deref()
    {
        Some("JOIN" | "FROM" | "UPDATE" | "INTO") => Expect::Tables,
        Some("ON") => Expect::Columns,
        _ => Expect::Any,
    }
}

/// Sort key: FK suggestions, then what `expect` asks for, then the rest.
fn rank(expect: Expect, i: &CompletionItem) -> u8 {
    if i.detail
        .as_deref()
        .is_some_and(|d| d.starts_with("FK ") || d == t!("sq.fk_on").as_ref())
    {
        return 0;
    }
    match (expect, i.kind) {
        (Expect::Tables, Some(CompletionItemKind::CLASS | CompletionItemKind::INTERFACE)) => 1,
        (Expect::Columns, Some(CompletionItemKind::FIELD)) => 1,
        (_, Some(CompletionItemKind::KEYWORD)) => 3,
        _ => 2,
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
        context: CompletionContext,
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
        let span = replace_span(&window, cur);
        let prefix = window[span.word_start..cur].to_string();
        let rest = &window[..span.start];
        // The statement up to the cursor without the opening quote, so the position
        // logic below sees `prefix` where the user typed `"prefix`.
        let before_stmt = {
            let b = range
                .map(|r| &window[r.start.min(cur)..cur])
                .unwrap_or(before);
            match span.quote {
                Some(q) if b.len() > prefix.len() && b[..b.len() - prefix.len()].ends_with(q) => {
                    format!("{}{prefix}", &b[..b.len() - prefix.len() - q.len_utf8()])
                }
                _ => b.to_string(),
            }
        };
        // A space typed on its own opens the menu only where a table or condition is
        // expected (after JOIN / ON); a manual invoke (Ctrl+Space) always does.
        if context.trigger_kind == lsp_types::CompletionTriggerKind::TRIGGER_CHARACTER
            && prefix.is_empty()
            && before.ends_with(|c: char| c.is_whitespace())
            && joinsug::context(&before_stmt, "").is_none()
        {
            return Task::ready(Ok(CompletionResponse::Array(Vec::new())));
        }
        let qualifier = rest.strip_suffix('.').map(|r| {
            r.chars()
                .rev()
                .take_while(|c| is_word(*c) || is_quote(*c))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<String>()
        });
        let qualifier = qualifier.map(|q| unquote(&q));
        // What an accepted item replaces (absolute offsets in the document).
        let at = |rel: usize| text.offset_to_position(start + rel);
        let edit_range = lsp_types::Range {
            start: at(span.start),
            end: at(span.end),
        };
        let quote = span.quote;
        // Snippets by prefix (not after `alias.` or inside quotes), replacing the typed word.
        let snippets: Vec<CompletionItem> = if qualifier.is_some() || quote.is_some() {
            Vec::new()
        } else {
            let all = snippet_store().lock().clone();
            let range = lsp_types::Range {
                start: at(span.word_start),
                end: at(span.word_end),
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
            for i in &mut items {
                if i.text_edit.is_none() {
                    let wrap = quote.filter(|_| wraps_in_quote(i, &fk_on));
                    i.text_edit = Some(lsp_types::CompletionTextEdit::Edit(lsp_types::TextEdit {
                        range: edit_range,
                        new_text: replacement(&i.label, wrap),
                    }));
                }
            }
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
            .is_some_and(|c| is_word(c) || c == '.' || c.is_whitespace())
    }
}

#[cfg(test)]
mod expect_tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn position_decides_what_comes_first() {
        assert_eq!(expect_of("SELECT * FROM a JOIN ", ""), Expect::Tables);
        assert_eq!(expect_of("select * from ", ""), Expect::Tables);
        assert_eq!(expect_of("UPDATE ", ""), Expect::Tables);
        assert_eq!(expect_of("INSERT INTO ", ""), Expect::Tables);
        assert_eq!(expect_of("SELECT * FROM a JOIN b ON ", ""), Expect::Columns);
        assert_eq!(expect_of("SELECT * FROM a JOIN b", "b"), Expect::Tables);
        assert_eq!(expect_of("SELECT ", ""), Expect::Any);
        assert_eq!(expect_of("SELECT * FROM a", ""), Expect::Any);
    }

    #[::core::prelude::v1::test]
    fn tables_before_schemas_before_keywords() {
        let mk = |l: &str, k| item(l.to_string(), k, None);
        let mut v = [
            mk("SELECT", CompletionItemKind::KEYWORD),
            mk("public", CompletionItemKind::MODULE),
            mk("orders", CompletionItemKind::CLASS),
            mk("v_sales", CompletionItemKind::INTERFACE),
        ];
        v.sort_by_key(|i| rank(Expect::Tables, i));
        let names: Vec<_> = v.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(names, ["orders", "v_sales", "public", "SELECT"]);
    }
}

#[cfg(test)]
mod accept_tests {
    use super::*;

    /// Accepts `label` at the `|` in `doc` the way the editor applies the item's
    /// edit: returns the new text with `|` where the cursor ends up.
    fn accept(doc: &str, label: &str) -> String {
        accept_as(doc, label, true)
    }

    fn accept_as(doc: &str, label: &str, ident: bool) -> String {
        let cursor = doc.find('|').expect("a | marks the cursor");
        let text = doc.replacen('|', "", 1);
        let s = replace_span(&text, cursor);
        let new = replacement(label, s.quote.filter(|_| ident));
        format!("{}{new}|{}", &text[..s.start], &text[s.end..])
    }

    #[::core::prelude::v1::test]
    fn replaces_the_typed_prefix_not_appends() {
        assert_eq!(
            accept("SELECT * FROM stu|", "student"),
            "SELECT * FROM student|"
        );
        assert_eq!(accept("stu|", "student"), "student|");
        assert_eq!(accept("|", "student"), "student|");
        assert_eq!(accept("SELECT |", "student"), "SELECT student|");
    }

    #[::core::prelude::v1::test]
    fn mid_word_takes_the_rest_of_the_identifier() {
        assert_eq!(accept("FROM st|ud WHERE", "student"), "FROM student| WHERE");
        assert_eq!(accept("FROM stu|dent2", "student"), "FROM student|");
    }

    #[::core::prelude::v1::test]
    fn mixed_case_prefix_is_replaced() {
        assert_eq!(accept("FROM StU|", "student"), "FROM student|");
    }

    #[::core::prelude::v1::test]
    fn quoted_identifiers_keep_their_quotes() {
        assert_eq!(accept("FROM \"boo|", "booking"), "FROM \"booking\"|");
        assert_eq!(accept("FROM \"boo|\"", "booking"), "FROM \"booking\"|");
        assert_eq!(accept("FROM \"bo|ok\"", "booking"), "FROM \"booking\"|");
        assert_eq!(accept("FROM `boo|", "booking"), "FROM `booking`|");
        assert_eq!(accept("FROM `boo|`", "booking"), "FROM `booking`|");
        assert_eq!(accept("FROM \"|", "my table"), "FROM \"my table\"|");
    }

    #[::core::prelude::v1::test]
    fn closing_quote_is_not_an_opening_one() {
        // `"a"` is complete; the cursor after it is not inside a quote.
        assert_eq!(accept("FROM \"a\" b|", "bar"), "FROM \"a\" bar|");
        assert_eq!(accept("SELECT \"a\"|", "x"), "SELECT \"a\"x|");
        // Quote count is per line.
        assert_eq!(
            accept("SELECT \"a\n\"bo|", "booking"),
            "SELECT \"a\n\"booking\"|"
        );
    }

    #[::core::prelude::v1::test]
    fn qualified_names_replace_only_the_last_part() {
        assert_eq!(accept("booking.bo|", "booking_id"), "booking.booking_id|");
        assert_eq!(
            accept("\"booking\".\"bo|", "booking_id"),
            "\"booking\".\"booking_id\"|"
        );
        assert_eq!(accept("`db`.`bo|`", "booking"), "`db`.`booking`|");
        assert_eq!(accept("s.na|", "name"), "s.name|");
        assert_eq!(accept("s.|", "name"), "s.name|");
    }

    #[::core::prelude::v1::test]
    fn keywords_and_fk_items_are_not_quoted() {
        let kw = item("GROUP BY".into(), CompletionItemKind::KEYWORD, None);
        assert!(!wraps_in_quote(&kw, "fk"));
        let fk = item(
            "\"a\" ON x".into(),
            CompletionItemKind::CLASS,
            Some("FK a".into()),
        );
        assert!(!wraps_in_quote(&fk, "fk"));
        let col = item("id".into(), CompletionItemKind::FIELD, None);
        assert!(wraps_in_quote(&col, "fk"));
        assert_eq!(accept_as("FROM \"gr|", "GROUP BY", false), "FROM GROUP BY|");
    }

    #[::core::prelude::v1::test]
    fn multibyte_text_stays_on_char_boundaries() {
        assert_eq!(
            accept("SELECT '學生' FROM 學|", "學生表"),
            "SELECT '學生' FROM 學生表|"
        );
        assert_eq!(accept("學生 FROM st|", "student"), "學生 FROM student|");
        assert_eq!(accept("FROM \"學|\"", "學生表"), "FROM \"學生表\"|");
        assert_eq!(accept("FROM 學|生", "學生表"), "FROM 學生表|");
        assert_eq!(accept("é.stu|", "student"), "é.student|");
    }

    #[::core::prelude::v1::test]
    fn end_of_document_and_blank_cursor() {
        assert_eq!(accept("SELECT\nstu|", "student"), "SELECT\nstudent|");
        assert_eq!(accept("a\n|", "SELECT"), "a\nSELECT|");
    }
}

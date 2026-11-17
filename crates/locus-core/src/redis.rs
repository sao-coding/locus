//! Redis logic without a window: connection config, command classification
//! (read / write / dangerous) for Production protection and the console,
//! the console's command-line splitter, the key namespace tree, reply
//! formatting and value views. The driver is `db_redis`; the views are
//! `crates/app/src/redis`.
use crate::store::{DbKind, SavedConnection, Tls};
use db_redis::Reply;
use std::collections::{BTreeMap, HashSet};

/// Driver config of a saved Redis connection (`database` = DB index 0-15).
pub fn config(c: &SavedConnection, password: String) -> db_redis::Config {
    debug_assert_eq!(c.kind, DbKind::Redis);
    db_redis::Config {
        host: c.host.clone(),
        port: c.port,
        user: (!c.user.is_empty()).then(|| c.user.clone()),
        password: (!password.is_empty()).then_some(password),
        db: db_index(c.database.as_deref()),
        tls: c.tls != Tls::Disabled,
        insecure: c.tls == Tls::Unverified,
    }
}

pub fn db_index(s: Option<&str>) -> u8 {
    s.and_then(|s| s.trim().parse::<u8>().ok())
        .filter(|d| *d <= 15)
        .unwrap_or(0)
}

// ---- command classification ---------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmdClass {
    Read,
    /// Changes data, config or server state. Unknown commands count as writes.
    Write,
}

const READ_CMDS: &[&str] = &[
    "GET",
    "MGET",
    "GETRANGE",
    "STRLEN",
    "EXISTS",
    "TYPE",
    "TTL",
    "PTTL",
    "EXPIRETIME",
    "PEXPIRETIME",
    "KEYS",
    "SCAN",
    "RANDOMKEY",
    "DBSIZE",
    "HGET",
    "HMGET",
    "HGETALL",
    "HKEYS",
    "HVALS",
    "HLEN",
    "HEXISTS",
    "HSTRLEN",
    "HSCAN",
    "LRANGE",
    "LLEN",
    "LINDEX",
    "LPOS",
    "SMEMBERS",
    "SCARD",
    "SISMEMBER",
    "SMISMEMBER",
    "SRANDMEMBER",
    "SSCAN",
    "SINTER",
    "SUNION",
    "SDIFF",
    "ZRANGE",
    "ZRANGEBYSCORE",
    "ZREVRANGE",
    "ZREVRANGEBYSCORE",
    "ZRANGEBYLEX",
    "ZCARD",
    "ZSCORE",
    "ZMSCORE",
    "ZRANK",
    "ZREVRANK",
    "ZCOUNT",
    "ZLEXCOUNT",
    "ZSCAN",
    "XRANGE",
    "XREVRANGE",
    "XLEN",
    "XREAD",
    "XINFO",
    "XPENDING",
    "PING",
    "ECHO",
    "TIME",
    "INFO",
    "LASTSAVE",
    "OBJECT",
    "MEMORY",
    "STRALGO",
    "DUMP",
    "BITCOUNT",
    "BITPOS",
    "GETBIT",
    "PFCOUNT",
    "GEOPOS",
    "GEODIST",
    "GEOHASH",
    "GEOSEARCH",
    "GEORADIUS_RO",
    "GEORADIUSBYMEMBER_RO",
    "JSON.GET",
    "JSON.MGET",
    "JSON.TYPE",
    "SELECT",
    "CLIENT",
    "COMMAND",
    "CONFIG",
    "SLOWLOG",
    "LATENCY",
    "DEBUG",
    "MODULE",
    "ACL",
    "WAIT",
    "HELLO",
    "AUTH",
    "SUBSCRIBE",
    "UNSUBSCRIBE",
    "SORT_RO",
    "LCS",
    "HRANDFIELD",
    "ZRANDMEMBER",
    "EVAL_RO",
    "EVALSHA_RO",
    "FCALL_RO",
    "TOUCH",
];

/// Subcommands that make an otherwise read-style command a write.
fn sub_is_write(cmd: &str, sub: &str) -> bool {
    match cmd {
        "CONFIG" => matches!(sub, "SET" | "REWRITE" | "RESETSTAT"),
        "CLIENT" => matches!(
            sub,
            "KILL"
                | "PAUSE"
                | "UNPAUSE"
                | "SETNAME"
                | "SETINFO"
                | "NO-EVICT"
                | "REPLY"
                | "TRACKING"
                | "UNBLOCK"
        ),
        "MEMORY" => matches!(sub, "PURGE"),
        "SLOWLOG" => matches!(sub, "RESET"),
        "LATENCY" => matches!(sub, "RESET"),
        "MODULE" => matches!(sub, "LOAD" | "UNLOAD" | "LOADEX"),
        "ACL" => matches!(sub, "SETUSER" | "DELUSER" | "LOAD" | "SAVE"),
        "DEBUG" => true,
        _ => false,
    }
}

/// Read or write. A locked Production Redis connection only sends reads.
pub fn classify(args: &[String]) -> CmdClass {
    let Some(first) = args.first() else {
        return CmdClass::Read;
    };
    let cmd = first.to_ascii_uppercase();
    if !READ_CMDS.contains(&cmd.as_str()) {
        return CmdClass::Write;
    }
    let sub = args
        .get(1)
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_default();
    if sub_is_write(&cmd, &sub) {
        CmdClass::Write
    } else {
        CmdClass::Read
    }
}

/// Why a command needs a confirm in the console (`None` = harmless enough).
pub fn danger(args: &[String]) -> Option<&'static str> {
    let cmd = args.first()?.to_ascii_uppercase();
    let sub = args
        .get(1)
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_default();
    match cmd.as_str() {
        "FLUSHALL" => Some("flushall"),
        "FLUSHDB" => Some("flushdb"),
        "SHUTDOWN" => Some("shutdown"),
        "DEBUG" => Some("debug"),
        "CONFIG" if sub_is_write("CONFIG", &sub) => Some("config_set"),
        "KEYS" => Some("keys"),
        "MIGRATE" | "REPLICAOF" | "SLAVEOF" | "FAILOVER" => Some("admin"),
        "ACL" if matches!(sub.as_str(), "SETUSER" | "DELUSER") => Some("admin"),
        "SCRIPT" if sub == "FLUSH" => Some("admin"),
        "MODULE" if sub_is_write("MODULE", &sub) => Some("admin"),
        _ => None,
    }
}

// ---- console command line -----------------------------------------------------------

/// Splits a console line like `redis-cli`: whitespace separated, `"..."` with
/// `\n \r \t \\ \" \xHH` escapes, `'...'` literal (with `\'`).
pub fn split_command(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut it = line.chars().peekable();
    loop {
        while it.peek().is_some_and(|c| c.is_whitespace()) {
            it.next();
        }
        let Some(&c) = it.peek() else { break };
        let mut cur = String::new();
        if c == '"' || c == '\'' {
            it.next();
            let mut closed = false;
            while let Some(ch) = it.next() {
                if ch == c {
                    closed = true;
                    break;
                }
                if ch == '\\' {
                    match (it.next(), c) {
                        (Some('\''), '\'') => cur.push('\''),
                        (Some(e), '\'') => {
                            cur.push('\\');
                            cur.push(e);
                        }
                        (Some('n'), _) => cur.push('\n'),
                        (Some('r'), _) => cur.push('\r'),
                        (Some('t'), _) => cur.push('\t'),
                        (Some('x'), _) => {
                            let h: String = (0..2).filter_map(|_| it.next()).collect();
                            match u8::from_str_radix(&h, 16) {
                                Ok(b) => cur.push(b as char),
                                Err(_) => return Err("bad \\x escape".into()),
                            }
                        }
                        (Some(e), _) => cur.push(e),
                        (None, _) => return Err("unbalanced quotes".into()),
                    }
                } else {
                    cur.push(ch);
                }
            }
            if !closed {
                return Err("unbalanced quotes".into());
            }
            if it.peek().is_some_and(|c| !c.is_whitespace()) {
                return Err("closing quote must be followed by a space".into());
            }
        } else {
            while let Some(&ch) = it.peek() {
                if ch.is_whitespace() {
                    break;
                }
                cur.push(ch);
                it.next();
            }
        }
        out.push(cur);
    }
    Ok(out)
}

// ---- key namespace tree -------------------------------------------------------------

/// Keys grouped by a separator (`:`): a trie, flattened into rows for the
/// browser. A segment can be a key and a folder at the same time.
#[derive(Default)]
pub struct KeyTree {
    root: Node,
    sep: String,
}

#[derive(Default)]
struct Node {
    children: BTreeMap<String, Node>,
    is_key: bool,
    /// Keys at or below this node.
    count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeRow {
    pub depth: usize,
    pub label: String,
    /// Folder: the prefix including the trailing separator. Key: the full key.
    pub path: String,
    pub is_key: bool,
    pub is_folder: bool,
    pub open: bool,
    pub count: usize,
}

impl KeyTree {
    pub fn new(sep: &str) -> Self {
        Self {
            root: Node::default(),
            sep: if sep.is_empty() {
                ":".into()
            } else {
                sep.into()
            },
        }
    }

    pub fn from_keys<'a>(keys: impl IntoIterator<Item = &'a String>, sep: &str) -> Self {
        let mut t = Self::new(sep);
        for k in keys {
            t.insert(k);
        }
        t
    }

    pub fn insert(&mut self, key: &str) {
        let mut node = &mut self.root;
        let mut path: Vec<&str> = key.split(self.sep.as_str()).collect();
        // `a::b` and a trailing separator produce empty segments: keep them as labels.
        if path.is_empty() {
            path.push("");
        }
        let mut seen_new = false;
        let n = path.len();
        let mut dup = false;
        for (i, seg) in path.iter().enumerate() {
            let child = node.children.entry(seg.to_string()).or_default();
            if i + 1 == n {
                if child.is_key {
                    dup = true;
                }
                child.is_key = true;
            }
            node = child;
            seen_new = true;
        }
        let _ = seen_new;
        if !dup {
            // Second pass for counts (the borrow above prevents doing it inline).
            let mut node = &mut self.root;
            node.count += 1;
            for seg in &path {
                node = node.children.get_mut(*seg).expect("just inserted");
                node.count += 1;
            }
        }
    }

    pub fn len(&self) -> usize {
        self.root.count
    }

    pub fn is_empty(&self) -> bool {
        self.root.count == 0
    }

    /// Rows to draw: folders first, then keys, each sorted by name; a folder's
    /// children appear while its prefix is in `expanded`.
    pub fn rows(&self, expanded: &HashSet<String>) -> Vec<TreeRow> {
        let mut out = Vec::new();
        self.walk(&self.root, "", 0, expanded, &mut out);
        out
    }

    fn walk(
        &self,
        node: &Node,
        prefix: &str,
        depth: usize,
        expanded: &HashSet<String>,
        out: &mut Vec<TreeRow>,
    ) {
        let mut names: Vec<(&String, &Node)> = node.children.iter().collect();
        // Folders (nodes with children) before plain keys.
        names.sort_by(|a, b| {
            a.1.children
                .is_empty()
                .cmp(&b.1.children.is_empty())
                .then_with(|| a.0.cmp(b.0))
        });
        for (name, child) in names {
            let full = format!("{prefix}{name}");
            let folder = !child.children.is_empty();
            if folder {
                let fpath = format!("{full}{}", self.sep);
                let open = expanded.contains(&fpath);
                out.push(TreeRow {
                    depth,
                    label: name.clone(),
                    path: fpath.clone(),
                    is_key: false,
                    is_folder: true,
                    open,
                    count: child.count,
                });
                if open {
                    if child.is_key {
                        out.push(TreeRow {
                            depth: depth + 1,
                            label: name.clone(),
                            path: full.clone(),
                            is_key: true,
                            is_folder: false,
                            open: false,
                            count: 1,
                        });
                    }
                    self.walk(child, &fpath, depth + 1, expanded, out);
                }
            } else {
                out.push(TreeRow {
                    depth,
                    label: name.clone(),
                    path: full,
                    is_key: true,
                    is_folder: false,
                    open: false,
                    count: 1,
                });
            }
        }
    }
}

// ---- reply formatting -----------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Str,
    Int,
    Nil,
    Status,
    Error,
    /// `(array)` / `(map)` / `(set)` header.
    Container,
    Index,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplyLine {
    pub depth: usize,
    /// `1)` index / map key label, empty for a top-level scalar.
    pub label: String,
    pub text: String,
    pub kind: LineKind,
}

/// A reply as indented lines, capped at `max_lines` (a trailing `... N more` line says so).
pub fn reply_lines(r: &Reply, max_lines: usize) -> Vec<ReplyLine> {
    let mut out = Vec::new();
    let mut total = 0usize;
    walk_reply(r, 0, String::new(), max_lines, &mut out, &mut total);
    if total > out.len() {
        out.push(ReplyLine {
            depth: 0,
            label: String::new(),
            text: format!("... {} more", total - out.len()),
            kind: LineKind::Nil,
        });
    }
    out
}

fn push(out: &mut Vec<ReplyLine>, total: &mut usize, max: usize, l: ReplyLine) {
    *total += 1;
    if out.len() < max {
        out.push(l);
    }
}

fn walk_reply(
    r: &Reply,
    depth: usize,
    label: String,
    max: usize,
    out: &mut Vec<ReplyLine>,
    total: &mut usize,
) {
    let line = |text: String, kind: LineKind, out: &mut Vec<ReplyLine>, total: &mut usize| {
        push(
            out,
            total,
            max,
            ReplyLine {
                depth,
                label: label.clone(),
                text,
                kind,
            },
        )
    };
    match r {
        Reply::Nil => line("(nil)".into(), LineKind::Nil, out, total),
        Reply::Int(i) => line(format!("(integer) {i}"), LineKind::Int, out, total),
        Reply::Double(d) => line(format!("(double) {d}"), LineKind::Int, out, total),
        Reply::Bool(b) => line(format!("(boolean) {b}"), LineKind::Int, out, total),
        Reply::Status(s) => line(s.clone(), LineKind::Status, out, total),
        Reply::Error(e) => line(format!("(error) {e}"), LineKind::Error, out, total),
        Reply::Bulk(b) => line(display_bytes(b), LineKind::Str, out, total),
        Reply::Array(a) | Reply::Set(a) => {
            let name = if matches!(r, Reply::Set(_)) {
                "set"
            } else {
                "array"
            };
            if a.is_empty() {
                line(format!("(empty {name})"), LineKind::Container, out, total);
            } else {
                line(
                    format!("({name}, {} items)", a.len()),
                    LineKind::Container,
                    out,
                    total,
                );
                for (i, x) in a.iter().enumerate() {
                    if *total >= max.saturating_mul(2).max(max + 1) {
                        *total += a.len() - i; // enough: count the rest without walking it
                        break;
                    }
                    walk_reply(x, depth + 1, format!("{})", i + 1), max, out, total);
                }
            }
        }
        Reply::Map(m) => {
            if m.is_empty() {
                line("(empty map)".into(), LineKind::Container, out, total);
            } else {
                line(
                    format!("(map, {} entries)", m.len()),
                    LineKind::Container,
                    out,
                    total,
                );
                for (i, (k, v)) in m.iter().enumerate() {
                    if *total >= max.saturating_mul(2).max(max + 1) {
                        *total += m.len() - i;
                        break;
                    }
                    walk_reply(v, depth + 1, k.text(), max, out, total);
                }
            }
        }
    }
}

/// Bytes as text: UTF-8 as is (quoted), otherwise `\xNN` escapes like redis-cli.
pub fn display_bytes(b: &[u8]) -> String {
    match std::str::from_utf8(b) {
        Ok(s) => format!(
            "\"{}\"",
            s.replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\n', "\\n")
                .replace('\r', "\\r")
        ),
        Err(_) => {
            let mut s = String::from("\"");
            for &x in b {
                if (0x20..0x7f).contains(&x) && x != b'"' && x != b'\\' {
                    s.push(x as char);
                } else {
                    s.push_str(&format!("\\x{x:02x}"));
                }
            }
            s.push('"');
            s
        }
    }
}

// ---- value views -------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValueView {
    Text(String),
    /// Parsed as JSON: the pretty-printed text.
    Json(String),
    /// Not UTF-8: hex dump lines (`offset  bytes  ascii`).
    Hex(String),
}

/// How a string value is shown: JSON pretty-printed when it parses (objects
/// and arrays only: `42` stays text), text when it is UTF-8, else hex.
pub fn value_view(b: &[u8]) -> ValueView {
    match std::str::from_utf8(b) {
        Ok(s) => {
            let t = s.trim();
            if (t.starts_with('{') || t.starts_with('['))
                && let Ok(v) = serde_json::from_str::<serde_json::Value>(t)
                && let Ok(p) = serde_json::to_string_pretty(&v)
            {
                return ValueView::Json(p);
            }
            ValueView::Text(s.to_string())
        }
        Err(_) => ValueView::Hex(hex_dump(b)),
    }
}

pub fn hex_dump(b: &[u8]) -> String {
    let mut out = String::new();
    for (i, chunk) in b.chunks(16).enumerate() {
        let h = crate::hexfmt::lower(chunk);
        let hex: Vec<&str> = (0..chunk.len())
            .filter_map(|k| crate::hexfmt::pair(&h, k))
            .collect();
        out.push_str(&format!(
            "{:08x}  {:<47}  {}\n",
            i * 16,
            hex.join(" "),
            crate::hexfmt::ascii(chunk)
        ));
    }
    out
}

/// Cell text of a hash / list / set member: lossy UTF-8, long values cut.
pub fn cell_text(b: &[u8], max_chars: usize) -> String {
    let s = String::from_utf8_lossy(b);
    let one: String = s
        .chars()
        .map(|c| {
            if c == '\n' || c == '\r' || c == '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    if one.chars().count() > max_chars {
        format!("{}...", one.chars().take(max_chars).collect::<String>())
    } else {
        one
    }
}

/// `1h 02m 03s`-style TTL; `None` = no expiry.
pub fn fmt_ttl(ms: Option<i64>) -> String {
    let Some(ms) = ms else {
        return "no expiry".into();
    };
    let s = ms / 1000;
    let (d, h, m, sec) = (s / 86400, s % 86400 / 3600, s % 3600 / 60, s % 60);
    if d > 0 {
        format!("{d}d {h}h {m:02}m")
    } else if h > 0 {
        format!("{h}h {m:02}m {sec:02}s")
    } else if m > 0 {
        format!("{m}m {sec:02}s")
    } else if s > 0 {
        format!("{s}s")
    } else {
        format!("{ms} ms")
    }
}

/// `45s`, `10m`, `2h`, `1d` or plain seconds -> seconds.
pub fn parse_ttl_input(s: &str) -> Option<u64> {
    let s = s.trim();
    let (num, mul) = match s.chars().last()? {
        's' => (&s[..s.len() - 1], 1),
        'm' => (&s[..s.len() - 1], 60),
        'h' => (&s[..s.len() - 1], 3600),
        'd' => (&s[..s.len() - 1], 86400),
        c if c.is_ascii_digit() => (s, 1),
        _ => return None,
    };
    num.trim()
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .map(|n| n * mul)
}

pub fn fmt_bytes(n: u64) -> String {
    const U: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024.0 && i < 3 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> Vec<String> {
        split_command(s).unwrap()
    }

    #[test]
    fn classification() {
        assert_eq!(classify(&a("GET k")), CmdClass::Read);
        assert_eq!(classify(&a("hgetall h")), CmdClass::Read);
        assert_eq!(classify(&a("SET k v")), CmdClass::Write);
        assert_eq!(classify(&a("DEL k")), CmdClass::Write);
        assert_eq!(classify(&a("EXPIRE k 10")), CmdClass::Write);
        assert_eq!(classify(&a("FLUSHALL")), CmdClass::Write);
        assert_eq!(classify(&a("SOMEFUTURECMD x")), CmdClass::Write);
        assert_eq!(classify(&a("CONFIG GET maxmemory")), CmdClass::Read);
        assert_eq!(classify(&a("config set maxmemory 1")), CmdClass::Write);
        assert_eq!(classify(&a("CLIENT LIST")), CmdClass::Read);
        assert_eq!(classify(&a("CLIENT KILL ID 3")), CmdClass::Write);
        assert_eq!(classify(&a("DEBUG SLEEP 0")), CmdClass::Write);
        assert_eq!(classify(&a("ACL WHOAMI")), CmdClass::Read);
        assert_eq!(classify(&a("ACL SETUSER x on")), CmdClass::Write);
        assert_eq!(classify(&a("XADD s * a 1")), CmdClass::Write);
        assert_eq!(classify(&a("XRANGE s - +")), CmdClass::Read);
        assert_eq!(classify(&a("SCAN 0")), CmdClass::Read);
        assert_eq!(classify(&[]), CmdClass::Read);
    }

    #[test]
    fn dangerous() {
        assert_eq!(danger(&a("flushall")), Some("flushall"));
        assert_eq!(danger(&a("FLUSHDB ASYNC")), Some("flushdb"));
        assert_eq!(danger(&a("CONFIG SET x 1")), Some("config_set"));
        assert_eq!(danger(&a("CONFIG GET x")), None);
        assert_eq!(danger(&a("SHUTDOWN NOSAVE")), Some("shutdown"));
        assert_eq!(danger(&a("DEBUG OBJECT k")), Some("debug"));
        assert_eq!(danger(&a("KEYS *")), Some("keys"));
        assert_eq!(danger(&a("ACL WHOAMI")), None);
        assert_eq!(danger(&a("ACL DELUSER bob")), Some("admin"));
        assert_eq!(danger(&a("GET k")), None);
    }

    #[test]
    fn command_line() {
        assert_eq!(a("SET key value"), ["SET", "key", "value"]);
        assert_eq!(a("  SET   \"a b\"  'c d' "), ["SET", "a b", "c d"]);
        assert_eq!(
            a(r#"SET k "line\nnext \"q\" \x41""#),
            ["SET", "k", "line\nnext \"q\" A"]
        );
        assert_eq!(a(r"SET k 'it\'s\n'"), ["SET", "k", "it's\\n"]);
        assert!(split_command("SET \"open").is_err());
        assert!(split_command("SET \"a\"b").is_err());
        assert!(a("").is_empty());
    }

    #[test]
    fn namespace_tree() {
        let keys: Vec<String> = [
            "user:1:profile",
            "user:2:profile",
            "user:1:name",
            "greeting",
            "user",
            "a::b",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let t = KeyTree::from_keys(&keys, ":");
        assert_eq!(t.len(), 6);
        let mut open = HashSet::new();
        let r = t.rows(&open);
        let labels: Vec<_> = r
            .iter()
            .map(|x| (x.label.as_str(), x.is_folder, x.count))
            .collect();
        assert_eq!(
            labels,
            [("a", true, 1), ("user", true, 4), ("greeting", false, 1)]
        );
        open.insert("user:".to_string());
        open.insert("user:1:".to_string());
        let r = t.rows(&open);
        let flat: Vec<_> = r.iter().map(|x| (x.depth, x.path.as_str())).collect();
        assert_eq!(
            flat,
            [
                (0, "a:"),
                (0, "user:"),
                (1, "user"),
                (1, "user:1:"),
                (2, "user:1:name"),
                (2, "user:1:profile"),
                (1, "user:2:"),
                (0, "greeting")
            ]
        );
        // duplicates do not double count
        let mut t2 = KeyTree::new(":");
        t2.insert("x:y");
        t2.insert("x:y");
        assert_eq!(t2.len(), 1);
        assert!(KeyTree::new(":").is_empty());
    }

    #[test]
    fn reply_tree() {
        let r = Reply::Array(vec![
            Reply::Int(1),
            Reply::Bulk(b"a\"b".to_vec()),
            Reply::Nil,
            Reply::Array(vec![]),
            Reply::Error("ERR x".into()),
        ]);
        let l = reply_lines(&r, 100);
        let text: Vec<_> = l
            .iter()
            .map(|x| (x.depth, x.label.as_str(), x.text.as_str()))
            .collect();
        assert_eq!(
            text,
            [
                (0, "", "(array, 5 items)"),
                (1, "1)", "(integer) 1"),
                (1, "2)", "\"a\\\"b\""),
                (1, "3)", "(nil)"),
                (1, "4)", "(empty array)"),
                (1, "5)", "(error) ERR x")
            ]
        );
        let m = Reply::Map(vec![(Reply::Bulk(b"k".to_vec()), Reply::Int(2))]);
        assert_eq!(
            reply_lines(&m, 10)[1],
            ReplyLine {
                depth: 1,
                label: "k".into(),
                text: "(integer) 2".into(),
                kind: LineKind::Int
            }
        );
        // cap
        let big = Reply::Array((0..1000).map(Reply::Int).collect());
        let l = reply_lines(&big, 10);
        assert_eq!(l.len(), 11);
        assert!(l.last().unwrap().text.starts_with("... "));
        assert_eq!(reply_lines(&Reply::Status("OK".into()), 5)[0].text, "OK");
        assert_eq!(display_bytes(&[0xff, b'a']), "\"\\xffa\"");
    }

    #[test]
    fn value_views() {
        assert_eq!(value_view(b"hello"), ValueView::Text("hello".into()));
        assert_eq!(value_view(b"42"), ValueView::Text("42".into()));
        assert!(
            matches!(value_view(br#"{"a":[1,2]}"#), ValueView::Json(p) if p.contains("\n  \"a\""))
        );
        assert_eq!(value_view(b"{broken"), ValueView::Text("{broken".into()));
        let ValueView::Hex(h) = value_view(&[0, 1, 0xff, b'A']) else {
            panic!()
        };
        assert!(
            h.starts_with("00000000  00 01 ff 41") && h.trim_end().ends_with("...A"),
            "{h}"
        );
        assert_eq!(cell_text("a\nb".as_bytes(), 10), "a b");
        assert_eq!(cell_text(b"abcdef", 3), "abc...");
    }

    #[test]
    fn ttl_text() {
        assert_eq!(fmt_ttl(None), "no expiry");
        assert_eq!(fmt_ttl(Some(3_723_000)), "1h 02m 03s");
        assert_eq!(fmt_ttl(Some(90_000)), "1m 30s");
        assert_eq!(fmt_ttl(Some(200)), "200 ms");
        assert_eq!(fmt_ttl(Some(90_000_000)), "1d 1h 00m");
        assert_eq!(parse_ttl_input("90"), Some(90));
        assert_eq!(parse_ttl_input("10m"), Some(600));
        assert_eq!(parse_ttl_input("2h"), Some(7200));
        assert_eq!(parse_ttl_input("0"), None);
        assert_eq!(parse_ttl_input("x"), None);
        assert_eq!(fmt_bytes(1536), "1.5 KiB");
    }

    #[test]
    fn saved_connection_config() {
        let (c, pw) =
            crate::store::parse_url("rediss://app:s%3Acr@cache.example.com:6390/3").unwrap();
        assert_eq!(
            (
                c.kind,
                c.host.as_str(),
                c.port,
                c.user.as_str(),
                c.database.as_deref(),
                c.tls
            ),
            (
                DbKind::Redis,
                "cache.example.com",
                6390,
                "app",
                Some("3"),
                Tls::Required
            )
        );
        let cfg = config(&c, pw);
        assert_eq!(
            (
                cfg.db,
                cfg.tls,
                cfg.password.as_deref(),
                cfg.user.as_deref()
            ),
            (3, true, Some("s:cr"), Some("app"))
        );
        let (c, pw) = crate::store::parse_url("redis://:pw@127.0.0.1:6380/0").unwrap();
        assert_eq!(
            (c.user.as_str(), pw.as_str(), c.tls, c.subtitle().as_str()),
            ("", "pw", Tls::Disabled, "127.0.0.1:6380/0")
        );
        let (c, _) = crate::store::parse_url("redis://localhost").unwrap();
        assert_eq!((c.port, c.database.as_deref()), (6379, Some("0")));
        assert_eq!(db_index(Some("99")), 0);
    }
}

// ---- tab specs ------------------------------------------------------------------------

/// What a Redis tab shows; stored as JSON in the tab's `sql` field so tabs
/// survive a restart without changing `TabData`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Spec {
    Browser {
        db: u8,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<String>,
    },
    Key {
        db: u8,
        key: String,
    },
    Console {
        db: u8,
    },
    Info,
}

impl Spec {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    pub fn parse(s: &str) -> Option<Spec> {
        serde_json::from_str(s).ok()
    }
}

/// Commands the console refuses: they change per-connection state that the
/// shared multiplexed connection must not carry, or never return.
pub fn unsupported(args: &[String]) -> Option<&'static str> {
    let cmd = args.first()?.to_ascii_uppercase();
    match cmd.as_str() {
        "SELECT" => Some("select"),
        "SUBSCRIBE" | "PSUBSCRIBE" | "SSUBSCRIBE" | "UNSUBSCRIBE" | "PUNSUBSCRIBE" | "MONITOR"
        | "SYNC" | "PSYNC" => Some("stream"),
        "MULTI" | "EXEC" | "DISCARD" | "WATCH" | "UNWATCH" | "HELLO" | "AUTH" | "RESET"
        | "QUIT" => Some("session"),
        "BLPOP" | "BRPOP" | "BLMOVE" | "BRPOPLPUSH" | "BZPOPMIN" | "BZPOPMAX" | "BLMPOP"
        | "BZMPOP" => Some("blocking"),
        "CLIENT"
            if args.get(1).is_some_and(|s| {
                matches!(
                    s.to_ascii_uppercase().as_str(),
                    "REPLY" | "SETNAME" | "TRACKING" | "CACHING"
                )
            }) =>
        {
            Some("session")
        }
        _ => None,
    }
}

#[cfg(test)]
mod spec_tests {
    use super::*;

    #[test]
    fn spec_roundtrip_and_unsupported() {
        let s = Spec::Key {
            db: 2,
            key: "user:1".into(),
        };
        assert_eq!(Spec::parse(&s.to_json()), Some(s));
        assert_eq!(Spec::parse(&Spec::Info.to_json()), Some(Spec::Info));
        assert_eq!(Spec::parse("nope"), None);
        let a = |s: &str| split_command(s).unwrap();
        assert_eq!(unsupported(&a("select 1")), Some("select"));
        assert_eq!(unsupported(&a("SUBSCRIBE c")), Some("stream"));
        assert_eq!(unsupported(&a("GET k")), None);
        assert_eq!(unsupported(&a("client list")), None);
    }
}

// ---- key list helpers (master / detail browser) ---------------------------------------

/// Short badge text of a Redis type for the key list.
pub fn type_badge(ty: &str) -> &'static str {
    match ty {
        "string" => "STR",
        "hash" => "HASH",
        "list" => "LIST",
        "set" => "SET",
        "zset" => "ZSET",
        "stream" => "STREAM",
        "ReJSON-RL" => "JSON",
        _ => "?",
    }
}

/// TTL for the key list: "∞" without expiry, else the largest whole unit ("45s", "12m", "3h", "2d").
pub fn fmt_ttl_short(ms: Option<i64>) -> String {
    let Some(ms) = ms else { return "∞".into() };
    let s = ms.max(0) / 1000;
    match s {
        0 => "<1s".into(),
        1..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
    }
}

/// The prefix a folder row stands for as a `SCAN MATCH` pattern (glob characters escaped).
pub fn prefix_pattern(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + 1);
    for c in prefix.chars() {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('*');
    out
}

/// Flat (non-tree) key list for "List" view, filtered by a substring, sorted by name.
pub fn flat_keys<'a>(keys: impl IntoIterator<Item = &'a String>) -> Vec<String> {
    let mut v: Vec<String> = keys.into_iter().cloned().collect();
    v.sort();
    v.dedup();
    v
}

#[cfg(test)]
mod list_tests {
    use super::*;

    #[test]
    fn badges_and_short_ttl() {
        assert_eq!(type_badge("zset"), "ZSET");
        assert_eq!(type_badge("weird"), "?");
        assert_eq!(fmt_ttl_short(None), "∞");
        assert_eq!(fmt_ttl_short(Some(400)), "<1s");
        assert_eq!(fmt_ttl_short(Some(45_000)), "45s");
        assert_eq!(fmt_ttl_short(Some(12 * 60_000 + 59_000)), "12m");
        assert_eq!(fmt_ttl_short(Some(3 * 3_600_000)), "3h");
        assert_eq!(fmt_ttl_short(Some(2 * 86_400_000)), "2d");
    }

    #[test]
    fn prefix_pattern_escapes_globs() {
        assert_eq!(prefix_pattern("user:"), "user:*");
        assert_eq!(prefix_pattern("a[1]*:"), "a\\[1\\]\\*:*");
    }

    #[test]
    fn flat_list_sorted_unique() {
        let k = vec!["b".to_string(), "a".to_string(), "b".to_string()];
        assert_eq!(flat_keys(&k), vec!["a", "b"]);
    }

    #[test]
    fn spec_browser_with_selected_key() {
        let s = Spec::Browser {
            db: 1,
            key: Some("k".into()),
        };
        assert_eq!(Spec::parse(&s.to_json()), Some(s));
        // Old saved tabs have no "key" field.
        assert_eq!(
            Spec::parse(r#"{"kind":"browser","db":3}"#),
            Some(Spec::Browser { db: 3, key: None })
        );
    }
}

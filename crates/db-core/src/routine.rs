//! Stored code (Oracle packages, procedures, functions, triggers, types;
//! MySQL / MariaDB routines, triggers, events; PostgreSQL functions,
//! procedures, triggers): reading the source, turning the editor text into
//! the statements that save it, and Oracle compile errors. Pure: the app
//! runs the statements through the normal query path (no protocol changes).

use crate::dictionary::{Dialect, Row, Stmt};
use crate::{DbObjectKind, Param};

/// Which half of an Oracle package / type is shown. Other objects have only `Spec`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Part {
    Spec,
    Body,
}

/// Kinds a Source tab can open and save on this engine.
pub fn editable(d: Dialect, kind: DbObjectKind) -> bool {
    use DbObjectKind::*;
    match d {
        Dialect::Oracle => matches!(kind, Package | Procedure | Function | Trigger | Type),
        Dialect::Mysql => matches!(kind, Procedure | Function | Trigger | Event),
        Dialect::Postgres => matches!(kind, Function | Procedure | Trigger),
    }
}

/// The object has a separate body (Oracle package / type).
pub fn has_body(d: Dialect, kind: DbObjectKind) -> bool {
    d == Dialect::Oracle && matches!(kind, DbObjectKind::Package | DbObjectKind::Type)
}

/// `ALL_SOURCE.TYPE` / `ALL_ERRORS.TYPE` / `ALL_OBJECTS.OBJECT_TYPE` of a part.
pub fn oracle_type(kind: DbObjectKind, part: Part) -> Option<&'static str> {
    use DbObjectKind::*;
    Some(match (kind, part) {
        (Package, Part::Spec) => "PACKAGE",
        (Package, Part::Body) => "PACKAGE BODY",
        (Type, Part::Spec) => "TYPE",
        (Type, Part::Body) => "TYPE BODY",
        (Procedure, _) => "PROCEDURE",
        (Function, _) => "FUNCTION",
        (Trigger, _) => "TRIGGER",
        _ => return None,
    })
}

fn t(s: &str) -> Param {
    Param::Text(s.to_string())
}

/// Lines of source a read returns at most (a package body of 100k lines is rare).
pub const SOURCE_CAP: u32 = 200_000;

/// Oracle: the lines of one part from `ALL_SOURCE`, in order (one column: TEXT).
pub fn oracle_source_stmt(
    schema: &str,
    kind: DbObjectKind,
    name: &str,
    part: Part,
) -> Option<Stmt> {
    let ty = oracle_type(kind, part)?;
    Some(Stmt {
        sql: "SELECT TEXT FROM (SELECT TEXT FROM ALL_SOURCE WHERE OWNER = ? AND NAME = ? AND TYPE = ? ORDER BY LINE) \
              WHERE ROWNUM <= TO_NUMBER(?)"
            .into(),
        params: vec![t(schema), t(name), t(ty), t(&SOURCE_CAP.to_string())],
        optional: false,
    })
}

/// Oracle: compile errors and warnings of one part (columns: LINE, POSITION, TEXT, ATTRIBUTE).
pub fn oracle_errors_stmt(
    schema: &str,
    kind: DbObjectKind,
    name: &str,
    part: Part,
) -> Option<Stmt> {
    let ty = oracle_type(kind, part)?;
    Some(Stmt {
        sql: "SELECT LINE, POSITION, TEXT, ATTRIBUTE FROM (SELECT LINE, POSITION, TEXT, ATTRIBUTE FROM ALL_ERRORS \
              WHERE OWNER = ? AND NAME = ? AND TYPE = ? ORDER BY SEQUENCE) WHERE ROWNUM <= 500"
            .into(),
        params: vec![t(schema), t(name), t(ty)],
        optional: false,
    })
}

/// Oracle: status and last DDL time of every part (columns: OBJECT_TYPE, STATUS, LAST_DDL_TIME text).
pub fn oracle_status_stmt(schema: &str, kind: DbObjectKind, name: &str) -> Option<Stmt> {
    let spec = oracle_type(kind, Part::Spec)?;
    let body = oracle_type(kind, Part::Body).unwrap_or(spec);
    Some(Stmt {
        sql: "SELECT OBJECT_TYPE, STATUS, TO_CHAR(LAST_DDL_TIME, 'YYYY-MM-DD HH24:MI:SS') FROM ALL_OBJECTS \
              WHERE OWNER = ? AND OBJECT_NAME = ? AND OBJECT_TYPE IN (?, ?) AND ROWNUM <= 2"
            .into(),
        params: vec![t(schema), t(name), t(spec), t(body)],
        optional: false,
    })
}

/// The `ALL_SOURCE` lines as an editable `CREATE OR REPLACE` statement: the
/// unit keyword gets the prefix and the name its owner, so saving from a
/// session in another schema still writes the right object. Empty when the
/// part does not exist (a package without a body).
pub fn oracle_source_text(lines: &[Row], schema: &str) -> String {
    let mut s: String = lines
        .iter()
        .map(|r| r.first().cloned().flatten().unwrap_or_default())
        .collect();
    if s.trim().is_empty() {
        return String::new();
    }
    if !s.ends_with('\n') {
        s.push('\n');
    }
    let Some((kw_start, kw_end)) = unit_keyword(&s) else {
        return format!("CREATE OR REPLACE {s}");
    };
    let head = &s[..kw_start];
    let kw = &s[kw_start..kw_end];
    let rest = &s[kw_end..];
    format!(
        "{head}CREATE OR REPLACE {kw}{}",
        qualify_name(rest, &format!("\"{}\"", schema.replace('"', "\"\"")))
    )
}

/// Why compiling an Oracle object deserves more than the usual confirm.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shared {
    /// The object belongs to another schema than the session user.
    NotOwner,
    /// Own object, but the session is the E-Business Suite `APPS` user: its objects are shared by every module.
    Privileged,
}

/// Whether replacing `owner`'s object as `session_user` changes something shared.
pub fn shared_object(session_user: &str, owner: &str) -> Option<Shared> {
    if !session_user.eq_ignore_ascii_case(owner) {
        Some(Shared::NotOwner)
    } else if session_user.eq_ignore_ascii_case("APPS") {
        Some(Shared::Privileged)
    } else {
        None
    }
}

/// Oracle: which part of a package / type the statement `text` creates
/// (`PACKAGE` -> Spec, `PACKAGE BODY` -> Body), from its own words. `None`
/// when the text is neither (or the kind has no parts).
pub fn oracle_unit_part(kind: DbObjectKind, text: &str) -> Option<Part> {
    if !has_body(Dialect::Oracle, kind) {
        return None;
    }
    let body = strip_client_syntax(text);
    let mut i = skip_blank(&body, 0);
    if let Some(r) = strip_prefix_ci(&body[i..], "CREATE") {
        i = body.len() - r.len();
        loop {
            let j = skip_blank(&body, i);
            let r = &body[j..];
            let w_end = r.find(|c: char| !is_ident(c)).unwrap_or(r.len());
            if matches!(
                r[..w_end].to_ascii_uppercase().as_str(),
                "OR" | "REPLACE" | "EDITIONABLE" | "NONEDITIONABLE"
            ) {
                i = j + w_end;
            } else {
                i = j;
                break;
            }
        }
    }
    let (a, b) = unit_keyword(&body[i..])?;
    let kw = body[i + a..i + b].to_ascii_uppercase();
    Some(if kw.ends_with("BODY") {
        Part::Body
    } else {
        Part::Spec
    })
}

/// Byte range of `PACKAGE [BODY] | PROCEDURE | FUNCTION | TRIGGER | TYPE [BODY]`
/// at the start of stored Oracle source (blanks and comments skipped).
fn unit_keyword(s: &str) -> Option<(usize, usize)> {
    let i = skip_blank(s, 0);
    let words = [
        "PACKAGE BODY",
        "TYPE BODY",
        "PACKAGE",
        "PROCEDURE",
        "FUNCTION",
        "TRIGGER",
        "TYPE",
    ];
    for w in words {
        let mut j = i;
        let mut ok = true;
        for (k, part) in w.split(' ').enumerate() {
            if k > 0 {
                let after = skip_blank(s, j);
                if after == j {
                    ok = false;
                    break;
                }
                j = after;
            }
            if s.len() >= j + part.len()
                && s[j..j + part.len()].eq_ignore_ascii_case(part)
                && !s[j + part.len()..].starts_with(is_ident)
            {
                j += part.len();
            } else {
                ok = false;
                break;
            }
        }
        if ok {
            return Some((i, j));
        }
    }
    None
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || c == '#'
}

/// Index of the next byte that is not a blank or inside a comment.
fn skip_blank(s: &str, mut i: usize) -> usize {
    let b = s.as_bytes();
    loop {
        while i < b.len() && b[i].is_ascii_whitespace() {
            i += 1;
        }
        if b[i..].starts_with(b"--") {
            i = s[i..].find('\n').map_or(b.len(), |p| i + p);
        } else if b[i..].starts_with(b"/*") {
            i = s[i + 2..].find("*/").map_or(b.len(), |p| i + 2 + p + 2);
        } else {
            return i;
        }
    }
}

/// `rest` starts after the unit keyword: when its name has no owner yet,
/// writes `owner.` in front of it. `owner` is already quoted.
fn qualify_name(rest: &str, owner: &str) -> String {
    let i = skip_blank(rest, 0);
    let (name_end, quoted) = if rest[i..].starts_with('"') {
        (
            rest[i + 1..]
                .find('"')
                .map_or(rest.len(), |p| i + 1 + p + 1),
            true,
        )
    } else {
        (
            rest[i..]
                .find(|c: char| !is_ident(c))
                .map_or(rest.len(), |p| i + p),
            false,
        )
    };
    let _ = quoted;
    if name_end == i || rest[name_end..].trim_start().starts_with('.') {
        return rest.to_string();
    }
    format!("{}{owner}.{}", &rest[..i], &rest[i..])
}

/// MySQL `SHOW CREATE ...` writes the bare name: put the schema in front so
/// saving targets the schema the object came from.
pub fn mysql_qualify(ddl: &str, schema: &str, kind: DbObjectKind) -> String {
    let kw = match kind {
        DbObjectKind::Procedure => "PROCEDURE",
        DbObjectKind::Function => "FUNCTION",
        DbObjectKind::Trigger => "TRIGGER",
        DbObjectKind::Event => "EVENT",
        _ => return ddl.to_string(),
    };
    let Some(at) = find_word_outside_quotes(ddl, kw) else {
        return ddl.to_string();
    };
    let after = at + kw.len();
    let rest = &ddl[after..];
    let i = rest.len() - rest.trim_start().len();
    let r = &rest[i..];
    let name_end = if let Some(stripped) = r.strip_prefix('`') {
        stripped.find('`').map_or(r.len(), |p| p + 2)
    } else {
        r.find(|c: char| !is_ident(c)).unwrap_or(r.len())
    };
    if name_end == 0 || r[name_end..].starts_with('.') {
        return ddl.to_string();
    }
    format!(
        "{}{}`{}`.{}",
        &ddl[..after],
        &rest[..i],
        schema.replace('`', "``"),
        r
    )
}

/// First `word` (case-insensitive, whole word) outside `` `..` `` and `'..'`.
fn find_word_outside_quotes(s: &str, word: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            q @ (b'`' | b'\'' | b'"') => {
                i = s[i + 1..]
                    .find(q as char)
                    .map_or(b.len(), |p| i + 1 + p + 1);
            }
            _ => {
                if s.len() >= i + word.len()
                    && s.is_char_boundary(i + word.len())
                    && s[i..i + word.len()].eq_ignore_ascii_case(word)
                    && (i == 0 || !is_ident(s[..i].chars().next_back().unwrap_or(' ')))
                    && !s[i + word.len()..].starts_with(is_ident)
                {
                    return Some(i);
                }
                i += s[i..].chars().next().map_or(1, char::len_utf8);
            }
        }
    }
    None
}

/// PostgreSQL triggers come back as `CREATE TRIGGER`: make them replaceable
/// (PostgreSQL 14+).
pub fn postgres_source(ddl: &str, kind: DbObjectKind) -> String {
    if kind == DbObjectKind::Trigger
        && let Some(rest) = strip_prefix_ci(ddl.trim_start(), "CREATE TRIGGER")
    {
        return format!("CREATE OR REPLACE TRIGGER{rest}");
    }
    ddl.to_string()
}

fn strip_prefix_ci<'a>(s: &'a str, p: &str) -> Option<&'a str> {
    (s.len() >= p.len() && s.is_char_boundary(p.len()) && s[..p.len()].eq_ignore_ascii_case(p))
        .then(|| &s[p.len()..])
}

/// What "Compile" / "Save to database" sends, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavePlan {
    pub statements: Vec<String>,
    /// False when the old object is dropped first (MySQL): if the CREATE then
    /// fails, the object is gone. The UI must confirm.
    pub atomic: bool,
}

/// Removes what a client script adds around one statement: a trailing `/`
/// line (SQL*Plus), a trailing `//` or `$$` delimiter and `DELIMITER` lines.
pub fn strip_client_syntax(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| {
            !l.trim_start()
                .to_ascii_uppercase()
                .starts_with("DELIMITER ")
        })
        .collect();
    let mut s = lines.join("\n").trim().to_string();
    loop {
        let before = s.len();
        for end in ["\n/", "//", "$$"] {
            if s.ends_with(end) && !(end == "$$" && s.matches("$$").count().is_multiple_of(2)) {
                s.truncate(s.len() - end.len());
                s = s.trim_end().to_string();
            }
        }
        if s == "/" {
            s.clear();
        }
        if s.len() == before {
            return s;
        }
    }
}

/// `CREATE` → `CREATE OR REPLACE` when it is not there yet (the DEFINER of a
/// MariaDB routine is kept after it).
fn or_replace(s: &str) -> String {
    let i = skip_blank(s, 0);
    let Some(rest) = strip_prefix_ci(&s[i..], "CREATE") else {
        return s.to_string();
    };
    let j = skip_blank(rest, 0);
    if strip_prefix_ci(&rest[j..], "OR")
        .is_some_and(|r| strip_prefix_ci(r.trim_start(), "REPLACE").is_some())
    {
        return s.to_string();
    }
    format!("{}CREATE OR REPLACE{rest}", &s[..i])
}

/// The statements that save `text` as object `schema.name` of `kind`.
/// `mariadb` picks `CREATE OR REPLACE` over DROP + CREATE on MySQL servers.
pub fn save_plan(
    d: Dialect,
    mariadb: bool,
    schema: &str,
    kind: DbObjectKind,
    name: &str,
    text: &str,
) -> SavePlan {
    let body = strip_client_syntax(text);
    match d {
        Dialect::Oracle => {
            let i = skip_blank(&body, 0);
            let s = if unit_keyword(&body).is_some() {
                format!("{}CREATE OR REPLACE {}", &body[..i], &body[i..])
            } else {
                or_replace(&body)
            };
            SavePlan {
                statements: vec![s],
                atomic: true,
            }
        }
        Dialect::Postgres => SavePlan {
            statements: vec![or_replace(&body)],
            atomic: true,
        },
        Dialect::Mysql => {
            let body = body.trim_end_matches(';').trim_end().to_string();
            if mariadb {
                return SavePlan {
                    statements: vec![or_replace(&body)],
                    atomic: true,
                };
            }
            let what = match kind {
                DbObjectKind::Procedure => "PROCEDURE",
                DbObjectKind::Function => "FUNCTION",
                DbObjectKind::Trigger => "TRIGGER",
                DbObjectKind::Event => "EVENT",
                _ => {
                    return SavePlan {
                        statements: vec![body],
                        atomic: true,
                    };
                }
            };
            let q = |s: &str| format!("`{}`", s.replace('`', "``"));
            SavePlan {
                statements: vec![
                    format!("DROP {what} IF EXISTS {}.{}", q(schema), q(name)),
                    body,
                ],
                atomic: false,
            }
        }
    }
}

/// One line of `ALL_ERRORS`. `line` / `col` are 1-based in the stored source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileError {
    pub line: u32,
    pub col: u32,
    pub text: String,
    pub warning: bool,
}

pub fn parse_errors(rows: &[Row]) -> Vec<CompileError> {
    let num = |r: &Row, i: usize| {
        r.get(i)
            .cloned()
            .flatten()
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    };
    rows.iter()
        .map(|r| CompileError {
            line: num(r, 0),
            col: num(r, 1),
            text: r
                .get(2)
                .cloned()
                .flatten()
                .unwrap_or_default()
                .trim_end()
                .to_string(),
            warning: r
                .get(3)
                .cloned()
                .flatten()
                .is_some_and(|a| a.eq_ignore_ascii_case("WARNING")),
        })
        .collect()
}

/// Oracle numbers error lines from the unit keyword (`PACKAGE`, ...): the
/// 0-based editor line it sits on, to add to `line - 1`.
pub fn error_line_offset(text: &str) -> u32 {
    let start = skip_blank(text, 0);
    let mut i = start;
    if let Some(r) = strip_prefix_ci(&text[i..], "CREATE") {
        i = text.len() - r.len();
        loop {
            let j = skip_blank(text, i);
            let r = &text[j..];
            let w_end = r.find(|c: char| !is_ident(c)).unwrap_or(r.len());
            let w = r[..w_end].to_ascii_uppercase();
            if matches!(
                w.as_str(),
                "OR" | "REPLACE" | "EDITIONABLE" | "NONEDITIONABLE"
            ) {
                i = j + w_end;
            } else {
                i = j;
                break;
            }
        }
    }
    text[..i.min(text.len())].matches('\n').count() as u32
}

/// The server rejected the CREATE only because the code has errors: the
/// object is stored (INVALID) and `ALL_ERRORS` says why.
pub fn is_compiled_with_errors(message: &str) -> bool {
    message.contains("ORA-24344") || message.to_ascii_lowercase().contains("compilation error")
}

/// Same source, ignoring line-end style and trailing blanks: "has someone
/// else changed it since it was opened".
pub fn same_source(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_string()
    };
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use DbObjectKind::*;

    #[test]
    fn shared_objects_need_the_stronger_confirm() {
        assert_eq!(shared_object("ABM", "ABM"), None);
        assert_eq!(shared_object("abm", "ABM"), None);
        assert_eq!(shared_object("ABM", "APPS"), Some(Shared::NotOwner));
        assert_eq!(shared_object("APPS", "APPS"), Some(Shared::Privileged));
        assert_eq!(shared_object("APPS", "AR"), Some(Shared::NotOwner));
    }

    #[test]
    fn unit_part_from_text() {
        assert_eq!(
            oracle_unit_part(Package, "CREATE OR REPLACE PACKAGE \"A\".\"P\" AS\nEND;"),
            Some(Part::Spec)
        );
        assert_eq!(
            oracle_unit_part(
                Package,
                "-- c\ncreate or replace editionable package body p is\nend;\n/"
            ),
            Some(Part::Body)
        );
        assert_eq!(
            oracle_unit_part(Package, "PACKAGE BODY p IS END;"),
            Some(Part::Body)
        );
        assert_eq!(
            oracle_unit_part(Type, "CREATE TYPE BODY t AS END;"),
            Some(Part::Body)
        );
        assert_eq!(
            oracle_unit_part(Procedure, "CREATE PROCEDURE p IS BEGIN NULL; END;"),
            None
        );
        assert_eq!(oracle_unit_part(Package, "select 1"), None);
    }

    fn rows(lines: &[&str]) -> Vec<Row> {
        lines.iter().map(|l| vec![Some(l.to_string())]).collect()
    }

    #[test]
    fn oracle_source_becomes_create_or_replace_with_owner() {
        let spec = oracle_source_text(
            &rows(&[
                "PACKAGE wb_test_pkg AS\n",
                "  PROCEDURE a;\n",
                "END wb_test_pkg;",
            ]),
            "APPS",
        );
        assert_eq!(
            spec,
            "CREATE OR REPLACE PACKAGE \"APPS\".wb_test_pkg AS\n  PROCEDURE a;\nEND wb_test_pkg;\n"
        );
        let body = oracle_source_text(
            &rows(&["package body \"WB_TEST_PKG\" is\n", "end;\n"]),
            "APPS",
        );
        assert_eq!(
            body,
            "CREATE OR REPLACE package body \"APPS\".\"WB_TEST_PKG\" is\nend;\n"
        );
        // already qualified, and type bodies / triggers
        let q = oracle_source_text(&rows(&["PROCEDURE apps.p IS BEGIN NULL; END;\n"]), "APPS");
        assert_eq!(
            q,
            "CREATE OR REPLACE PROCEDURE apps.p IS BEGIN NULL; END;\n"
        );
        let tb = oracle_source_text(&rows(&["TYPE BODY t AS END;\n"]), "X");
        assert!(
            tb.starts_with("CREATE OR REPLACE TYPE BODY \"X\".t AS"),
            "{tb}"
        );
        let tr = oracle_source_text(&rows(&["TRIGGER t$1 BEFORE INSERT ON x\n"]), "X");
        assert!(
            tr.starts_with("CREATE OR REPLACE TRIGGER \"X\".t$1 BEFORE"),
            "{tr}"
        );
        assert_eq!(oracle_source_text(&[], "X"), "");
        // Oracle pads the stored header (seen on 11.2)
        let padded = oracle_source_text(
            &rows(&["PACKAGE BODY        WB_TEST_PKG AS\n", "END WB_TEST_PKG;"]),
            "APPS",
        );
        assert!(
            padded.starts_with("CREATE OR REPLACE PACKAGE BODY        \"APPS\".WB_TEST_PKG AS\n"),
            "{padded}"
        );
    }

    #[test]
    fn oracle_statements_bind_names() {
        let s = oracle_source_stmt("APPS", Package, "P", Part::Body).unwrap();
        assert!(
            !s.sql.contains("APPS") && s.sql.contains("ALL_SOURCE") && s.sql.contains("ROWNUM")
        );
        assert_eq!(s.params[2], t("PACKAGE BODY"));
        let e = oracle_errors_stmt("APPS", Package, "P", Part::Spec).unwrap();
        assert!(e.sql.contains("ALL_ERRORS") && e.params.len() == 3);
        let st = oracle_status_stmt("APPS", Procedure, "P").unwrap();
        assert_eq!(st.params[2], t("PROCEDURE"));
        assert!(oracle_source_stmt("APPS", Table, "T", Part::Spec).is_none());
    }

    #[test]
    fn kinds() {
        assert!(
            editable(Dialect::Oracle, Package)
                && editable(Dialect::Mysql, Event)
                && !editable(Dialect::Postgres, Event)
        );
        assert!(
            has_body(Dialect::Oracle, Package)
                && has_body(Dialect::Oracle, Type)
                && !has_body(Dialect::Oracle, Procedure)
        );
        assert!(!has_body(Dialect::Mysql, Procedure));
    }

    #[test]
    fn mysql_names_get_the_schema() {
        let d = "CREATE DEFINER=`root`@`%` PROCEDURE `p`(IN a INT)\nBEGIN SELECT 'PROCEDURE'; END";
        assert_eq!(
            mysql_qualify(d, "wb`test", Procedure),
            "CREATE DEFINER=`root`@`%` PROCEDURE `wb``test`.`p`(IN a INT)\nBEGIN SELECT 'PROCEDURE'; END"
        );
        let already = "CREATE FUNCTION `s`.`f`() RETURNS int RETURN 1";
        assert_eq!(mysql_qualify(already, "x", Function), already);
        let tr =
            "CREATE DEFINER=`u`@`h` TRIGGER t1 BEFORE INSERT ON `t` FOR EACH ROW SET NEW.a = 1";
        assert!(mysql_qualify(tr, "db", Trigger).contains("TRIGGER `db`.t1 BEFORE"));
        let ev = "CREATE DEFINER=`u`@`h` EVENT `e` ON SCHEDULE EVERY 1 DAY DO SELECT 1";
        assert!(mysql_qualify(ev, "db", Event).contains("EVENT `db`.`e` ON"));
    }

    #[test]
    fn postgres_triggers_are_replaceable() {
        assert_eq!(
            postgres_source("CREATE TRIGGER t BEFORE INSERT ON x", Trigger),
            "CREATE OR REPLACE TRIGGER t BEFORE INSERT ON x"
        );
        let f = "CREATE OR REPLACE FUNCTION public.f()\n RETURNS integer\n LANGUAGE sql\nAS $function$select 1$function$\n";
        assert_eq!(postgres_source(f, Function), f);
    }

    #[test]
    fn client_syntax_is_removed() {
        assert_eq!(
            strip_client_syntax("CREATE PACKAGE p AS END;\n/\n"),
            "CREATE PACKAGE p AS END;"
        );
        assert_eq!(
            strip_client_syntax(
                "DELIMITER //\nCREATE PROCEDURE p() BEGIN SELECT 1; END//\nDELIMITER ;\n"
            ),
            "CREATE PROCEDURE p() BEGIN SELECT 1; END"
        );
        assert_eq!(
            strip_client_syntax("CREATE PROCEDURE p() BEGIN END$$"),
            "CREATE PROCEDURE p() BEGIN END"
        );
        // a PostgreSQL dollar-quoted body keeps its closing $$
        let pg = "CREATE FUNCTION f() RETURNS int AS $$ select 1 $$";
        assert_eq!(strip_client_syntax(pg), pg);
        assert_eq!(strip_client_syntax("/"), "");
    }

    #[test]
    fn save_plans() {
        let o = save_plan(
            Dialect::Oracle,
            false,
            "APPS",
            Package,
            "P",
            "PACKAGE BODY p AS END;\n/",
        );
        assert_eq!(o.statements, ["CREATE OR REPLACE PACKAGE BODY p AS END;"]);
        let o = save_plan(
            Dialect::Oracle,
            false,
            "APPS",
            Package,
            "P",
            "-- c\ncreate package p as end;",
        );
        assert_eq!(o.statements, ["-- c\nCREATE OR REPLACE package p as end;"]);
        assert!(o.atomic);
        let o = save_plan(
            Dialect::Oracle,
            false,
            "APPS",
            Package,
            "P",
            "Create Or Replace package p as end;",
        );
        assert_eq!(o.statements, ["Create Or Replace package p as end;"]);

        let m = save_plan(
            Dialect::Mysql,
            false,
            "wb",
            Procedure,
            "p",
            "CREATE DEFINER=`root`@`%` PROCEDURE `wb`.`p`() BEGIN SELECT 1; END;",
        );
        assert_eq!(
            m.statements,
            [
                "DROP PROCEDURE IF EXISTS `wb`.`p`",
                "CREATE DEFINER=`root`@`%` PROCEDURE `wb`.`p`() BEGIN SELECT 1; END"
            ]
        );
        assert!(!m.atomic);
        let mm = save_plan(
            Dialect::Mysql,
            true,
            "wb",
            Trigger,
            "t",
            "CREATE DEFINER=`root`@`%` TRIGGER `wb`.`t` BEFORE INSERT ON x FOR EACH ROW SET NEW.a = 1",
        );
        assert_eq!(
            mm.statements,
            [
                "CREATE OR REPLACE DEFINER=`root`@`%` TRIGGER `wb`.`t` BEFORE INSERT ON x FOR EACH ROW SET NEW.a = 1"
            ]
        );
        assert!(mm.atomic);

        let p = save_plan(
            Dialect::Postgres,
            false,
            "public",
            Trigger,
            "t",
            "CREATE TRIGGER t AFTER INSERT ON x FOR EACH ROW EXECUTE FUNCTION f();",
        );
        assert_eq!(
            p.statements,
            ["CREATE OR REPLACE TRIGGER t AFTER INSERT ON x FOR EACH ROW EXECUTE FUNCTION f();"]
        );
    }

    #[test]
    fn errors_and_offsets() {
        let r = vec![
            vec![
                Some("3".into()),
                Some("5".into()),
                Some("PLS-00201: identifier 'X' must be declared\n".into()),
                Some("ERROR".into()),
            ],
            vec![
                Some("1".into()),
                Some("1".into()),
                Some("PLW-05018: no AUTHID".into()),
                Some("WARNING".into()),
            ],
        ];
        let e = parse_errors(&r);
        assert_eq!(
            e[0],
            CompileError {
                line: 3,
                col: 5,
                text: "PLS-00201: identifier 'X' must be declared".into(),
                warning: false
            }
        );
        assert!(e[1].warning);
        assert_eq!(error_line_offset("CREATE OR REPLACE PACKAGE p AS"), 0);
        assert_eq!(
            error_line_offset("-- header\n\nCREATE OR REPLACE\nPACKAGE BODY p AS"),
            3
        );
        assert_eq!(error_line_offset("\n\nPACKAGE p AS"), 2);
        assert!(is_compiled_with_errors(
            "ORA-24344: success with compilation error"
        ));
        assert!(!is_compiled_with_errors(
            "ORA-00942: table or view does not exist"
        ));
    }

    #[test]
    fn source_comparison() {
        assert!(same_source("a\r\nb  \r\n", "a\nb"));
        assert!(!same_source("a\nb", "a\nc"));
    }

    #[test]
    fn splitter_and_classifier_keep_bodies_whole() {
        let text = "CREATE OR REPLACE PACKAGE BODY p AS\n  PROCEDURE a IS BEGIN DELETE FROM t; END;\nEND p;\n/\n";
        let c = crate::classify::classify_script(text);
        assert_eq!(c.len(), 1);
        assert!(c[0].writes && c[0].kind == crate::classify::StatementKind::Ddl);
    }
}

//! Data-dictionary SQL for the extended catalog (see `Catalog::list_objects`
//! and friends), shared by every driver that reads the dictionary with plain
//! SQL: `db-mysql` runs it on its pool, the sidecar client through the `run`
//! frame. A driver only supplies a [`Fetch`] that executes one [`Stmt`] and
//! returns the rows as text.
//!
//! Statements use `?` placeholders for every schema / owner / table name
//! (values are bound, never spliced). Only the row cap, a `u32` formatted by
//! this module, and identifiers inside `SHOW CREATE` (escaped) are part of
//! the text on MySQL; on Oracle the cap is bound too. Oracle reads are
//! filtered by `OWNER` and wrapped in `ROWNUM <= cap`, so they also run on 11g.
//! Everything is a SELECT.

use crate::{
    CatalogFuture, DbError, DbObjectKind, ErrorKind, InfoItem, ObjectEntry, ObjectPage, Param,
    Principal, RoutineParam,
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Mysql,
    Oracle,
    Postgres,
}

impl Dialect {
    /// From the driver name the sidecar reports in its `ready` frame.
    pub fn of(driver: &str) -> Option<Dialect> {
        match driver {
            "mysql" => Some(Dialect::Mysql),
            "oracle" => Some(Dialect::Oracle),
            "postgres" => Some(Dialect::Postgres),
            _ => None,
        }
    }
}

/// One dictionary statement. `optional`: a failure (for example missing
/// privileges) means "nothing to show" instead of an error.
#[derive(Clone, Debug, PartialEq)]
pub struct Stmt {
    pub sql: String,
    pub params: Vec<Param>,
    pub optional: bool,
}

pub type Row = Vec<Option<String>>;

/// Runs one statement and returns its rows as text (`None` = NULL).
pub type Fetch = Arc<dyn Fn(Stmt) -> CatalogFuture<Vec<Row>> + Send + Sync>;

fn stmt(sql: impl Into<String>, params: Vec<Param>) -> Stmt {
    Stmt {
        sql: sql.into(),
        params,
        optional: false,
    }
}

fn opt(mut s: Stmt) -> Stmt {
    s.optional = true;
    s
}

fn t(s: &str) -> Param {
    Param::Text(s.to_string())
}

/// `ESCAPE` character of the name filter.
const LIKE_ESC: char = '!';

/// `%q%` for a lower-cased `LIKE ... ESCAPE '!'`.
pub fn like_pattern(q: &str) -> String {
    let mut s = String::from("%");
    for c in q.to_lowercase().chars() {
        if matches!(c, '%' | '_' | LIKE_ESC) {
            s.push(LIKE_ESC);
        }
        s.push(c);
    }
    s.push('%');
    s
}

/// Adds the optional name filter (an extra `AND` in the top-level WHERE) and
/// turns the statement into `COUNT(*)` of its rows when `count`.
fn finish(
    mut sql: String,
    mut params: Vec<Param>,
    name_col: &str,
    filter: Option<&str>,
    count: bool,
) -> Stmt {
    if let Some(q) = filter.filter(|q| !q.is_empty()) {
        // The last GROUP BY / ORDER BY is the statement's own (GROUP_CONCAT has an inner ORDER BY).
        let at = sql
            .rfind(" GROUP BY ")
            .or_else(|| sql.rfind(" ORDER BY "))
            .unwrap_or(sql.len());
        sql.insert_str(
            at,
            &format!(" AND LOWER({name_col}) LIKE ? ESCAPE '{LIKE_ESC}'"),
        );
        // The filter's `?` is the last one of the WHERE: after every other bound value.
        params.push(Param::Text(like_pattern(q)));
    }
    if count {
        let from = sql.find(" FROM ").unwrap_or(0);
        sql = format!("SELECT COUNT(*) FROM (SELECT 1 AS one{}) cnt", &sql[from..]);
    }
    stmt(sql, params)
}

/// `SELECT * FROM (<inner>) WHERE ROWNUM <= TO_NUMBER(?)`; the cap is the last parameter.
fn capped(inner: &str, mut params: Vec<Param>, cap: u32) -> Stmt {
    params.push(Param::Text(cap.to_string()));
    stmt(
        format!("SELECT * FROM ({inner}) WHERE ROWNUM <= TO_NUMBER(?)"),
        params,
    )
}

fn bt(name: &str) -> String {
    format!("`{}`", name.replace('`', "``"))
}

fn ora_type(kind: DbObjectKind) -> Option<&'static str> {
    Some(match kind {
        DbObjectKind::Table => "TABLE",
        DbObjectKind::View => "VIEW",
        DbObjectKind::MaterializedView => "MATERIALIZED VIEW",
        DbObjectKind::Index => "INDEX",
        DbObjectKind::Sequence => "SEQUENCE",
        DbObjectKind::Procedure => "PROCEDURE",
        DbObjectKind::Function => "FUNCTION",
        DbObjectKind::Package => "PACKAGE",
        DbObjectKind::Trigger => "TRIGGER",
        DbObjectKind::Synonym => "SYNONYM",
        DbObjectKind::Type => "TYPE",
        _ => return None,
    })
}

impl Dialect {
    pub fn supported_kinds(self) -> Vec<DbObjectKind> {
        use DbObjectKind::*;
        match self {
            Dialect::Mysql => vec![
                Table, View, Sequence, Index, Procedure, Function, Trigger, Event,
            ],
            Dialect::Oracle => vec![
                Table,
                View,
                MaterializedView,
                Index,
                Sequence,
                Procedure,
                Function,
                Package,
                Trigger,
                Type,
                Synonym,
            ],
            Dialect::Postgres => vec![
                Table,
                View,
                MaterializedView,
                Index,
                Sequence,
                Function,
                Procedure,
                Trigger,
                Type,
                Extension,
            ],
        }
    }

    /// The column a folder's name filter applies to.
    fn name_col(self, kind: DbObjectKind) -> Option<&'static str> {
        use DbObjectKind::*;
        Some(match self {
            Dialect::Mysql => match kind {
                Table | View | Sequence => "TABLE_NAME",
                Index => "INDEX_NAME",
                Procedure | Function => "ROUTINE_NAME",
                Trigger => "TRIGGER_NAME",
                Event => "EVENT_NAME",
                _ => return None,
            },
            Dialect::Postgres => match kind {
                Table | View | MaterializedView | Sequence => "c.relname",
                Index => "ci.relname",
                Function | Procedure => "p.proname",
                Trigger => "t.tgname",
                Type => "t.typname",
                Extension => "e.extname",
                _ => return None,
            },
            Dialect::Oracle => match kind {
                Index => "INDEX_NAME",
                Trigger => "TRIGGER_NAME",
                _ => "OBJECT_NAME",
            },
        })
    }

    /// Objects of one kind in a schema (columns: name, parent, detail).
    pub fn list_objects(self, schema: &str, kind: DbObjectKind, cap: u32) -> Option<Stmt> {
        self.children_of(schema, None, kind, cap, None, false)
    }

    /// Like [`list_objects`](Self::list_objects) with a case-insensitive name
    /// filter (bound, `%_!` escaped); `count` selects `COUNT(*)` of all matches instead.
    pub fn list_filtered(
        self,
        schema: &str,
        kind: DbObjectKind,
        filter: Option<&str>,
        cap: u32,
        count: bool,
    ) -> Option<Stmt> {
        self.children_of(schema, None, kind, cap, filter, count)
    }

    /// Indexes or triggers of one table.
    pub fn children(self, schema: &str, table: &str, kind: DbObjectKind, cap: u32) -> Option<Stmt> {
        if !matches!(kind, DbObjectKind::Index | DbObjectKind::Trigger) {
            return None;
        }
        self.children_of(schema, Some(table), kind, cap, None, false)
    }

    fn children_of(
        self,
        schema: &str,
        table: Option<&str>,
        kind: DbObjectKind,
        cap: u32,
        filter: Option<&str>,
        count: bool,
    ) -> Option<Stmt> {
        if !self.supported_kinds().contains(&kind) {
            return None;
        }
        let mut p = vec![t(schema)];
        let tbl = |col: &str, p: &mut Vec<Param>| match table {
            Some(x) => {
                p.push(t(x));
                format!(" AND {col} = ?")
            }
            None => String::new(),
        };
        Some(match self {
            Dialect::Mysql => {
                use DbObjectKind::*;
                let sql = match kind {
                    Table | View | Sequence => {
                        let ty = match kind {
                            Table => "TABLE_TYPE NOT IN ('VIEW', 'SYSTEM VIEW', 'SEQUENCE')",
                            View => "TABLE_TYPE IN ('VIEW', 'SYSTEM VIEW')",
                            _ => "TABLE_TYPE = 'SEQUENCE'",
                        };
                        format!(
                            "SELECT TABLE_NAME, NULL, NULL FROM information_schema.TABLES WHERE TABLE_SCHEMA = ? AND {ty} \
                             ORDER BY TABLE_NAME LIMIT {cap}"
                        )
                    }
                    Index => {
                        let f = tbl("TABLE_NAME", &mut p);
                        format!(
                            "SELECT INDEX_NAME, TABLE_NAME, CONCAT(IF(NON_UNIQUE = 0, 'UNIQUE ', ''), '(', \
                                    GROUP_CONCAT(COLUMN_NAME ORDER BY SEQ_IN_INDEX SEPARATOR ', '), ')') \
                             FROM information_schema.STATISTICS WHERE TABLE_SCHEMA = ?{f} \
                             GROUP BY TABLE_NAME, INDEX_NAME, NON_UNIQUE ORDER BY TABLE_NAME, INDEX_NAME LIMIT {cap}"
                        )
                    }
                    Procedure | Function => {
                        let ty = if kind == Procedure {
                            "PROCEDURE"
                        } else {
                            "FUNCTION"
                        };
                        format!(
                            "SELECT ROUTINE_NAME, NULL, NULL FROM information_schema.ROUTINES \
                             WHERE ROUTINE_SCHEMA = ? AND ROUTINE_TYPE = '{ty}' ORDER BY ROUTINE_NAME LIMIT {cap}"
                        )
                    }
                    Trigger => {
                        let f = tbl("EVENT_OBJECT_TABLE", &mut p);
                        format!(
                            "SELECT TRIGGER_NAME, EVENT_OBJECT_TABLE, CONCAT(ACTION_TIMING, ' ', EVENT_MANIPULATION) \
                             FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = ?{f} ORDER BY TRIGGER_NAME LIMIT {cap}"
                        )
                    }
                    Event => format!(
                        "SELECT EVENT_NAME, NULL, STATUS FROM information_schema.EVENTS WHERE EVENT_SCHEMA = ? \
                         ORDER BY EVENT_NAME LIMIT {cap}"
                    ),
                    _ => return None,
                };
                finish(sql, p, self.name_col(kind)?, filter, count)
            }
            Dialect::Postgres => {
                use DbObjectKind::*;
                let sql = match kind {
                    Table | View | MaterializedView | Sequence => {
                        let k = match kind {
                            Table => "'r', 'p', 'f'",
                            View => "'v'",
                            MaterializedView => "'m'",
                            _ => "'S'",
                        };
                        format!(
                            "SELECT c.relname, NULL, NULL FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
                             WHERE n.nspname = ? AND c.relkind IN ({k}) ORDER BY c.relname LIMIT {cap}"
                        )
                    }
                    Index => {
                        let f = tbl("ct.relname", &mut p);
                        format!(
                            "SELECT ci.relname, ct.relname, CASE WHEN i.indisunique THEN 'UNIQUE ' ELSE '' END || \
                                    regexp_replace(pg_get_indexdef(i.indexrelid), '^.* USING [a-z_0-9]+ ', '') \
                             FROM pg_index i JOIN pg_class ci ON ci.oid = i.indexrelid JOIN pg_class ct ON ct.oid = i.indrelid \
                             JOIN pg_namespace n ON n.oid = ct.relnamespace WHERE n.nspname = ?{f} \
                             ORDER BY ct.relname, ci.relname LIMIT {cap}"
                        )
                    }
                    Function | Procedure => {
                        let kind = if kind == Function { "f" } else { "p" };
                        format!(
                            "SELECT quote_ident(p.proname) || '(' || pg_get_function_identity_arguments(p.oid) || ')', NULL, \
                                    CASE WHEN p.prokind = 'p' THEN NULL ELSE pg_get_function_result(p.oid) END \
                             FROM pg_proc p JOIN pg_namespace n ON n.oid = p.pronamespace \
                             WHERE n.nspname = ? AND p.prokind = '{kind}' ORDER BY 1 LIMIT {cap}"
                        )
                    }
                    Trigger => {
                        let f = tbl("c.relname", &mut p);
                        format!(
                            "SELECT t.tgname, c.relname, \
                                    CASE WHEN t.tgtype & 2 = 2 THEN 'BEFORE' WHEN t.tgtype & 64 = 64 THEN 'INSTEAD OF' ELSE 'AFTER' END || ' ' || \
                                    concat_ws(' OR ', CASE WHEN t.tgtype & 4 = 4 THEN 'INSERT' END, CASE WHEN t.tgtype & 8 = 8 THEN 'DELETE' END, \
                                              CASE WHEN t.tgtype & 16 = 16 THEN 'UPDATE' END, CASE WHEN t.tgtype & 32 = 32 THEN 'TRUNCATE' END) \
                             FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
                             WHERE NOT t.tgisinternal AND n.nspname = ?{f} ORDER BY t.tgname, c.relname LIMIT {cap}"
                        )
                    }
                    Type => format!(
                        "SELECT t.typname, NULL, CASE t.typtype WHEN 'e' THEN 'enum' WHEN 'd' THEN 'domain' WHEN 'r' THEN 'range' \
                                WHEN 'm' THEN 'multirange' ELSE 'composite' END \
                         FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
                         WHERE n.nspname = ? AND (t.typtype IN ('e', 'd', 'r', 'm') OR \
                               (t.typtype = 'c' AND EXISTS (SELECT 1 FROM pg_class c WHERE c.oid = t.typrelid AND c.relkind = 'c'))) \
                         ORDER BY t.typname LIMIT {cap}"
                    ),
                    Extension => format!(
                        "SELECT e.extname, NULL, e.extversion FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace \
                         WHERE n.nspname = ? ORDER BY e.extname LIMIT {cap}"
                    ),
                    _ => return None,
                };
                finish(sql, p, self.name_col(kind)?, filter, count)
            }
            Dialect::Oracle => {
                use DbObjectKind::*;
                let inner = match kind {
                    Index if table.is_some() => {
                        p.push(t(table.unwrap()));
                        "SELECT i.INDEX_NAME, i.TABLE_NAME, i.UNIQUENESS || ' (' || \
                                (SELECT LISTAGG(c.COLUMN_NAME, ', ') WITHIN GROUP (ORDER BY c.COLUMN_POSITION) \
                                 FROM ALL_IND_COLUMNS c WHERE c.INDEX_OWNER = i.OWNER AND c.INDEX_NAME = i.INDEX_NAME) || ')' \
                         FROM ALL_INDEXES i WHERE i.OWNER = ? AND i.TABLE_NAME = ? ORDER BY i.INDEX_NAME"
                            .to_string()
                    }
                    Index => "SELECT INDEX_NAME, TABLE_NAME, UNIQUENESS FROM ALL_INDEXES WHERE OWNER = ? ORDER BY TABLE_NAME, INDEX_NAME".to_string(),
                    Trigger => {
                        let f = tbl("TABLE_NAME", &mut p);
                        format!(
                            "SELECT TRIGGER_NAME, TABLE_NAME, TRIGGER_TYPE || ' ' || TRIGGERING_EVENT FROM ALL_TRIGGERS \
                             WHERE OWNER = ?{f} ORDER BY TRIGGER_NAME"
                        )
                    }
                    // Spec and body: INVALID when either is (MIN sorts it first).
                    Package | Type => {
                        let ty = ora_type(kind)?;
                        format!(
                            "SELECT OBJECT_NAME, CAST(NULL AS VARCHAR2(1)), MIN(STATUS) FROM ALL_OBJECTS                              WHERE OWNER = ? AND OBJECT_TYPE IN ('{ty}', '{ty} BODY') AND OBJECT_NAME NOT LIKE 'BIN$%'                              GROUP BY OBJECT_NAME ORDER BY OBJECT_NAME"
                        )
                    }
                    _ => {
                        let ty = ora_type(kind)?;
                        format!(
                            "SELECT OBJECT_NAME, CAST(NULL AS VARCHAR2(1)), STATUS FROM ALL_OBJECTS \
                             WHERE OWNER = ? AND OBJECT_TYPE = '{ty}' AND OBJECT_NAME NOT LIKE 'BIN$%' ORDER BY OBJECT_NAME"
                        )
                    }
                };
                if filter.is_none() && !count {
                    return Some(capped(&inner, p, cap));
                }
                let mut s = finish(inner, p, self.name_col(kind)?, filter, count);
                if !count {
                    s = capped(&s.sql, s.params, cap);
                }
                s
            }
        })
    }

    /// Parameters of a routine (columns: position, name, mode, type).
    pub fn routine_params(
        self,
        schema: &str,
        kind: DbObjectKind,
        name: &str,
        cap: u32,
    ) -> Option<Stmt> {
        let mysql_ty = match kind {
            DbObjectKind::Procedure => "PROCEDURE",
            DbObjectKind::Function => "FUNCTION",
            _ => return None,
        };
        Some(match self {
            Dialect::Mysql => stmt(
                format!(
                    "SELECT ORDINAL_POSITION, PARAMETER_NAME, PARAMETER_MODE, DTD_IDENTIFIER FROM information_schema.PARAMETERS \
                     WHERE SPECIFIC_SCHEMA = ? AND SPECIFIC_NAME = ? AND ROUTINE_TYPE = ? ORDER BY ORDINAL_POSITION LIMIT {cap}"
                ),
                vec![t(schema), t(name), t(mysql_ty)],
            ),
            Dialect::Postgres => {
                let target = PG_ROUTINE_OID;
                stmt(
                    format!(
                        "SELECT * FROM (\
                         SELECT t.ord::int, p.proargnames[t.ord]::text, \
                                CASE COALESCE(p.proargmodes[t.ord], 'i') WHEN 'i' THEN 'IN' WHEN 'b' THEN 'INOUT' WHEN 'v' THEN 'VARIADIC' ELSE 'OUT' END, \
                                format_type(t.typ, NULL) \
                         FROM pg_proc p CROSS JOIN LATERAL unnest(COALESCE(p.proallargtypes, p.proargtypes::oid[])) WITH ORDINALITY AS t(typ, ord) \
                         WHERE p.oid = {target} \
                         UNION ALL \
                         SELECT 0, NULL, NULL, format_type(p.prorettype, NULL) FROM pg_proc p WHERE p.oid = {target} AND p.prokind = 'f'\
                         ) x ORDER BY 1 LIMIT {cap}"
                    ),
                    vec![t(schema), t(name), t(schema), t(name)],
                )
            }
            Dialect::Oracle => capped(
                "SELECT POSITION, ARGUMENT_NAME, IN_OUT, DATA_TYPE FROM ALL_ARGUMENTS \
                 WHERE OWNER = ? AND OBJECT_NAME = ? AND PACKAGE_NAME IS NULL AND DATA_LEVEL = 0 ORDER BY SEQUENCE",
                vec![t(schema), t(name)],
                cap,
            ),
        })
    }

    /// The statement that returns the CREATE text, and the result column
    /// that holds it. `None` for kinds without a DDL source.
    pub fn ddl(self, schema: &str, kind: DbObjectKind, name: &str) -> Option<(Stmt, usize)> {
        use DbObjectKind::*;
        match self {
            Dialect::Mysql => {
                let (what, col) = match kind {
                    Table | View => ("TABLE", 1),
                    Sequence => ("SEQUENCE", 1),
                    Procedure => ("PROCEDURE", 2),
                    Function => ("FUNCTION", 2),
                    Trigger => ("TRIGGER", 2),
                    Event => ("EVENT", 3),
                    _ => return None,
                };
                Some((
                    stmt(
                        format!("SHOW CREATE {what} {}.{}", bt(schema), bt(name)),
                        vec![],
                    ),
                    col,
                ))
            }
            Dialect::Postgres => {
                let sql = match kind {
                    Table => {
                        r#"SELECT 'CREATE TABLE ' || quote_ident(n.nspname) || '.' || quote_ident(c.relname) || E' (\n' ||
                        string_agg('    ' || quote_ident(a.attname) || ' ' || format_type(a.atttypid, a.atttypmod) ||
                            CASE WHEN a.attnotnull THEN ' NOT NULL' ELSE '' END ||
                            COALESCE(' DEFAULT ' || pg_get_expr(d.adbin, d.adrelid), ''), E',\n' ORDER BY a.attnum) ||
                        COALESCE((SELECT E',\n    CONSTRAINT ' || string_agg(quote_ident(k.conname) || ' ' || pg_get_constraintdef(k.oid), E',\n    CONSTRAINT ' ORDER BY k.contype, k.conname)
                                  FROM pg_constraint k WHERE k.conrelid = c.oid AND k.contype <> 'n'), '') || E'\n);'
                        FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                        JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
                        LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
                        WHERE n.nspname = ? AND c.relname = ? AND c.relkind IN ('r', 'p', 'f')
                        GROUP BY n.nspname, c.relname, c.oid"#
                    }
                    View | MaterializedView => {
                        r#"SELECT CASE c.relkind WHEN 'm' THEN 'CREATE MATERIALIZED VIEW ' ELSE 'CREATE OR REPLACE VIEW ' END ||
                        quote_ident(n.nspname) || '.' || quote_ident(c.relname) || E' AS\n' || pg_get_viewdef(c.oid, true)
                        FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
                        WHERE n.nspname = ? AND c.relname = ? AND c.relkind IN ('v', 'm')"#
                    }
                    Sequence => {
                        r#"SELECT 'CREATE SEQUENCE ' || quote_ident(n.nspname) || '.' || quote_ident(c.relname) || ' AS ' || format_type(s.seqtypid, NULL) ||
                        ' START WITH ' || s.seqstart || ' INCREMENT BY ' || s.seqincrement || ' MINVALUE ' || s.seqmin || ' MAXVALUE ' || s.seqmax ||
                        ' CACHE ' || s.seqcache || CASE WHEN s.seqcycle THEN ' CYCLE' ELSE ' NO CYCLE' END || ';'
                        FROM pg_sequence s JOIN pg_class c ON c.oid = s.seqrelid JOIN pg_namespace n ON n.oid = c.relnamespace
                        WHERE n.nspname = ? AND c.relname = ?"#
                    }
                    Index => {
                        r#"SELECT pg_get_indexdef(i.indexrelid) || ';' FROM pg_index i JOIN pg_class ci ON ci.oid = i.indexrelid
                        JOIN pg_namespace n ON n.oid = ci.relnamespace WHERE n.nspname = ? AND ci.relname = ?"#
                    }
                    Function | Procedure => PG_ROUTINE_DEF.as_str(),
                    Trigger => {
                        r#"SELECT pg_get_triggerdef(t.oid, true) || ';' FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid
                        JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = ? AND t.tgname = ? AND NOT t.tgisinternal
                        ORDER BY c.relname LIMIT 1"#
                    }
                    Type => {
                        r#"SELECT 'CREATE TYPE ' || quote_ident(n.nspname) || '.' || quote_ident(t.typname) || ' AS ENUM (' ||
                        (SELECT string_agg(quote_literal(e.enumlabel), ', ' ORDER BY e.enumsortorder) FROM pg_enum e WHERE e.enumtypid = t.oid) || ');'
                        FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace
                        WHERE n.nspname = ? AND t.typname = ? AND t.typtype = 'e'"#
                    }
                    Extension => {
                        r#"SELECT 'CREATE EXTENSION ' || quote_ident(e.extname) || ' WITH SCHEMA ' || quote_ident(n.nspname) ||
                        ' VERSION ' || quote_literal(e.extversion) || ';' FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace
                        WHERE n.nspname = ? AND e.extname = ?"#
                    }
                    _ => return None,
                };
                Some((stmt(sql, vec![t(schema), t(name)]), 0))
            }
            Dialect::Oracle => {
                let ty = ora_type(kind)?.replace(' ', "_");
                Some((
                    stmt(
                        "SELECT DBMS_METADATA.GET_DDL(?, ?, ?) FROM DUAL",
                        vec![t(&ty), t(name), t(schema)],
                    ),
                    0,
                ))
            }
        }
    }

    /// Users first, then roles (columns: name, detail); roles are optional.
    pub fn principals(self, cap: u32) -> Vec<(&'static str, Stmt)> {
        match self {
            Dialect::Mysql => vec![
                (
                    "user",
                    stmt(
                        format!(
                            "SELECT DISTINCT GRANTEE, NULL FROM information_schema.USER_PRIVILEGES ORDER BY GRANTEE LIMIT {cap}"
                        ),
                        vec![],
                    ),
                ),
                (
                    "role",
                    opt(stmt(
                        format!(
                            "SELECT DISTINCT ROLE_NAME, NULL FROM information_schema.APPLICABLE_ROLES ORDER BY ROLE_NAME LIMIT {cap}"
                        ),
                        vec![],
                    )),
                ),
            ],
            Dialect::Postgres => vec![
                (
                    "user",
                    stmt(
                        format!(
                            "SELECT rolname, CASE WHEN rolsuper THEN 'superuser' END FROM pg_roles WHERE rolcanlogin ORDER BY rolname LIMIT {cap}"
                        ),
                        vec![],
                    ),
                ),
                (
                    "role",
                    stmt(
                        format!(
                            "SELECT rolname, NULL FROM pg_roles WHERE NOT rolcanlogin AND left(rolname, 3) <> 'pg_' ORDER BY rolname LIMIT {cap}"
                        ),
                        vec![],
                    ),
                ),
            ],
            Dialect::Oracle => vec![
                (
                    "user",
                    capped(
                        "SELECT USERNAME, TO_CHAR(CREATED, 'YYYY-MM-DD') FROM ALL_USERS ORDER BY USERNAME",
                        vec![],
                        cap,
                    ),
                ),
                (
                    "role",
                    opt(capped(
                        "SELECT ROLE, CAST(NULL AS VARCHAR2(1)) FROM SESSION_ROLES ORDER BY ROLE",
                        vec![],
                        cap,
                    )),
                ),
            ],
        }
    }

    /// Groups of (name, value) rows: version, charset, session variables, storage.
    pub fn server_info(self, cap: u32) -> Vec<(&'static str, Stmt)> {
        match self {
            Dialect::Mysql => vec![
                (
                    "server",
                    stmt(
                        "SELECT 'version', VERSION() UNION ALL SELECT 'version_comment', @@version_comment UNION ALL SELECT 'hostname', @@hostname",
                        vec![],
                    ),
                ),
                (
                    "charset",
                    stmt(
                        "SELECT VARIABLE_NAME, VARIABLE_VALUE FROM information_schema.GLOBAL_VARIABLES WHERE VARIABLE_NAME IN \
                     ('CHARACTER_SET_SERVER', 'CHARACTER_SET_DATABASE', 'CHARACTER_SET_CLIENT', 'CHARACTER_SET_CONNECTION', \
                      'CHARACTER_SET_RESULTS', 'COLLATION_SERVER', 'COLLATION_DATABASE', 'COLLATION_CONNECTION') ORDER BY VARIABLE_NAME",
                        vec![],
                    ),
                ),
                (
                    "session variable",
                    stmt(
                        format!(
                            "SELECT VARIABLE_NAME, VARIABLE_VALUE FROM information_schema.SESSION_VARIABLES ORDER BY VARIABLE_NAME LIMIT {cap}"
                        ),
                        vec![],
                    ),
                ),
                (
                    "storage",
                    opt(stmt(
                        format!(
                            "SELECT ENGINE, SUPPORT FROM information_schema.ENGINES ORDER BY ENGINE LIMIT {cap}"
                        ),
                        vec![],
                    )),
                ),
            ],
            Dialect::Postgres => vec![
                (
                    "server",
                    stmt(
                        "SELECT 'version', version() UNION ALL SELECT 'server_version', current_setting('server_version') \
                     UNION ALL SELECT 'database', current_database() UNION ALL SELECT 'user', current_user \
                     UNION ALL SELECT 'database size', pg_size_pretty(pg_database_size(current_database()))",
                        vec![],
                    ),
                ),
                (
                    "charset",
                    stmt(
                        "SELECT 'server_encoding', current_setting('server_encoding') UNION ALL SELECT 'client_encoding', current_setting('client_encoding') \
                     UNION ALL SELECT 'lc_collate', datcollate FROM pg_database WHERE datname = current_database()",
                        vec![],
                    ),
                ),
                (
                    "session variable",
                    stmt(
                        format!("SELECT name, setting FROM pg_settings ORDER BY name LIMIT {cap}"),
                        vec![],
                    ),
                ),
                (
                    "tablespace",
                    opt(stmt(
                        format!(
                            "SELECT spcname, pg_size_pretty(pg_tablespace_size(oid)) FROM pg_tablespace ORDER BY spcname LIMIT {cap}"
                        ),
                        vec![],
                    )),
                ),
            ],
            Dialect::Oracle => vec![
                (
                    "server",
                    capped(
                        "SELECT PRODUCT, VERSION FROM PRODUCT_COMPONENT_VERSION",
                        vec![],
                        cap,
                    ),
                ),
                (
                    "charset",
                    opt(capped(
                        "SELECT PARAMETER, VALUE FROM NLS_DATABASE_PARAMETERS WHERE PARAMETER IN ('NLS_CHARACTERSET', 'NLS_NCHAR_CHARACTERSET') ORDER BY PARAMETER",
                        vec![],
                        cap,
                    )),
                ),
                (
                    "session variable",
                    capped(
                        "SELECT PARAMETER, VALUE FROM NLS_SESSION_PARAMETERS ORDER BY PARAMETER",
                        vec![],
                        cap,
                    ),
                ),
                (
                    "tablespace",
                    opt(capped(
                        "SELECT TABLESPACE_NAME, STATUS || ', ' || CONTENTS || ', ' || LOGGING FROM USER_TABLESPACES ORDER BY TABLESPACE_NAME",
                        vec![],
                        cap,
                    )),
                ),
            ],
        }
    }
}

fn cell(r: &Row, i: usize) -> Option<String> {
    r.get(i).cloned().flatten()
}

fn unsupported() -> DbError {
    DbError::new(ErrorKind::Internal, "not supported for this object kind")
}

async fn run(fetch: &Fetch, s: Stmt) -> Result<Vec<Row>, DbError> {
    let optional = s.optional;
    match fetch(s).await {
        Err(_) if optional => Ok(Vec::new()),
        other => other,
    }
}

pub fn list_objects(
    d: Dialect,
    fetch: Fetch,
    schema: &str,
    kind: DbObjectKind,
    cap: u32,
) -> CatalogFuture<Vec<ObjectEntry>> {
    entries(d.list_objects(schema, kind, cap), fetch)
}

/// One page of a folder with the server-side name filter, plus the number of matches.
pub fn list_page(
    d: Dialect,
    fetch: Fetch,
    schema: &str,
    kind: DbObjectKind,
    filter: Option<&str>,
    cap: u32,
) -> CatalogFuture<ObjectPage> {
    let list = d.list_filtered(schema, kind, filter, cap, false);
    let count = d.list_filtered(schema, kind, filter, u32::MAX, true);
    Box::pin(async move {
        let Some(list) = list else {
            return Ok(ObjectPage {
                entries: Vec::new(),
                total: Some(0),
            });
        };
        let entries: Vec<ObjectEntry> = run(&fetch, list)
            .await?
            .iter()
            .map(|r| ObjectEntry {
                name: cell(r, 0).unwrap_or_default(),
                parent: cell(r, 1),
                detail: cell(r, 2),
            })
            .collect();
        // A short page is the whole answer; only a full one needs a count.
        let total = if (entries.len() as u64) < u64::from(cap) {
            Some(entries.len() as u64)
        } else if let Some(c) = count {
            run(&fetch, opt(c))
                .await?
                .first()
                .and_then(|r| cell(r, 0))
                .and_then(|n| n.trim().parse().ok())
        } else {
            None
        };
        Ok(ObjectPage { entries, total })
    })
}

pub fn children(
    d: Dialect,
    fetch: Fetch,
    schema: &str,
    table: &str,
    kind: DbObjectKind,
    cap: u32,
) -> CatalogFuture<Vec<ObjectEntry>> {
    entries(d.children(schema, table, kind, cap), fetch)
}

fn entries(s: Option<Stmt>, fetch: Fetch) -> CatalogFuture<Vec<ObjectEntry>> {
    Box::pin(async move {
        let Some(s) = s else { return Ok(Vec::new()) };
        Ok(run(&fetch, s)
            .await?
            .iter()
            .map(|r| ObjectEntry {
                name: cell(r, 0).unwrap_or_default(),
                parent: cell(r, 1),
                detail: cell(r, 2),
            })
            .collect())
    })
}

pub fn routine_params(
    d: Dialect,
    fetch: Fetch,
    schema: &str,
    kind: DbObjectKind,
    name: &str,
    cap: u32,
) -> CatalogFuture<Vec<RoutineParam>> {
    let s = d.routine_params(schema, kind, name, cap);
    Box::pin(async move {
        let Some(s) = s else { return Ok(Vec::new()) };
        Ok(run(&fetch, s)
            .await?
            .iter()
            .map(|r| {
                let position = cell(r, 0).and_then(|p| p.trim().parse().ok()).unwrap_or(0);
                RoutineParam {
                    position,
                    name: cell(r, 1),
                    mode: if position == 0 { None } else { cell(r, 2) },
                    type_name: cell(r, 3).unwrap_or_default(),
                }
            })
            .collect())
    })
}

pub fn ddl(
    d: Dialect,
    fetch: Fetch,
    schema: &str,
    kind: DbObjectKind,
    name: &str,
) -> CatalogFuture<String> {
    let s = d.ddl(schema, kind, name);
    Box::pin(async move {
        let (s, col) = s.ok_or_else(unsupported)?;
        let rows = run(&fetch, s).await?;
        rows.first()
            .and_then(|r| cell(r, col))
            .ok_or_else(|| DbError::new(ErrorKind::Query, "the server returned no DDL"))
    })
}

pub fn principals(d: Dialect, fetch: Fetch, cap: u32) -> CatalogFuture<Vec<Principal>> {
    let list = d.principals(cap);
    Box::pin(async move {
        let mut out = Vec::new();
        for (kind, s) in list {
            for r in run(&fetch, s).await? {
                out.push(Principal {
                    name: cell(&r, 0).unwrap_or_default(),
                    kind: kind.to_string(),
                    detail: cell(&r, 1),
                });
            }
        }
        Ok(out)
    })
}

pub fn server_info(d: Dialect, fetch: Fetch, cap: u32) -> CatalogFuture<Vec<InfoItem>> {
    let list = d.server_info(cap);
    Box::pin(async move {
        let mut out = Vec::new();
        for (group, s) in list {
            for r in run(&fetch, s).await? {
                out.push(InfoItem {
                    group: group.to_string(),
                    name: cell(&r, 0).unwrap_or_default(),
                    value: cell(&r, 1).unwrap_or_default(),
                });
            }
        }
        Ok(out)
    })
}

/// The oid of a PostgreSQL function or procedure from (schema, name). The
/// name may be the form the object list shows, `f(IN a integer)`, the
/// type-only form `f(integer)`, or a bare `f`. `to_regprocedure` accepts only
/// the type-only form and raises a syntax error on argument names or modes.
const PG_ROUTINE_OID: &str = "(SELECT p.oid FROM (SELECT ?::text AS s, ?::text AS nm) a \
     JOIN pg_namespace n ON n.nspname = a.s JOIN pg_proc p ON p.pronamespace = n.oid \
     WHERE quote_ident(p.proname) || '(' || pg_get_function_identity_arguments(p.oid) || ')' = a.nm \
        OR quote_ident(p.proname) || '(' || oidvectortypes(p.proargtypes) || ')' = a.nm \
        OR p.proname = a.nm OR quote_ident(p.proname) = a.nm \
     ORDER BY p.oid LIMIT 1)";

static PG_ROUTINE_DEF: std::sync::LazyLock<String> =
    std::sync::LazyLock::new(|| format!("SELECT pg_get_functiondef({PG_ROUTINE_OID})"));

#[cfg(test)]
mod tests {
    use super::*;
    use DbObjectKind::*;

    /// The object list shows routines as `f(IN a integer)`; the lookups must
    /// bind that text as a value (one `?` per bound param), not parse it.
    #[test]
    fn pg_routine_lookups_bind_the_shown_name() {
        let shown = "f(IN a integer)";
        for kind in [Function, Procedure] {
            let (s, _) = Dialect::Postgres.ddl("s", kind, shown).unwrap();
            assert!(!s.sql.contains("to_regprocedure"));
            assert_eq!(s.sql.matches('?').count(), s.params.len());
            let p = Dialect::Postgres
                .routine_params("s", kind, shown, 50)
                .unwrap();
            assert_eq!(p.sql.matches('?').count(), p.params.len());
        }
    }

    #[test]
    fn every_statement_binds_names_and_filters_by_owner() {
        for d in [Dialect::Mysql, Dialect::Oracle] {
            for kind in d.supported_kinds() {
                let s = d
                    .list_objects("S'; DROP", kind, 500)
                    .unwrap_or_else(|| panic!("{d:?} {kind:?}"));
                assert!(
                    !s.sql.contains("DROP") && s.params[0] == Param::Text("S'; DROP".into()),
                    "{}",
                    s.sql
                );
                assert!(s.sql.contains("= ?"), "{}", s.sql);
                if d == Dialect::Oracle {
                    assert!(
                        s.sql.contains("ROWNUM <= TO_NUMBER(?)") && s.sql.contains("OWNER = ?"),
                        "{}",
                        s.sql
                    );
                    assert_eq!(s.params.last(), Some(&Param::Text("500".into())));
                } else {
                    assert!(s.sql.contains("LIMIT 500"), "{}", s.sql);
                }
                assert!(s.sql.trim_start().to_uppercase().starts_with("SELECT"));
            }
        }
        assert!(Dialect::Oracle.list_objects("S", Event, 10).is_none());
        assert!(Dialect::Mysql.list_objects("S", Package, 10).is_none());
        let c = Dialect::Oracle.children("S", "T", Index, 10).unwrap();
        assert_eq!(
            c.params[..2],
            [Param::Text("S".into()), Param::Text("T".into())]
        );
        assert!(Dialect::Mysql.children("S", "T", Table, 10).is_none());
    }

    #[test]
    fn paging_filter_and_count_statements() {
        assert_eq!(like_pattern("a_B%!"), "%a!_b!%!!%");
        for d in [Dialect::Mysql, Dialect::Oracle, Dialect::Postgres] {
            for kind in d.supported_kinds() {
                let plain = d.list_objects("S", kind, 500).unwrap();
                let f = d
                    .list_filtered("S", kind, Some("Ab_c"), 500, false)
                    .unwrap_or_else(|| panic!("{d:?} {kind:?}"));
                let n = |s: &Stmt| s.sql.matches('?').count();
                assert_eq!(n(&f), f.params.len(), "{}", f.sql);
                assert_eq!(n(&f), plain.params.len() + 1, "{}", f.sql);
                assert!(
                    f.sql.contains("LIKE ? ESCAPE '!'")
                        && f.params.contains(&Param::Text("%ab!_c%".into())),
                    "{}",
                    f.sql
                );
                if d == Dialect::Oracle {
                    assert_eq!(
                        f.params.last(),
                        Some(&Param::Text("500".into())),
                        "{}",
                        f.sql
                    );
                } else {
                    assert!(f.sql.contains("LIMIT 500"), "{}", f.sql);
                }
                let c = d
                    .list_filtered("S", kind, Some("x"), u32::MAX, true)
                    .unwrap();
                assert!(
                    c.sql
                        .starts_with("SELECT COUNT(*) FROM (SELECT 1 AS one FROM "),
                    "{}",
                    c.sql
                );
                assert_eq!(n(&c), c.params.len(), "{}", c.sql);
                let c0 = d.list_filtered("S", kind, None, u32::MAX, true).unwrap();
                assert_eq!(n(&c0), c0.params.len(), "{}", c0.sql);
                // No filter: the same statement as the plain list.
                assert_eq!(d.list_filtered("S", kind, None, 500, false).unwrap(), plain);
            }
        }
        // The filter goes before GROUP BY / ORDER BY, not into GROUP_CONCAT.
        let s = Dialect::Mysql
            .list_filtered("S", Index, Some("x"), 10, false)
            .unwrap();
        assert!(s.sql.contains("LIKE ? ESCAPE '!' GROUP BY"), "{}", s.sql);
    }

    #[test]
    fn ddl_statements() {
        let (s, col) = Dialect::Mysql.ddl("my`db", Table, "t").unwrap();
        assert_eq!((s.sql.as_str(), col), ("SHOW CREATE TABLE `my``db`.`t`", 1));
        assert_eq!(Dialect::Mysql.ddl("s", Event, "e").unwrap().1, 3);
        assert!(Dialect::Mysql.ddl("s", Index, "i").is_none());
        let (s, _) = Dialect::Oracle.ddl("HR", MaterializedView, "MV").unwrap();
        assert_eq!(
            s.params,
            vec![
                Param::Text("MATERIALIZED_VIEW".into()),
                Param::Text("MV".into()),
                Param::Text("HR".into())
            ]
        );
        assert!(s.sql.starts_with("SELECT DBMS_METADATA.GET_DDL"));
    }

    #[test]
    fn oracle_server_reads_are_selects_with_cap() {
        for (_, s) in Dialect::Oracle
            .server_info(100)
            .into_iter()
            .chain(Dialect::Oracle.principals(100))
        {
            assert!(
                s.sql.starts_with("SELECT * FROM (") && s.sql.contains("ROWNUM <= TO_NUMBER(?)"),
                "{}",
                s.sql
            );
        }
    }
}

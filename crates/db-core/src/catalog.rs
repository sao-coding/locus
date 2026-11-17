//! Schema introspection, for the schema browser and autocomplete.

use crate::{DbError, ErrorKind};
use std::future::Future;
use std::pin::Pin;

/// Awaitable from any executor; the driver does the I/O on its own runtime.
pub type CatalogFuture<T> = Pin<Box<dyn Future<Output = Result<T, DbError>> + Send + 'static>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectKind {
    Table,
    View,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SchemaObject {
    pub name: String,
    pub kind: ObjectKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    /// Full type as the server writes it, e.g. `varchar(191)`.
    pub type_name: String,
    pub nullable: bool,
    pub primary_key: bool,
}

/// One foreign key: `table.columns` references `ref_table.ref_columns`,
/// column lists in key order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForeignKey {
    pub name: String,
    pub table: String,
    pub columns: Vec<String>,
    pub ref_schema: String,
    pub ref_table: String,
    pub ref_columns: Vec<String>,
}

pub trait Catalog: Send + Sync {
    /// Every foreign key declared on the tables of one schema, sorted by
    /// table then key name. For the relationship diagram.
    fn foreign_keys(&self, schema: &str) -> CatalogFuture<Vec<ForeignKey>>;
    /// Schemas (MySQL: databases) the user can see, in server order.
    fn schemas(&self) -> CatalogFuture<Vec<String>>;
    /// Tables and views of one schema, sorted by name.
    fn objects(&self, schema: &str) -> CatalogFuture<Vec<SchemaObject>>;
    /// Columns of one table or view, in ordinal order.
    fn columns(&self, schema: &str, object: &str) -> CatalogFuture<Vec<ColumnInfo>>;
    /// The schema new queries run in, if any (MySQL: `SELECT DATABASE()`).
    fn current_schema(&self) -> CatalogFuture<Option<String>>;

    // ---- additive: more kinds, details, DDL, server level ----
    // Every list takes a row cap (`cap`) and is filtered by schema / owner.
    // Defaults: empty list / "not supported", so a driver opts in per method.

    /// Kinds this driver can list with [`list_objects`](Self::list_objects).
    fn supported_kinds(&self) -> Vec<DbObjectKind> {
        Vec::new()
    }

    /// Objects of one kind in one schema, sorted by name, at most `cap`.
    fn list_objects(
        &self,
        schema: &str,
        kind: DbObjectKind,
        cap: u32,
    ) -> CatalogFuture<Vec<ObjectEntry>> {
        let _ = (schema, kind, cap);
        Box::pin(async { Ok(Vec::new()) })
    }

    /// One page of a folder: the first `cap` objects whose name contains
    /// `filter` (case-insensitive, evaluated on the server), and how many
    /// match in all when the driver can count. Default: `list_objects`.
    fn list_page(
        &self,
        schema: &str,
        kind: DbObjectKind,
        filter: Option<&str>,
        cap: u32,
    ) -> CatalogFuture<ObjectPage> {
        let fut = self.list_objects(schema, kind, cap);
        let f = filter.map(str::to_lowercase);
        Box::pin(async move {
            let mut entries = fut.await?;
            if let Some(f) = f {
                entries.retain(|e| e.name.to_lowercase().contains(&f));
            }
            Ok(ObjectPage {
                entries,
                total: None,
            })
        })
    }

    /// Indexes or triggers of one table (`kind` = Index / Trigger).
    fn children(
        &self,
        schema: &str,
        table: &str,
        kind: DbObjectKind,
        cap: u32,
    ) -> CatalogFuture<Vec<ObjectEntry>> {
        let _ = (schema, table, kind, cap);
        Box::pin(async { Ok(Vec::new()) })
    }

    /// Parameters of a procedure or function, in order.
    fn routine_params(
        &self,
        schema: &str,
        kind: DbObjectKind,
        name: &str,
        cap: u32,
    ) -> CatalogFuture<Vec<RoutineParam>> {
        let _ = (schema, kind, name, cap);
        Box::pin(async { Ok(Vec::new()) })
    }

    /// The CREATE text of one object. `Err` (kind Internal) when the driver
    /// cannot produce it for this kind.
    fn ddl(&self, schema: &str, kind: DbObjectKind, name: &str) -> CatalogFuture<String> {
        let _ = (schema, kind, name);
        Box::pin(async {
            Err(DbError::new(
                ErrorKind::Internal,
                "DDL is not supported for this object",
            ))
        })
    }

    /// Everything the Structure tab shows about one table or view except its
    /// DDL (columns, indexes, foreign keys both ways, checks, triggers).
    fn structure(
        &self,
        schema: &str,
        table: &str,
        cap: u32,
    ) -> CatalogFuture<crate::structure::TableStructure> {
        let _ = (schema, table, cap);
        Box::pin(async {
            Err(DbError::new(
                ErrorKind::Internal,
                "table structure is not supported by this driver",
            ))
        })
    }

    /// Users (and roles) the connected account can see.
    fn principals(&self, cap: u32) -> CatalogFuture<Vec<Principal>> {
        let _ = cap;
        Box::pin(async { Ok(Vec::new()) })
    }

    /// Server information: version, charset, session variables, storage.
    fn server_info(&self, cap: u32) -> CatalogFuture<Vec<InfoItem>> {
        let _ = cap;
        Box::pin(async { Ok(Vec::new()) })
    }
}

// ---- more object kinds, details, DDL, server level (all additive) ----

/// Kinds of objects an object browser can list under a schema. More will be
/// added: always keep a `_` arm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DbObjectKind {
    Table,
    View,
    MaterializedView,
    Index,
    Sequence,
    Procedure,
    Function,
    Package,
    Trigger,
    Event,
    Synonym,
    Type,
    Extension,
}

impl DbObjectKind {
    /// Plural English label for a tree node (the UI translates it).
    pub fn label(self) -> &'static str {
        match self {
            DbObjectKind::Table => "Tables",
            DbObjectKind::View => "Views",
            DbObjectKind::MaterializedView => "Materialized views",
            DbObjectKind::Index => "Indexes",
            DbObjectKind::Sequence => "Sequences",
            DbObjectKind::Procedure => "Procedures",
            DbObjectKind::Function => "Functions",
            DbObjectKind::Package => "Packages",
            DbObjectKind::Trigger => "Triggers",
            DbObjectKind::Event => "Events",
            DbObjectKind::Synonym => "Synonyms",
            DbObjectKind::Type => "Types",
            DbObjectKind::Extension => "Extensions",
        }
    }
}

/// One listed object. `parent` is the table an index or trigger belongs to;
/// `detail` is a short second column: index columns / uniqueness, trigger
/// timing and event, event status, object status (VALID / INVALID).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectEntry {
    pub name: String,
    pub parent: Option<String>,
    pub detail: Option<String>,
}

/// Result of [`Catalog::list_page`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectPage {
    pub entries: Vec<ObjectEntry>,
    /// Number of objects that match, when the server could count them.
    pub total: Option<u64>,
}

/// One parameter of a routine. `position` 0 with no name is the return value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoutineParam {
    pub position: u32,
    pub name: Option<String>,
    /// `IN`, `OUT`, `INOUT` (`None` for a return value).
    pub mode: Option<String>,
    pub type_name: String,
}

/// A user or role of the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Principal {
    pub name: String,
    /// `user` or `role`.
    pub kind: String,
    pub detail: Option<String>,
}

/// One line of server information. `group` is e.g. `server`, `charset`,
/// `session variable`, `storage` (MySQL engines), `tablespace` (Oracle).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InfoItem {
    pub group: String,
    pub name: String,
    pub value: String,
}

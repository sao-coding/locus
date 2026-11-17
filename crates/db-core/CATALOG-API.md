# Extended catalog API (db_core::Catalog, additive)

All new `Catalog` methods have default impls (empty list, or `Err(Internal)` for
`ddl`), so a driver that does not implement them just shows nothing.
`DbObjectKind` is `#[non_exhaustive]`: always keep a `_` arm. Every list takes a
row cap and is filtered by schema / owner. Reach it with `driver.catalog()`.

```rust
#[non_exhaustive]
pub enum DbObjectKind { Table, View, MaterializedView, Index, Sequence,
    Procedure, Function, Package, Trigger, Event, Synonym }
impl DbObjectKind { pub fn label(self) -> &'static str }   // "Tables", "Views", ...  (English, translate in UI)

pub struct ObjectEntry  { pub name: String, pub parent: Option<String>, pub detail: Option<String> }
  // parent = table of an index / trigger; detail = index "UNIQUE (a, b)" (MySQL) or UNIQUENESS (Oracle),
  // trigger "BEFORE INSERT", event status, Oracle object STATUS (VALID / INVALID)
pub struct RoutineParam { pub position: u32, pub name: Option<String>, pub mode: Option<String>, pub type_name: String }
  // position 0, name None, mode None = return value
pub struct Principal    { pub name: String, pub kind: String /* "user" | "role" */, pub detail: Option<String> }
pub struct InfoItem     { pub group: String, pub name: String, pub value: String }
  // groups: "server", "charset", "session variable", "storage" (MySQL engines), "tablespace" (Oracle)

// new methods of trait Catalog (CatalogFuture<T> = Pin<Box<dyn Future<Output = Result<T, DbError>> + Send>>)
fn supported_kinds(&self) -> Vec<DbObjectKind>;          // sync; default empty
fn list_objects(&self, schema: &str, kind: DbObjectKind, cap: u32) -> CatalogFuture<Vec<ObjectEntry>>;
fn children(&self, schema: &str, table: &str, kind: DbObjectKind, cap: u32) -> CatalogFuture<Vec<ObjectEntry>>; // kind = Index | Trigger
fn routine_params(&self, schema: &str, kind: DbObjectKind, name: &str, cap: u32) -> CatalogFuture<Vec<RoutineParam>>;
fn ddl(&self, schema: &str, kind: DbObjectKind, name: &str) -> CatalogFuture<String>;   // CREATE text; Err for kinds without one
fn principals(&self, cap: u32) -> CatalogFuture<Vec<Principal>>;
fn server_info(&self, cap: u32) -> CatalogFuture<Vec<InfoItem>>;
```

Supported kinds: MySQL / MariaDB = Table, View, Sequence (MariaDB), Index,
Procedure, Function, Trigger, Event. Oracle = Table, View, MaterializedView,
Index, Sequence, Procedure, Function, Package, Trigger, Synonym.
DDL: MySQL `SHOW CREATE ...` (no DDL for Index); Oracle `DBMS_METADATA.GET_DDL`
(Package gives the spec only). Lists are sorted by name and capped (use 1000 for
a tree node). The existing `schemas() / objects() / columns() / ...` are unchanged.

Both `db-mysql` (`MysqlDriver`) and the sidecar client (MySQL and Oracle) implement all of it.

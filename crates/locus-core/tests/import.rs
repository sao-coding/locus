//! Import (`locus_core::import`) against SQLite (temp file, always runs)
//! and the local MariaDB (`BENCH_DSN`; start it first). MariaDB uses the
//! schema `wbimport`, created and dropped by the test. Fixtures are written by
//! the test into a temp directory.

use db_core::{Cell, Driver, Outcome, QueryEvent, QueryOptions};
use locus_core::driver::AnyDriver;
use locus_core::import::infer::infer_columns;
use locus_core::import::source::{self, CsvOptions, SourceSpec};
use locus_core::import::{
    ColumnMap, ConvertOptions, ImportJob, ImportOptions, OnError, Report, TargetKind, TxScope,
    plan_new_table, run,
};
use locus_core::store::{DbKind, SavedConnection, Tls};
use locus_core::txn::Engine;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

async fn scalar(d: &dyn Driver, sql: &str) -> String {
    let mut h = d.query(sql.into(), QueryOptions::default());
    let mut v = String::new();
    while let Some(ev) = h.next().await {
        match ev {
            QueryEvent::Batch(b) if b.rows() > 0 => {
                if let Cell::Text(t) = b.cell(0, 0) {
                    v = t.to_string();
                }
            }
            QueryEvent::Finished(Outcome::Failed(e)) => panic!("{sql}: {e:?}"),
            _ => {}
        }
    }
    v
}

async fn sql(d: &dyn Driver, sql: &str) {
    let mut h = d.query(sql.into(), QueryOptions::default());
    while let Some(ev) = h.next().await {
        if let QueryEvent::Finished(Outcome::Failed(e)) = ev {
            panic!("{sql}: {e:?}");
        }
    }
}

fn conn(kind: DbKind, host: &str, port: u16, user: &str, db: Option<String>) -> SavedConnection {
    SavedConnection {
        env: Default::default(),
        kind,
        id: format!("t-{kind:?}"),
        name: "t".into(),
        host: host.into(),
        port,
        user: user.into(),
        database: db,
        tls: Tls::Disabled,
        color: None,
        icon: None,
        read_only: false,
        ssh: None,
    }
}

fn dsn() -> String {
    std::env::var("BENCH_DSN").unwrap_or_else(|_| {
        let f = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../devenv/tools/bench.env");
        let text = std::fs::read_to_string(&f)
            .expect("BENCH_DSN is not set (mysql://user:pass@host:port/db of a local MariaDB)");
        text.lines()
            .find_map(|l| l.strip_prefix("BENCH_DSN="))
            .expect("no BENCH_DSN line")
            .trim()
            .to_string()
    })
}

struct TmpDir(PathBuf);
impl TmpDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("wb-import-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TmpDir(p)
    }
    fn file(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for TmpDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Target {
    d: Arc<AnyDriver>,
    engine: Engine,
    schema: Option<String>,
}

impl Target {
    fn q(&self, table: &str) -> String {
        locus_core::import::sqlgen::qualified(self.engine, self.schema.as_deref(), table)
    }
    /// Does the table exist? (a count that fails means no)
    async fn exists(&self, table: &str) -> bool {
        let mut h = self.d.query(
            format!("SELECT COUNT(*) FROM {}", self.q(table)),
            QueryOptions::default(),
        );
        let mut ok = true;
        while let Some(ev) = h.next().await {
            if let QueryEvent::Finished(Outcome::Failed(_)) = ev {
                ok = false;
            }
        }
        ok
    }
    async fn count(&self, table: &str) -> u64 {
        scalar(&*self.d, &format!("SELECT COUNT(*) FROM {}", self.q(table)))
            .await
            .parse()
            .unwrap()
    }
}

/// Wizard step "new table": detect, preview, infer, plan.
fn new_table_job(t: &Target, spec: SourceSpec, table: &str, options: ImportOptions) -> ImportJob {
    let pv = source::preview(&spec, 100).unwrap();
    let names = source::column_names(&pv.header, pv.ncols);
    let types = infer_columns(&pv.rows, pv.ncols, &options.convert);
    let sql_types: Vec<String> = types.iter().map(|c| c.sql(t.engine)).collect();
    let (create, maps) = plan_new_table(&names, &sql_types, &[], t.engine);
    ImportJob {
        source: spec,
        engine: t.engine,
        schema: t.schema.clone(),
        table: table.into(),
        create: Some(create),
        maps,
        options,
    }
}

async fn go(t: &Target, job: ImportJob) -> Report {
    run(t.d.clone(), job, Arc::new(AtomicBool::new(false)), |_| {}).await
}

fn csv_spec(path: &Path) -> SourceSpec {
    let det = source::detect(path).unwrap();
    SourceSpec {
        path: path.to_path_buf(),
        sheet: None,
        csv: CsvOptions {
            delimiter: det.delimiter,
            encoding: det.encoding,
            ..CsvOptions::default()
        },
    }
}

const PEOPLE: &[(&str, &str, &str)] = &[
    ("1", "王小明", "2024-03-05"),
    ("2", "李大華", "2024-03-06"),
    ("3", "陳美玲", "2024-03-07"),
];

fn people_text() -> String {
    let mut s = String::from("id,姓名,生日\n");
    for (a, b, c) in PEOPLE {
        s.push_str(&format!("{a},{b},{c}\n"));
    }
    s
}

async fn check_people(t: &Target, table: &str) {
    assert_eq!(t.count(table).await, 3);
    let q = t.q(table);
    assert_eq!(
        scalar(
            &*t.d,
            &format!(
                "SELECT {} FROM {q} WHERE id = 2",
                locus_core::import::sqlgen::quote_ident(t.engine, "姓名")
            )
        )
        .await,
        "李大華"
    );
    assert_eq!(
        scalar(&*t.d, &format!("SELECT SUM(id) FROM {q}"))
            .await
            .trim_end_matches(".0"),
        "6"
    );
}

async fn encodings(t: &Target, dir: &TmpDir) {
    let text = people_text();
    let utf8 = text.clone().into_bytes();
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice(&utf8);
    let (big5, _, bad) = encoding_rs::BIG5.encode(&text);
    assert!(!bad);
    let mut u16: Vec<u8> = vec![0xFF, 0xFE];
    u16.extend(text.encode_utf16().flat_map(|c| c.to_le_bytes()));
    for (i, (name, bytes, enc)) in [
        ("utf8", utf8, "UTF-8"),
        ("bom", bom, "UTF-8"),
        ("big5", big5.into_owned(), "Big5"),
        ("utf16", u16, "UTF-16LE"),
    ]
    .into_iter()
    .enumerate()
    {
        let p = dir.file(&format!("people-{name}.csv"));
        std::fs::write(&p, bytes).unwrap();
        let spec = csv_spec(&p);
        assert_eq!(spec.csv.encoding, enc, "detected encoding of {name}");
        let table = format!("enc_{i}");
        let job = new_table_job(t, spec, &table, ImportOptions::default());
        let rep = go(t, job).await;
        assert!(rep.ok(), "{name}: {:?}", rep.fatal);
        assert_eq!(rep.inserted, 3);
        check_people(t, &table).await;
    }
}

async fn xlsx_two_sheets(t: &Target, dir: &TmpDir) {
    use rust_xlsxwriter::{Format, Workbook};
    let p = dir.file("book.xlsx");
    let mut wb = Workbook::new();
    let a = wb.add_worksheet().set_name("Other").unwrap();
    a.write_string(0, 0, "x").unwrap();
    a.write_number(1, 0, 99.0).unwrap();
    let b = wb.add_worksheet().set_name("Scores").unwrap();
    b.write_string(0, 0, "name").unwrap();
    b.write_string(0, 1, "score").unwrap();
    b.write_string(0, 2, "day").unwrap();
    let date = Format::new().set_num_format("yyyy-mm-dd");
    for i in 0..5u32 {
        b.write_string(i + 1, 0, format!("學生{i}")).unwrap();
        b.write_number(i + 1, 1, 60.5 + i as f64).unwrap();
        b.write_number_with_format(i + 1, 2, 45000.0 + i as f64, &date)
            .unwrap();
    }
    wb.save(&p).unwrap();
    assert_eq!(source::sheet_names(&p).unwrap(), vec!["Other", "Scores"]);
    let mut spec = SourceSpec::new(&p);
    spec.sheet = Some("Scores".into());
    let job = new_table_job(t, spec, "sheet2", ImportOptions::default());
    let rep = go(t, job).await;
    assert!(rep.ok(), "{:?}", rep.fatal);
    assert_eq!(rep.inserted, 5);
    let q = t.q("sheet2");
    assert_eq!(
        scalar(&*t.d, &format!("SELECT name FROM {q} WHERE name = '學生3'")).await,
        "學生3"
    );
    assert_eq!(
        scalar(&*t.d, &format!("SELECT COUNT(*) FROM {q} WHERE score > 62")).await,
        "3"
    );
    let d = scalar(&*t.d, &format!("SELECT MIN(day) FROM {q}")).await;
    assert!(d.starts_with("2023-03-15"), "{d}");
    // The first sheet by default.
    let rep = go(
        t,
        new_table_job(t, SourceSpec::new(&p), "sheet1", ImportOptions::default()),
    )
    .await;
    assert_eq!(rep.inserted, 1);
}

fn write_big(path: &Path, rows: u32) {
    let mut w = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    writeln!(w, "id,name,amount,day").unwrap();
    for i in 1..=rows {
        writeln!(
            w,
            "{i},name {i},\"{}.{:02}\",2024-01-{:02}",
            i % 1000,
            i % 100,
            1 + i % 28
        )
        .unwrap();
    }
}

async fn big_stream(t: &Target, dir: &TmpDir) {
    let p = dir.file("big.csv");
    let n = 200_000u32;
    write_big(&p, n);
    let o = ImportOptions {
        batch_size: 2000,
        ..Default::default()
    };
    let job = new_table_job(t, csv_spec(&p), "big", o);
    let mut last = 0.0;
    let mut calls = 0;
    let rep = run(t.d.clone(), job, Arc::new(AtomicBool::new(false)), |p| {
        assert!(p.fraction >= last - 1e-9);
        last = p.fraction;
        calls += 1;
    })
    .await;
    assert!(rep.ok(), "{:?}", rep.fatal);
    assert_eq!((rep.read, rep.inserted), (n as u64, n as u64));
    assert!(calls >= 1 && (last - 1.0).abs() < 1e-9);
    assert_eq!(t.count("big").await, n as u64);
    assert_eq!(
        scalar(&*t.d, &format!("SELECT MAX(id) FROM {}", t.q("big"))).await,
        n.to_string()
    );
}

async fn error_rows(t: &Target, dir: &TmpDir) {
    // Existing table: id integer primary key, qty integer, note text.
    let q = t.q("errs");
    sql(
        &*t.d,
        &format!("CREATE TABLE {q} (id INTEGER PRIMARY KEY, qty INTEGER, note VARCHAR(20))"),
    )
    .await;
    let p = dir.file("errs.csv");
    // line 3: qty is not a number; line 5: duplicate key; line 6: note too long (MariaDB strict) is not
    // portable, so it stays valid.
    std::fs::write(
        &p,
        "id,qty,note\n1,10,a\n2,abc,b\n3,30,c\n3,31,dup\n4,40,d\n",
    )
    .unwrap();
    let maps = vec![
        ColumnMap {
            source: 0,
            target: "id".into(),
            kind: TargetKind::Integer,
        },
        ColumnMap {
            source: 1,
            target: "qty".into(),
            kind: TargetKind::Integer,
        },
        ColumnMap {
            source: 2,
            target: "note".into(),
            kind: TargetKind::Text,
        },
    ];
    let job = |on_error, log: Option<PathBuf>| ImportJob {
        source: csv_spec(&p),
        engine: t.engine,
        schema: t.schema.clone(),
        table: "errs".into(),
        create: None,
        maps: maps.clone(),
        options: ImportOptions {
            on_error,
            error_log: log,
            ..ImportOptions::default()
        },
    };
    // Stop: the table stays empty, the message names the line.
    let rep = go(t, job(OnError::Stop, None)).await;
    assert!(!rep.ok() && rep.rolled_back, "{rep:?}");
    assert!(
        rep.fatal.as_deref().unwrap().contains("line 3"),
        "{:?}",
        rep.fatal
    );
    assert_eq!(t.count("errs").await, 0);
    // Skip: the good rows go in, the bad ones are listed by line and saved.
    let log = dir.file("errs.errors.csv");
    let rep = go(t, job(OnError::Skip, Some(log.clone()))).await;
    assert!(rep.ok(), "{:?}", rep.fatal);
    assert_eq!((rep.read, rep.inserted, rep.skipped), (5, 3, 2), "{rep:?}");
    let lines: Vec<u64> = rep.errors.iter().map(|e| e.line).collect();
    assert_eq!(lines, vec![3, 5]);
    assert_eq!(t.count("errs").await, 3);
    assert_eq!(rep.error_log.as_deref(), Some(log.as_path()));
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(
        text.starts_with("line,error\n3,") && text.contains("abc") && text.contains("dup"),
        "{text}"
    );
    assert_eq!(text.lines().count(), 3, "one record per line: {text}");
    // Clean run writes no log.
    let rep = go(
        t,
        ImportJob {
            options: ImportOptions {
                clear_first: true,
                error_log: Some(dir.file("none.csv")),
                ..ImportOptions::default()
            },
            source: SourceSpec {
                path: write_clean(dir),
                ..csv_spec(&p)
            },
            ..job(OnError::Stop, None)
        },
    )
    .await;
    assert!(
        rep.ok() && rep.error_log.is_none() && !dir.file("none.csv").exists(),
        "{rep:?}"
    );
    assert_eq!(
        t.count("errs").await,
        2,
        "clear_first replaced the old rows"
    );
}

fn write_clean(dir: &TmpDir) -> PathBuf {
    let p = dir.file("clean.csv");
    std::fs::write(&p, "id,qty,note\n1,1,x\n2,2,y\n").unwrap();
    p
}

async fn cancel(t: &Target, dir: &TmpDir) {
    let p = dir.file("cancel.csv");
    write_big(&p, 400_000);
    // One transaction: cancelled → nothing remains.
    let mut o = ImportOptions {
        batch_size: 1000,
        ..Default::default()
    };
    let job = new_table_job(t, csv_spec(&p), "cancel_one", o.clone());
    let flag = Arc::new(AtomicBool::new(false));
    let f2 = flag.clone();
    let rep = run(t.d.clone(), job, flag, move |p| {
        if p.read > 0 && p.fraction < 1.0 {
            f2.store(true, Ordering::Relaxed);
        }
    })
    .await;
    assert!(
        rep.cancelled && rep.rolled_back && rep.fatal.is_none(),
        "{rep:?}"
    );
    assert!(
        rep.table_dropped && !t.exists("cancel_one").await,
        "a cancelled import into a table it created leaves no empty table: {rep:?}"
    );
    assert!(rep.read < 400_000);
    // A table that was already there is never dropped: import in full, then cancel a second one.
    let full = new_table_job(t, csv_spec(&p), "cancel_exist", o.clone());
    let rep = run(t.d.clone(), full, Arc::new(AtomicBool::new(false)), |_| {}).await;
    assert!(rep.ok(), "{rep:?}");
    let mut again = new_table_job(t, csv_spec(&p), "cancel_exist", o.clone());
    again.create = None;
    let flag = Arc::new(AtomicBool::new(false));
    let f2 = flag.clone();
    let rep = run(t.d.clone(), again, flag, move |p| {
        if p.read > 0 && p.fraction < 1.0 {
            f2.store(true, Ordering::Relaxed);
        }
    })
    .await;
    assert!(
        rep.cancelled && rep.rolled_back && !rep.table_dropped,
        "{rep:?}"
    );
    assert_eq!(
        t.count("cancel_exist").await,
        400_000,
        "the rows of the first import stay"
    );
    // Per batch: what was committed stays, and the report says how much.
    o.scope = TxScope::PerBatch;
    let job = new_table_job(t, csv_spec(&p), "cancel_batch", o);
    let flag = Arc::new(AtomicBool::new(false));
    let f2 = flag.clone();
    let rep = run(t.d.clone(), job, flag, move |p| {
        if p.read > 0 && p.fraction < 1.0 {
            f2.store(true, Ordering::Relaxed);
        }
    })
    .await;
    assert!(rep.cancelled && !rep.rolled_back, "{rep:?}");
    let n = t.count("cancel_batch").await;
    assert_eq!(n, rep.inserted);
    assert!(n > 0 && n < 400_000, "{n}");
    assert!(!rep.table_dropped, "a table that holds rows is kept");
}

async fn suite(t: Target, dir: TmpDir) {
    encodings(&t, &dir).await;
    xlsx_two_sheets(&t, &dir).await;
    error_rows(&t, &dir).await;
    big_stream(&t, &dir).await;
    cancel(&t, &dir).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_import() {
    let p = std::env::temp_dir().join(format!("wb-import-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&p);
    db_sqlite::create_database(&p).unwrap();
    let c = conn(DbKind::Sqlite, "", 0, "", Some(p.display().to_string()));
    let rt = tokio::runtime::Handle::current();
    let d = Arc::new(
        AnyDriver::connect(&rt, &c, String::new(), Default::default())
            .await
            .unwrap(),
    );
    suite(
        Target {
            d,
            engine: Engine::Sqlite,
            schema: None,
        },
        TmpDir::new("sqlite"),
    )
    .await;
    let _ = std::fs::remove_file(&p);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mariadb_import() {
    let (mut c, pw) = locus_core::store::parse_dsn(&dsn()).expect("BENCH_DSN");
    c.tls = Tls::Disabled;
    let rt = tokio::runtime::Handle::current();
    let d = Arc::new(
        AnyDriver::connect(&rt, &c, pw, Default::default())
            .await
            .expect("local MariaDB: set BENCH_DSN and start the server"),
    );
    sql(&*d, "DROP DATABASE IF EXISTS wbimport").await;
    sql(&*d, "CREATE DATABASE wbimport").await;
    let t = Target {
        d: d.clone(),
        engine: Engine::Mysql,
        schema: Some("wbimport".into()),
    };
    // Always drop the schema, also when an assertion fails.
    let res = tokio::spawn(suite(t, TmpDir::new("mariadb"))).await;
    sql(&*d, "DROP DATABASE IF EXISTS wbimport").await;
    if let Err(e) = res {
        std::panic::resume_unwind(e.into_panic());
    }
}

#[test]
fn unused() {
    let _ = ConvertOptions::default();
}

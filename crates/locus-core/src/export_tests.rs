//! Unit tests of the export writers (one per format).
use crate::copyfmt::Dialect;
use crate::export::*;

fn s(v: &str) -> Option<String> {
    Some(v.to_string())
}

fn spec(kinds: &[ColKind]) -> Spec {
    let mut sp = Spec::new((0..kinds.len()).map(|i| format!("c{i}")).collect());
    sp.kinds = kinds.to_vec();
    sp
}

fn write(fmt: Format, sp: Spec, rows: &[Row]) -> String {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "wb-fmt-{:?}-{}-{}.out",
        fmt,
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let mut w = Writer::create_with(&p, fmt, sp).unwrap();
    for r in rows {
        w.row(r).unwrap();
    }
    w.finish().unwrap();
    let t = String::from_utf8(std::fs::read(&p).unwrap()).unwrap();
    let _ = std::fs::remove_file(p);
    t
}

fn sample() -> Vec<Row> {
    vec![
        vec![s("1"), s("a\tb\\c\nd"), None],
        vec![s("02"), s("中文 <b> & \"q\" | x"), s("0xCAFE")],
    ]
}

#[test]
fn tsv_escapes_tab_newline_backslash_and_empties_null() {
    let t = write(
        Format::Tsv,
        spec(&[ColKind::Number, ColKind::Text, ColKind::Binary]),
        &sample(),
    );
    assert_eq!(
        t,
        "c0\tc1\tc2\r\n1\ta\\tb\\\\c\\nd\t\r\n02\t中文 <b> & \"q\" | x\t0xCAFE\r\n"
    );
}

#[test]
fn csv_header_option() {
    let mut sp = spec(&[ColKind::Text]);
    sp.header = false;
    assert_eq!(write(Format::Csv, sp, &[vec![s("a,b")]]), "\"a,b\"\r\n");
}

#[test]
fn json_numbers_vs_text_null_and_cjk_in_column_order() {
    let mut sp = spec(&[ColKind::Number, ColKind::Text, ColKind::Binary]);
    sp.columns = vec!["z".into(), "名".into(), "a".into()];
    let t = write(Format::Json, sp, &sample());
    let v: serde_json::Value = serde_json::from_str(&t).unwrap();
    assert_eq!(v[0]["z"], 1);
    assert_eq!(v[1]["z"], "02"); // not a JSON number: stays text
    assert_eq!(v[1]["名"], "中文 <b> & \"q\" | x");
    assert_eq!(v[0]["a"], serde_json::Value::Null);
    assert!(
        t.find("\"z\"").unwrap() < t.find("\"名\"").unwrap()
            && t.find("\"名\"").unwrap() < t.find("\"a\"").unwrap()
    );
}

#[test]
fn json_empty_is_valid() {
    let t = write(Format::Json, spec(&[ColKind::Text]), &[]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&t).unwrap(),
        serde_json::json!([])
    );
}

#[test]
fn json_lines_one_object_per_line() {
    let t = write(
        Format::JsonLines,
        spec(&[ColKind::Number, ColKind::Text, ColKind::Binary]),
        &sample(),
    );
    let lines: Vec<&str> = t.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], "{\"c0\":1,\"c1\":\"a\\tb\\\\c\\nd\",\"c2\":null}");
    let v: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(v["c1"], "中文 <b> & \"q\" | x");
}

#[test]
fn markdown_table() {
    let t = write(
        Format::Markdown,
        spec(&[ColKind::Text, ColKind::Text, ColKind::Text]),
        &sample(),
    );
    let l: Vec<&str> = t.lines().collect();
    assert_eq!(l[0], "| c0 | c1 | c2 |");
    assert_eq!(l[1], "|---|---|---|");
    assert_eq!(l[2], "| 1 | a\tb\\c<br>d | NULL |");
    assert_eq!(l[3], "| 02 | 中文 <b> & \"q\" \\| x | 0xCAFE |");
}

#[test]
fn html_table_escapes() {
    let t = write(
        Format::Html,
        spec(&[ColKind::Text, ColKind::Text, ColKind::Text]),
        &sample(),
    );
    assert!(t.contains("<th>c1</th>"), "{t}");
    assert!(
        t.contains("<td>中文 &lt;b&gt; &amp; &quot;q&quot; | x</td>"),
        "{t}"
    );
    assert!(t.contains("<td class=\"null\"></td>"), "{t}");
    assert!(t.trim_end().ends_with("</html>"));
}

#[test]
fn sql_insert_batches_quoting_and_literals() {
    let mut sp = spec(&[ColKind::Number, ColKind::Text, ColKind::Binary]);
    sp.table = "my`db.t".into();
    sp.batch = 2;
    let rows = vec![
        sample()[0].clone(),
        sample()[1].clone(),
        vec![s("3"), s("it's"), None],
    ];
    let t = write(Format::Sql, sp.clone(), &rows);
    assert_eq!(
        t,
        "INSERT INTO `my``db`.`t` (`c0`, `c1`, `c2`) VALUES\n(1, 'a\tb\\\\c\nd', NULL),\n('02', '中文 <b> & \"q\" | x', 0xCAFE);\nINSERT INTO `my``db`.`t` (`c0`, `c1`, `c2`) VALUES\n(3, 'it''s', NULL);\n"
    );
    // PostgreSQL: double quotes, bytea hex, plain backslash.
    sp.dialect = Dialect::Postgres;
    sp.batch = 10;
    let t = write(Format::Sql, sp.clone(), &rows[..2]);
    assert!(
        t.starts_with(
            "INSERT INTO \"my`db\".\"t\" (\"c0\", \"c1\", \"c2\") VALUES\n(1, 'a\tb\\c\nd', NULL),"
        ),
        "{t}"
    );
    assert!(t.contains("'\\xCAFE'") && t.ends_with(");\n"), "{t}");
}

#[test]
fn sql_oracle_one_row_per_statement_and_date_literals() {
    let mut sp = spec(&[ColKind::Date, ColKind::Date]);
    sp.dialect = Dialect::Oracle;
    sp.batch = 50;
    let t = write(
        Format::Sql,
        sp,
        &[
            vec![s("2024-05-06"), s("2024-05-06 07:08:09.5")],
            vec![s("0000-00-00"), None],
        ],
    );
    assert_eq!(t.matches("INSERT INTO").count(), 2);
    assert!(
        t.contains("(DATE '2024-05-06', TIMESTAMP '2024-05-06 07:08:09.5');"),
        "{t}"
    );
    assert!(t.contains("('0000-00-00', NULL);"), "{t}");
}

#[test]
fn column_kinds_from_type_names() {
    assert_eq!(col_kind("DECIMAL(10,2)"), ColKind::Number);
    assert_eq!(col_kind("datetime"), ColKind::Date);
    assert_eq!(col_kind("TIMESTAMP WITH TIME ZONE"), ColKind::Date);
    assert_eq!(col_kind("LONGBLOB"), ColKind::Binary);
    assert_eq!(col_kind("varchar(20)"), ColKind::Text);
}

// ---- xlsx ----------------------------------------------------------------

/// One member of the xlsx (a zip) through the system `tar`, which reads zip.
fn zip_text(path: &std::path::Path, name: &str) -> String {
    let out = std::process::Command::new("tar")
        .arg("-xOf")
        .arg(path)
        .arg(name)
        .output()
        .expect("tar");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn xlsx(sp: Spec, rows: &[Row]) -> std::path::PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let p = std::env::temp_dir().join(format!(
        "wb-x-{}-{}.xlsx",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    let mut w = Writer::create_with(&p, Format::Xlsx, sp).unwrap();
    for r in rows {
        w.row(r).unwrap();
    }
    w.finish().unwrap();
    p
}

#[test]
fn xlsx_numbers_dates_text_null_cjk_header_and_freeze() {
    let rows = vec![
        vec![
            s("42"),
            s("2024-05-06"),
            s("中文 & <x>"),
            None,
            s("1234567890123456789"),
            s("007"),
        ],
        vec![
            s("-1.5"),
            s("2024-05-06 07:08:09"),
            s("=SUM(1)"),
            s("x"),
            s("1e3"),
            s("0000-00-00"),
        ],
    ];
    let sp = spec(&[
        ColKind::Number,
        ColKind::Date,
        ColKind::Text,
        ColKind::Text,
        ColKind::Number,
        ColKind::Date,
    ]);
    let p = xlsx(sp, &rows);
    let sheet = zip_text(&p, "xl/worksheets/sheet1.xml");
    assert!(
        sheet.contains("<pane ") && sheet.contains("state=\"frozen\""),
        "{sheet}"
    );
    assert!(sheet.contains("<cols>"), "{sheet}");
    // Numbers are numeric cells, long integers stay text, dates are numeric serials.
    assert!(
        sheet.contains("<v>42</v>")
            && sheet.contains("<v>-1.5</v>")
            && sheet.contains("<v>1000</v>"),
        "{sheet}"
    );
    assert!(
        sheet.contains("1234567890123456789") && !sheet.contains("<v>1234567890123456789</v>"),
        "{sheet}"
    );
    assert!(
        sheet.contains("<v>45418</v>"),
        "date serial of 2024-05-06: {sheet}"
    );
    assert!(sheet.contains("<v>45418.297"), "date-time serial: {sheet}");
    assert!(sheet.contains("中文 &amp; &lt;x&gt;"), "{sheet}");
    assert!(
        sheet.contains("=SUM(1)") && !sheet.contains("<f>"),
        "a formula-looking text stays text: {sheet}"
    );
    assert!(
        sheet.contains("0000-00-00") && sheet.contains("007"),
        "{sheet}"
    );
    // NULL: no cell for (row 2, column D).
    assert!(!sheet.contains("r=\"D2\""), "{sheet}");
    let styles = zip_text(&p, "xl/styles.xml");
    assert!(
        styles.contains("<b/>") && styles.contains("yyyy-mm-dd"),
        "{styles}"
    );
    let _ = std::fs::remove_file(p);
}

#[test]
fn xlsx_splits_into_sheets_at_the_row_limit() {
    let mut sp = spec(&[ColKind::Number]);
    sp.sheet_rows = 11; // header + 10 rows per sheet
    let rows: Vec<Row> = (0..25).map(|i| vec![Some(i.to_string())]).collect();
    let p = xlsx(sp, &rows);
    let wb = zip_text(&p, "xl/workbook.xml");
    assert!(
        wb.contains("name=\"Result\"")
            && wb.contains("name=\"Result 2\"")
            && wb.contains("name=\"Result 3\""),
        "{wb}"
    );
    assert!(!wb.contains("Result 4"), "{wb}");
    let s2 = zip_text(&p, "xl/worksheets/sheet2.xml");
    assert!(s2.contains("c0"), "each sheet repeats the header: {s2}");
    assert!(
        s2.contains("<v>10</v>") && s2.contains("<v>19</v>") && !s2.contains("<v>20</v>"),
        "{s2}"
    );
    let s3 = zip_text(&p, "xl/worksheets/sheet3.xml");
    assert!(
        s3.contains("<v>24</v>") && !s3.contains("<v>19</v>"),
        "{s3}"
    );
    let _ = std::fs::remove_file(p);
}

#[test]
fn xlsx_empty_result_has_header_only() {
    let p = xlsx(spec(&[ColKind::Text, ColKind::Text]), &[]);
    let sheet = zip_text(&p, "xl/worksheets/sheet1.xml");
    assert!(
        sheet.contains("c0") && sheet.contains("c1") && !sheet.contains("r=\"A2\""),
        "{sheet}"
    );
    let _ = std::fs::remove_file(p);
}

#[test]
fn xlsx_long_text_is_cut_and_control_chars_dropped() {
    let long = "x".repeat(40_000);
    let p = xlsx(
        spec(&[ColKind::Text, ColKind::Text]),
        &[vec![Some(long), s("a\u{1}b")]],
    );
    let sheet = zip_text(&p, "xl/worksheets/sheet1.xml");
    assert!(sheet.contains(&"x".repeat(32_767)) && !sheet.contains(&"x".repeat(32_768)));
    assert!(sheet.contains(">ab<"), "ctrl char dropped");
    let _ = std::fs::remove_file(p);
}

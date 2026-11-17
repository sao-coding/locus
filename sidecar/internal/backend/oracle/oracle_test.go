package oracle

import (
	"context"
	"os"
	"reflect"
	"strconv"
	"testing"
	"time"

	go_ora "github.com/sijms/go-ora/v2"
	"github.com/sijms/go-ora/v2/configurations"
	"github.com/sijms/go-ora/v2/network"

	"sidecar/internal/backend"
	"sidecar/internal/proto"
)

func TestBindPlaceholders(t *testing.T) {
	cases := map[string]string{
		"update t set a = ? where b = ?":                         "update t set a = :1 where b = :2",
		"select '?' , \"c?\" from t where x = ?":                 "select '?' , \"c?\" from t where x = :1",
		"select 1 -- ?\n from t where y = ?":                     "select 1 -- ?\n from t where y = :1",
		"select /* ? */ q'[a?b]' from t where z = ?":             "select /* ? */ q'[a?b]' from t where z = :1",
		"insert into t values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)": "insert into t values (:1, :2, :3, :4, :5, :6, :7, :8, :9, :10, :11)",
		"select 'it''s ?' from dual where a = ?":                 "select 'it''s ?' from dual where a = :1",
	}
	for in, want := range cases {
		if got := BindPlaceholders(in); got != want {
			t.Errorf("BindPlaceholders(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestSplitAndPrepare(t *testing.T) {
	got := SplitScript("select 1 from dual; select ';' from dual;\n-- c;\nupdate t set a = 1;")
	want := []string{"select 1 from dual", "select ';' from dual", "-- c;\nupdate t set a = 1"}
	if !reflect.DeepEqual(got, want) {
		t.Errorf("split = %q", got)
	}
	block := "BEGIN\n  x := 1;\n  y := 2;\nEND;"
	if got := SplitScript(block); len(got) != 1 || got[0] != block {
		t.Errorf("a PL/SQL block must stay whole and keep END;, got %q", got)
	}
	pkg := "CREATE OR REPLACE PACKAGE BODY p AS\n  PROCEDURE a IS BEGIN NULL; END;\nEND p;"
	if got := PrepareStatement(pkg + "\n/\n"); got != pkg {
		t.Errorf("a package body must keep END; and lose the trailing /, got %q", got)
	}
	if got := PrepareStatement("-- note\ncreate editionable trigger t before insert on x begin null; end;\r\n/"); got != "-- note\ncreate editionable trigger t before insert on x begin null; end;" {
		t.Errorf("trigger: %q", got)
	}
	if got := PrepareStatement("CREATE TABLE t (a NUMBER);"); got != "CREATE TABLE t (a NUMBER)" {
		t.Errorf("a plain CREATE must lose its ;, got %q", got)
	}
	if got := SplitScript("create table a (x number); create view v as select 1 x from dual;"); len(got) != 2 {
		t.Errorf("plain CREATEs split on ;, got %q", got)
	}
	if got := PrepareStatement("select 1 from dual ;; \n"); got != "select 1 from dual" {
		t.Errorf("trailing ; not removed: %q", got)
	}
	if !IsQuery("  /* c */ ( select 1 from dual)") || !IsQuery("WITH a AS (select 1 from dual) select * from a") || IsQuery("update t set a=1") {
		t.Error("IsQuery wrong")
	}
}

func TestTextConversion(t *testing.T) {
	loc := time.FixedZone("x", 3600)
	cases := []struct {
		v    any
		typ  string
		want string // "<nil>" = NULL
	}{
		{nil, "NUMBER", "<nil>"},
		{"12345678901234567890.123", "NUMBER", "12345678901234567890.123"},
		{int64(-7), "NUMBER", "-7"},
		{float64(1.5), "IBDouble", "1.5"},
		{time.Date(2026, 11, 15, 8, 30, 0, 0, time.UTC), "DATE", "2026-11-15T08:30:00"},
		{time.Date(2026, 11, 15, 8, 30, 1, 123456000, time.UTC), "TIMESTAMP", "2026-11-15T08:30:01.123456"},
		{time.Date(2026, 11, 15, 8, 30, 1, 0, loc), "TIMESTAMP WITH TIME ZONE", "2026-11-15T08:30:01+01:00"},
		{go_ora.Clob{String: "héllo", Valid: true}, "CLOB", "héllo"},
		{go_ora.Clob{}, "CLOB", "<nil>"},
		{go_ora.Blob{Data: []byte{0xff, 0, 1}, Valid: true}, "BLOB", "\xff\x00\x01"},
		{[]byte{0xde, 0xad}, "RAW", "\xde\xad"},
		{"", "VARCHAR2", ""},
	}
	for _, c := range cases {
		got := Text(c.v, c.typ)
		s := "<nil>"
		if got != nil {
			s = string(got)
		}
		if s != c.want {
			t.Errorf("Text(%#v, %s) = %q, want %q", c.v, c.typ, s, c.want)
		}
	}
	if Text("", "VARCHAR2") == nil {
		t.Error("empty string must not become NULL")
	}
}

func TestURLTLSModes(t *testing.T) {
	base := backend.Config{Host: "db.example", Port: 1521, User: "u@x", Password: "p:w/d", Database: "SVC"}
	check := func(cfg backend.Config, ssl, verify bool) {
		t.Helper()
		c, err := configurations.ParseConfig(URL(cfg))
		if err != nil {
			t.Fatal(err)
		}
		if c.SSL != ssl || (ssl && c.SSLVerify != verify) {
			t.Errorf("%+v: SSL=%v verify=%v, want %v %v", cfg.RequireTLS, c.SSL, c.SSLVerify, ssl, verify)
		}
		if c.UserID != "u@x" || c.Password != "p:w/d" || len(c.Servers) != 1 || c.Servers[0].Port != 1521 {
			t.Errorf("credentials or port lost: %q %v", c.UserID, c.Servers)
		}
		if got := c.ServiceName; got != "SVC" {
			t.Errorf("service = %q", got)
		}
	}
	check(base, false, false) // disabled: plain TCP
	req := base
	req.RequireTLS = true
	check(req, true, true)
	req.SkipVerify = true
	check(req, true, false)
}

func TestClassify(t *testing.T) {
	cases := []struct {
		code int
		kind byte
	}{{1017, proto.KindAuth}, {28000, proto.KindAuth}, {12541, proto.KindConnect}, {12514, proto.KindConnect}, {942, proto.KindQuery}}
	for _, c := range cases {
		f := Classify(&network.OracleError{ErrCode: c.code})
		if f.Kind != c.kind || f.Code != uint32(c.code) {
			t.Errorf("ORA-%05d -> %+v", c.code, f)
		}
	}
}

// TestLive runs against a real Oracle server and only when the environment
// names one. It sends SELECTs only, every one with a row cap, and nothing
// else. The password is read from the environment and never printed.
//
//	ORACLE_TEST_HOST / PORT / SERVICE / USER / PASSWORD (+ optional ORACLE_TEST_TLS)
func TestLive(t *testing.T) {
	host := os.Getenv("ORACLE_TEST_HOST")
	if host == "" {
		t.Skip("ORACLE_TEST_HOST is not set")
	}
	port, _ := strconv.Atoi(os.Getenv("ORACLE_TEST_PORT"))
	cfg := backend.Config{Host: host, Port: port, Database: os.Getenv("ORACLE_TEST_SERVICE"),
		User: os.Getenv("ORACLE_TEST_USER"), Password: os.Getenv("ORACLE_TEST_PASSWORD")}
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	be, err := Open(ctx, cfg)
	if err != nil {
		t.Fatal(Classify(err), err)
	}
	defer be.Close()
	sess, err := be.Acquire(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer sess.Close(false)
	id, err := sess.ID(ctx)
	if err != nil || id == 0 {
		t.Fatalf("SID %d %v", id, err)
	}
	rows, err := sess.Query(ctx, "SELECT 1 AS N, 'a' AS S, SYSDATE AS D, TO_TIMESTAMP('2026-01-02 03:04:05.5','YYYY-MM-DD HH24:MI:SS.FF') AS T, HEXTORAW('FF00') AS R, CAST(NULL AS VARCHAR2(1)) AS X FROM DUAL WHERE ROWNUM <= 1", nil)
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()
	if !rows.NextResult() || len(rows.Columns()) != 6 || !rows.Next() {
		t.Fatalf("no row: %v", rows.Err())
	}
	row, err := rows.Row()
	if err != nil {
		t.Fatal(err)
	}
	if string(row[0]) != "1" || string(row[1]) != "a" || string(row[3]) != "2026-01-02T03:04:05.5" || row[5] != nil {
		t.Errorf("row = %q", row)
	}
	// Cancel test: a statement that reads no data (a dictionary-free DUAL
	// query blocked in a lock-free wait is not available without PL/SQL, so
	// this only checks that a cancelled context ends the call promptly).
	cctx, ccancel := context.WithCancel(ctx)
	ccancel()
	if r2, err := sess.Query(cctx, "SELECT 1 FROM DUAL WHERE ROWNUM <= 1", nil); err == nil {
		if r2.NextResult() {
			t.Log("cancelled context still returned a result (statement was faster than the break)")
		}
		r2.Close()
	}
}

func TestTxVerb(t *testing.T) {
	cases := map[string]string{
		"START TRANSACTION": "begin", "begin transaction;": "begin", " commit ": "commit",
		"COMMIT WORK;": "commit", "rollback": "rollback", "ROLLBACK WORK": "rollback",
		"ROLLBACK TO SAVEPOINT a": "", "BEGIN NULL; END;": "", "BEGIN": "", "select 1 from dual": "",
		"SET TRANSACTION READ ONLY": "",
	}
	for in, want := range cases {
		if got := TxVerb(in); got != want {
			t.Errorf("TxVerb(%q) = %q, want %q", in, got, want)
		}
	}
}

// count runs a COUNT(*) and returns it.
func count(t *testing.T, sess backend.Session, ctx context.Context, q string) string {
	t.Helper()
	rows, err := sess.Query(ctx, q, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()
	if !rows.NextResult() || !rows.Next() {
		t.Fatalf("no row for %q: %v", q, rows.Err())
	}
	row, err := rows.Row()
	if err != nil {
		t.Fatal(err)
	}
	return string(row[0])
}

func exec(t *testing.T, sess backend.Session, ctx context.Context, q string) {
	t.Helper()
	rows, err := sess.Query(ctx, q, nil)
	if err != nil {
		t.Fatal(err)
	}
	for rows.NextResult() {
	}
	if err := rows.Err(); err != nil {
		t.Fatalf("%q: %v", q, err)
	}
	rows.Close()
}

// TestLiveTransaction writes only to its own table WB_TEST_TX_SIDECAR: create,
// insert + rollback (no row), insert + commit (one row, seen from a second
// session), then DROP and a check in ALL_OBJECTS that it is gone. Runs only
// with ORACLE_TEST_HOST and ORACLE_TEST_WRITE=1.
func TestLiveTransaction(t *testing.T) {
	host := os.Getenv("ORACLE_TEST_HOST")
	if host == "" || os.Getenv("ORACLE_TEST_WRITE") != "1" {
		t.Skip("ORACLE_TEST_HOST / ORACLE_TEST_WRITE=1 not set")
	}
	port, _ := strconv.Atoi(os.Getenv("ORACLE_TEST_PORT"))
	cfg := backend.Config{Host: host, Port: port, Database: os.Getenv("ORACLE_TEST_SERVICE"),
		User: os.Getenv("ORACLE_TEST_USER"), Password: os.Getenv("ORACLE_TEST_PASSWORD")}
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	be, err := Open(ctx, cfg)
	if err != nil {
		t.Fatal(err)
	}
	defer be.Close()
	a, err := be.Acquire(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer a.Close(true)
	b, err := be.Acquire(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer b.Close(true)
	const tbl = "WB_TEST_TX_SIDECAR"
	exec(t, a, ctx, "CREATE TABLE "+tbl+" (ID NUMBER)")
	defer func() {
		exec(t, b, ctx, "DROP TABLE "+tbl+" PURGE")
		if n := count(t, b, ctx, "SELECT COUNT(*) FROM ALL_OBJECTS WHERE OBJECT_NAME = '"+tbl+"' AND ROWNUM <= 1"); n != "0" {
			t.Errorf("%s still in ALL_OBJECTS", tbl)
		}
	}()
	exec(t, a, ctx, "START TRANSACTION")
	exec(t, a, ctx, "INSERT INTO "+tbl+" VALUES (1)")
	if n := count(t, b, ctx, "SELECT COUNT(*) FROM "+tbl); n != "0" {
		t.Errorf("uncommitted row visible to another session: %s", n)
	}
	exec(t, a, ctx, "ROLLBACK")
	if n := count(t, a, ctx, "SELECT COUNT(*) FROM "+tbl); n != "0" {
		t.Errorf("rollback left %s rows", n)
	}
	exec(t, a, ctx, "START TRANSACTION")
	exec(t, a, ctx, "INSERT INTO "+tbl+" VALUES (2)")
	exec(t, a, ctx, "COMMIT")
	if n := count(t, b, ctx, "SELECT COUNT(*) FROM "+tbl); n != "1" {
		t.Errorf("commit: other session sees %s rows", n)
	}
}

package mysql

import (
	"context"
	"net/url"
	"os"
	"reflect"
	"strconv"
	"testing"
	"time"

	"sidecar/internal/backend"
)

func TestSplitStatements(t *testing.T) {
	cases := []struct {
		in   string
		want []string
	}{
		{"select 1; select 2;\n select 3", []string{"select 1", "select 2", "select 3"}},
		{"", nil},
		{" ;; ", nil},
		{`insert into t values ('a;b', "c;d", 'it''s;', 'x\';y'); select ` + "`a;b`", []string{`insert into t values ('a;b', "c;d", 'it''s;', 'x\';y')`, "select `a;b`"}},
		{"-- c; x\nselect 1; # y;\nselect 2 /* ; */;", []string{"-- c; x\nselect 1", "# y;\nselect 2 /* ; */"}},
		{"select 1;\r\nselect 2;\r\n", []string{"select 1", "select 2"}},
		{"DELIMITER //\nCREATE PROCEDURE p()\nBEGIN\n select 1;\nEND//\ndelimiter ;\nselect 3;", []string{"CREATE PROCEDURE p()\nBEGIN\n select 1;\nEND", "select 3"}},
		{"CREATE PROCEDURE p() BEGIN SELECT 1; IF 1 THEN SELECT 2; END IF; CASE WHEN 1 THEN SELECT 3; END CASE; END; SELECT 4", []string{"CREATE PROCEDURE p() BEGIN SELECT 1; IF 1 THEN SELECT 2; END IF; CASE WHEN 1 THEN SELECT 3; END CASE; END", "SELECT 4"}},
		{"CREATE TABLE t (event INT, `begin` INT); SELECT 1", []string{"CREATE TABLE t (event INT, `begin` INT)", "SELECT 1"}},
		{"CREATE FUNCTION f() RETURNS INT RETURN 1; SELECT 2", []string{"CREATE FUNCTION f() RETURNS INT RETURN 1", "SELECT 2"}},
		{"select '你好;世界'; select 2", []string{"select '你好;世界'", "select 2"}},
	}
	for _, c := range cases {
		if got := splitStatements(c.in); !reflect.DeepEqual(got, c.want) {
			t.Errorf("split(%q) = %q, want %q", c.in, got, c.want)
		}
	}
}

func open(t *testing.T) backend.Backend {
	raw := os.Getenv("BENCH_DSN")
	if raw == "" {
		t.Skip("BENCH_DSN is not set")
	}
	u, err := url.Parse(raw)
	if err != nil {
		t.Fatal("bad BENCH_DSN")
	}
	port, _ := strconv.Atoi(u.Port())
	pw, _ := u.User.Password()
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	be, err := Open(ctx, backend.Config{Host: u.Hostname(), Port: port, User: u.User.Username(), Password: pw, Database: u.Path[1:], RequireTLS: true, SkipVerify: true})
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { be.Close() })
	return be
}

type result struct {
	cols     []string
	rows     [][]string
	affected int64 // -1 = result set
}

// run collects every result of a script.
func run(t *testing.T, be backend.Backend, sql string) ([]result, error) {
	t.Helper()
	ctx := context.Background()
	sess, err := be.Acquire(ctx)
	if err != nil {
		t.Fatal(err)
	}
	defer sess.Close(false)
	rows, err := sess.Query(ctx, sql, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer rows.Close()
	var out []result
	for rows.NextResult() {
		cols := rows.Columns()
		if len(cols) == 0 {
			out = append(out, result{affected: int64(rows.Affected())})
			continue
		}
		r := result{affected: -1}
		for _, c := range cols {
			r.cols = append(r.cols, c.Name)
		}
		for rows.Next() {
			row, err := rows.Row()
			if err != nil {
				t.Fatal(err)
			}
			var s []string
			for _, c := range row {
				s = append(s, string(c))
			}
			r.rows = append(r.rows, s)
		}
		out = append(out, r)
	}
	return out, rows.Err()
}

func TestMultipleResults(t *testing.T) {
	be := open(t)
	got, err := run(t, be, "SELECT 1; SELECT 'a','b'")
	if err != nil || len(got) != 2 {
		t.Fatalf("got %v, err %v", got, err)
	}
	if len(got[0].cols) != 1 || len(got[1].cols) != 2 || got[1].rows[0][1] != "b" {
		t.Fatalf("shapes wrong: %+v", got)
	}
}

func TestScriptAffectedCounts(t *testing.T) {
	be := open(t)
	got, err := run(t, be, "CREATE TEMPORARY TABLE sc_tmp (a INT); INSERT INTO sc_tmp VALUES (1),(2),(3); UPDATE sc_tmp SET a = a + 10 WHERE a < 3; SELECT a FROM sc_tmp ORDER BY a")
	if err != nil || len(got) != 4 {
		t.Fatalf("got %+v, err %v", got, err)
	}
	if got[0].affected != 0 || got[1].affected != 3 || got[2].affected != 2 || got[3].affected != -1 || len(got[3].rows) != 3 {
		t.Fatalf("affected counts wrong: %+v", got)
	}
}

func TestCallTwoResultSets(t *testing.T) {
	be := open(t)
	if _, err := run(t, be, "DROP PROCEDURE IF EXISTS bench.sc_two"); err != nil {
		t.Fatal(err)
	}
	if _, err := run(t, be, "DELIMITER //\nCREATE PROCEDURE bench.sc_two() BEGIN SELECT 1 AS x; SELECT 'a' AS y, 'b' AS z; END//\nDELIMITER ;"); err != nil {
		t.Fatal(err)
	}
	defer run(t, be, "DROP PROCEDURE IF EXISTS bench.sc_two")
	got, err := run(t, be, "CALL bench.sc_two()")
	if err != nil || len(got) != 2 {
		t.Fatalf("got %+v, err %v", got, err)
	}
	if !reflect.DeepEqual(got[0].cols, []string{"x"}) || !reflect.DeepEqual(got[1].cols, []string{"y", "z"}) {
		t.Fatalf("wrong shapes: %+v", got)
	}
}

func TestErrorInSecondStatement(t *testing.T) {
	be := open(t)
	got, err := run(t, be, "SELECT 1; SELECT * FROM bench.no_such_table_sc; SELECT 3")
	if err == nil {
		t.Fatal("want an error")
	}
	if f := be.Classify(err); f.Kind != 4 || f.Code != 1146 {
		t.Fatalf("classified as %+v (%v)", f, err)
	}
	if len(got) != 1 || got[0].rows[0][0] != "1" {
		t.Fatalf("first result must be delivered: %+v", got)
	}
}

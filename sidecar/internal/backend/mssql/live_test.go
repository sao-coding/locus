package mssql

// Against a real SQL Server; skipped unless MSSQL_TEST_HOST is set (CI starts
// one in a container, see .github/workflows/ci.yml). Writes only to #temp
// tables of its own session.
//
//	MSSQL_TEST_HOST, MSSQL_TEST_PORT (1433), MSSQL_TEST_USER, MSSQL_TEST_PASSWORD

import (
	"context"
	"os"
	"strconv"
	"testing"
	"time"

	"sidecar/internal/backend"
	"sidecar/internal/proto"
)

func liveSession(t *testing.T) (backend.Backend, backend.Session) {
	t.Helper()
	host := os.Getenv("MSSQL_TEST_HOST")
	if host == "" {
		t.Skip("MSSQL_TEST_HOST is not set")
	}
	port, err := strconv.Atoi(os.Getenv("MSSQL_TEST_PORT"))
	if err != nil {
		port = 1433
	}
	cfg := backend.Config{
		Host: host, Port: port,
		User: os.Getenv("MSSQL_TEST_USER"), Password: os.Getenv("MSSQL_TEST_PASSWORD"),
		RequireTLS: true, SkipVerify: true, // the container has a self-signed certificate
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	be, err := Open(ctx, cfg)
	if err != nil {
		t.Fatalf("open: %v (%+v)", err, Classify(err))
	}
	t.Cleanup(func() { be.Close() })
	s, err := be.Acquire(ctx)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { s.Close(true) })
	return be, s
}

// item is one result: rows (first column as text) or an affected count.
type item struct {
	rows     []string
	affected uint64
	isRows   bool
}

func run(t *testing.T, ctx context.Context, s backend.Session, q string, params ...proto.Param) ([]item, error) {
	t.Helper()
	rows, err := s.Query(ctx, q, params)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []item
	for rows.NextResult() {
		if len(rows.Columns()) == 0 {
			out = append(out, item{affected: rows.Affected()})
			continue
		}
		it := item{isRows: true, rows: []string{}}
		for rows.Next() {
			r, err := rows.Row()
			if err != nil {
				return out, err
			}
			if r[0] == nil {
				it.rows = append(it.rows, "NULL")
			} else {
				it.rows = append(it.rows, string(r[0]))
			}
		}
		if rows.Err() != nil {
			break
		}
		out = append(out, it)
	}
	return out, rows.Err()
}

func TestLiveScriptsAndCounts(t *testing.T) {
	_, s := liveSession(t)
	ctx := context.Background()
	got, err := run(t, ctx, s,
		"CREATE TABLE #t (v int)\nGO\nINSERT INTO #t VALUES (1), (2), (3); SELECT v FROM #t ORDER BY v; UPDATE #t SET v = v + 1 WHERE v > 1\nGO\nSELECT N'x' UNION ALL SELECT NULL")
	if err != nil {
		t.Fatal(err)
	}
	// INSERT 3, SELECT rows, UPDATE 2, then the second batch's rows.
	want := []item{
		{affected: 3},
		{isRows: true, rows: []string{"1", "2", "3"}},
		{affected: 2},
		{isRows: true, rows: []string{"x", "NULL"}},
	}
	if len(got) != len(want) {
		t.Fatalf("got %+v, want %+v", got, want)
	}
	for i := range want {
		if got[i].isRows != want[i].isRows || got[i].affected != want[i].affected ||
			len(got[i].rows) != len(want[i].rows) {
			t.Fatalf("item %d: got %+v, want %+v", i, got[i], want[i])
		}
		for j := range want[i].rows {
			if got[i].rows[j] != want[i].rows[j] {
				t.Fatalf("item %d: got %+v, want %+v", i, got[i], want[i])
			}
		}
	}
}

func TestLiveEmptyResultAndError(t *testing.T) {
	_, s := liveSession(t)
	got, err := run(t, context.Background(), s, "SELECT 1 AS a WHERE 1 = 0")
	if err != nil || len(got) != 1 || !got[0].isRows || len(got[0].rows) != 0 {
		t.Fatalf("empty result: %+v %v", got, err)
	}
	_, err = run(t, context.Background(), s, "SELECT * FROM no_such_table_wb")
	if f := Classify(err); err == nil || f.Kind != proto.KindQuery || f.Code != 208 {
		t.Fatalf("want error 208, got %v %+v", err, f)
	}
}

func TestLiveParamsAndTypes(t *testing.T) {
	_, s := liveSession(t)
	ctx := context.Background()
	got, err := run(t, ctx, s, "SELECT ? + N'!'", proto.Param{Kind: proto.ParamText, Data: []byte("hi")})
	if err != nil || len(got) != 1 || got[0].rows[0] != "hi!" {
		t.Fatalf("params: %+v %v", got, err)
	}
	cases := map[string]string{
		"SELECT CAST('2024-05-06T07:08:09.5' AS datetime2(1))":                    "2024-05-06T07:08:09.5",
		"SELECT CAST('2024-05-06' AS date)":                                       "2024-05-06",
		"SELECT CAST('12.50' AS decimal(10,2))":                                   "12.50",
		"SELECT CAST('6F9619FF-8B86-D011-B42D-00C04FC964FF' AS uniqueidentifier)": "6F9619FF-8B86-D011-B42D-00C04FC964FF",
		"SELECT CAST(1 AS bit)":                                                   "1",
	}
	for q, want := range cases {
		got, err := run(t, ctx, s, q)
		if err != nil || len(got) != 1 || got[0].rows[0] != want {
			t.Errorf("%s: got %+v %v, want %s", q, got, err, want)
		}
	}
}

// The point of using go-mssqldb: a cancel stops only the statement, and the
// session keeps its open transaction and #temp tables.
func TestLiveCancelKeepsTransaction(t *testing.T) {
	_, s := liveSession(t)
	bg := context.Background()
	if _, err := run(t, bg, s, "CREATE TABLE #t (v int); BEGIN TRANSACTION; INSERT INTO #t VALUES (1), (2)"); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(bg)
	done := make(chan error, 1)
	go func() {
		_, err := run(t, ctx, s, "WAITFOR DELAY '00:00:30'")
		done <- err
	}()
	time.Sleep(500 * time.Millisecond)
	t0 := time.Now()
	cancel()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("cancelled statement reported success")
		}
	case <-time.After(10 * time.Second):
		t.Fatal("cancel did not stop the statement within 10 s")
	}
	t.Logf("cancel took %v", time.Since(t0))
	got, err := run(t, bg, s, "SELECT @@TRANCOUNT; SELECT COUNT(*) FROM #t")
	if err != nil {
		t.Fatalf("session unusable after cancel: %v", err)
	}
	if len(got) != 2 || got[0].rows[0] != "1" || got[1].rows[0] != "2" {
		t.Fatalf("transaction or temp table lost after cancel: %+v", got)
	}
	if _, err := run(t, bg, s, "ROLLBACK"); err != nil {
		t.Fatal(err)
	}
}

func TestLiveKillSession(t *testing.T) {
	be, s := liveSession(t)
	id, err := s.ID(context.Background())
	if err != nil || id == 0 {
		t.Fatalf("id: %d %v", id, err)
	}
	if err := be.KillSession(context.Background(), 32000); err != nil {
		t.Fatalf("KILL of a missing session should count as done: %v", err)
	}
}

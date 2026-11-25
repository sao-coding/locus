package mssql

import (
	"reflect"
	"testing"
)

func TestSplitBatches(t *testing.T) {
	cases := map[string][]string{
		"select 1\nGO\nselect 2\n go ; \nselect 3": {"select 1\n", "select 2\n", "select 3"},
		"select 'a\ngo\nb'":                        {"select 'a\ngo\nb'"},
		"/* x\ngo\n*/ select 1":                    {"/* x\ngo\n*/ select 1"},
		"select 1 -- c\ngo\n":                      {"select 1 -- c\n"},
		"go\n\ngo":                                 nil,
		"select gone\n":                            {"select gone\n"},
	}
	for in, want := range cases {
		if got := SplitBatches(in); !reflect.DeepEqual(got, want) {
			t.Errorf("SplitBatches(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestBindPlaceholders(t *testing.T) {
	cases := map[string]string{
		"update t set a = ?, [b?] = '?' where c = ? -- ?\n": "update t set a = @p1, [b?] = '?' where c = @p2 -- ?\n",
		"select '?''?' , ? /* ? */":                         "select '?''?' , @p1 /* ? */",
		"select ?,?,?,?,?,?,?,?,?,?":                        "select @p1,@p2,@p3,@p4,@p5,@p6,@p7,@p8,@p9,@p10",
		"select N'it''s'":                                   "select N'it''s'",
	}
	for in, want := range cases {
		if got := BindPlaceholders(in); got != want {
			t.Errorf("BindPlaceholders(%q) = %q, want %q", in, got, want)
		}
	}
}

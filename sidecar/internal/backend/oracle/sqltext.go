package oracle

import (
	"strconv"
	"strings"
)

// scan walks sql and calls fn for every byte outside string literals, quoted
// identifiers and comments. q-quoted strings (q'[...]') are handled.
func scan(sql string, fn func(i int)) {
	n := len(sql)
	for i := 0; i < n; {
		c := sql[i]
		switch {
		case (c == 'q' || c == 'Q') && i+2 < n && sql[i+1] == '\'' && !isIdent(prev(sql, i)):
			closer := map[byte]byte{'[': ']', '{': '}', '(': ')', '<': '>'}[sql[i+2]]
			if closer == 0 {
				closer = sql[i+2]
			}
			j := strings.Index(sql[i+3:], string(closer)+"'")
			if j < 0 {
				return
			}
			i += 3 + j + 2
		case c == '\'' || c == '"':
			j := i + 1
			for j < n {
				if sql[j] == c {
					if j+1 < n && sql[j+1] == c {
						j += 2
						continue
					}
					break
				}
				j++
			}
			i = j + 1
		case c == '-' && i+1 < n && sql[i+1] == '-':
			j := strings.IndexByte(sql[i:], '\n')
			if j < 0 {
				return
			}
			i += j + 1
		case c == '/' && i+1 < n && sql[i+1] == '*':
			j := strings.Index(sql[i+2:], "*/")
			if j < 0 {
				return
			}
			i += 2 + j + 2
		default:
			fn(i)
			i++
		}
	}
}

func prev(s string, i int) byte {
	if i == 0 {
		return ' '
	}
	return s[i-1]
}

func isIdent(c byte) bool {
	return c == '_' || c == '$' || c == '#' || (c >= '0' && c <= '9') || (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')
}

// firstWord is the first keyword of a statement, upper case, skipping
// comments, blanks and opening parentheses.
func firstWord(sql string) string {
	s := sql
	for {
		s = strings.TrimLeft(s, " \t\r\n(")
		switch {
		case strings.HasPrefix(s, "--"):
			j := strings.IndexByte(s, '\n')
			if j < 0 {
				return ""
			}
			s = s[j+1:]
		case strings.HasPrefix(s, "/*"):
			j := strings.Index(s, "*/")
			if j < 0 {
				return ""
			}
			s = s[j+2:]
		default:
			j := 0
			for j < len(s) && isIdent(s[j]) {
				j++
			}
			return strings.ToUpper(s[:j])
		}
	}
}

// IsQuery reports whether the statement returns rows (SELECT or WITH).
func IsQuery(sql string) bool {
	w := firstWord(sql)
	return w == "SELECT" || w == "WITH"
}

func isBlock(sql string) bool {
	switch firstWord(sql) {
	case "BEGIN", "DECLARE":
		return true
	case "CREATE":
		return isPLSQLUnit(sql)
	}
	return false
}

// isPLSQLUnit reports whether a CREATE statement makes a PL/SQL unit
// (CREATE [OR REPLACE] [EDITIONABLE | NONEDITIONABLE] PACKAGE [BODY] |
// PROCEDURE | FUNCTION | TRIGGER | TYPE [BODY]): its `END;` is part of it.
// A plain CREATE TABLE / VIEW / INDEX loses its trailing `;` like any
// other statement. Comments in front of CREATE are skipped by firstWord's
// caller; here the words after it are read.
func isPLSQLUnit(sql string) bool {
	s := strings.TrimLeft(sql, " \t\r\n(")
	for strings.HasPrefix(s, "--") || strings.HasPrefix(s, "/*") {
		if strings.HasPrefix(s, "--") {
			j := strings.IndexByte(s, '\n')
			if j < 0 {
				return false
			}
			s = strings.TrimLeft(s[j+1:], " \t\r\n(")
		} else {
			j := strings.Index(s, "*/")
			if j < 0 {
				return false
			}
			s = strings.TrimLeft(s[j+2:], " \t\r\n(")
		}
	}
	w := strings.Fields(strings.ToUpper(strings.NewReplacer("(", " ", "\"", " ").Replace(s)))
	i := 1 // w[0] is CREATE
	if i+1 < len(w) && w[i] == "OR" && w[i+1] == "REPLACE" {
		i += 2
	}
	if i < len(w) && (w[i] == "EDITIONABLE" || w[i] == "NONEDITIONABLE") {
		i++
	}
	if i >= len(w) {
		return false
	}
	switch w[i] {
	case "PACKAGE", "PROCEDURE", "FUNCTION", "TRIGGER", "TYPE", "LIBRARY", "JAVA":
		return true
	}
	return false
}

// PrepareStatement removes the terminating `;` of a plain statement (Oracle
// rejects it). PL/SQL blocks and CREATE statements keep theirs: `END;` is
// part of the block.
func PrepareStatement(sql string) string {
	s := strings.TrimSpace(sql)
	if isBlock(s) {
		return strings.TrimRight(strings.TrimSuffix(s, "\n/"), " \t\r\n")
	}
	for strings.HasSuffix(s, ";") {
		s = strings.TrimRight(strings.TrimSuffix(s, ";"), " \t\r\n")
	}
	return s
}

// SplitScript splits a script on `;` outside strings and comments. A script
// that starts with BEGIN / DECLARE / CREATE is one statement (its inner `;`
// belong to the block).
func SplitScript(sql string) []string {
	if isBlock(sql) {
		if s := PrepareStatement(sql); s != "" {
			return []string{s}
		}
		return nil
	}
	var out []string
	start := 0
	add := func(end int) {
		if s := PrepareStatement(sql[start:end]); s != "" && firstWord(s) != "" {
			out = append(out, s)
		}
	}
	scan(sql, func(i int) {
		if sql[i] == ';' {
			add(i)
			start = i + 1
		}
	})
	add(len(sql))
	return out
}

// BindPlaceholders rewrites the host's `?` placeholders into Oracle's
// positional `:1`, `:2`, ... outside strings, quoted identifiers and
// comments.
func BindPlaceholders(sql string) string {
	var b strings.Builder
	last, n := 0, 0
	scan(sql, func(i int) {
		if sql[i] == '?' {
			n++
			b.WriteString(sql[last:i])
			b.WriteString(":" + strconv.Itoa(n))
			last = i + 1
		}
	})
	b.WriteString(sql[last:])
	return b.String()
}

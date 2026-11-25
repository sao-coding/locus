package mssql

import (
	"strconv"
	"strings"
)

// SplitBatches splits a script into batches on lines that hold only GO (any
// case, optional trailing ';' and spaces), the way SSMS and sqlcmd do. GO is a
// client-side separator; the server never sees it. Lines inside string
// literals or block comments are not separators. Empty batches are dropped.
func SplitBatches(sql string) []string {
	var out []string
	var cur strings.Builder
	st := stCode
	depth := 0
	push := func() {
		if strings.TrimSpace(cur.String()) != "" {
			out = append(out, cur.String())
		}
		cur.Reset()
	}
	for _, line := range strings.SplitAfter(sql, "\n") {
		if line == "" {
			continue
		}
		if st == stCode && isGo(line) {
			push()
			continue
		}
		st, depth = scanLine(line, st, depth)
		cur.WriteString(line)
	}
	push()
	return out
}

func isGo(line string) bool {
	t := strings.TrimRight(strings.TrimSpace(line), "; \t")
	return strings.EqualFold(t, "go")
}

const (
	stCode = iota
	stStr
	stBlock
)

// scanLine returns the lexer state at the end of line.
func scanLine(line string, st, depth int) (int, int) {
	b := line
	for i := 0; i < len(b); i++ {
		switch st {
		case stCode:
			switch {
			case b[i] == '\'':
				st = stStr
			case b[i] == '-' && i+1 < len(b) && b[i+1] == '-':
				return stCode, 0
			case b[i] == '/' && i+1 < len(b) && b[i+1] == '*':
				st, depth = stBlock, 1
				i++
			}
		case stStr:
			if b[i] == '\'' {
				if i+1 < len(b) && b[i+1] == '\'' {
					i++
				} else {
					st = stCode
				}
			}
		case stBlock:
			switch {
			case b[i] == '*' && i+1 < len(b) && b[i+1] == '/':
				depth--
				if depth == 0 {
					st = stCode
				}
				i++
			case b[i] == '/' && i+1 < len(b) && b[i+1] == '*':
				depth++
				i++
			}
		}
	}
	return st, depth
}

// BindPlaceholders rewrites the protocol's '?' placeholders (outside strings,
// [names], "names" and comments) to @p1, @p2, ... in order.
func BindPlaceholders(sql string) string {
	var out strings.Builder
	n := 0
	for i := 0; i < len(sql); i++ {
		c := sql[i]
		switch {
		case c == '\'' || c == '"':
			j := i + 1
			for j < len(sql) {
				if sql[j] == c {
					if j+1 < len(sql) && sql[j+1] == c {
						j += 2
						continue
					}
					break
				}
				j++
			}
			end := min(j+1, len(sql))
			out.WriteString(sql[i:end])
			i = end - 1
		case c == '[':
			j := strings.IndexByte(sql[i:], ']')
			end := len(sql)
			if j >= 0 {
				end = i + j + 1
			}
			out.WriteString(sql[i:end])
			i = end - 1
		case c == '-' && i+1 < len(sql) && sql[i+1] == '-':
			j := strings.IndexByte(sql[i:], '\n')
			end := len(sql)
			if j >= 0 {
				end = i + j
			}
			out.WriteString(sql[i:end])
			i = end - 1
		case c == '/' && i+1 < len(sql) && sql[i+1] == '*':
			depth, j := 0, i
			for j < len(sql) {
				if sql[j] == '/' && j+1 < len(sql) && sql[j+1] == '*' {
					depth++
					j += 2
					continue
				}
				if sql[j] == '*' && j+1 < len(sql) && sql[j+1] == '/' {
					depth--
					j += 2
					if depth == 0 {
						break
					}
					continue
				}
				j++
			}
			out.WriteString(sql[i:j])
			i = j - 1
		case c == '?':
			n++
			out.WriteString("@p")
			out.WriteString(strconv.Itoa(n))
		default:
			out.WriteByte(c)
		}
	}
	return out.String()
}

package mysql

import "strings"

// splitStatements cuts a script into statements, without the terminating
// delimiter and without surrounding blanks. It is the Go twin of
// db_core::sql::split_statements and follows the same rules: no split inside
// '..' ".." `..` strings, `-- ` / `#` / `/* */` comments, and the client
// command `DELIMITER xx` changes the terminator (and is not returned).
//
// The sidecar runs the statements one by one because go-sql-driver cannot
// report the affected rows of each statement of a multi-statement call.
func splitStatements(sql string) []string {
	var out []string
	delim := ";"
	start := -1
	hasCode := false
	n := len(sql)
	emit := func(end int) {
		if s := strings.TrimRight(sql[start:end], " \t\r\n\f\v"); s != "" {
			out = append(out, s)
		}
	}
	for i := 0; i < n; {
		c := sql[i]
		if c == ' ' || c == '\t' || c == '\r' || c == '\n' || c == '\f' || c == '\v' {
			i++
			continue
		}
		if c == delim[0] && strings.HasPrefix(sql[i:], delim) {
			if delim == ";" && hasCode && start >= 0 && openRoutine(sql[start:i]) {
				i += len(delim) // a `;` inside a routine body
				continue
			}
			if hasCode && start >= 0 {
				emit(i)
			}
			start, hasCode = -1, false
			i += len(delim)
			continue
		}
		if !hasCode && (c == 'd' || c == 'D') && isDelimiterCmd(sql, i) {
			end := strings.IndexByte(sql[i:], '\n')
			if end < 0 {
				end = n
			} else {
				end += i
			}
			if arg := strings.TrimSpace(sql[i+9 : end]); arg != "" {
				delim = arg
			}
			start = -1
			i = end
			continue
		}
		if start < 0 {
			start = i
		}
		switch {
		case c == '#':
			i = lineEnd(sql, i)
		case c == '-' && i+1 < n && sql[i+1] == '-' && (i+2 >= n || sql[i+2] <= ' '):
			i = lineEnd(sql, i)
		case c == '/' && i+1 < n && sql[i+1] == '*':
			if p := strings.Index(sql[i+2:], "*/"); p >= 0 {
				i += 2 + p + 2
			} else {
				i = n
			}
		case c == '\'' || c == '"' || c == '`':
			hasCode = true
			i = quotedEnd(sql, i)
		default:
			hasCode = true
			i++
		}
	}
	if hasCode && start >= 0 {
		emit(n)
	}
	return out
}

func isDelimiterCmd(sql string, i int) bool {
	const word = "delimiter"
	if len(sql) < i+len(word)+1 || !strings.EqualFold(sql[i:i+len(word)], word) {
		return false
	}
	if sql[i+len(word)] != ' ' && sql[i+len(word)] != '\t' {
		return false
	}
	// Only blanks between the line start and the word.
	prefix := sql[:i]
	nl := strings.LastIndexByte(prefix, '\n')
	return strings.TrimLeft(prefix[nl+1:], " \t\r") == ""
}

func lineEnd(sql string, i int) int {
	if p := strings.IndexByte(sql[i:], '\n'); p >= 0 {
		return i + p
	}
	return len(sql)
}

func quotedEnd(sql string, i int) int {
	q := sql[i]
	for j := i + 1; j < len(sql); {
		switch c := sql[j]; {
		case c == '\\' && q != '`':
			j += 2
		case c == q:
			if j+1 < len(sql) && sql[j+1] == q {
				j += 2
			} else {
				return j + 1
			}
		default:
			j++
		}
	}
	return len(sql)
}

// openRoutine reports whether chunk is a CREATE PROCEDURE / FUNCTION /
// TRIGGER / EVENT whose BEGIN ... END block is still open. A `;` inside such
// a body does not end the statement: the server parses the whole text as one
// statement, and the app sends a routine as one statement (the DELIMITER
// trick is a client-side convention only).
func openRoutine(chunk string) bool {
	words := scanWords(chunk, 8)
	if len(words) == 0 || !strings.EqualFold(words[0], "create") {
		return false
	}
	routine := false
	for _, w := range words[1:] {
		switch strings.ToLower(w) {
		case "procedure", "function", "trigger", "event":
			routine = true
		case "table", "view", "index", "database", "schema", "sequence", "user":
			return false
		}
	}
	if !routine {
		return false
	}
	depth := 0
	all := scanWords(chunk, -1)
	for i, w := range all {
		if i > 0 && strings.EqualFold(all[i-1], "end") && strings.EqualFold(w, "case") {
			continue // the CASE of END CASE closes, it does not open
		}
		switch strings.ToLower(w) {
		case "begin", "case":
			depth++
		case "end":
			if i+1 < len(all) {
				switch strings.ToLower(all[i+1]) {
				case "if", "loop", "while", "repeat":
					continue
				}
			}
			depth--
		}
	}
	return depth > 0
}

// scanWords returns the identifier-like words of sql outside strings,
// quoted identifiers and comments; max < 0 means all.
func scanWords(sql string, max int) []string {
	var words []string
	n := len(sql)
	for i := 0; i < n && (max < 0 || len(words) < max); {
		c := sql[i]
		switch {
		case c == '#':
			i = lineEnd(sql, i)
		case c == '-' && i+1 < n && sql[i+1] == '-' && (i+2 >= n || sql[i+2] <= ' '):
			i = lineEnd(sql, i)
		case c == '/' && i+1 < n && sql[i+1] == '*':
			if p := strings.Index(sql[i+2:], "*/"); p >= 0 {
				i += 2 + p + 2
			} else {
				i = n
			}
		case c == '\'' || c == '"' || c == '`':
			i = quotedEnd(sql, i)
		case c == '_' || c == '$' || c >= 0x80 || (c|0x20 >= 'a' && c|0x20 <= 'z') || (c >= '0' && c <= '9'):
			j := i + 1
			for j < n && (sql[j] == '_' || sql[j] == '$' || sql[j] >= 0x80 || (sql[j]|0x20 >= 'a' && sql[j]|0x20 <= 'z') || (sql[j] >= '0' && sql[j] <= '9')) {
				j++
			}
			words = append(words, sql[i:j])
			i = j
		default:
			i++
		}
	}
	return words
}

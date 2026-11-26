# Parameters and variables

Locus has two different things that look alike. Know which one you are using.

|  | Parameter | Variable |
|---|---|---|
| Written as | `:name`, `?`, `@name`, `$name`, `$1`, `&name` | `${name}` |
| Set by | the Run dialog | `@set name = value` or the Run dialog |
| What happens | sent to the database as a **bound value** | **pasted into the SQL text** before anything else |
| Can be a table name, keyword, clause | no | yes |
| Protects against SQL injection | yes | no, it is text |

The Production guard, the confirmation dialogs, the audit log and the History
all see the statement **after** the variables are substituted, so a variable
cannot hide a `DROP` or an `UPDATE` without `WHERE` from them.

## Parameters

A statement with placeholders opens the **Run dialog**: one row per name, with a
type (text, number, date, NULL). Values are remembered per tab. Enter runs, Esc
cancels, Tab moves between the rows and the first empty row has the focus.
Parameters work with one statement at a time.

Settings › Queries decides what counts as a placeholder (like DBeaver's
"Named parameter prefix"):

| Setting | Default | Meaning |
|---|---|---|
| Query parameters | on | master switch; off = every statement is sent as written |
| Parameter prefix `:name` | on | `:id` |
| Parameter prefix `@name` | off | `@id`, on engines where it is not native |
| Parameter prefix `$name` | off | `$id`, on engines where it is not native |
| Anonymous parameters `?` | on | MySQL, SQLite, SQL Server (never PostgreSQL, where `?` is an operator) |

The defaults give the behaviour from before these settings existed. Each engine's
own styles stay on: `$1` on PostgreSQL, `&name` on Oracle, `@name` / `$name` /
`?NNN` on SQLite.

Where `@name` clashes with the engine, with `@` enabled:

- **MySQL / MariaDB** (`@var` is a session variable) and **SQL Server** (`@var`
  is a T-SQL variable): a `@name` that the same script assigns or declares
  (`SET @n =`, `@n :=`, `SELECT .. INTO @n`, `DECLARE @n`, SQL Server
  `SELECT @n = ..`) is left alone, so `SET @x = 1; SELECT @x;` keeps working.
  `@@system` variables are never parameters.
- **PostgreSQL**: `@` is an operator (`@>`, `@@`, `@ x`). Only `@` directly
  followed by a name counts (`@id`), never `@ x`.
- **Oracle**: `@file` at the start of a line is a SQL*Plus script call. `t@link`
  is a database link.
- Strings, quoted identifiers, comments, dollar quotes (`$$ .. $$`), Oracle
  `q'[..]'` literals and `::` casts never count.

## Variables

Client-side commands, one per line, at the start of a statement (as in DBeaver):

| Command | Meaning |
|---|---|
| `@set name = value` | defines `name`; the value is the rest of the line, trimmed (a final `;` is dropped) and may use other variables |
| `@unset name` | removes `name` |
| `@echo text` | prints `text` (variables substituted) first in the Messages tab |

`${name}` in a statement is replaced by the value. The command lines are never
sent to the database. Variables are kept for the tab, so later runs see them.

```sql
@set tbl = booking_rec
@echo reading ${tbl}
SELECT * FROM ${tbl} WHERE id = :id;
```

An undefined `${name}` opens the Run dialog with a **Variable** row for it
(parameters are marked **Parameter**), instead of failing. What you type is kept
for the tab.

Run (Ctrl+Enter) on an `@set` line defines just that variable. Typing `${` in the
editor suggests the variable names (kept ones and the `@set` lines of the text).

**Show variables** in the command palette lists the tab's variables: edit a
value, delete one, or add a new one.

### Not done

- `@include` (running another file) is not supported.
- `:{name}` is not recognised; use `${name}`.
- Variables and parameter values are per tab; there is no "remember across tabs
  for this connection" yet.
- The editor does not give parameters and `${var}` their own colour yet.

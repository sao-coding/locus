# MCP server

Locus has a built-in [Model Context Protocol](https://modelcontextprotocol.io) server. AI tools
such as Claude Code or Cursor can list your connections, read schemas and run SQL through it,
under the same per-connection rules you set in Locus.

- Transport: MCP **Streamable HTTP**, served inside the running `locus.exe` (no second program,
  no stdio mode). It works while Locus is open.
- Address: `http://127.0.0.1:<port>/mcp`, port **7781** unless you pick another.
- **Off by default.** Nothing listens until you switch it on.

## Setup

1. Settings › MCP › switch **MCP server** on. The status line says `Listening on
   http://127.0.0.1:7781/mcp`. If the port is busy it says so; choose another port and press
   Apply (Locus never moves to another port by itself).
2. Copy one of the snippets on that page. They contain the token.

**Claude Code**

```sh
claude mcp add --transport http locus http://127.0.0.1:7781/mcp --header "Authorization: Bearer <token>"
```

**Cursor** (`.cursor/mcp.json`, or `~/.cursor/mcp.json`) and other clients that take JSON:

```json
{
  "mcpServers": {
    "locus": {
      "url": "http://127.0.0.1:7781/mcp",
      "headers": { "Authorization": "Bearer <token>" }
    }
  }
}
```

The status bar shows **MCP** while the server is on, and `MCP · run_sql on <connection>` while
a client is working; hover it to see which client. Statements sent through MCP appear in
History with an **MCP** badge.

## Permissions

Each connection has an **MCP access** level (connection dialog › MCP page). Without a choice it
follows the connection's environment:

| Environment | Default |
|---|---|
| Local, Dev, Team, Staging | Read-write |
| Prod | Read-only |

| Access | Reads | Writes and DDL |
|---|---|---|
| Off | The connection is invisible to MCP (not listed, not found by id or name) | — |
| Read-only | Run inside a server-side read-only transaction | Refused before anything is sent |
| Read-write, not Production | Run | Run at once, no question |
| Read-write, Production | Run | Locus shows a dialog (connection, "requested by MCP client …", the SQL). **Allow** runs it; **Deny**, closing the dialog, or 60 s without an answer refuses it |

Settings › MCP lists every connection with its access, with a link to change it.

Redis connections are never offered (they have no SQL).

## Security model

- **Loopback only.** The server binds `127.0.0.1`, never `0.0.0.0`.
- **Bearer token.** Every request needs `Authorization: Bearer <token>`. The token is 32 random
  bytes (base64url), kept in Windows Credential Manager (`Locus/mcp/token`), compared in
  constant time, and can be regenerated in Settings (old clients stop working at once). With
  `LOCUS_DATA_DIR` set (test and demo data folders) the token is kept in memory only.
- **DNS-rebinding protection.** A request whose `Origin` header is present and not
  `http(s)://localhost`, `127.0.0.1` or `[::1]` is refused (403), as the MCP specification asks
  of local servers. The `Host` header must be `127.0.0.1:<port>`, `localhost:<port>` or
  `[::1]:<port>` (403 otherwise). These checks run before the token check.
- **Limits.** Request bodies over 1 MiB are refused (413). One statement may run 30 s; it is then
  stopped on the server. When the client cancels or disconnects, the running query is cancelled
  on the server as well (KILL / interrupt).
- **Same guard as your tabs.** All SQL is split with the editor's statement splitter, classified
  by the same classifier as the Production guard, and decided with
  `locus_core::guard::decide` for the connection's environment. Unsure text (an unterminated
  string, a MySQL `/*! … */` comment, an unknown keyword) counts as a write.
- **Defence in depth for Read-only.** Besides the classifier, a read-only connection's session is
  read-only on the server, and its statements run inside a read-only transaction:

  | Engine | Session | Transaction |
  |---|---|---|
  | MySQL / MariaDB | `SET SESSION TRANSACTION READ ONLY` | `START TRANSACTION READ ONLY` … `ROLLBACK` |
  | PostgreSQL | `default_transaction_read_only = on` | `BEGIN READ ONLY` … `ROLLBACK` |
  | Oracle | — | `SET TRANSACTION READ ONLY` … `ROLLBACK` |
  | SQLite | `PRAGMA query_only` (and the file's own read-only flag) | — |
  | SQL Server | — | — (classifier only) |

  Transaction control (`BEGIN`, `COMMIT`, `ROLLBACK`) is refused on Read-only connections, so a
  script cannot end the read-only transaction.
- **Own sessions.** MCP opens its own connections (one per connection and access level), never
  the sessions of your tabs, so it cannot run inside a manual transaction you have open. SSH
  tunnels, the Oracle / SQL Server helper and saved passwords work as in the tabs.
- **Audit.** Every statement MCP sends, refuses or asks about is written to `audit.log` with
  `"source":"mcp"` and the client name (events `mcp_sent`, `blocked`, `confirmed_sent`,
  `mcp_denied`, `mcp_timeout`).
- Passwords are never part of any tool output. `list_connections` shows host, port and database
  name.
- Variables of the editor (`${x}`, `@set`) do not apply: MCP SQL is sent as given.

The client name comes from the client's `initialize` request (stateless HTTP requests do not
repeat it; Locus remembers it per User-Agent).

## Tools

Every tool takes `connection` as the connection's **id or unique name** (case-insensitive when
not exact). Errors are short and say what to do, for example
`connection 'prod-db' is read-only for MCP; DELETE refused (nothing was sent)`.

| Tool | Arguments | Returns |
|---|---|---|
| `list_connections` | — | id, name, kind, env, access, host:port/database |
| `list_objects` | `connection`, `schema?`, `kind?` (`all` / `table` / `view` / `routine`) | Without `schema`: the schemas (current one marked). With it: one object per line |
| `describe_table` | `connection`, `table`, `schema?` | Columns (type, nullability, default), PK, indexes, FKs both ways. No DDL |
| `run_sql` | `connection`, `sql`, `max_rows?` | Per statement a table or `N rows affected`; statements are numbered when there are several, and the run stops at the first error |
| `explain` | `connection`, `sql` (one statement) | The plan as indented steps with their share of the cost (MySQL / MariaDB, PostgreSQL, SQLite) |
| `get_ddl` | `connection`, `object`, `schema?` | The `CREATE` text of a table, view or routine |

`schema` defaults to the connection's current schema; `table` / `object` may also be written
`schema.name`.

## Results and limits

Results are compact text, not JSON per row:

- TSV with the header once; `NULL` for nulls, bytes as `0x…`, tabs and line breaks escaped.
- 200 rows by default; `max_rows` up to **5000**.
- Cells longer than 200 characters are cut and marked `…[+N]` (N characters more).
- A footer: `(3 rows)`, or `(200 of 18,342 rows shown, truncated)`. Rows past the shown ones
  are counted up to 100,000 more, then reading stops (`100,200+`).

For a 200-row, 5-column result this is roughly a third of the tokens the same rows cost as
JSON objects (`[{"id":1,"name":…}, …]`), because the column names are sent once.

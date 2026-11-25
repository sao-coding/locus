---
default: minor
---

# Built-in MCP server (Settings › MCP, off by default): Claude Code, Cursor and other MCP clients can list connections, read schemas, explain and run SQL over `http://127.0.0.1:7781/mcp` with a bearer token. Each connection has MCP access Off / Read-only / Read-write (Production defaults to read-only); a Production write asks in Locus first, MCP runs show in the status bar and in History with an MCP badge, and every statement is audited.

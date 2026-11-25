---
default: minor
---

# SQL Server connections, through the built-in sidecar on Microsoft's go-mssqldb: scripts with `GO` batches, streamed result sets, bound parameters, transactions, and a schema browser with tables, views, columns, primary and foreign keys. Cancel and timeout stop only the running statement on the server and keep the connection and an open transaction; when the server ends the transaction itself (`XACT_ABORT ON`, deadlock) the tab says so. Object kinds, DDL, routines, the structure editor and the activity view are not there yet for SQL Server.

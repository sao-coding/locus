---
default: minor
---

# Parameters follow DBeaver: Settings › Queries has switches for `:name`, `@name`, `$name` and `?` (and a master switch); on MySQL, SQL Server, PostgreSQL and Oracle an `@name` that the script itself sets, an operator or a SQL*Plus `@file` is left alone. New client-side variables: `@set name = value`, `@unset`, `@echo` and `${name}` text substitution (the Production guard and audit see the substituted statement), an undefined `${name}` asks in the Run dialog, "Show variables" in the palette lists, edits and deletes them, and `${` completes variable names. See docs/parameters.md.

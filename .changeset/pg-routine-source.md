---
default: patch
---

# PostgreSQL procedure and function source

Opening the source or parameters of a PostgreSQL procedure or function whose arguments have names or modes (`IN p_id integer`) no longer fails with `syntax error at or near "IN"`.

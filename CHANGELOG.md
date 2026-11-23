# Changelog
## 0.1.1 (2026-11-23)

### Fixes

- Fix the `dev` feature build

#### Column header filter and order menu

Each result column header has a dropdown with Order by ASC / DESC, filters from the selected cell, a typed value or the clipboard (=, <>, >, <, LIKE, ILIKE), IS NULL / IS NOT NULL, and a raw WHERE clause. Filters re-run the query on the server, show as removable chips above the grid, and can be saved per query.

# Changelog
## 0.1.2 (2026-11-24)

### Fixes

- completion after JOIN, single kill confirm, Structure tab section, cancelled import cleanup (#31)
- Completion now opens right after the space that follows `JOIN`, `FROM`, `UPDATE`, `INTO` or `ON`, listing tables (or columns after `ON`) before keywords. (#11)
- Cancelling an import into a new table now drops the table it created when it is still empty. (#14)
- Killing a query from the activity page on a Production connection asks for confirmation once instead of twice. (#12)
- Reopening a Structure tab returns to the section last used for that table. (#13)

## 0.1.1 (2026-11-23)

### Fixes

- Fix the `dev` feature build

#### Column header filter and order menu

Each result column header has a dropdown with Order by ASC / DESC, filters from the selected cell, a typed value or the clipboard (=, <>, >, <, LIKE, ILIKE), IS NULL / IS NOT NULL, and a raw WHERE clause. Filters re-run the query on the server, show as removable chips above the grid, and can be saved per query.

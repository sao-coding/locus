---
default: minor
---

# Git panel: the folder bound to a Space is its work tree. The right rail has a Git panel (Ctrl+Shift+G) with the branch switcher (list, create, switch), ahead / behind, Fetch, Pull, Push (cancellable, git's own messages shown), staged and unstaged changes with stage, unstage and confirmed discard, a commit box (Ctrl+Enter, amend, "commit all" when nothing is staged, hooks respected) and paged history with per-file history. A click opens a read-only diff tab (unified or side by side, SQL colours). Tabs of changed files show M / A / U / C, the status bar shows `branch · N changes`, and committing warns about passwords in connection URLs, `password=` lines, private keys and `.env` files. It runs your installed git, so your credentials and hooks apply; Settings › Git sets the executable, pull mode, auto-fetch and the credential scan.

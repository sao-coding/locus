# Changelog
## 0.2.0 (2026-11-25)

### Features

- Space colour picker redesign (1-3 colours, presets, intensity, light/dark) and popover click-through fix (#34)
- Inno Setup installer next to the portable exe (#37)
- sidecar hello handshake with protocol and build check (#38)
- name result tabs after their table; off-white grid with zebra striping (#35)
- check GitHub for a new release and show it (#40)
- download, verify and install updates (#41)
- Releases now include an installer, `locus-<version>-windows-x64-setup.exe`: per-user install with a Start menu entry and an uninstaller, next to the portable exe.
- Result tabs of a multi-statement run are named after the table a SELECT reads (schema dropped, duplicates numbered, full statement in the tooltip). The result grid gets an off-white body in the light theme and optional subtle zebra striping (Settings → Appearance).
- Redesigned Space colour picker: one to three colours on a soft hue / saturation pad (two or three tint the window as a gradient), nine preset swatches, an intensity slider, and a per-Space light / dark / follow-the-app choice. Spaces saved before keep their colour.
- Locus now checks GitHub for a new release about once a day and shows it in the status bar; Settings › About has the details, a "Check now" button and an off switch.
- Locus can update itself: a new version is downloaded in the background, its minisign signature checked, and installed when you pick "Restart and update" (installer copies run the new setup silently, portable copies swap the exe). Scoop copies are still updated with `scoop update`.

### Fixes

- release builds know their full version (RC suffix) (#46)
- Release downloads now come with minisign signatures (`*.minisig`); the public key is `minisign.pub` in the repository.
- The Oracle helper (sidecar) now reports its protocol and build first; Locus refuses one from another build before it connects, and skips an outdated `sidecar.exe` next to `locus.exe` in favour of the built-in copy.
- Clicks and drags on the Space popover no longer reach what is under it (the sidebar edge, tabs).

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

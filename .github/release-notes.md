## Download
| File | |
|---|---|
| `locus-{version}-windows-x64-setup.exe` | Installer, Windows 10/11 x64. Installs for the current user (no admin) with a Start menu entry and an uninstaller. |
| `locus-{version}-windows-x64.exe` | Portable, Windows 10/11 x64. One file, nothing to install; the Oracle helper is embedded. |
| `*.sha256` | SHA-256 checksums |
| `THIRD_PARTY_LICENSES.md` | Licenses of bundled dependencies |

Verify: `(Get-FileHash .\locus-{version}-windows-x64.exe).Hash -eq (Get-Content .\locus-{version}-windows-x64.exe.sha256).Split()[0]`
Provenance: `gh attestation verify locus-{version}-windows-x64.exe -R sao-coding/locus`

> The exes are not code-signed yet, so Windows SmartScreen may warn on first run: choose "More info" → "Run anyway".

## What's changed
{changelog}
{extra}

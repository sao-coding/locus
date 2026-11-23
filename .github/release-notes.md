## Download
| File | |
|---|---|
| `locus-{version}-windows-x64.exe` | Portable, Windows 10/11 x64. No installer; the Oracle helper is embedded. |
| `locus-{version}-windows-x64.exe.sha256` | SHA-256 checksum |
| `THIRD_PARTY_LICENSES.md` | Licenses of bundled dependencies |

Verify: `(Get-FileHash .\locus-{version}-windows-x64.exe).Hash -eq (Get-Content .\locus-{version}-windows-x64.exe.sha256).Split()[0]`
Provenance: `gh attestation verify locus-{version}-windows-x64.exe -R sao-coding/locus`

> The exe is not code-signed yet, so Windows SmartScreen may warn on first run: choose "More info" → "Run anyway".

## What's changed
{changelog}
{extra}

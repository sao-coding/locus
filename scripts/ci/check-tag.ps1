# Release guard: the pushed tag (GITHUB_REF_NAME, or -Tag) must be v<version> from Cargo.toml, optionally
# with a -suffix (v0.2.0-rc.1).
param([string]$Tag = $env:GITHUB_REF_NAME)
$ErrorActionPreference = 'Stop'
$cargo = Join-Path (Split-Path (Split-Path $PSScriptRoot)) 'Cargo.toml'
$v = (Select-String -Path $cargo -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
if (-not $Tag) { throw 'no tag: set GITHUB_REF_NAME or pass -Tag' }
if ($Tag -ne "v$v" -and $Tag -notlike "v$v-*") { throw "tag $Tag does not match Cargo.toml version $v" }
Write-Host "tag $Tag matches Cargo.toml $v"

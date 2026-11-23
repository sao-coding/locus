# Release guard: the tag (-Tag, or GITHUB_REF_NAME) must be vX.Y.Z or vX.Y.Z-rc.N, and X.Y.Z must be
# the Cargo.toml version.
param([string]$Tag = $env:GITHUB_REF_NAME)
$ErrorActionPreference = 'Stop'
$cargo = Join-Path (Split-Path (Split-Path $PSScriptRoot)) 'Cargo.toml'
$v = (Select-String -Path $cargo -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value
if (-not $Tag) { throw 'no tag: set GITHUB_REF_NAME or pass -Tag' }
if ($Tag -notmatch '^v\d+\.\d+\.\d+(-rc\.\d+)?$') { throw "unrecognized tag $Tag (expected v1.2.3 or v1.2.3-rc.N)" }
if ($Tag -ne "v$v" -and $Tag -notlike "v$v-rc.*") { throw "tag $Tag does not match Cargo.toml version $v" }
Write-Host "tag $Tag matches Cargo.toml $v"

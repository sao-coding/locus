# Renders .github/release-notes.md for a release: the CHANGELOG.md section for -Version without its
# heading, the file names, and the compare link from the previous v* tag. Used by release.yml;
# run it locally to preview: ./scripts/ci/release-notes.ps1 -Version 0.1.1
# -Version may carry a suffix (0.2.0-rc.1); its notes come from the 0.2.0 section when there is one.
param(
  [Parameter(Mandatory)][string]$Version,
  [string]$Out
)
$ErrorActionPreference = 'Stop'
$root = Split-Path (Split-Path $PSScriptRoot)
$Version = $Version.TrimStart('v')
$base = $Version -replace '-.*$', ''

$lines = Get-Content (Join-Path $root 'CHANGELOG.md')
$body = @()
$in = $false
foreach ($l in $lines) {
  if ($l -match '^## (\S+)') {
    if ($in) { break }
    if ($Matches[1] -eq $Version -or $Matches[1] -eq $base) { $in = $true; continue }
  }
  if ($in) { $body += $l }
}
$changelog = ($body -join "`n").Trim()
if (-not $changelog) { $changelog = "See [CHANGELOG.md](https://github.com/sao-coding/locus/blob/v$Version/CHANGELOG.md)." }

$prev = git -C $root tag --list 'v*' --sort=-v:refname | Where-Object { $_ -ne "v$Version" } | Select-Object -First 1
$compare = if ($prev) { "https://github.com/sao-coding/locus/compare/$prev...v$Version" }
           else { "https://github.com/sao-coding/locus/commits/v$Version" }

$text = (Get-Content (Join-Path $root '.github/release-notes.md') -Raw -Encoding utf8).
  Replace('{changelog}', $changelog).Replace('{compare}', $compare).Replace('{version}', $Version)
if ($Out) { [IO.File]::WriteAllText($Out, $text, [Text.UTF8Encoding]::new($false)) } else { $text }

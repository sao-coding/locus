# Renders .github/release-notes.md for a release: the CHANGELOG.md section for -Version without its
# heading, "Thanks to" for contributors written as [@name], and the compare link from the previous
# v* tag (omitted when there is none). Used by release.yml; preview: ./scripts/ci/release-notes.ps1 -Version 0.1.1
# -Version may carry a suffix (0.2.0-rc.1); its notes come from the 0.2.0 section when there is one.
param(
  [Parameter(Mandatory)][string]$Version,
  [string]$Out
)
$ErrorActionPreference = 'Stop'
$root = Split-Path (Split-Path $PSScriptRoot)
$Version = $Version.TrimStart('v')
$base = $Version -replace '-.*$', ''

$body = @()
$in = $false
foreach ($l in Get-Content (Join-Path $root 'CHANGELOG.md')) {
  if ($l -match '^## (\S+)') {
    if ($in) { break }
    if ($Matches[1] -eq $Version -or $Matches[1] -eq $base) { $in = $true; continue }
  }
  if ($in) { $body += $l }
}
$changelog = ($body -join "`n").Trim()
if (-not $changelog) { $changelog = "See [CHANGELOG.md](https://github.com/sao-coding/locus/blob/v$Version/CHANGELOG.md)." }

$extra = ''
# Plain @name: GitHub builds its Contributors row from those; knope writes [@name](url).
$who = [regex]::Matches($changelog, '\[@([A-Za-z0-9-]+)\]') | ForEach-Object { '@' + $_.Groups[1].Value } | Sort-Object -Unique
if ($who) { $extra += "`nThanks to $($who -join ' ')`n" }
# Previous tag: from the release commit's parent (the tag if it exists, else HEAD).
$at = if (git -C $root rev-parse -q --verify "refs/tags/v$Version") { "v$Version^" } else { 'HEAD' }
$prev = git -C $root describe --tags --abbrev=0 --match 'v*' $at 2>$null
if ($LASTEXITCODE -eq 0 -and $prev) {
  $extra += "`n**Full Changelog**: https://github.com/sao-coding/locus/compare/$prev...v$Version`n"
}

$text = (Get-Content (Join-Path $root '.github/release-notes.md') -Raw -Encoding utf8).
  Replace('{changelog}', $changelog).Replace('{extra}', $extra.TrimEnd()).Replace('{version}', $Version).TrimEnd() + "`n"
if ($Out) { [IO.File]::WriteAllText($Out, $text, [Text.UTF8Encoding]::new($false)) } else { $text }

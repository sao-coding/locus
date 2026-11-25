# Release signing: signs every .exe in -Dist with minisign, as <file>.minisig, and checks each
# signature against the committed public key (minisign.pub at the repo root), so a secret that
# does not match it fails the release instead of publishing signatures nobody can check.
#
# The key comes from MINISIGN_SECRET_KEY (the text of a key file made with `minisign -G -W`, no
# password); it is written to a temp file only for the signing and removed afterwards. The trusted
# comment, which the key signs too, is "file:<name> version:<version>": Locus's updater checks both,
# so a signature can't be passed off for another file or an older release.
#
#   $env:MINISIGN_SECRET_KEY = Get-Content -Raw key; ./scripts/ci/sign.ps1 -Dist dist -Version 0.1.3
param(
    [Parameter(Mandatory)][string]$Dist,
    [Parameter(Mandatory)][string]$Version,
    [string]$PublicKey = (Join-Path (Split-Path (Split-Path $PSScriptRoot)) 'minisign.pub')
)
$ErrorActionPreference = 'Stop'
if (-not $env:MINISIGN_SECRET_KEY) { throw 'MINISIGN_SECRET_KEY is not set' }
if (-not (Get-Command minisign -ErrorAction SilentlyContinue)) { throw 'minisign is not on PATH' }
if (-not (Test-Path $PublicKey)) { throw "no public key at $PublicKey" }
$files = @(Get-ChildItem $Dist -Filter *.exe)
if (-not $files) { throw "no .exe in $Dist" }

$key = Join-Path ([IO.Path]::GetTempPath()) "minisign-$([guid]::NewGuid()).key"
try {
    [IO.File]::WriteAllText($key, $env:MINISIGN_SECRET_KEY.Trim() + "`n")
    if (-not $IsWindows) { chmod 600 $key }
    foreach ($f in $files) {
        $comment = "file:$($f.Name) version:$Version"
        # No password: stdin is empty so an encrypted key fails here instead of waiting for one.
        '' | minisign -S -s $key -m $f.FullName -t $comment
        if ($LASTEXITCODE) { throw "signing $($f.Name) failed" }
        minisign -V -q -p $PublicKey -m $f.FullName
        if ($LASTEXITCODE) { throw "$($f.Name).minisig does not verify with $PublicKey (wrong MINISIGN_SECRET_KEY?)" }
        $trusted = (Get-Content "$($f.FullName).minisig" | Where-Object { $_ -like 'trusted comment: *' })
        if ($trusted -ne "trusted comment: $comment") { throw "unexpected trusted comment in $($f.Name).minisig: $trusted" }
        Write-Host "signed $($f.Name) ($comment)"
    }
}
finally {
    Remove-Item $key -Force -ErrorAction SilentlyContinue
}

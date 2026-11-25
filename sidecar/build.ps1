# Builds sidecar\bin\sidecar.exe, stamped with the workspace version
# (Cargo.toml [workspace.package]) that its hello frame reports.
Set-Location $PSScriptRoot
$m = Select-String -Path ..\Cargo.toml -Pattern '^version = "([^"]+)"' | Select-Object -First 1
if (-not $m) { throw "no workspace version in Cargo.toml" }
$version = $m.Matches[0].Groups[1].Value
go build -trimpath -ldflags "-s -w -X main.build=$version" -o bin\sidecar.exe .
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

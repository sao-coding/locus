# Local use only: stops the servers started by setup-*.ps1 (by the PIDs they recorded, nothing else).
. "$PSScriptRoot\lib.ps1"
$d = Join-Path (Get-CiRoot) 'pids'
Get-ChildItem $d -Filter *.pid -ErrorAction SilentlyContinue | ForEach-Object {
  $id = [int](Get-Content $_.FullName)
  $p = Get-Process -Id $id -ErrorAction SilentlyContinue
  if ($p -and $_.BaseName -like 'postgres-*') {
    $port = $_.BaseName -replace '\D'
    $pgbin = if ($env:PGBIN) { $env:PGBIN } else { Split-Path (Get-Process -Id $id).Path }
    & "$pgbin\pg_ctl.exe" stop -D "$(Get-CiRoot)\postgres-$port\data" -m fast | Out-Null
  } elseif ($p) { Stop-Process -Id $id }
  Remove-Item $_.FullName
  Write-Host "stopped $($_.BaseName) (pid $id)"
}

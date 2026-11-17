# Fails fast, with the server logs, when a server the tests need is not accepting connections.
# Reads BENCH_DSN, REDIS_URL and PG_TEST_PORT from the environment (what the setup scripts exported).
. "$PSScriptRoot\lib.ps1"
$ports = [ordered]@{}
if ($env:BENCH_DSN -match '@[^:/]+:(\d+)/') { $ports['MariaDB'] = [int]$Matches[1] }
if ($env:REDIS_URL -match '@[^:/]+:(\d+)/') { $ports['Redis'] = [int]$Matches[1] }
if ($env:PG_TEST_PORT) { $ports['PostgreSQL'] = [int]$env:PG_TEST_PORT }
$down = @()
foreach ($n in $ports.Keys) {
  $c = [System.Net.Sockets.TcpClient]::new()
  try { $c.ConnectAsync('127.0.0.1', $ports[$n]).Wait(3000) | Out-Null; $ok = $c.Connected } catch { $ok = $false } finally { $c.Dispose() }
  Write-Host "$n 127.0.0.1:$($ports[$n]) $(if ($ok) { 'up' } else { 'DOWN' })"
  if (-not $ok) { $down += $n }
}
if ($down) {
  foreach ($f in Get-ChildItem (Get-CiRoot) -Recurse -Include mariadbd.log, redis.out, redis.err, postgres.log -ErrorAction SilentlyContinue) {
    Write-Host "::group::$($f.Name)"; Get-Content $f -Tail 60; Write-Host '::endgroup::'
  }
  throw "not running: $($down -join ', ')"
}

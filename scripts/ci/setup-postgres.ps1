# Starts a throwaway PostgreSQL cluster for crates\db-postgres\tests\live.rs using the PostgreSQL that the
# GitHub windows runner image already has ($env:PGBIN; its own service stays disabled and is not touched).
# Locally pass -PgBin or install PostgreSQL first. Creates role/database `locus_ci` with a random password
# and exports PG_TEST_HOST / PG_TEST_PORT / PG_TEST_USER / PG_TEST_DB / PG_TEST_PASSWORD.
# Every external call is non-interactive and has a timeout; the server is started by pg_ctl (the only one that
# drops administrator rights) with its output in files, so nothing waits on a pipe the daemon inherited.
param(
  [int]$Port = 5432,
  [string]$PgBin = $env:PGBIN
)
. "$PSScriptRoot\lib.ps1"
if (-not $PgBin) {
  $PgBin = Get-ChildItem 'C:\Program Files\PostgreSQL' -Directory -ErrorAction SilentlyContinue |
    Sort-Object { [int]($_.Name -replace '\D', '') } | Select-Object -Last 1 | ForEach-Object { "$($_.FullName)\bin" }
}
if (-not $PgBin -or -not (Test-Path "$PgBin\initdb.exe")) { throw "PostgreSQL binaries not found (PGBIN='$PgBin'); pass -PgBin" }
if (-not (Test-PortFree $Port)) { throw "port $Port is already in use; refusing to touch whatever listens there" }
Write-Phase "using $PgBin"

$tools = Join-Path (Get-CiRoot) "postgres-$Port"
if (Test-Path $tools) { Remove-Item -Recurse -Force $tools }
New-Item -ItemType Directory -Force $tools | Out-Null
$data = "$tools\data"
$su = New-RandomPassword
$pw = New-RandomPassword
Set-Content -Path "$tools\su.pw" -Value $su -Encoding ascii -NoNewline

Write-Phase 'initdb'
Invoke-Native "$PgBin\initdb.exe" @('-D', $data, '-U', 'postgres', '--auth=scram-sha-256', "--pwfile=$tools\su.pw", '-E', 'UTF8') 180 | Out-Null
Remove-Item "$tools\su.pw"

Write-Phase 'starting postgres'
# pg_ctl (not postgres.exe) because only pg_ctl drops the administrator rights the runner user has;
# postgres.exe refuses to run with them. Its output goes to files, so nothing waits on a pipe, and it
# returns once the server is up (-w).
$ctl = Start-Process -FilePath "$PgBin\pg_ctl.exe" -PassThru -WindowStyle Hidden `
  -ArgumentList "start -D `"$data`" -l `"$tools\postgres.log`" -w -t 60 -o `"-p $Port -c listen_addresses=127.0.0.1`"" `
  -RedirectStandardOutput "$tools\pg_ctl.out" -RedirectStandardError "$tools\pg_ctl.err"
if (-not $ctl.WaitForExit(90000)) { $ctl.Kill($true); throw "pg_ctl start did not return in 90 s`n$(Get-Content "$tools\postgres.log" -Raw -ErrorAction SilentlyContinue)" }
if ($ctl.ExitCode -ne 0) { throw "pg_ctl start exited $($ctl.ExitCode):`n$(Get-Content "$tools\pg_ctl.out", "$tools\pg_ctl.err" -Raw)`n$(Get-Content "$tools\postgres.log" -Raw -ErrorAction SilentlyContinue)" }
$pgPid = [int](Get-Content "$data\postmaster.pid" -TotalCount 1)
Save-Pid "postgres-$Port" $pgPid
$up = $false
for ($i = 0; $i -lt 60 -and -not $up; $i++) {
  if (-not (Get-Process -Id $pgPid -ErrorAction SilentlyContinue)) { throw "postgres exited early:`n$(Get-Content "$tools\postgres.log" -Raw)" }
  try { Invoke-Native "$PgBin\pg_isready.exe" @('-h', '127.0.0.1', '-p', "$Port") 10 | Out-Null; $up = $true } catch { $up = $false }
  if (-not $up) { Start-Sleep -Milliseconds 500 }
}
if (-not $up) { throw "postgres did not accept connections on port $Port`n$(Get-Content "$tools\postgres.log" -Raw)" }

Write-Phase 'creating role and database'
$env:PGPASSWORD = $su
try {
  foreach ($q in "CREATE ROLE locus_ci LOGIN PASSWORD '$pw'", 'CREATE DATABASE locus_ci OWNER locus_ci') {
    Invoke-Native "$PgBin\psql.exe" @('-w', '-h', '127.0.0.1', '-p', "$Port", '-U', 'postgres', '-d', 'postgres', '-v', 'ON_ERROR_STOP=1', '-q', '-c', $q) 60 | Out-Null
  }
  Write-Host ((Invoke-Native "$PgBin\psql.exe" @('-w', '-h', '127.0.0.1', '-p', "$Port", '-U', 'postgres', '-d', 'postgres', '-At', '-c', 'SHOW server_version') 60).Trim()) "on 127.0.0.1:$Port (pid $pgPid)"
} finally { Remove-Item Env:PGPASSWORD }

Set-CiEnv 'PG_TEST_HOST' '127.0.0.1'
Set-CiEnv 'PG_TEST_PORT' "$Port"
Set-CiEnv 'PG_TEST_USER' 'locus_ci'
Set-CiEnv 'PG_TEST_DB' 'locus_ci'
Set-CiEnv 'PG_TEST_PASSWORD' $pw -Secret

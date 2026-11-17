# Installs a pinned MariaDB (official zip, SHA-256 checked), starts it on 127.0.0.1:<Port> the same way
# devenv\db\start.ps1 does (--local-infile=1, auto-generated self-signed TLS certificate for the TLS tests),
# creates the `bench` user (random password) and loads the fixtures the integration tests expect:
#   bench.student       <- synthetic 40,450 rows            (sidecar tests)
#   bench.student_real  <- devenv\fixtures-real\student.tsv (the real masked file when you have it,
#                          otherwise a synthetic one made by gen-fixtures.ps1)
#   erfk                <- scripts\ci\sql\er-fk.sql         (db-mysql structure test)
# and exports BENCH_DSN. Never uses a server it did not start: it stops if the port is taken.
param(
  [int]$Port = 3307,
  [string]$Version = '11.8.5',
  [string]$Sha256 = '75332DC1F437D9ECEE253C2D751D02A69628B109DDD031006F8A8D9BA59DBE0D'
)
. "$PSScriptRoot\lib.ps1"
$repo = Split-Path (Split-Path $PSScriptRoot)
if (-not (Test-PortFree $Port)) { throw "port $Port is already in use; refusing to touch whatever listens there" }

$zip = Get-Pinned "https://archive.mariadb.org/mariadb-$Version/winx64-packages/mariadb-$Version-winx64.zip" $Sha256 "mariadb-$Version-winx64.zip"
$tools = Join-Path (Get-CiRoot) "mariadb-$Port"
if (Test-Path $tools) { Remove-Item -Recurse -Force $tools }
New-Item -ItemType Directory -Force $tools | Out-Null
Write-Phase "unpacking $zip"
Expand-Zip $zip $tools
$bin = "$tools\mariadb-$Version-winx64\bin"
$data = "$tools\data"

# Fixtures: generate whatever is missing. The real masked file is used as is and never overwritten.
$real = "$repo\devenv\fixtures-real\student.tsv"
$syn = "$repo\devenv\fixtures\student.tsv"
if (-not (Test-Path $real) -or -not (Test-Path $syn)) {
  $gen = if ((Test-Path $real)) { Join-Path $tools 'gen' } else { "$repo\devenv" }
  & "$PSScriptRoot\gen-fixtures.ps1" -OutDir $gen
  if (-not (Test-Path $syn)) { $syn = "$gen\fixtures\student.tsv" }
  if (-not (Test-Path $real)) { $real = "$gen\fixtures-real\student.tsv" }
}

& "$bin\mariadb-install-db.exe" "--datadir=$data" | Out-Null
if ($LASTEXITCODE -ne 0) { throw 'mariadb-install-db failed' }
$log = "$tools\mariadbd.log"
$pidFile = Start-Detached "$bin\mariadbd.exe" @("--defaults-file=$data\my.ini", "--port=$Port", '--bind-address=127.0.0.1', '--local-infile=1', '--console') $tools "$tools\mariadbd.out" $log
Save-Pid "mariadb-$Port" $pidFile
$p = Get-Process -Id $pidFile

function Invoke-Maria([string]$Query, [string]$File) {
  $ErrorActionPreference = 'Continue'  # mariadb.exe prints a WARNING on stderr; only the exit code matters
  $a = @('-uroot', '--host=127.0.0.1', "--port=$Port", '--connect-timeout=2', '--local-infile=1', '--default-character-set=utf8mb4', '-N')
  $out = if ($File) { & "$bin\mariadb.exe" @a -e "source $($File -replace '\\', '/')" 2>&1 } else { $Query | & "$bin\mariadb.exe" @a 2>&1 }
  if ($LASTEXITCODE -ne 0) { throw "mariadb.exe failed: $out" }
  $out | Where-Object { "$_" -notmatch '^WARNING' }
}
$up = $false
for ($i = 0; $i -lt 120 -and -not $up; $i++) {
  if ($p.HasExited) { throw "mariadbd exited early:`n$(Get-Content $log -Raw)" }
  try { Invoke-Maria 'SELECT 1' | Out-Null; $up = $true } catch { Start-Sleep -Milliseconds 500 }
}
if (-not $up) { throw "mariadbd did not answer on port $Port`n$(Get-Content $log -Raw)" }

Write-Phase "loading fixtures"
$pw = New-RandomPassword
Invoke-Maria (Get-Content -Raw -Encoding utf8 "$PSScriptRoot\sql\student_schema.sql") | Out-Null
Invoke-Maria "LOAD DATA LOCAL INFILE '$($syn -replace '\\', '/')' INTO TABLE bench.student CHARACTER SET utf8mb4 FIELDS TERMINATED BY '\t' LINES TERMINATED BY '\n' IGNORE 1 LINES;" | Out-Null

# student_real: the DDL comes from the file's own header line (same as scripts\load-fixture-real.ps1).
$cols = (Get-Content -TotalCount 1 -Encoding utf8 $real).Split("`t")
if ($cols.Count -ne 23) { throw "expected 23 columns, found $($cols.Count)" }
$defs = foreach ($c in $cols) { "  ``$c`` $(if ($c -eq 'desc') { 'TEXT' } else { 'VARCHAR(191)' }) NOT NULL" }
Invoke-Maria @"
CREATE DATABASE IF NOT EXISTS bench CHARACTER SET utf8mb4;
DROP TABLE IF EXISTS bench.student_real;
CREATE TABLE bench.student_real (
$($defs -join ",`n"),
  KEY idx_userId (``userId``)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4 COLLATE=utf8mb4_unicode_ci;
"@ | Out-Null
# ESCAPED BY '' keeps backslashes in the data literally (the file is raw, not MySQL-escaped).
Invoke-Maria "LOAD DATA LOCAL INFILE '$($real -replace '\\', '/')' INTO TABLE bench.student_real CHARACTER SET utf8mb4 FIELDS TERMINATED BY '\t' ESCAPED BY '' LINES TERMINATED BY '\n' IGNORE 1 LINES;" | Out-Null

Invoke-Maria -File "$PSScriptRoot\sql\er-fk.sql" | Out-Null

# Grants as on the local server: the schemas the tests create and drop, session visibility for KILL checks,
# routine metadata.
Invoke-Maria @"
CREATE USER bench@'127.0.0.1' IDENTIFIED BY '$pw';
GRANT ALL ON bench.* TO bench@'127.0.0.1';
GRANT ALL ON erfk.* TO bench@'127.0.0.1';
GRANT ALL ON wbimport.* TO bench@'127.0.0.1';
GRANT ALL ON wbexport.* TO bench@'127.0.0.1';
GRANT PROCESS, CONNECTION ADMIN ON *.* TO bench@'127.0.0.1';
GRANT SELECT ON mysql.proc TO bench@'127.0.0.1';
FLUSH PRIVILEGES;
"@ | Out-Null

$n = [int](Invoke-Maria 'SELECT COUNT(*) FROM bench.student_real')
$s = [int](Invoke-Maria 'SELECT COUNT(*) FROM bench.student')
$v = Invoke-Maria 'SELECT VERSION(), @@have_ssl'
Write-Host "mariadb $v on 127.0.0.1:$Port, student=$s student_real=$n (pid $($p.Id))"
if ($n -ne 40450 -or $s -ne 40450) { throw "expected 40450 rows in each fixture table, got student=$s student_real=$n" }
Set-CiEnv 'BENCH_DSN' "mysql://bench:$pw@127.0.0.1:$Port/bench" -Secret
Set-CiEnv 'LOCUS_STUDENT_TSV' $real

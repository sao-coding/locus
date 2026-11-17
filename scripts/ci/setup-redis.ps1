# Installs a pinned Redis for Windows (redis-windows/redis-windows msys2 build, SHA-256 checked: the same
# build the local devenv uses) and starts it on 127.0.0.1:<Port> with a random password, no persistence.
# Exports REDIS_URL. Stops if the port is taken.
param(
  [int]$Port = 6380,
  [string]$Version = '8.10.3',
  [string]$Sha256 = 'AF16DF340525B431E9EC8DABFD625D5E7A233F82805B5418819904EA909C447F'
)
. "$PSScriptRoot\lib.ps1"
if (-not (Test-PortFree $Port)) { throw "port $Port is already in use; refusing to touch whatever listens there" }

$zip = Get-Pinned "https://github.com/redis-windows/redis-windows/releases/download/$Version/Redis-$Version-Windows-x64-msys2.zip" $Sha256 "Redis-$Version-Windows-x64-msys2.zip"
$tools = Join-Path (Get-CiRoot) "redis-$Port"
if (Test-Path $tools) { Remove-Item -Recurse -Force $tools }
New-Item -ItemType Directory -Force $tools | Out-Null
Write-Phase "unpacking $zip"
Expand-Zip $zip $tools
$bin = "$tools\Redis-$Version-Windows-x64-msys2"
$data = "$tools\data"
New-Item -ItemType Directory -Force $data | Out-Null

$pw = New-RandomPassword
@"
bind 127.0.0.1
port $Port
protected-mode yes
requirepass $pw
dir $($data -replace "\\", "/")
dbfilename dump.rdb
appendonly no
save ""
"@ | Set-Content -Encoding ascii "$data\redis.conf"

$redisPid = Start-Detached "$bin\redis-server.exe" @('redis.conf') $data "$tools\redis.out" "$tools\redis.err"
Save-Pid "redis-$Port" $redisPid
$p = Get-Process -Id $redisPid
$env:REDISCLI_AUTH = $pw  # keeps the password off the command line
$ok = $false
for ($i = 0; $i -lt 40 -and -not $ok; $i++) {
  if ($p.HasExited) { throw "redis-server exited early:`n$(Get-Content "$tools\redis.out" -Raw)" }
  $ok = ((& "$bin\redis-cli.exe" -h 127.0.0.1 -p $Port ping 2>$null) -eq 'PONG')
  if (-not $ok) { Start-Sleep -Milliseconds 250 }
}
if (-not $ok) { throw "redis did not answer PONG on port $Port" }
Write-Host "$(& "$bin\redis-cli.exe" -h 127.0.0.1 -p $Port info server | Select-String 'redis_version') on 127.0.0.1:$Port (pid $($p.Id))"
Remove-Item Env:REDISCLI_AUTH
Set-CiEnv 'REDIS_URL' "redis://:$pw@127.0.0.1:$Port/0" -Secret

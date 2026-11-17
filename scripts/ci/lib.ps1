# Shared helpers for the CI setup scripts (dot-source this file). Works on a GitHub runner and locally.
$ErrorActionPreference = 'Stop'

# Where downloads and unpacked tools go. `.ci-tools` is git-ignored.
function Get-CiRoot {
  if ($env:LOCUS_CI_ROOT) { return $env:LOCUS_CI_ROOT }
  Join-Path (Split-Path (Split-Path $PSScriptRoot)) '.ci-tools'
}

# 24 random letters/digits (nothing easily confused), from the OS CSPRNG.
function New-RandomPassword {
  $chars = [char[]]'ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789'
  -join (1..24 | ForEach-Object { $chars[[System.Security.Cryptography.RandomNumberGenerator]::GetInt32($chars.Length)] })
}

# Downloads $Url into the cache dir (reused when present and the hash matches) and checks SHA-256.
function Get-Pinned([string]$Url, [string]$Sha256, [string]$FileName) {
  $dir = Join-Path (Get-CiRoot) 'downloads'
  New-Item -ItemType Directory -Force $dir | Out-Null
  $file = Join-Path $dir $FileName
  if ((Test-Path $file) -and ((Get-FileHash $file -Algorithm SHA256).Hash -ne $Sha256)) { Remove-Item $file }
  if (-not (Test-Path $file)) {
    Write-Phase "downloading $Url"
    for ($i = 1; $i -le 4; $i++) {
      try { Invoke-WebRequest -Uri $Url -OutFile $file -UseBasicParsing; break }
      catch { if ($i -eq 4) { throw }; Start-Sleep -Seconds (5 * $i) }
    }
    $got = (Get-FileHash $file -Algorithm SHA256).Hash
    if ($got -ne $Sha256) { Remove-Item $file; throw "${FileName}: SHA-256 $got, expected $Sha256" }
  }
  $file
}

# Exports NAME=value to the next workflow steps (GITHUB_ENV) and to this process. Locally it only sets the
# process variable and appends to .ci-tools\ci.env (git-ignored). -Secret keeps the value out of the log.
function Set-CiEnv([string]$Name, [string]$Value, [switch]$Secret) {
  Set-Item -Path "Env:$Name" -Value $Value
  if ($env:GITHUB_ENV) {
    if ($Secret) { Write-Host "::add-mask::$Value" }
    Add-Content -Path $env:GITHUB_ENV -Value "$Name=$Value" -Encoding utf8
  } else {
    $f = Join-Path (Get-CiRoot) 'ci.env'
    New-Item -ItemType Directory -Force (Split-Path $f) | Out-Null
    Add-Content -Path $f -Value "$Name=$Value" -Encoding ascii
  }
  if (-not $Secret) { Write-Host "$Name=$Value" }
}

function Test-PortFree([int]$Port) {
  $l = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, $Port)
  try { $l.Start(); $true } catch { $false } finally { try { $l.Stop() } catch {} }
}

# Records a process we started so a local run can stop exactly it (by PID) later.
function Save-Pid([string]$Name, [int]$ProcId) {
  $d = Join-Path (Get-CiRoot) 'pids'
  New-Item -ItemType Directory -Force $d | Out-Null
  Set-Content -Path (Join-Path $d "$Name.pid") -Value $ProcId -Encoding ascii
}

function Write-Phase([string]$Message) { Write-Host ('[{0:HH:mm:ss}] {1}' -f (Get-Date), $Message) }

# Unpacks a zip with the Windows tar (bsdtar, not Git's GNU tar, which can't read zips): Expand-Archive takes minutes on a zip with thousands of files.
function Expand-Zip([string]$Zip, [string]$Destination) {
  New-Item -ItemType Directory -Force $Destination | Out-Null
  & "$env:SystemRoot\System32\tar.exe" -xf $Zip -C $Destination
  if ($LASTEXITCODE -ne 0) { throw "tar failed on $Zip" }
}

# Runs a program with stdin closed and output captured to files, and kills it if it runs longer than
# $TimeoutSec, so a prompt or a hang fails the step in seconds instead of eating the job. Returns the output.
function Invoke-Native([string]$Exe, [string[]]$Arguments, [int]$TimeoutSec = 120) {
  $o = [IO.Path]::GetTempFileName(); $e = [IO.Path]::GetTempFileName(); $in = [IO.Path]::GetTempFileName()
  try {
    # Start-Process joins an array with spaces and does not quote, so `-c "CREATE ROLE ..."` would
    # arrive as several arguments: quote each one the Windows (CommandLineToArgvW) way.
    $quoted = $Arguments | ForEach-Object {
      if ($_ -eq '' -or $_ -match '[\s"]') { '"' + ($_ -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"' } else { $_ }
    }
    $p = Start-Process -FilePath $Exe -ArgumentList ($quoted -join ' ') -NoNewWindow -PassThru `
      -RedirectStandardInput $in -RedirectStandardOutput $o -RedirectStandardError $e
    if (-not $p.WaitForExit($TimeoutSec * 1000)) {
      $p.Kill($true)
      throw "$(Split-Path $Exe -Leaf) did not finish in $TimeoutSec s:`n$(Get-Content $o, $e -Raw)"
    }
    $out = Get-Content $o -Raw
    if ($p.ExitCode -ne 0) { throw "$(Split-Path $Exe -Leaf) exited $($p.ExitCode):`n$out$(Get-Content $e -Raw)" }
    $out
  } finally { Remove-Item $o, $e, $in -ErrorAction SilentlyContinue }
}

# Starts a program that outlives the calling step. GitHub's Windows runner kills every process a step
# started (job object) when the step ends, and Start-Process children are inside it; a process created
# through WMI is a child of the WMI service instead. stdout/stderr go to files. Returns the PID.
function Start-Detached([string]$Exe, [string[]]$Arguments, [string]$WorkDir, [string]$OutFile, [string]$ErrFile) {
  $args1 = ($Arguments | ForEach-Object { if ($_ -match '[\s"]' -or $_ -eq '') { '"' + ($_ -replace '"', '\"') + '"' } else { $_ } }) -join ' '
  # cmd /c "<line>": the whole line is wrapped in one more pair of quotes so cmd keeps the inner ones.
  $line = "cmd.exe /c `"`"$Exe`" $args1 > `"$OutFile`" 2> `"$ErrFile`"`""
  $r = Invoke-CimMethod -ClassName Win32_Process -MethodName Create -Arguments @{ CommandLine = $line; CurrentDirectory = $WorkDir }
  if ($r.ReturnValue -ne 0) { throw "Win32_Process.Create failed ($($r.ReturnValue)) for $Exe" }
  # The PID is cmd.exe's; the server is its child.
  $child = Get-CimInstance Win32_Process -Filter "ParentProcessId=$($r.ProcessId)" | Select-Object -First 1
  if ($child) { $child.ProcessId } else { $r.ProcessId }
}

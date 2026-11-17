# Builds sidecar\bin\sidecar.exe
Set-Location $PSScriptRoot
go build -trimpath -ldflags "-s -w" -o bin\sidecar.exe .

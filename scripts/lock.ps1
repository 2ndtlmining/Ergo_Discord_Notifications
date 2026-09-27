# Regenerate Cargo.lock (after adding or bumping a dependency).
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
docker build --target lock -o type=local,dest=.lock-out .
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Move-Item -Force .lock-out/Cargo.lock ./Cargo.lock
Remove-Item -Recurse -Force .lock-out
Write-Host "Cargo.lock updated." -ForegroundColor Green

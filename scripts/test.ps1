# fmt check + clippy + unit tests, all inside Docker.
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
docker build --target test --progress plain .
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
Write-Host "All checks passed." -ForegroundColor Green

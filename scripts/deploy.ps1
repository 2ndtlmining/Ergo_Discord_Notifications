# Build and (re)start the monitor in the background with the local .env.
# Safe to re-run: use it after pulling new code or editing .env.
# The container restarts automatically after crashes and reboots.
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)

if (-not (Test-Path .env)) {
    Write-Host ".env not found. Copy .env.example to .env and fill it in first." -ForegroundColor Red
    exit 1
}

$env:GIT_SHA = (git rev-parse --short HEAD)
$env:BUILT_AT = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
docker compose up -d --build --remove-orphans
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$port = (Select-String -Path .env -Pattern '^HTTP_PORT=(\d+)' | ForEach-Object { $_.Matches[0].Groups[1].Value } | Select-Object -First 1)
if (-not $port) { $port = 7777 }
Write-Host "ergo-monitor is running: http://localhost:$port  (logs: docker compose logs -f)" -ForegroundColor Green

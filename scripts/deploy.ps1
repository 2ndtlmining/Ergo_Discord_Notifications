# Build and (re)start the monitor in the background with the local .env.
# Safe to re-run: use it after editing .env, or with -Pull to fetch the
# latest code first. The container restarts automatically after crashes and reboots.
#
#   scripts/deploy.ps1          # rebuild + restart (picks up .env changes)
#   scripts/deploy.ps1 -Pull    # git pull, then rebuild + restart
param([switch]$Pull)
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)

if (-not (Test-Path .env)) {
    Write-Host ".env not found. Copy .env.example to .env and fill it in first." -ForegroundColor Red
    exit 1
}

if ($Pull) {
    git pull --ff-only
    if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
}

$env:GIT_SHA = (git rev-parse --short HEAD)
$env:BUILT_AT = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
docker compose up -d --build --force-recreate --remove-orphans
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# "Started" isn't "working": a bad .env makes the monitor exit and restart in
# a loop. Wait for its own health check to pass before reporting success.
Write-Host "Waiting for the monitor to come up..."
$ErrorActionPreference = 'Continue'
for ($i = 0; $i -lt 20; $i++) {
    docker compose exec -T ergo-monitor /usr/local/bin/ergo-monitor healthcheck *> $null
    if ($LASTEXITCODE -eq 0) {
        $port = (Select-String -Path .env -Pattern '^HTTP_PORT=(\d+)' | ForEach-Object { $_.Matches[0].Groups[1].Value } | Select-Object -First 1)
        if (-not $port) { $port = 7777 }
        Write-Host "ergo-monitor $env:GIT_SHA is running: http://localhost:$port  (logs: docker compose logs -f)" -ForegroundColor Green
        exit 0
    }
    Start-Sleep -Seconds 2
}

Write-Host "ergo-monitor did not come up. Last log lines:" -ForegroundColor Red
docker compose logs --tail 20 ergo-monitor
Write-Host "Fix the problem above (often a typo in .env), then run scripts/deploy.ps1 again." -ForegroundColor Red
exit 1

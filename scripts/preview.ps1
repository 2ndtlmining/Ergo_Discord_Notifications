# Dashboard with fake data (every node condition) on http://localhost:7777
# No real nodes, explorers or Discord involved.
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
docker build --target preview -t ergo-monitor:preview .
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
docker rm -f ergo-monitor-preview 2>$null | Out-Null
Write-Host "Preview: http://localhost:7777  (Ctrl+C to stop)" -ForegroundColor Green
docker run --rm --name ergo-monitor-preview -p 7777:7777 ergo-monitor:preview

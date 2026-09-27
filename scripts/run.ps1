# Build the runtime image and run it with the local .env on http://localhost:7777
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path $PSScriptRoot -Parent)
$sha = (git rev-parse --short HEAD)
$built = (Get-Date).ToUniversalTime().ToString('yyyy-MM-ddTHH:mm:ssZ')
docker build -t ergo-monitor --build-arg GIT_SHA=$sha --build-arg BUILT_AT=$built .
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
docker rm -f ergo-monitor 2>$null | Out-Null
docker run --rm --name ergo-monitor --env-file .env -p 7777:7777 ergo-monitor

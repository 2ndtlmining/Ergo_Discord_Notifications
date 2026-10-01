#!/usr/bin/env bash
# Build and (re)start the monitor in the background with the local .env.
# Linux/macOS version of deploy.ps1. Safe to re-run: use it after editing
# .env, or with --pull to fetch the latest code first.
#
#   scripts/deploy.sh           # rebuild + restart (picks up .env changes)
#   scripts/deploy.sh --pull    # git pull, then rebuild + restart
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ ! -f .env ]]; then
    echo ".env not found. Copy .env.example to .env and fill it in first." >&2
    exit 1
fi

if [[ "${1:-}" == "--pull" ]]; then
    git pull --ff-only
fi

# Use sudo only when this user can't reach the Docker daemon.
docker=(docker)
if ! docker info >/dev/null 2>&1; then
    docker=(sudo docker)
fi

export GIT_SHA BUILT_AT
GIT_SHA=$(git rev-parse --short HEAD)
BUILT_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
# sudo drops the environment, so pass the build args through explicitly.
"${docker[@]}" compose build --build-arg "GIT_SHA=$GIT_SHA" --build-arg "BUILT_AT=$BUILT_AT"
# up -d recreates the container, which re-reads .env (a plain restart does not).
"${docker[@]}" compose up -d --force-recreate --remove-orphans

port=$(sed -n 's/^HTTP_PORT=\([0-9][0-9]*\).*/\1/p' .env | head -n1)
port=${port:-7777}
echo "ergo-monitor $GIT_SHA is running: http://localhost:$port  (logs: ${docker[*]} compose logs -f)"

# AGENTS.md

Context for AI agents (Claude Code and others) working on this repo or acting
on its alerts. People should start with [README.md](README.md).

## What this is

`ergo-monitor` is a single Rust binary in one Docker container. It polls Ergo
nodes and the public explorers, sends Discord alerts, and serves a dashboard
plus a JSON API on port **7777**. All state is in memory; nothing is written
to disk.

## Acting on an alert

1. Get the current state: `curl -s http://<monitor>:7777/api/status`
   (or `/api/nodes/<id>` for one node). Use it to confirm the problem is still
   happening; alerts can be minutes old.
2. Open the runbook named in the node's `runbook` field (also shown in the
   Discord alert), e.g. `docs/runbooks/node-down.md`, and follow it in order.
3. Find how the node is run (host, docker/systemd, container name, SSH user)
   in `docs/infrastructure.md`. That file is gitignored and may not exist; if
   it's missing, ask the human instead of guessing hosts.
4. Confirm the fix through the API: the node's `condition` returns to `ok`,
   and a "Recovered" alert follows after 2 checks.

### Guardrails

| Safe without asking | Ask the human first |
|---|---|
| Read `/api/*`, the node's `/info`, and logs | Delete or resync node data, or wipe the `.ergo` folder |
| Restart a node's container/service **once** | Change a node's `.conf` (except as a runbook says) |
| Check disk space, memory, peers, connectivity | Anything touching wallets, keys, or node API keys |
| Restart the monitor itself | Edit `.env` or commit any env file; repeated restarts |

Never paste real IPs, wallet addresses or webhook URLs into GitHub issues or
PRs: this repo is public.

## API

All responses are JSON. The API is stable within `schema_version` 1.

| Endpoint | Returns |
|---|---|
| `GET /api/status` | Everything: reference height, summary, nodes, wallets, latest node release, `settings` |
| `GET /api/nodes/{id}` | One node (404 lists the known ids) |
| `GET /api/alerts` | Last 100 alerts, newest first; `delivery` is `pending`, `sent`, `failed` or `off` (no webhook) |
| `GET /healthz` | `{"status":"ok","commit":…,"last_poll":…}`; **503** with `"stale"` when the node poll has stopped (no poll for 3 intervals + 30s) |

```sh
# Which nodes are unhealthy, and why?
curl -s localhost:7777/api/status | jq '.nodes[] | select(.condition != "ok") | {id, condition, detail, runbook}'
```

Node fields (`/api/status` `.nodes[]`):

| Field | Meaning |
|---|---|
| `id` | Stable slug of the name, e.g. `duckpools-bot` |
| `status` | `ok` \| `behind` \| `down` \| `syncing` \| `unknown` |
| `condition` | Like `status`, but also `indexer-behind`. **Use this one** |
| `detail` | Human sentence, e.g. "Connection refused: … (node process stopped?)" |
| `status_since` | When the current condition was confirmed |
| `full_height`, `headers_height`, `indexed_height` | From the node; `indexed_height` is null without `extraIndex` |
| `full_lag`, `indexed_lag` | Blocks behind `reference.height` |
| `sync_progress` | 0–1 while syncing |
| `peers`, `version`, `latency_ms`, `is_mining`, `is_explorer` | From `/info` (`version` is the last known one while down) |
| `latest_version`, `latest_version_url` | Newest release of the node's own `major.minor` line (6.0.x, 6.1.x), else the stable release |
| `version_outdated` | `true` if `version` is older than `latest_version`; null if either is unknown |
| `last_ok`, `last_error` | Last successful response; last failure text |
| `runbook` | Path of the runbook for the current condition, or null |

`settings` holds `lag_threshold_blocks`, `node_poll_seconds` and `wallet_poll_seconds`.

`reference.height` is the highest height reported by the reachable explorers;
null means both are unreachable, and then lag can't be judged (`unknown`).

`latest_release` (`version`, `url`, `lines`, `checked_at`) is the newest stable Ergo
node release on GitHub plus the newest release of each line in `lines` (Ergo
marks the newer line pre-release). Checked every 6 hours; null until the first
check succeeds.

## How the condition is decided (`src/node.rs`)

- **down**: `/info` failed. The reason tells you where to look:
  *refused* (host up, node process stopped), *timed out* (host or network),
  *HTTP error*, or *bad response*.
- **syncing**: more than 5 blocks behind **and** headers are more than 720 blocks ahead of
  processed blocks (initial sync or resync).
- **behind**: node block height more than `LAG_THRESHOLD_BLOCKS` (5) behind the tip.
- **indexer-behind**: node in sync, but `indexedHeight` more than 5 behind.
  Only HTTP 400/404/501 from `/blockchain/indexedHeight` means "no indexer";
  if the request fails otherwise, the last known indexed height is used and
  `last_error` starts with `Indexer:`.
- Alerts need the same condition on 2 checks in a row (`src/alerts.rs`).

## Code layout

| Path | Purpose |
|---|---|
| `src/main.rs` | Startup, HTTP routes, `healthcheck` / `preview` subcommands |
| `src/config.rs` | `.env` / environment parsing and validation |
| `src/explorer.rs` | Reference height from the explorers |
| `src/node.rs` | Node probing and condition classification |
| `src/alerts.rs` | Debounce / reminder / recovery state machine |
| `src/monitor.rs` | Polling loop, shared `AppState`, alert text |
| `src/release.rs` | Latest Ergo node release from GitHub, version comparison |
| `src/wallet.rs` | Wallet balances and incoming-transaction alerts |
| `src/discord.rs` | Webhook embeds, icons, rate limits |
| `src/preview.rs` | Fake data for `ergo-monitor preview` |
| `web/` | Dashboard (plain HTML/CSS/JS, compiled into the binary) |
| `assets/discord/` | Discord status icons (`scripts/gen_icons.py`) |
| `docs/runbooks/` | Step-by-step fixes, one per condition |

## Building and testing

There is no local Rust toolchain; everything runs through Docker
(Noxide-style Dockerfile targets):

```sh
docker build --target test .                          # fmt check, clippy -D warnings, tests
docker build --target fmt -o type=local,dest=. .      # apply rustfmt
docker build --target lock -o type=local,dest=.lock-out .   # new Cargo.lock
docker build --target preview -t ergo-monitor:preview .     # fake-data dashboard
```

Conventions: no emojis in the UI or Discord (use the SVG/PNG icons); status
colours are ok `#30A46C`, down `#E5484D`, behind `#F5A524`, syncing
`#3E8BFF`, unknown `#8B8D98`, received `#FF5A1F`. Never commit `.env` files.

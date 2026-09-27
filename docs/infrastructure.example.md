# Infrastructure (template)

Copy this file to `docs/infrastructure.md` and fill it in. The real file is
**gitignored**, so it stays on your machine. Runbooks and AI agents use it to know
how to reach and restart each node. Keep secrets (passwords, keys) out of it;
use SSH keys.

## Monitor

| | |
|---|---|
| Host | `192.168.1.5` |
| How it runs | `docker compose` in `~/Ergo_Discord_Notifications` |
| Dashboard | `http://192.168.1.5:7777` |

## Nodes

The ids match the `id` field in `/api/status` (a slug of `NODE_<n>_NAME`).

| id | Host | SSH user | Runs as | Container / service | Data dir | Notes |
|---|---|---|---|---|---|---|
| `grid-bot` | `192.168.1.10` | `ergo` | docker | `ergo-node` | `/srv/ergo` | Grid bot uses this node |
| `hosted-explorer` | `192.168.1.11` | `ergo` | docker | `explorer-node` | `/srv/ergo` | extraIndex on |
| `mining-pool` | `192.168.1.12` | `ergo` | systemd | `ergo.service` | `/home/ergo/.ergo` | Mining enabled |

## Restart commands

```sh
ssh ergo@192.168.1.10 'docker restart ergo-node'
ssh ergo@192.168.1.12 'sudo systemctl restart ergo'
```

## Who to call

| When | Who |
|---|---|
| Host unreachable, hardware, disk full | *name / Discord handle* |

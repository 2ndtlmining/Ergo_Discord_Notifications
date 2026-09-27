# Node down

**Alert:** "Down" · `condition: down`. The node's REST API (`/info`) didn't answer on 2 checks in a row.

Replace `<node>` with the node's `ip:port` from the alert, and `<host>`, `<container>` or `<service>` with the details
from `docs/infrastructure.md`.

## 1. Read the reason in the alert

| Reason | Most likely cause | Go to |
|---|---|---|
| **Connection refused** | Host is up, node process stopped or API port closed | Step 2 |
| **Timed out** | Host off or unreachable, or the node is hung | Step 3 |
| **HTTP 5xx / bad response** | Node running but unhealthy | Step 4 |

## 2. Connection refused: is the node process running?

```sh
ssh <user>@<host>
docker ps -a --filter name=<container>      # Docker install
systemctl status <service>                  # systemd install
```

- **Stopped/exited:** check why before restarting:
  `docker logs --tail 200 <container>` or `journalctl -u <service> -n 200`.
  Look for `OutOfMemoryError`, `No space left on device`, or a crash trace.
- **Disk full?** `df -h`. A full disk is the most common reason a node stops. Freeing space
  needs a human decision; don't delete node data yourself.
- Restart **once**: `docker start <container>` or `sudo systemctl restart <service>`.
- **Running but still refused:** the API may be bound to localhost only.
  From the host, `curl -s localhost:9053/info` works but `<node>` doesn't. Fix
  `restApi.bindAddress = "0.0.0.0:9053"` in the node `.conf` (ask a human first).

## 3. Timed out: can we reach the host?

```sh
ping -c 3 <host-ip>
nc -zv <host-ip> 9053        # or: Test-NetConnection <host-ip> -Port 9053
```

- No ping: the machine is off, asleep, or off the network. This needs a human.
- Ping works but the port times out: a firewall is blocking it, or the node is hung. SSH in and use
  step 2. A hung JVM shows as running with high CPU and no new log lines;
  restart it once.

## 4. Running but unhealthy

`docker logs --tail 200 <container>`. Look for repeated exceptions, database
errors, or `OutOfMemoryError`. If memory is the cause, raising `-Xmx` is a config change, so
ask a human.

## Check it worked

```sh
curl -s http://<node>/info | jq '{fullHeight, headersHeight, peersCount}'
curl -s http://<monitor>:7777/api/nodes/<id> | jq '{condition, detail}'
```

The monitor sends "Recovered" after 2 good checks (about 1 minute). If the node was
down a long time, expect `syncing` or `behind` first; see
[node-behind.md](node-behind.md).

## Hand over to a human when

- The host is unreachable, the disk is full, or the node won't stay up after one restart
- Logs show database corruption (a resync decision)

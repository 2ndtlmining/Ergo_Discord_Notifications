# Node behind or syncing

**Alerts:**
- **Behind** (`condition: behind`): block height is more than `LAG_THRESHOLD_BLOCKS` (5) behind the chain tip.
- **Syncing** (`condition: syncing`): the node is far behind but its headers are well ahead of processed blocks, which is an initial sync or a resync.

## Syncing: usually just wait

A resync from scratch takes hours to days. Check that it's **moving**:

```sh
curl -s http://<monitor>:7777/api/nodes/<id> | jq '{full_height, headers_height, sync_progress, peers}'
# run again 10 minutes later: full_height should go up
```

- It's rising: nothing to do. The monitor reminds you once a day and says
  "Recovered" when the node reaches the tip.
- It's not moving after 30 minutes: treat it as **behind** (below).

## Behind: find out why it stopped following the chain

1. **Peers.** `peers` in the API, or `curl -s http://<node>/peers/connected | jq length`.
   - 0 to 2 peers: networking problem. Check that the host has internet access and
     that the node's P2P port (9030) isn't blocked. A restart often reconnects.
2. **Is it stuck on one block?** Compare `full_height` across two checks
   a few minutes apart. If it doesn't change and `headers_height` is at the tip,
   the node is stuck applying a block. Look at the logs:
   `docker logs --tail 300 <container>` for repeated errors about the same block.
3. **Resources.** `df -h` (disk full), `free -h` (memory), `top` (CPU pinned).
   A slow or full disk makes a node fall slowly behind.
4. **Fix:** restart the node **once** (`docker restart <container>` or
   `sudo systemctl restart <service>`), then check again after 5 to 10 minutes.

## Check it worked

`full_lag` drops to 5 or less and `condition` becomes `ok`; a "Recovered" alert follows.

## Hand over to a human when

- It's still stuck on the same block after a restart (it may need a resync, which deletes
  data and takes a long time)
- The disk is full, or the host is overloaded
- Peers stay at 0 after a restart (network or firewall change)

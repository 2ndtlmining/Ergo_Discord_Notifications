# Indexer behind

**Alert:** "Indexer behind" (`condition: indexer-behind`). The node itself is in sync, but its
extra index (`/blockchain/indexedHeight`, enabled by `extraIndex = true`) is
more than 5 blocks behind the tip. Anything that reads indexed data from this
node, such as an explorer, bot or pool, sees stale balances and transactions.

## 1. Is it catching up or stuck?

```sh
curl -s http://<node>/blockchain/indexedHeight
# {"indexedHeight": 1881631, "fullHeight": 1882085}. Run it again after 5 minutes.
```

- `indexedHeight` is rising: it's catching up (normal after a restart or a burst of
  blocks). Wait. The lag should shrink every check.
- `indexedHeight` hasn't changed: the indexer is **stuck**. Continue.
- The gap keeps **growing** while `fullHeight` moves: stuck. Continue.

## 2. Look at the logs

```sh
docker logs --tail 300 <container> 2>&1 | grep -iE "index|exception|error"
```

Note any repeated error and the block height it mentions.

## 3. Restart once

`docker restart <container>` or `sudo systemctl restart <service>`. The
indexer resumes from where it stopped. Check step 1 again after 10 minutes.

## Check it worked

`indexed_lag` in `/api/nodes/<id>` drops to 5 or less, `condition` becomes `ok`, and a
"Recovered" alert follows.

## Hand over to a human when

- It's still stuck at the same height after a restart. Rebuilding the index means
  deleting the index data and re-indexing from scratch (hours), which is a human
  decision.
- The logs show the same exception at the same height every time.

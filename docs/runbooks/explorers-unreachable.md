# Explorers unreachable

**Alert:** "Explorers unreachable". Neither reference explorer answered on 2 checks in a row:

- Mainnet: `https://api.ergoplatform.com/api/v1/networkState`
- P2P: `https://api-102.ergoplatform.com/api/v1/networkState`

Without a chain tip, the monitor can't tell whether a node is behind, so **lag
alerts are paused**. Nodes show `unknown`, but **down alerts still work**.

## 1. Is it them or us?

```sh
curl -s -m 10 https://api.ergoplatform.com/api/v1/networkState | jq .height
curl -s -m 10 https://api-102.ergoplatform.com/api/v1/networkState | jq .height
```

Run this **from the machine running the monitor** (or `docker exec` into the
container's network).

- Both fail from here, but a site like explorer.ergoplatform.com loads from
  elsewhere: the monitor host has lost internet or DNS. Check its connection.
- They fail everywhere: the public explorers are down. Nothing to fix on our side;
  the monitor recovers on its own and sends "Explorers reachable".

## 2. Optional: point at a different explorer

If one explorer is down for a long time, set `EXPLORER_MAINNET_API` or
`EXPLORER_P2P_API` in `.env` to another explorer API that has
`/api/v1/networkState`, then run `scripts/deploy.ps1`. This changes `.env`, so ask a human first.

## Check it worked

`curl -s http://<monitor>:7777/api/status | jq .reference.height` is a number again.

# Wallet alerts

**Alert:** "Received · <wallet>" with the amount as the title. A new transaction
added ERG to a watched wallet. This is informational; nothing is broken.

## What the numbers mean

- **Amount** is the *net* change for that wallet in the transaction (ERG received minus ERG
  spent from the same address), so your own change isn't counted as income.
- **Balance** is the confirmed balance just after the check.
- Only transactions in blocks newer than the last check are announced. On startup the monitor
  records the current state silently, so restarts never repeat old alerts.

## Check a transaction

Open the "View transaction" link in the alert, or:

```sh
curl -s https://api.ergoplatform.com/api/v1/transactions/<tx-id> | jq '{inclusionHeight, numConfirmations}'
```

## Common questions

- **Too many tiny alerts from a bot wallet?** Set `WALLET_MIN_ALERT_ERG` (e.g.
  `0.01`) in `.env` and run `scripts/deploy.ps1`.
- **Expected a payment that never came?** Check the address on the explorer. Only
  confirmed transactions show up; the monitor checks every `WALLET_POLL_SECONDS` (5 minutes).
- **"Balance unavailable" on the dashboard?** The public explorer API didn't respond;
  see [explorers-unreachable.md](explorers-unreachable.md).

Never move funds, unlock wallets, or handle keys on the back of an alert. Wallets are a human-only matter.

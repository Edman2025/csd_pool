# CSD Pool API Spec

## 1. API Principles

- Public read APIs require no authentication.
- Operator APIs require authentication and are not described as public stable
  APIs.
- Amount strings use decimal CSD for display and base units for exact machine
  fields where needed.
- Hashrate fields end in `_hs` and are H/s.
- Timestamps are Unix seconds unless field name ends in `_at`, which is ISO
  8601.
- Runtime counters such as currently connected workers come from the in-process
  Stratum state. When PostgreSQL is configured, block history, payments, miner
  balances, worker totals, and pool-fee revenue come from the durable database;
  otherwise those fields fall back to the in-memory development snapshot.

## 2. `GET /`

Public dashboard HTML. The first screen is the actual pool console, not a
marketing landing page. It reads public API endpoints from the same origin:

- `/api/pool`
- `/api/metrics`
- `/api/history`
- `/api/blocks`
- `/api/payments`
- `/api/miner/:address`
- `/api/miner/:address/workers`
- `/api/getting-started`

The page is responsive and includes pool KPIs, share activity, recent blocks,
recent payments, miner address lookup, service-health placeholders, share
quality, and active-alert summary space. The share activity chart reads
`/api/history` through 12h, 24h, and 7d range buttons, and falls back to live
counters if history is unavailable.

All API responses include baseline security headers:

- `Content-Security-Policy`
- `X-Content-Type-Options: nosniff`
- `X-Frame-Options: DENY`
- `Referrer-Policy: no-referrer`
- `Permissions-Policy`

Operator and signer bearer token comparisons use a fixed-time equality helper.

For operators, the page can store a bearer token in browser `localStorage`
under `csd_pool_operator_token` and use it to read:

- `/api/operator/health`
- `/api/operator/alerts?status=active&limit=20`

## 3. `GET /getting-started`

Miner-facing setup HTML. This page is short and copy-paste oriented, with the
public Stratum endpoint, username format, command examples, port tiers, and
payout rules. It reads `/api/getting-started` from the same origin.

## 4. `GET /api/getting-started`

Machine-readable miner setup contract.

Response:

```json
{
  "pool_name": "CSD Pool",
  "stratum_endpoint": "pool.example.com:3333",
  "username_format": "<addr20>.<worker>",
  "address_format": "40 lowercase hex characters; optional 0x prefix accepted by dashboard lookup",
  "worker_name_rules": "letters, numbers, dash, underscore, and dot; keep names short and stable",
  "port_tiers": [
    {
      "port": 3333,
      "label": "standard",
      "starting_difficulty": 8.0,
      "enabled": true
    }
  ],
  "commands": [
    {
      "label": "Generic Stratum miner",
      "command": "csd-pool-miner --url stratum+tcp://pool.example.com:3333 --user <addr20>.rig-01 --pass x"
    }
  ],
  "payout": {
    "minimum_payout_csd": "1.0",
    "payout_interval_secs": 1800,
    "confirm_depth": 10,
    "fee_percent": 1.0
  },
  "public_endpoints": [
    "/",
    "/getting-started",
    "/status",
    "/api/pool",
    "/api/miner/<addr20>"
  ]
}
```

Operators should set `CSD_POOL_PUBLIC_STRATUM_ADDR` to the public HAProxy or
edge Stratum address. Optional `CSD_POOL_PUBLIC_PORT_TIERS` entries use
`port:label:starting_difficulty[:disabled]`.

## 5. `GET /status`

Public status page HTML for uptime monitors and miner-facing communication. It
reads `/api/status` from the same origin and does not require an operator token.

## 6. `GET /metrics`

Prometheus exposition format. This endpoint intentionally exports pool-level
aggregates only and avoids miner address labels.

Current metrics:

```text
csd_pool_workers_online
csd_pool_hashrate_hs
csd_pool_round_share_difficulty
csd_pool_stratum_connections
csd_pool_shares_total{result="accepted"}
csd_pool_shares_total{result="rejected"}
csd_pool_shares_total{result="stale"}
csd_pool_share_validation_seconds_sum
csd_pool_share_validation_seconds_count
csd_pool_share_validation_seconds_avg
csd_pool_blocks_found_total
csd_pool_blocks_submitted_total
csd_pool_blocks_confirmed_total
csd_pool_blocks_orphaned_total
csd_pool_fee_revenue_base_units
csd_pool_jobs_created_total
csd_pool_job_age_seconds
csd_pool_payout_batches_total{status}
csd_pool_payout_amount_base_units_total
csd_pool_service_up{service}
csd_node_rpc_latency_seconds{node}
csd_node_height{node}
csd_node_peers{node}
csd_pool_next_payout_seconds
csd_pool_updated_timestamp_seconds
```

## 7. `GET /api/status`

Public status summary for status pages and uptime monitors. This endpoint does
not require an operator token and only exposes aggregate health:

```json
{
  "status": "operational",
  "service": "csd-pool",
  "release": {
    "version": "0.1.0",
    "name": "csd-pool-1a2b3c4d5e6f-20260623T010000Z",
    "revision": "1a2b3c4d5e6f",
    "timestamp_utc": "20260623T010000Z"
  },
  "data_source": "postgres",
  "api_ok": true,
  "workers_online": 55,
  "shares_accepted": 77586,
  "shares_rejected": 4830,
  "shares_stale": 112,
  "active_alerts": 0,
  "unhealthy_services": 0,
  "node_count": 3,
  "payouts_enabled": false,
  "latest_sample_at": "2026-06-16 10:00:00+00",
  "updated_ts": 1781542961
}
```

`status` is `degraded` when latest health samples include unhealthy services,
active alerts exist, or PostgreSQL is configured but no CSD node sample exists.
Without PostgreSQL, the endpoint falls back to in-memory Stratum/API state and
uses `data_source: "memory"`.
`release` is populated from `/opt/csd-pool/release.env` when deployed through
`ops/bin/csd-pool-install-release.sh`; go-live evidence checks it against
`RELEASE-MANIFEST.txt`.

## 8. `GET /api/pool`

Live pool overview.

Response:

```json
{
  "pool_hashrate_hs": 744449784792.9026,
  "network_hashrate_hs": 2276860335272.5796,
  "network_share_pct": 32.69,
  "round_effort_pct": 83.2,
  "expected_block_secs": 366.9,
  "total_blocks": 180,
  "canonical_blocks": 3726,
  "immature_blocks": 3,
  "orphaned_blocks": 1,
  "avg_block_effort_pct_24h": 82.5,
  "avg_block_effort_pct_7d": 105.4,
  "avg_block_effort_pct_lifetime": 98.7,
  "block_luck_pct_24h": 121.21,
  "block_luck_pct_7d": 94.88,
  "block_luck_pct_lifetime": 101.32,
  "workers_online": 55,
  "miners_online": 41,
  "shares_accepted": 77586,
  "shares_rejected": 4830,
  "shares_stale": 112,
  "pool_fee_pct": 1.0,
  "payout_interval_secs": 1800,
  "next_payout_secs": 347,
  "confirm_depth": 10,
  "updated_ts": 1781542961
}
```

When PostgreSQL is configured, `next_payout_secs` is derived from the most
recent payout batch `created_at` plus the configured payout interval. Before the
first batch exists, or when the API runs without a repository, it falls back to
the configured default countdown.

When `CSD_POOL_NETWORK_URL` is set, the API requests
`<CSD_POOL_NETWORK_URL>/api/network` and fills `network_hashrate_hs` from either
`hashrate` in H/s or `hashrateGHs` converted to H/s. If the telemetry request
times out or fails, the endpoint keeps serving pool data and returns `0` for
network fields. `CSD_POOL_NETWORK_TIMEOUT_SECS` controls the timeout and
defaults to 2 seconds. The private adapter token is not forwarded to this
endpoint; protected telemetry endpoints use the dedicated
`CSD_POOL_NETWORK_TOKEN`.

`pool_hashrate_hs` is estimated from accepted Stratum share difficulty over the
current process lifetime using the standard difficulty-1 work unit. It remains
`0` until there are at least two accepted-share timestamps. `network_share_pct`
and `expected_block_secs` are derived only when both pool and network hashrate
are non-zero.

`round_effort_pct` is estimated from current-round accepted share difficulty
divided by `network_hashrate_hs * target_block_secs / 2^32`. The in-memory round
work resets after a block candidate is found. If network telemetry is missing,
round effort remains `0`.

Average block effort fields are aggregated from persisted `blocks.effort_pct`.
Luck is derived as `10000 / avg_block_effort_pct`; `100` means expected luck,
higher is luckier, and lower is less lucky. Empty windows return `0`.

## 9. `GET /api/metrics`

Worker summary. This endpoint mirrors Yamaduo's shape for compatibility.

Response:

```json
{
  "workers": {
    "<addr20>": {
      "shares_accepted": 161,
      "shares_rejected": 2,
      "shares_stale": 0,
      "blocks_found": 1,
      "last_difficulty": 76.35,
      "last_seen_ts": 1781542961
    }
  },
  "totals": {
    "workers_online": 55,
    "shares_accepted": 77602,
    "shares_rejected": 4830,
    "shares_stale": 112,
    "blocks_found": 180
  },
  "fee_revenue_csd": "0.00000000"
}
```

## 10. `GET /api/history`

Chart samples.

Query params:

```text
range = 12h | 24h | 7d | 30d
```

Response:

```json
{
  "interval_secs": 60,
  "samples": [
    {
      "ts": 1781542961,
      "pool_hs": 714457493852.7365,
      "net_hs": 716388623459.6765,
      "workers": 55,
      "shares_accepted": 120,
      "shares_rejected": 3,
      "shares_stale": 1
    }
  ]
}
```

When PostgreSQL is configured, samples are durable time buckets aggregated from
accepted `shares` and rejected/stale `share_events`. `pool_hs` is calculated
from accepted share difficulty in each bucket using the standard difficulty-1
work unit. If `CSD_POOL_NETWORK_URL` is configured, `net_hs` is filled from the
current network telemetry request; it is not yet a persisted historical network
series. Without PostgreSQL, the endpoint returns the current in-memory pool
snapshot as a single sample.

## 11. `GET /api/miner/:address`

Miner detail for one CSD address.

Response:

```json
{
  "address": "<addr20>",
  "online": true,
  "workers_online": 3,
  "hashrate_hs": 28600000000.0,
  "pending_csd": "0.50000000",
  "pending_base_units": "50000000",
  "owed_csd": "1.25000000",
  "owed_base_units": "125000000",
  "paid_lifetime_csd": "42.00000000",
  "paid_lifetime_base_units": "4200000000",
  "eta_secs": 1200,
  "csd_per_hour": null,
  "csd_per_day": null,
  "session_csd": null,
  "session_secs": null,
  "shares_accepted": 1000,
  "shares_rejected": 12,
  "shares_stale": 5,
  "last_difficulty": 64,
  "last_seen_ts": 1781542961,
  "confirming_blocks": [],
  "payments": []
}
```

When PostgreSQL is configured, miner-level `shares_accepted` comes from accepted
`shares`; `shares_rejected` and `shares_stale` come from persisted
`share_events` grouped across the miner's workers. Without PostgreSQL, live
runtime state is used.

The session-rate fields are `null` until session boundaries and a real payout
rate estimator are persisted. The API never uses zero as a placeholder for
unknown earnings. `confirming_blocks` is derived from persisted blocks in
PostgreSQL mode; it is an empty list in the static development mode.

## 12. `GET /api/miner/:address/workers`

Per-worker detail.

Response:

```json
{
  "address": "<addr20>",
  "workers": [
    {
      "name": "rig-01",
      "online": true,
      "hashrate_hs": 28600000000.0,
      "shares_accepted": 500,
      "shares_rejected": 2,
      "shares_stale": 1,
      "blocks_found": 1,
      "last_difficulty": 64,
      "connected_at": "2026-06-16T01:00:00Z",
      "last_seen_at": "2026-06-16T02:00:00Z"
    }
  ]
}
```

Worker rows use the same persisted sources as the miner summary: accepted
`shares` plus rejected/stale `share_events`, pre-aggregated per worker to avoid
multi-join count inflation.

## 13. `GET /api/blocks`

Block history.

Query params:

```text
limit = 1..200
status = submitted | immature | confirmed | orphaned
```

Response:

```json
{
  "blocks": [
    {
      "height": 33605,
      "hash": "0x...",
      "finder": "<addr20>",
      "worker": "rig-01",
      "reward_csd": "50.00000000",
      "status": "confirmed",
      "confirmations": 12,
      "effort_pct": 91.2,
      "found_at": "2026-06-16T01:00:00Z",
      "confirmed_at": "2026-06-16T01:24:00Z"
    }
  ]
}
```

`effort_pct` is persisted when the pool submits a block candidate. It compares
current-round accepted share difficulty to the job's network target difficulty,
so the block table can show found-block effort without recomputing it later.
The built-in dashboard Recent Blocks table displays finder/worker, status,
confirmations, reward, and this per-block effort value.

## 14. `GET /api/payments`

Public payout history.

Response:

```json
{
  "payments": [
    {
      "batch_id": "uuid",
      "address": "<addr20>",
      "amount_csd": "1.25000000",
      "amount_base_units": "125000000",
      "txid": "0x...",
      "status": "confirmed",
      "created_at": "2026-06-16T01:00:00Z",
      "confirmed_at": "2026-06-16T01:05:00Z"
    }
  ]
}
```

## 15. Operator Payout APIs

Operator APIs require:

```text
Authorization: Bearer <CSD_POOL_OPERATOR_TOKEN>
```

`GET /api/operator/health`

Returns the latest stored CSD node and signer health samples:

```json
{
  "ok": true,
  "samples": [
    {
      "node_name": "node:node-a",
      "height": 12345,
      "chainwork": "0x...",
      "peers": 8,
      "mempool_size": null,
      "rpc_ms": 12.5,
      "ok": true,
      "sampled_at": "2026-06-16 01:00:00+00"
    }
  ]
}
```

`GET /api/operator/alerts?status=active`

```json
{
  "alerts": [
    {
      "fingerprint": "payout_stuck:payout-1781542961000",
      "severity": "warning",
      "status": "active",
      "kind": "payout_stuck",
      "subject": "payout-1781542961000",
      "message": "payout batch payout-1781542961000 is stuck in submitted",
      "first_seen_at": "2026-06-16 01:00:00+00",
      "last_seen_at": "2026-06-16 01:30:00+00",
      "resolved_at": null,
      "details": {
        "status": "submitted"
      }
    }
  ]
}
```

`POST /api/operator/alerts/{fingerprint}/resolve`

```json
{
  "resolved": true
}
```

The built-in dashboard exposes the same resolve action for active alerts when an
operator bearer token is configured in the browser.

`GET /api/operator/payouts/status`

```json
{
  "payouts_enabled": false
}
```

New deployments and upgrades default to paused payouts. Operators should call
`POST /api/operator/payouts/resume` only after signer and wallet controls are
verified. Pause and resume actions append payout audit events.

`POST /api/operator/payouts/pause`

```json
{
  "payouts_enabled": false
}
```

`POST /api/operator/payouts/resume`

```json
{
  "payouts_enabled": true
}
```

`GET /api/operator/payouts`

```json
{
  "batches": [
    {
      "batch_id": "payout-1781542961000",
      "status": "created",
      "total_base_units": "125000000",
      "total_csd": "1.25000000",
      "txid": null,
      "raw_tx_hash": null,
      "recipients": [
        {
          "miner": "<addr20>",
          "address": "<addr20>",
          "amount_base_units": "125000000",
          "amount_csd": "1.25000000"
        }
      ]
    }
  ]
}
```

`GET /api/operator/payouts/export.csv`

Returns one CSV row per payout recipient:

```csv
batch_id,status,txid,recipient,amount_base_units,amount_csd,total_base_units,total_csd
payout-1781542961000,created,,<addr20>,125000000,1.25000000,125000000,1.25000000
```

`GET /api/operator/payouts/preview`

Returns the next payout selection without creating a batch or locking balances:

```json
{
  "minimum_payout_base_units": "100000000",
  "minimum_payout_csd": "1.00000000",
  "max_payout_batch_base_units": "100000000000",
  "max_payout_batch_csd": "1000.00000000",
  "max_daily_payout_base_units": "500000000000",
  "max_daily_payout_csd": "5000.00000000",
  "manual_payout_approval_base_units": "25000000000",
  "manual_payout_approval_csd": "250.00000000",
  "daily_payout_used_base_units": "0",
  "daily_payout_used_csd": "0.00000000",
  "daily_remaining_base_units": "500000000000",
  "daily_remaining_csd": "5000.00000000",
  "recipient_count": 1,
  "total_base_units": "125000000",
  "total_csd": "1.25000000",
  "would_create_batch": true,
  "cap_exceeded": false,
  "daily_cap_exceeded": false,
  "manual_approval_required": false,
  "recipients": [
    {
      "miner": "<addr20>",
      "address": "<addr20>",
      "amount_base_units": "125000000",
      "amount_csd": "1.25000000"
    }
  ]
}
```

`POST /api/operator/payouts/{batch_id}/cancel`

Optional request body:

```json
{
  "reason": "operator cancelled payout"
}
```

Response:

```json
{
  "cancelled": true,
  "batch": {
    "batch_id": "payout-1781542961000",
    "status": "cancelled",
    "total_base_units": "125000000",
    "total_csd": "1.25000000",
    "txid": null,
    "raw_tx_hash": null,
    "recipients": []
  }
}
```

`POST /api/operator/payouts/{batch_id}/approve`

Approves a `needs_approval` batch and moves it to `created`, making it eligible
for `sign-payouts`.

```json
{
  "approved": true
}
```

`GET /api/operator/payouts/audit?limit=20&batch_id=<batch_id>`

Returns recent payout audit events. `batch_id` is optional.

```json
{
  "events": [
    {
      "batch_id": "payout-1781542961000",
      "actor": "operator",
      "action": "approve",
      "details": {},
      "created_at": "2026-06-16 10:00:00+00"
    }
  ]
}
```

`GET /api/operator/payouts/audit/export.csv?limit=1000&batch_id=<batch_id>`

Exports payout audit events as CSV:

```csv
created_at,batch_id,actor,action,details_json
2026-06-16 10:00:00+00,payout-1781542961000,operator,approve,{}
```

`POST /api/operator/payouts/{batch_id}/retry`

Retries a `failed` or `cancelled` batch by creating a new `created` batch with
the same recipients and locking balances again.

```json
{
  "retried": true,
  "new_batch_id": "retry-payout-1781542961000-1781542999000",
  "batch": {
    "batch_id": "retry-payout-1781542961000-1781542999000",
    "status": "created",
    "total_base_units": "125000000",
    "total_csd": "1.25000000",
    "txid": null,
    "raw_tx_hash": null,
    "recipients": []
  }
}
```

The pause flag is stored in PostgreSQL `pool_settings`. Worker commands that
create, sign, or submit payouts stop while payouts are disabled; reconciliation
continues for already-submitted transactions.

## 16. Signer API

The payout signer should run on an isolated host. If `CSD_POOL_SIGNER_TOKEN` is
set, requests require:

```text
Authorization: Bearer <CSD_POOL_SIGNER_TOKEN>
```

`POST /api/payout/sign`

Request:

```json
{
  "batch_id": "payout-1781542961000",
  "total_base_units": 125000000,
  "outputs": [
    {
      "address": "<addr20>",
      "amount_base_units": 125000000
    }
  ]
}
```

Response:

```json
{
  "node_tx": {
    "version": 1,
    "inputs": [{
      "prevout": { "txid": [0], "vout": 0 },
      "script_sig": [0]
    }],
    "outputs": [{ "value": 125000000, "script_pubkey": [0] }],
    "locktime": 0,
    "app": "None"
  },
  "txid": "0x followed by 64 hex chars"
}
```

The bundled `csd-pool-signer` validates exact totals and outputs, then returns a
deterministic mock transaction for integration testing. Production signing
must use the official CSD node transaction shape produced by the reference Rust
wallet or `@inversealtruism/csd-tx`. Each input carries a 32-byte prevout txid
and 99-byte `script_sig`; each output carries a positive value and 20-byte
`script_pubkey`. The legacy `raw_tx_hex` response remains accepted only for the
bundled local mock path and is rejected by real go-live evidence.

`csd-pool-workers check-signer` is the deployment-time contract check for this
API. It calls `/health`, then sends a one-output, 546-base-unit dust-safe
`contract-check-*` signing request back to the expected signer wallet. It
validates the official `node_tx` structure, signed input scripts, transaction ID,
and exact requested output without broadcasting the returned transaction.

## 17. Error Shape

HTTP errors:

```json
{
  "error": {
    "code": "invalid_address",
    "message": "address must be 40 hex chars"
  }
}
```

Use stable machine codes; message text may change.

## 18. Caching

Recommended cache headers:

```text
/api/pool      no-store or max-age=2
/api/getting-started max-age=60
/api/metrics   max-age=5
/api/history   max-age=30
/api/miner/*   max-age=5
/api/blocks    max-age=10
/api/payments  max-age=30
```

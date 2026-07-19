# CSD Mining Pool Product Design

## 1. Product Positioning

Build a CSD mining pool similar in user experience to Herominers, Pearlhash, and
Yamaduo:

- miners connect with a CSD payout address
- pool accepts shares through a Stratum-compatible endpoint
- rewards are distributed by share contribution
- payouts happen automatically
- users can inspect hashrate, workers, rewards, blocks, and payments
- operators can monitor nodes, stale rate, payouts, wallet risk, and abuse

The first public-compatible target should be the existing CSD pool miner from
`dangraagu/CSD-Mining-pool-public`.

## 2. User Roles

### 2.1 Anonymous Miner

Uses only a payout address. No login required.

Core needs:

- copy pool connection command
- see whether workers are online
- see accepted/rejected shares
- estimate earnings
- track unpaid, pending, and paid CSD
- verify payments by txid

### 2.2 Large Miner

Runs many rigs or cloud GPU nodes.

Core needs:

- worker naming
- per-worker hashrate
- offline worker alerts
- rejected/stale share diagnostics
- downloadable stats
- stable payout rules

### 2.3 Pool Operator

Runs pool infrastructure.

Core needs:

- node health and failover
- pool hashrate and network share
- block propagation and orphan rate
- wallet balance and payout queue
- suspicious miner detection
- service health alerts

### 2.4 Finance / Owner

Tracks business performance.

Core needs:

- pool fee revenue
- total rewards mined
- paid/unpaid liabilities
- wallet balance
- payout history
- miner concentration

## 3. Public Pages

### 3.1 Home / Pool Overview

Default landing page should be the actual pool dashboard, not a marketing page.

Metrics:

- pool hashrate
- network hashrate
- pool dominance
- online miners
- online workers
- current round effort
- expected block time
- blocks mined
- immature blocks
- confirmed blocks
- orphaned blocks
- accepted shares
- rejected shares
- next payout countdown based on the most recent payout batch
- payout interval
- confirmation depth
- pool fee

Charts:

- pool hashrate over 12h / 24h / 7d
- network hashrate over 12h / 24h / 7d
- pool dominance over time
- accepted vs rejected shares
- blocks found timeline

### 3.2 Getting Started

Must be short and copy-paste friendly.

Sections:

- create or paste CSD address
- download miner
- Windows one-command setup
- Linux one-command setup
- HiveOS setup
- manual binary setup
- GPU backend selection: auto / CUDA / OpenCL / CPU
- worker naming format
- pool ports and difficulty tiers
- payout rules

Example connection formats:

```text
csd-pool-miner --address <addr20>
csd-pool-miner --address <addr20> --backend cuda
```

For our own pool-compatible release, endpoint should be configurable or compiled
into the release binary depending on the distribution strategy.

Current implementation note: `/getting-started` serves the miner-facing setup
page and `/api/getting-started` returns the same setup contract as JSON. The
public Stratum display address comes from `CSD_POOL_PUBLIC_STRATUM_ADDR`, while
optional port tiers can be published with `CSD_POOL_PUBLIC_PORT_TIERS`.

### 3.3 Miner Lookup

Input:

- CSD address, with or without `0x`

Display:

- address
- current status: online / offline / unknown
- active workers
- session start time
- session length
- estimated CSD/hour
- estimated CSD/day
- accepted shares
- rejected shares
- stale shares
- last difficulty
- pending confirming CSD
- confirmed payable CSD
- paid lifetime CSD
- next payable ETA
- last payout txid

### 3.4 Worker Detail

For each worker:

- worker name
- IP region or masked IP
- connected since
- last seen
- hashrate estimate
- accepted shares
- rejected shares
- stale shares
- last share time
- last assigned difficulty
- blocks found
- backend if reported: CUDA / OpenCL / CPU
- software version

Worker states:

- online
- idle
- stale-heavy
- high reject rate
- offline

### 3.5 Blocks

Table fields:

- height
- block hash
- found time
- finder address / worker
- reward
- status: submitted / immature / confirmed / orphan
- confirmations
- effort
- luck
- txid / coinbase reference if available

Block detail:

- job id
- submit time
- propagation time
- node accepted time
- canonical confirmation time
- share difficulty of winning share
- worker contribution snapshot for reward split

### 3.6 Payments

Public payment table:

- payout time
- address
- amount
- txid
- status
- fee

Miner-specific payment table:

- amount
- txid
- created time
- confirmed time
- payout batch id

### 3.7 API Docs

Document stable public endpoints:

```text
GET /api/pool
GET /api/metrics
GET /api/history
GET /api/miner/:address
GET /api/blocks
GET /api/payments
GET /api/workers/:address
```

## 4. Miner Protocol Product Requirements

### 4.1 Pool Endpoints

Initial ports:

```text
3333  standard miners
5555  high-difficulty miners
7777  large GPU farms
8888  solo mode
```

Optional:

```text
443   TLS/WebSocket bridge if needed for restricted networks
```

### 4.2 Supported Protocol

Must support the existing CSD miner's Stratum v1 flow:

```text
mining.subscribe
mining.authorize
mining.set_difficulty
mining.notify
mining.submit
```

### 4.3 Worker Identity

Launch identity format:

```text
addr20
```

Later extension:

```text
addr20.worker
```

Rules:

- validate 40-hex address with optional `0x`
- normalize to lowercase no-prefix internally
- reject malformed addresses early
- allow worker names with safe characters only

### 4.4 Difficulty

Requirements:

- starting difficulty per port
- vardiff per connection: implemented as in-memory per-session state
- target share interval, for example 15-30 seconds
- min and max difficulty
- difficulty shown in UI
- reject-rate-aware difficulty adjustment

Current implementation uses `[stratum].initial_difficulty`,
`[stratum].min_difficulty`, `[stratum].max_difficulty`, and
`[stratum].target_share_secs`. It smooths accepted-share intervals with an EWMA,
waits at least 120 seconds between changes, requires separate fast/slow
hysteresis thresholds, and caps each adjustment. A finite positive
`mining.suggest_difficulty` is accepted only before the first accepted share and
is clamped to the configured range. Difficulty increases retain the prior
difficulty for a bounded transition grace so already-running work can finish.
The assigned difficulty is enforced during share validation by dividing the
base share target by the assigned difficulty. Multi-instance shared vardiff
state remains a Redis-backed follow-up.

## 5. Reward Design

### 5.1 Launch Reward Mode

Recommended default:

```text
PPLNS
```

Why:

- resistant to pool hopping
- familiar to miners
- fair for continuous miners

Suggested additional mode:

```text
solo port
```

Solo port rules:

- finder receives full block reward minus fee
- no sharing with other miners
- useful for large miners who want pool infrastructure but solo variance

### 5.2 Reward Lifecycle

Block reward states:

```text
found -> submitted -> canonical -> immature -> confirmed -> credited -> paid
```

Miner balance states:

```text
pending_confirming
confirmed_payable
paid_lifetime
```

### 5.3 Confirmations

Start with:

```text
confirm_depth = 10
```

This matches the currently observed Yamaduo-style API behavior and can be tuned
after measuring reorg/orphan risk.

### 5.4 Fees

Configurable:

- pool fee percentage
- solo fee percentage
- minimum payout
- payout transaction fee reserve

UI must show fees plainly.

## 6. Payout Product Design

### 6.1 Payout Schedule

Recommended launch:

```text
every 30 minutes or every 60 minutes
```

Yamaduo currently exposes a 1800-second payout interval, so 30 minutes is a
reasonable compatibility target.

### 6.2 Thresholds

Configurable:

- minimum payout, for example 1 CSD
- optional large-miner threshold override
- dust prevention threshold

### 6.3 Payout Batch Page

Operator and public views should show:

- next-batch preview before locking balances
- batch id
- created time
- number of recipients
- total amount
- txid
- status
- failure reason if failed

### 6.4 Wallet Safety

Product constraints:

- web server never stores private keys
- signer runs separately
- hot wallet keeps limited funds
- cold wallet receives sweep excess
- payout batch must be reproducible from database records
- emergency pause for payouts

Current implementation status: automatic batch creation refuses totals above
`[pool].max_payout_batch_csd` or the remaining `[pool].max_daily_payout_csd`.
Manual-threshold batches are stored as `needs_approval`, with balances locked
but signing blocked until an operator approves the batch. Operator preview exposes `cap_exceeded`,
`daily_cap_exceeded`, and `manual_approval_required` before any balance lock
entries are written. The operator console also shows recent payout audit events
covering create, approve, cancel, retry, sign, submit, confirm, and failure
paths. Payouts start paused on new deployments and upgrades until the operator
resumes them after wallet signoff, and the operator console exposes the current
payout status plus pause/resume controls.

## 7. Operator Console

### 7.1 Overview

Operator-only dashboard:

- pool services status
- Stratum connections
- active jobs
- active CSD nodes
- current tip
- node lag
- RPC latency
- mempool size
- payout queue
- signer status

### 7.2 Node Health

For each CSD node:

- height
- chainwork
- peers
- RPC health
- block template freshness
- last submit result
- propagation latency
- disk usage
- CPU/memory/network

### 7.3 Share Quality

Views:

- accepted shares per minute
- rejected shares per reason
- stale share rate
- duplicate share rate
- invalid PoW rate
- high reject workers
- high stale workers

### 7.4 Blocks And Luck

Views:

- round effort
- expected vs actual blocks
- luck by 24h / 7d / lifetime
- orphan rate
- propagation delay
- winning worker distribution

Current implementation note: round effort is live for the active in-memory
Stratum round. It uses accepted share difficulty divided by the expected network
difficulty implied by current network hashrate and target block time, and resets
after a block candidate is found. Found block records also persist `effort_pct`
at candidate submission time using active-round share difficulty divided by the
job target difficulty, so block history can show found-block effort. The pool
overview aggregates block effort into 24h, 7d, and lifetime luck percentages.
The built-in dashboard Recent Blocks table surfaces finder/worker, status,
confirmations, reward, and per-block effort for quick operator triage.

### 7.5 Payout Operations

Controls:

- pause payouts
- resume payouts
- dry-run next payout
- approve manual payout batch
- retry failed payout
- export payout CSV

### 7.6 Miner Management

Controls:

- ban IP
- ban address
- throttle connection
- force disconnect
- add allowlist for private beta
- note suspicious worker

## 8. Alerts

### 8.1 Operator Alerts

Channels:

- DingTalk
- Telegram
- Discord
- email webhook

Events:

- node height lag
- RPC down
- Stratum listener down
- no new jobs
- no accepted shares
- high stale rate
- high reject rate
- block found
- block orphaned
- payout failed
- wallet balance low
- signer offline
- abnormal miner concentration

### 8.2 Miner Alerts

Optional miner-facing alerts:

- worker offline
- payout sent
- high reject rate
- high stale rate
- block found by worker

Current implementation status: `template_age`, `block_submission`,
`no_accepted_shares`, `worker_offline`, `high_reject_rate`, and
`high_stale_rate` are generated as operator alerts by
`csd-pool-workers check-alerts`.
`block_submission` uses `blocks.submit_response_json` and
`CSD_POOL_BLOCK_SUBMISSION_STUCK_MINUTES`; `template_age` uses latest `jobs.created_at` and
`CSD_POOL_MAX_TEMPLATE_AGE_SECS`; `no_accepted_shares` uses latest
`shares.created_at` and `CSD_POOL_NO_ACCEPTED_SHARE_MINUTES`; `worker_offline`
uses `workers.last_seen_at` and `CSD_POOL_WORKER_OFFLINE_MINUTES`. Share quality
alerts use accepted shares plus persisted rejected/stale `share_events` over
`CSD_POOL_SHARE_QUALITY_WINDOW_MINUTES`. Operators can review and resolve active
alerts from the built-in dashboard or through the operator API. Miner-facing
delivery channels can reuse the same alert kinds later.

## 9. Public API Shape

### 9.1 `GET /api/pool`

Returns pool-level live metrics:

```json
{
  "pool_hashrate_hs": 744449784792.9026,
  "network_hashrate_hs": 2276860335272.5796,
  "network_share_pct": 32.69,
  "total_blocks": 180,
  "canonical_blocks": 3726,
  "workers_online": 55,
  "shares_accepted": 77586,
  "payout_interval_secs": 1800,
  "next_payout_secs": 347,
  "confirm_depth": 10
}
```

`next_payout_secs` should count down from the latest payout batch creation time
when persistence is available, falling back to the configured default before the
first batch is created.

### 9.2 `GET /api/metrics`

Returns worker summary:

```json
{
  "workers": {
    "<addr20>": {
      "shares_accepted": 161,
      "shares_rejected": 2,
      "blocks_found": 1,
      "last_difficulty": 76.35
    }
  },
  "totals": {
    "workers_online": 55,
    "shares_accepted": 77602,
    "shares_rejected": 4830,
    "blocks_found": 180
  },
  "fee_revenue": 0
}
```

### 9.3 `GET /api/history`

Returns chart samples:

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

Current implementation note: with PostgreSQL enabled, history samples are
bucketed from accepted `shares` plus rejected/stale `share_events`; without
PostgreSQL, the API returns one live in-memory sample so local demos stay usable.
`net_hs` uses the current network telemetry source when configured, not yet a
persisted historical network series.
The built-in dashboard share chart supports 12h, 24h, and 7d range buttons
backed by `/api/history`, and only uses live counter fallback when the history
endpoint is unavailable.

### 9.4 `GET /api/miner/:address`

Returns miner-specific state:

```json
{
  "address": "<addr20>",
  "pending_csd": "0.50000000",
  "owed_csd": "1.25000000",
  "paid_lifetime_csd": "42.00000000",
  "eta_secs": 1200,
  "csd_per_hour": "0.1234",
  "session_csd": "0.4567",
  "session_secs": 3600,
  "shares_accepted": 1000,
  "shares_rejected": 12,
  "shares_stale": 5,
  "last_difficulty": 64,
  "payments": []
}
```

Current implementation note: with PostgreSQL enabled, miner and worker share
quality totals read accepted shares from `shares` and rejected/stale totals from
persisted `share_events`, so miner lookup reflects historical invalid/stale
submissions rather than only current process memory.

## 10. Data Model

Core entities:

- miner account / address
- worker
- worker session
- Stratum connection
- job
- share
- block
- reward allocation
- balance
- payout batch
- payout recipient
- CSD node sample
- alert event

Important indexes:

- shares by time
- shares by worker
- blocks by height/hash
- balances by address
- payouts by txid
- sessions by last seen

## 11. Anti-Abuse And Reliability

### 11.1 Anti-Abuse

Controls:

- connection rate limit
- malformed frame ban
- duplicate share detection
- invalid PoW threshold ban
- excessive login failures ban
- per-IP worker cap
- per-address worker cap
- vardiff floor for spam prevention

Current implementation status:

- per-IP active connection cap is enforced in `csd-pool-bridge`
- per-address active session cap is enforced during `mining.authorize`
- malformed frame, authorization failure, and invalid share counters trigger a
  temporary in-memory IP ban
- duplicate submits are rejected in-session and guarded by database uniqueness
- multi-instance global bans still require Redis or a Stratum proxy layer
- dashboard tables, alert rows, health rows, and payout rows HTML-escape
  API-sourced strings before rendering
- API responses include baseline browser security headers covering CSP, content
  sniffing, framing, referrers, and browser permissions
- operator and signer bearer token checks use fixed-time equality comparisons

### 11.2 Reliability

Requirements:

- multiple CSD nodes
- job source failover
- block submit to multiple nodes
- persistent share buffer
- idempotent payout batches
- restart-safe accounting
- backup and restore procedure through `backup-db` and gated `restore-db`
- service health checks

## 12. Mobile Design

Mobile-first views:

- pool cards in 2-column grid
- miner lookup prominent
- worker list collapsible
- block table simplified
- payout txids truncated with copy button
- charts readable at small width

Avoid decorative landing sections. The first screen should show live pool state.

## 13. MVP Scope

### Must Have

- compatible Stratum endpoint
- address authorization
- share validation
- worker stats
- pool stats
- block tracking
- PPLNS accounting
- confirmed/payable balances
- automatic payout batches
- operator payout preview
- public dashboard
- public status page
- operator alerts

### Should Have

- solo port
- vardiff tuning UI
- worker offline alerts
- miner API
- block luck stats

### Later

- account login
- email/Telegram miner alerts
- geo endpoints
- TLS Stratum
- mobile app wrapper

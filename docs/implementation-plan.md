# CSD Pool Implementation Plan

## 1. Milestones

### M0: Repository Scaffold

Deliverables:

- Rust workspace
- crates for protocol, bridge, API, workers
- SQL migrations
- config loader
- local dev compose for PostgreSQL/Redis-backed services plus deployable ops
  templates
- CI for fmt/clippy/test plus release, preflight, verifier, and local e2e gates

Exit criteria:

- `cargo test` passes
- `docker-compose.yml` defines local PostgreSQL and Redis dependencies with
  health checks, and `ops/bin/csd-pool-dev-env.sh` can prepare a migrated local
  database plus generated env file
- `.github/workflows/ci.yml` runs fmt, clippy, tests, release-check, preflight,
  verifier, local e2e, and release artifact validation
- `ops/bin/csd-pool-ci-local.sh` mirrors the CI gate for release-candidate
  rehearsals
- empty bridge starts and exposes health endpoint

### M1: Protocol Compatibility

Deliverables:

- Stratum frame parser
- subscribe/authorize handling
- Yamaduo-compatible `notify` model
- submit parser
- fixture tests from `CSD-Mining-pool-public`

Exit criteria:

- public CSD miner can connect to local bridge
- miner authorizes successfully
- bridge can send a static test job

### M2: Template And Share Validation

Deliverables:

- CSD node client
- template builder or template RPC adapter
- 84-byte header reconstruction
- share target validation
- duplicate share handling
- accepted/rejected counters

Exit criteria:

- known valid share fixture accepted
- known invalid share rejected
- duplicate share rejected
- CSD node or adapter exposes `GET /api/rpc/mining/template?address=<pool_addr20>`
- CSD node or adapter exposes `POST /api/rpc/block/submit`
- CSD node or adapter exposes block and transaction status/submit endpoints used
  by `reconcile-blocks`, `submit-payouts`, and `reconcile-payouts`
- `csd-pool-mock-node` can serve the local adapter contract for deterministic
  CI and release-candidate checks
- `csd-pool-workers check-node-template` passes against the configured template
  node before miners are moved to live mode
- bridge can run with either static development jobs or `LiveTemplateProvider`
  selected by `CSD_POOL_TEMPLATE_MODE=live`
- when `CSD_POOL_DATABASE_URL` is set, bridge persists jobs and accepted shares
  through `MiningRepository`
- duplicate shares are blocked in-session and by the PostgreSQL unique index
- when `CSD_POOL_SUBMIT_CANDIDATES=true`, bridge submits block candidates to the
  configured submit node using the structured pool-adapter payload
- submitted candidates can be persisted to `blocks` with candidate payload and
  node response JSON
- `csd-pool-workers reconcile-blocks` can advance submitted blocks from the
  CSD watch adapter status response

### M3: Live Mining Private Beta

Deliverables:

- live CSD jobs from CSD nodes
- candidate block submission
- block watcher
- PostgreSQL share persistence
- `/api/pool`, `/api/metrics`, `/api/history`
- simple dashboard
- unified `csd-pool-daemon` runs Stratum and API with shared runtime state

Exit criteria:

- one internal miner submits accepted shares
- candidate block submission path is tested on testnet or controlled mainnet
- dashboard shows live worker, shares, and miner address lookup
- API counters reflect Stratum authorize/share activity in the same process

### M4: Rewards And Payout Dry Run

Deliverables:

- PPLNS window
- reward allocation
- ledger entries
- balance cache
- payout batch dry-run
- operator payout preview

Exit criteria:

- confirmed block produces allocations
- `csd-pool-workers settle-rewards` writes `reward_immature` and `pool_fee`
  ledger entries for confirmed, unsettled blocks
- reward ledger insertion updates immature miner balances
- `csd-pool-workers mature-rewards` writes `reward_mature` entries after
  confirmation depth and moves balances from immature to confirmed
- `csd-pool-workers reverse-orphans` writes idempotent
  `reward_orphan_reversal` entries for orphaned rewarded blocks and subtracts
  the affected miner balance from the immature or confirmed bucket
- balances can be recomputed
- dry-run payout batch matches ledger
- PPLNS pure logic preserves reward total minus fee exactly
- payout selection respects threshold and batch recipient limit
- reward dry-run emits immutable ledger entries
- payout dry-run emits payout batch lock entries
- dry-run workers write through `PoolRepository`
- repository can list persisted ledger entries, balances, and payout batches
- ledger JSON uses the same snake_case kind values as PostgreSQL constraints
- SQL migration keeps duplicate share and ledger idempotency constraints valid
- workers can apply PostgreSQL migrations and optionally persist dry-runs to
  PostgreSQL through `PgRepository`
- public API can use `DashboardRepository` to read persisted blocks, payments,
  miner balances, worker totals, and fee revenue when PostgreSQL is configured

### M5: Automatic Payouts

Deliverables:

- isolated signer HTTP contract: `POST /api/payout/sign`
- runnable `csd-pool-signer` deterministic mock signer for integration tests
- payout worker commands: `payout-preview`, `create-payouts`, `sign-payouts`,
  `submit-payouts`, `reconcile-payouts`
- CSD tx submit/status adapter calls:
  `POST /api/rpc/tx/submit`, `GET /api/rpc/tx/status`
- transactional payout balance states:
  `payout_lock`, `payout_sent`, `payout_failed_unlock`
- payout retry logic through idempotent batch ids and ledger entries
- payout pause switch via `pool_settings.payouts_enabled` and operator API
- operator preview, cancel, retry, and CSV export for payout batches

Exit criteria:

- payable confirmed balances can be locked into one batch
- signer receives exact batch outputs
- development signer validates outputs and returns raw transaction plus txid
- submitted payout can be marked confirmed and paid
- operator can pause/resume create, sign, and submit payout workers
- operator can preview the next payout selection without locking balances
- operator can cancel unreleased batches and retry failed/cancelled batches
- operator can export payout batches as recipient-level CSV
- operator can export immutable accounting ledger CSV
- small real payout succeeds
- retry does not double-pay
- failed payout unlocks balances safely

### M6: Public Beta Hardening

Deliverables:

- rate limits: in-memory per-IP Stratum connection cap and per-address session
  cap are implemented
- ban manager: in-memory temporary IP bans for malformed frames, auth failures,
  and invalid shares are implemented
- vardiff: in-memory per-session retargeting is implemented from `[stratum]`
  difficulty bounds
- Stratum proxy/HAProxy: single-host HAProxy template added under `ops/`
- Prometheus metrics: `/metrics` exports pool-level aggregate counters/gauges
- alerts: health sampling, stuck payout detection, active/resolved event store
- backup/restore runbook: `backup-db` and gated `restore-db` worker commands
  documented
- public getting-started docs and ops templates

Exit criteria:

- load test passes
- backup restore tested against a separate restore database before production
- signer isolated
- public dashboard complete: API root serves a responsive pool console
- operator can inspect health samples and active alerts

Current implementation note: `csd-pool-workers stratum-load-test` reuses the
Stratum smoke handshake against 100+ simulated miners and reports pass/fail,
latency, connection rate, and failure summaries. `ops/bin/csd-pool-verify.sh`
can run it with `CSD_POOL_VERIFY_LOAD=1`.

## 2. Work Breakdown

### Protocol

- `stratum::frame`
- `stratum::methods`
- `stratum::session`
- `stratum::vardiff`
- `stratum::errors`

### CSD Consensus Adapter

- `csd::header`
- `csd::coinbase`
- `csd::target`
- `csd::template`
- `csd::submit`
- `csd::rpc`

### Persistence

- migrations
- repositories
- share writer
- block repository
- ledger repository
- payout repository

### Workers

- block watcher
- reward engine
- payout worker
- alert worker
- metrics sampler

### Web/API

- public API
- operator API
- dashboard frontend
- static docs

## 3. Suggested Rust Workspace

```text
crates/
  csd-pool-protocol/
  csd-pool-consensus/
  csd-pool-bridge/
  csd-pool-api/
  csd-pool-daemon/
  csd-pool-workers/
  csd-pool-db/
  csd-pool-config/
  csd-pool-signer/
migrations/
web/
ops/
```

## 4. Development Order

Recommended order:

1. protocol parser and tests
2. CSD header/coinbase reconstruction tests
3. static Stratum bridge
4. CSD node RPC client
5. live template manager
6. share persistence
7. dashboard API
8. block watcher
9. reward ledger
10. payout dry-run
11. signer and real payout
12. hardening

## 5. Benchmarks

Benchmark before switching internal fleet:

- public `csd-pool-miner` on one A100 host
- current internal `csd-cuda-search` solo miner on same host
- accepted share rate
- stale/reject rate
- GPU utilization
- wall power if available
- CSD/hour estimated over 2-4 hours

Decision rule:

```text
Switch more machines only if pool mode has equal or better effective yield
after rejects/stales/fees, or if smoother payout is worth a small fee loss.
```

## 6. Definition Of Done

Private beta is done when:

- one A100 worker mines through our pool for 24h
- no accounting drift
- no duplicate payout risk
- dashboard matches database
- operator alerts work

Public beta is done when:

- external miner can connect from clean install docs
- 100+ simulated workers pass load test
- payouts are automatic and confirmed
- all critical alerts are enabled
- backup restore is tested

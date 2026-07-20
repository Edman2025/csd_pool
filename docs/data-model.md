# CSD Pool Data Model

## 1. Accounting Model

Use a ledger-first accounting model:

- immutable reward entries
- immutable payout entries
- compensating entries for reorgs or corrections
- cached balances can be rebuilt from ledger

Balance categories:

```text
immature      reward exists but block not confirmed
confirmed     reward confirmed and payable
locked        included in pending payout batch
paid          paid on-chain
fee           pool operator fee
```

## 2. Core Tables

### 2.1 Miners And Workers

```sql
create table miners (
  id bigserial primary key,
  address text not null unique,
  created_at timestamptz not null default now(),
  last_seen_at timestamptz
);

create table workers (
  id bigserial primary key,
  miner_id bigint not null references miners(id),
  name text not null,
  created_at timestamptz not null default now(),
  last_seen_at timestamptz,
  unique(miner_id, name)
);

create table sessions (
  id uuid primary key,
  worker_id bigint not null references workers(id),
  remote_addr inet,
  remote_port integer,
  user_agent text,
  extranonce1 text not null,
  server_session_id bigint,
  server_release text not null default 'unknown',
  server_instance text not null default 'default',
  assigned_difficulty numeric not null default 8,
  difficulty_updated_at timestamptz not null default now(),
  started_at timestamptz not null default now(),
  ended_at timestamptz
);
```

### 2.2 Jobs And Shares

```sql
create table jobs (
  id text primary key,
  prev_hash text not null,
  height bigint,
  version_hex text not null,
  nbits_hex text not null,
  ntime_hex text not null,
  network_target bytea not null,
  coinb1_hex text not null,
  coinb2_hex text not null,
  merkle_branches_json jsonb not null,
  clean_jobs boolean not null,
  job_reason text not null default 'tip_change'
    check (job_reason in ('tip_change', 'heartbeat')),
  created_at timestamptz not null default now(),
  retired_at timestamptz
);

create table shares (
  id bigserial primary key,
  worker_id bigint not null references workers(id),
  session_id uuid references sessions(id),
  job_id text not null references jobs(id),
  difficulty numeric not null,
  hash bytea not null,
  extranonce2 text not null,
  ntime text not null,
  nonce text not null,
  is_block_candidate boolean not null default false,
  created_at timestamptz not null default now(),
  unique(job_id, worker_id, extranonce2, ntime, nonce)
);
```

For public scale, add a later partitioning migration after the ingestion path has an
explicit partition key strategy. The MVP schema intentionally keeps `shares` as a
regular table so the duplicate-share guard can stay globally unique:

```sql
unique(job_id, worker_id, extranonce2, ntime, nonce)
```

Dashboard history uses `shares.created_at` for accepted-share buckets and
`sum(shares.difficulty)` to estimate bucket hashrate. Rejected and stale bucket
counts come from `share_events.created_at`, grouped by `kind`.

Rejected and stale submissions are recorded separately for quality alerts:

```sql
create table share_events (
  id bigserial primary key,
  miner_id bigint not null references miners(id),
  worker_id bigint not null references workers(id),
  job_id text,
  kind text not null check (kind in ('rejected', 'stale')),
  reason text not null,
  created_at timestamptz not null default now()
);
```

### 2.3 Blocks

```sql
create table blocks (
  id bigserial primary key,
  height bigint,
  hash text unique,
  job_id text references jobs(id),
  finder_worker_id bigint references workers(id),
  reward_base_units numeric not null default 0,
  status text not null,
  confirmations integer not null default 0,
  effort_pct numeric,
  candidate_payload_json jsonb not null default '{}'::jsonb,
  submit_response_json jsonb not null default '{}'::jsonb,
  submitted_at timestamptz,
  seen_at timestamptz,
  confirmed_at timestamptz,
  orphaned_at timestamptz
);
```

`effort_pct` is written when a candidate block is submitted. It is derived from
the accepted share difficulty accumulated in the active round divided by the
network difficulty implied by the job's target.

Valid statuses:

```text
submitted
seen_on_chain
immature
confirmed
orphaned
```

### 2.4 Ledger And Balances

```sql
create table ledger_entries (
  id bigserial primary key,
  miner_id bigint references miners(id),
  amount_base_units numeric not null,
  kind text not null,
  ref_type text not null,
  ref_id text not null,
  created_at timestamptz not null default now()
);

create unique index ledger_entries_idempotency_idx
  on ledger_entries((coalesce(miner_id::text, 'pool')), kind, ref_type, ref_id);

create table balance_cache (
  miner_id bigint primary key references miners(id),
  immature_base_units numeric not null default 0,
  confirmed_base_units numeric not null default 0,
  locked_base_units numeric not null default 0,
  paid_base_units numeric not null default 0,
  updated_at timestamptz not null default now()
);
```

Ledger kinds:

```text
reward_immature
reward_mature
reward_orphan_reversal
payout_lock
payout_sent
payout_failed_unlock
pool_fee
manual_adjustment
```

### 2.5 Payouts

```sql
create table payout_batches (
  id text primary key,
  status text not null,
  total_base_units numeric not null,
  recipient_count integer not null,
  txid text,
  raw_tx_hash text,
  created_at timestamptz not null default now(),
  signed_at timestamptz,
  submitted_at timestamptz,
  confirmed_at timestamptz,
  failed_at timestamptz,
  failure_reason text
);

create table payout_recipients (
  batch_id text not null references payout_batches(id),
  miner_id bigint not null references miners(id),
  address text not null,
  amount_base_units numeric not null,
  primary key(batch_id, miner_id)
);
```

Payout statuses:

```text
created
dry_run_ok
signed
submitted
confirmed
failed
cancelled
```

Payout ledger effects:

```text
payout_lock           confirmed -> locked
payout_sent           locked -> paid
payout_failed_unlock  locked -> confirmed
```

The payout worker treats a failed signer, rejected broadcast, or failed
transaction status as a compensating ledger event rather than editing the
original lock rows.

### 2.6 Runtime Settings

```sql
create table pool_settings (
  key text primary key,
  value text not null,
  updated_at timestamptz not null default now()
);
```

Initial settings:

```text
payouts_enabled = false
```

New deployments and upgrades default to `payouts_enabled=false` so payouts
start paused until operator signoff. This pauses payout creation, signing, and
submission. Payout reconciliation should continue so already-submitted
transactions can be marked confirmed or failed.

### 2.6 Node Samples

```sql
create table node_samples (
  id bigserial primary key,
  node_name text not null,
  height bigint,
  chainwork text,
  peers integer,
  mempool_size integer,
  rpc_ms numeric,
  ok boolean not null,
  sampled_at timestamptz not null default now()
);
```

### 2.7 Alert Events

```sql
create table alert_events (
  id bigserial primary key,
  fingerprint text not null unique,
  severity text not null,
  status text not null default 'active',
  kind text not null,
  subject text not null,
  message text not null,
  first_seen_at timestamptz not null default now(),
  last_seen_at timestamptz not null default now(),
  resolved_at timestamptz,
  details_json jsonb not null default '{}'::jsonb
);
```

Current generated alert kinds:

```text
service_health
payout_stuck
no_accepted_shares
worker_offline
high_reject_rate
high_stale_rate
```

## 3. Redis Keys

Suggested keys:

```text
session:{session_id}
worker_live:{worker_id}
job:current
job:{job_id}
vardiff:{session_id}
duplicate_share:{job_id}:{worker_id}:{extranonce2}:{ntime}:{nonce}
pool:counters
api:pool_snapshot
```

Duplicate share keys should expire after job retirement plus stale grace.

## 4. Recompute Procedures

Required admin commands:

```text
recompute-balances
recompute-pool-hashrate
recompute-miner-hashrate
reconcile-blocks
reconcile-payouts
```

Every cached API number must be derivable from durable tables.

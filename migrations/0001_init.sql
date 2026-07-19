create table if not exists schema_migrations (
  version bigint primary key,
  name text not null,
  applied_at timestamptz not null default now()
);

create table if not exists miners (
  id bigserial primary key,
  address text not null unique,
  created_at timestamptz not null default now(),
  last_seen_at timestamptz,
  constraint miners_address_addr20 check (address ~ '^[0-9a-f]{40}$')
);

create table if not exists workers (
  id bigserial primary key,
  miner_id bigint not null references miners(id),
  name text not null,
  created_at timestamptz not null default now(),
  last_seen_at timestamptz,
  unique(miner_id, name)
);

create table if not exists sessions (
  id uuid primary key,
  worker_id bigint not null references workers(id),
  remote_addr inet,
  user_agent text,
  extranonce1 text not null,
  started_at timestamptz not null default now(),
  ended_at timestamptz,
  constraint sessions_extranonce1_hex check (extranonce1 ~ '^[0-9a-f]{8}$')
);

create table if not exists jobs (
  id text primary key,
  prev_hash text not null,
  height bigint,
  version_hex text not null,
  nbits_hex text not null,
  ntime_hex text not null,
  network_target bytea not null,
  share_target bytea not null,
  coinb1_hex text not null,
  coinb2_hex text not null,
  merkle_branches_json jsonb not null,
  clean_jobs boolean not null,
  created_at timestamptz not null default now(),
  retired_at timestamptz,
  constraint jobs_prev_hash_hex check (prev_hash ~ '^[0-9a-f]{64}$'),
  constraint jobs_version_hex check (version_hex ~ '^[0-9a-f]{8}$'),
  constraint jobs_nbits_hex check (nbits_hex ~ '^[0-9a-f]{8}$'),
  constraint jobs_ntime_hex check (ntime_hex ~ '^[0-9a-f]{8}$')
);

create table if not exists shares (
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
  unique(job_id, worker_id, extranonce2, ntime, nonce),
  constraint shares_extranonce2_hex check (extranonce2 ~ '^[0-9a-f]{8}$'),
  constraint shares_ntime_hex check (ntime ~ '^[0-9a-f]{8}$'),
  constraint shares_nonce_hex check (nonce ~ '^[0-9a-f]{8}$')
);

create table if not exists blocks (
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
  orphaned_at timestamptz,
  constraint blocks_status check (
    status in ('submitted', 'seen_on_chain', 'immature', 'confirmed', 'orphaned')
  )
);

create table if not exists ledger_entries (
  id bigserial primary key,
  miner_id bigint references miners(id),
  amount_base_units numeric not null,
  kind text not null,
  ref_type text not null,
  ref_id text not null,
  created_at timestamptz not null default now(),
  constraint ledger_kind check (
    kind in (
      'reward_immature',
      'reward_mature',
      'reward_orphan_reversal',
      'payout_lock',
      'payout_sent',
      'payout_failed_unlock',
      'pool_fee',
      'manual_adjustment'
    )
  )
);

create table if not exists balance_cache (
  miner_id bigint primary key references miners(id),
  immature_base_units numeric not null default 0,
  confirmed_base_units numeric not null default 0,
  locked_base_units numeric not null default 0,
  paid_base_units numeric not null default 0,
  updated_at timestamptz not null default now()
);

create table if not exists payout_batches (
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
  failure_reason text,
  constraint payout_batches_status check (
    status in ('needs_approval', 'created', 'dry_run_ok', 'signed', 'submitted', 'confirmed', 'failed', 'cancelled')
  )
);

create table if not exists payout_recipients (
  batch_id text not null references payout_batches(id),
  miner_id bigint not null references miners(id),
  address text not null,
  amount_base_units numeric not null,
  primary key(batch_id, miner_id),
  constraint payout_recipients_address_addr20 check (address ~ '^[0-9a-f]{40}$')
);

create table if not exists node_samples (
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

create index if not exists miners_last_seen_idx on miners(last_seen_at);
create index if not exists workers_last_seen_idx on workers(last_seen_at);
create index if not exists sessions_worker_started_idx on sessions(worker_id, started_at desc);
create index if not exists jobs_created_idx on jobs(created_at desc);
create index if not exists blocks_height_idx on blocks(height desc);
create index if not exists blocks_status_idx on blocks(status);
create index if not exists blocks_submitted_idx on blocks(submitted_at desc);
create index if not exists ledger_miner_created_idx on ledger_entries(miner_id, created_at desc);
create unique index if not exists ledger_entries_idempotency_idx
  on ledger_entries((coalesce(miner_id::text, 'pool')), kind, ref_type, ref_id);
create index if not exists payout_batches_status_idx on payout_batches(status);
create index if not exists node_samples_node_sampled_idx on node_samples(node_name, sampled_at desc);

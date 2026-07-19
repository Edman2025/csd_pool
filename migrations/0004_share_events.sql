create table if not exists share_events (
  id bigserial primary key,
  miner_id bigint not null references miners(id),
  worker_id bigint not null references workers(id),
  job_id text,
  kind text not null,
  reason text not null,
  created_at timestamptz not null default now(),
  constraint share_events_kind check (kind in ('rejected', 'stale'))
);

create index if not exists share_events_created_idx on share_events(created_at desc);
create index if not exists share_events_worker_kind_created_idx
  on share_events(worker_id, kind, created_at desc);

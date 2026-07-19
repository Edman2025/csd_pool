create table if not exists payout_audit_events (
  id bigserial primary key,
  batch_id text not null,
  actor text not null,
  action text not null,
  details_json jsonb not null default '{}'::jsonb,
  created_at timestamptz not null default now()
);

create index if not exists payout_audit_events_batch_created_idx
  on payout_audit_events(batch_id, created_at desc);

create index if not exists payout_audit_events_created_idx
  on payout_audit_events(created_at desc);

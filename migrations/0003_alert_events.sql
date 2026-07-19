create table if not exists alert_events (
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
  details_json jsonb not null default '{}'::jsonb,
  constraint alert_events_severity check (severity in ('info', 'warning', 'critical')),
  constraint alert_events_status check (status in ('active', 'resolved'))
);

create index if not exists alert_events_status_last_seen_idx
  on alert_events(status, last_seen_at desc);

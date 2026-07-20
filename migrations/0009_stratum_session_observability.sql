alter table sessions
  add column if not exists server_session_id bigint,
  add column if not exists server_release text not null default 'unknown',
  add column if not exists server_instance text not null default 'default',
  add column if not exists remote_port integer
    check (remote_port is null or remote_port between 1 and 65535),
  add column if not exists assigned_difficulty numeric not null default 8,
  add column if not exists difficulty_updated_at timestamptz not null default now();

alter table share_events
  add column if not exists session_id uuid references sessions(id);

create index if not exists sessions_active_started_idx
  on sessions(started_at desc)
  where ended_at is null;

create index if not exists sessions_worker_started_idx
  on sessions(worker_id, started_at desc);

create index if not exists sessions_instance_active_idx
  on sessions(server_instance, started_at desc)
  where ended_at is null;

create index if not exists sessions_user_agent_idx
  on sessions(user_agent)
  where user_agent is not null;

create index if not exists share_events_session_created_idx
  on share_events(session_id, created_at desc)
  where session_id is not null;

create index if not exists shares_session_created_idx
  on shares(session_id, created_at desc)
  where session_id is not null;

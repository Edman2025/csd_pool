create table if not exists pool_settings (
  key text primary key,
  value text not null,
  updated_at timestamptz not null default now()
);

insert into pool_settings(key, value)
values ('payouts_enabled', 'false')
on conflict(key) do nothing;

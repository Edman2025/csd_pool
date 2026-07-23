alter table blocks drop constraint if exists blocks_status;

alter table blocks add constraint blocks_status check (
  status in (
    'submitted',
    'submitted_secondary',
    'submitted_degraded',
    'relay_failed',
    'seen_on_chain',
    'immature',
    'confirmed',
    'orphaned'
  )
);

create index if not exists blocks_relay_failed_idx
  on blocks(submitted_at asc)
  where status in ('submitted_secondary', 'submitted_degraded', 'relay_failed');

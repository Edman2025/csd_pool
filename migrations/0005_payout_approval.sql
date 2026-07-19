alter table payout_batches drop constraint if exists payout_batches_status;

alter table payout_batches
  add constraint payout_batches_status check (
    status in (
      'needs_approval',
      'created',
      'dry_run_ok',
      'signed',
      'submitted',
      'confirmed',
      'failed',
      'cancelled'
    )
  );

create index if not exists payout_batches_status_idx on payout_batches(status);

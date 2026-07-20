alter table jobs
  add column if not exists job_reason text not null default 'tip_change';

alter table jobs
  drop constraint if exists jobs_job_reason_check;

alter table jobs
  add constraint jobs_job_reason_check
  check (job_reason in ('tip_change', 'heartbeat'));

create index if not exists jobs_reason_created_idx
  on jobs(job_reason, created_at desc);

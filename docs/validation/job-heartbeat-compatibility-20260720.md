# Same-tip job heartbeat compatibility

## Scope

This change addresses legacy miner reconnect storms when the chain tip does not
change for more than 300 seconds. It changes job publication and in-memory job
retention only. It does not change consensus, templates, candidate
construction, signer behavior, wallet state, payout rules, or production
services.

Production port 3333 remains on the restored `csd-pool-77eb176@77eb176`
daemon. The existing two-miner port-3334 canary, both nodes, signer, wallet, and
miners remain unchanged while this work is validated.

## Policy

- `CSD_POOL_JOB_HEARTBEAT_SECS` defaults to 120 seconds. Zero disables the
  heartbeat.
- `CSD_POOL_JOB_RETENTION_SECS` defaults to 900 seconds and is clamped to at
  least twice the heartbeat interval and at least 300 seconds.
- A new chain tip publishes a new job with `clean_jobs=true`, records
  `job_reason=tip_change`, and invalidates retained jobs from the old tip.
- An unchanged tip may publish a fresh template and job ID with
  `clean_jobs=false` and `job_reason=heartbeat`.
- Jobs from the same tip remain submit-capable through the retention window.
  A heartbeat therefore does not cancel a miner's in-flight nonce sweep.
- A valid first `mining.suggest_difficulty` is accepted at most once per
  Stratum session and only before the first accepted share. Late or repeated
  suggestions are acknowledged without changing difficulty. Applied
  difficulty changes send both
  `mining.set_difficulty` and a non-clean notify so legacy clients apply the
  target without abandoning same-tip work.

Migration 10 adds the constrained `jobs.job_reason` field. Pool state and
Prometheus expose total tip-change and heartbeat notifications plus the age of
the most recent notify.

## Root-cause evidence

During the failed production gate, the fleet entered a reconnect cycle after a
563.999-second same-tip interval. Short sessions had a 13.993-second median
lifetime, matching the miners' 15-second watchdog tick. Server journals showed
client EOF rather than a server-side close, parser rejection, ban, or
rate-limit action. A separate 304.980-second interval after rollback reproduced
one legacy reconnect cycle on the old daemon.

The short sessions also amplified `low_difficulty`: each reconnect reset
initial difficulty, repeated `mining.suggest_difficulty`, rotated extranonce1,
and could receive an in-flight result from the old sweep. This change removes
the unnecessary same-tip reconnect and limits suggestions to once per session.
That mechanism is supported by session timelines, but its long-run reject-rate
improvement still requires private-port and production measurement.

## Offline verification

The bridge uses Tokio's monotonic clock for heartbeat scheduling and retention,
including paused-clock tests. The protocol-level 540-second same-tip
simulation creates one legacy-r72 session and one liveness-v2 session, advances
through four 120-second heartbeats plus 60 seconds, and verifies:

- both TCP/Stratum sessions remain active;
- neither client needs a reconnect;
- heartbeat notifications have `clean_jobs=false`;
- the original same-tip job remains valid;
- both sessions can submit an accepted share against that original job after
  the simulated long gap.

Additional tests cover heartbeat publication without cleaning, tip-change
invalidation, first-suggestion application, repeat- and late-suggestion
suppression, non-clean notify on difficulty changes, database migration, state
metrics, and Prometheus output.

## Remaining gates

1. Run `cargo fmt --all -- --check`, the full workspace tests, strict Clippy,
   and the release check on the final tree.
2. Build and checksum the isolated daemon artifact.
3. Reproduce a greater-than-300-second same-tip interval on a new private port
   with one legacy-r72 protocol client and one liveness-v2 protocol client.
   Preserve the existing port-3334 baseline.
4. Confirm continuous accepted work, zero repeat reconnect storm, retained old
   same-tip submission, job-reason metrics, and healthy operator/session data.
5. Only after the private gate passes, schedule a separate backed-up production
   daemon window. Re-run 15-minute and 60-minute gates before releasing any
   miner upgrade.

No production deployment is authorized by the offline test result alone.

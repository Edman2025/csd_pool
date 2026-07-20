# VarDiff and session observability rollout

## Scope

This release changes Stratum session accounting and per-session difficulty
control. It does not change consensus, block templates, candidate submission,
signer behavior, or payout state.

## VarDiff

- EWMA alpha: 0.25.
- Raise threshold: observed interval below 70% of the 20-second target.
- Lower threshold: observed interval above 140% of the target.
- Minimum automatic adjustment interval: 120 seconds.
- Maximum single adjustment: 2x up or 0.5x down.
- Previous lower difficulty grace after an increase: 120 seconds.
- `mining.suggest_difficulty` is advisory and receives the same min/max,
  single-step, and 120-second rate-limit clamps.
- Accepted shares are persisted at the difficulty they actually satisfied.

Difficulty 8 corresponds to an expected interval of about 11.5 seconds at
3.0 GH/s and 24.7 seconds at 1.39 GH/s, covering mature V100 and T4 workers
without an immediate adjustment.

## Session and version evidence

Migration 9 records:

- UUID and numeric server session ID;
- server instance and release;
- worker, remote address and source port, extranonce1, and miner
  user-agent/version;
- assigned difficulty and last difficulty update;
- start/end timestamps;
- accepted and rejected/stale share linkage.

At Stratum startup, only stale sessions belonging to the same
`CSD_POOL_INSTANCE_ID` are closed. A private canary must use a distinct
instance ID so it cannot close production sessions.

The authenticated endpoint `GET /api/operator/sessions?limit=100` returns
version summaries and recent session lifecycle. It does not expose bearer
tokens or signer material.

## Verification

- Full Rust workspace tests pass.
- Clippy passes with warnings denied.
- A disposable PostgreSQL instance successfully applies migrations 1 through
  9 and completes session open, difficulty update, accepted/rejected share
  linkage, version aggregation, recent-session query, and close.

## Production rollout

1. Keep the current production daemon unchanged through the miner upgrade
   freeze.
2. Back up PostgreSQL and record current Stratum connections, 5/15/60-minute
   accepted-difficulty hashrate, low-difficulty, stale, and unknown-job counts.
3. Start a private canary with distinct Stratum/API ports and
   `CSD_POOL_INSTANCE_ID=vardiff-canary`.
4. Connect one mature V100 and one T4. Confirm session rows, user-agent,
   release, share linkage, difficulty updates, and old-difficulty grace.
5. Observe at least 30 minutes with no connection storm, persistent reject
   increase, or effective-hashrate regression.
6. Perform one controlled production daemon restart. Do not restart either
   node or signer.
7. Gate for 15 minutes, then 60 minutes. Compare low-difficulty rate and
   accepted-difficulty hashrate with the frozen baseline.

## Rollback

Restore the previous daemon binary and configuration, then perform one daemon
restart. Migration 9 is forward-compatible with the previous binary and does
not need to be reverted. The old daemon ignores the additional nullable/default
columns.

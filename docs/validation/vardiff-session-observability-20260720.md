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
- Assigned difficulty is rounded to the nearest integer before it is sent,
  persisted, or used for share validation. This matches the official miner's
  target derivation and prevents fractional target disagreement.
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

## Private canary evidence

The first private-port run exposed a real compatibility defect: the official
miner rounded fractional assigned difficulty to the nearest integer while the
pool used `ceil`. At difficulty 9.4443, the miner therefore used difficulty 9
and the pool validated against difficulty 10. Four otherwise valid V100 shares
were rejected as `low_difficulty`. Production was not changed.

Commit `dbac74b92f37e3042ba544e6fc061d016d221288` applies nearest-integer
quantization consistently to difficulty notification, validation, transition
grace, and accounting. The Linux canary daemon SHA-256 is
`c7a16441dc9acfb59f1466ff9c85f467fff6bdac62f9e06248c4a4961da628a7`.

The corrected two-miner window began at 15:07:25 CST on private port 3334:

- T4 session `45a5cad1-adea-4870-b409-6c53b4dbcd3a` and V100 session
  `bab48537-827c-4343-a74e-8726395bef02` were uniquely linked to worker,
  NAT peer, user-agent, server release, integer difficulty, and shares.
- At the 30-minute gate, both sessions had continuous accepted submissions and
  zero low-difficulty, stale, or other rejects.
- Miner and pool accepted counts matched at the same boundary: 75 for T4 and
  114 for V100.
- Miner-side 15-minute raw rates remained 1.337 GH/s for T4 and 3.174 GH/s for
  V100, with unchanged process, binary, and GPU health. Lower pool-side
  accepted-difficulty estimates in the short dynamic-difficulty window were
  sampling variance, not missing submissions.
- The canary daemon, both nodes, signer, operator API, and migration 9 remained
  healthy. No production miner was admitted as part of this gate.

The corrected private canary passed.

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

## Production execution

The production baseline at 15:48:32 CST included the separately admitted eight
A100 workers:

- 316 connections on port 3333 and 2 private canary connections on port 3334;
- 971.436 / 964.831 / 947.907 GH/s over 5 / 15 / 60 minutes;
- low-difficulty counts 35 / 102 / 386, stale counts 10 / 15 / 72, and zero
  other rejects over those windows.

The pre-cut PostgreSQL custom-format backup, prior daemon, configuration,
environment, unit, checksums, and tested rollback script are stored under
`/data/csd-pool/backup/prod-vardiff-session-20260720-154932`. Migration 9 is
retained on rollback.

Production daemon release `csd-pool-vardiff-session-dbac74b` started at
15:51:11 CST with PID 108740. The daemon was the only restarted component.
All 316 production connections returned and persisted versioned sessions by
15:51:13. Both nodes, signer, and private canary kept their original PIDs.
The only immediate reject events were seven `unknown_job` stale submissions at
the restart boundary; low-difficulty and other rejects were zero.

The five-minute gate passed, but the 15-minute gate failed during a
563.999-second same-tip job interval. Legacy miners enforce a 300-second
no-new-job watchdog on a 15-second tick. The first affected short session ended
at 16:01:22 CST, and the fleet then produced 4,714 short sessions with a
13.993-second median lifetime. The daemon logged 5,030 `client disconnected`
events and no matching ban, parser, rate-limit, or server-error closures.
Source inspection confirms this log is emitted only after the socket reader
receives EOF. The production daemon was therefore not released for miner
upgrades, and the 60-minute gate was not attempted.

The previous daemon was restored at 16:06:49 CST:

- release `csd-pool-77eb176@77eb176`;
- binary SHA-256
  `24ee9956f0c129e602b70511f9a76432aa587adbd4c9d98d003a21827fa225c6`;
- PID 112180 and `NRestarts=0` after recovery;
- 316 production connections on port 3333 and two unchanged private-canary
  connections on port 3334.

Neither node, signer, the private canary, wallet state, nor any miner was
restarted or modified by the rollback. Both nodes remained on the same tip.
The retained migration 9 remained compatible with the restored daemon.

After rollback, another 304.980-second same-tip job interval produced one
121-connect/121-disconnect legacy watchdog cycle. This independently confirms
that the repeated reconnects were initiated by the miner's no-new-job
watchdog, rather than by the new session or VarDiff implementation.

The production release is frozen on the restored daemon. A same-tip heartbeat
compatibility change must pass code, protocol, private-port, 15-minute, and
60-minute gates before another production attempt.

## Rollback

Restore the previous daemon binary and configuration, then perform one daemon
restart. Migration 9 is forward-compatible with the previous binary and does
not need to be reverted. The old daemon ignores the additional nullable/default
columns.

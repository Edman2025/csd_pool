# Candidate Parallel Submit Canary

Status: code and isolated validation only. Production remains disabled.

## Scope

The candidate submitter can fan a solved block out to node-a and node-b when
`CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_ENABLED=true`.

- node-a remains authoritative for the pool's accepted or rejected result;
- node-b is best-effort and has a shorter timeout;
- a process-local atomic one-shot budget permits node-b for at most the first
  candidate entering the submit path; the budget is consumed even if the
  health gate skips node-b;
- a background health watch keeps bounded-age snapshots for both nodes;
- node-a and node-b submissions start together only when the cached snapshots
  show the same height, tip, and chainwork, and that tip matches the candidate
  header's parent hash;
- missing health data, health errors, timeouts, or chain-state differences
  degrade to node-a only;
- both node attempts are recorded under `submit_response.parallel_submit`,
  including start/end timestamps, elapsed time, response, error or timeout,
  and the health snapshots used by the safety gate.

The bounded node-b call may delay persistence of the combined audit response,
but it does not delay node-a submission or network propagation. This
deliberately preserves the existing candidate DB and notification ordering;
candidate fast-path work is outside this change.

`relay_enqueue` continues to mean local node queue admission. It is not a peer
first-seen or network propagation acknowledgement.

## Configuration

```dotenv
CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_ENABLED=false
CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_BUDGET=1
CSD_POOL_PRIMARY_SUBMIT_NODE_NAME=node-a
CSD_POOL_SECONDARY_SUBMIT_NODE_NAME=node-b
CSD_POOL_SECONDARY_SUBMIT_NODE_URL=http://127.0.0.1:8791
CSD_POOL_PRIMARY_SUBMIT_TIMEOUT_MS=2000
CSD_POOL_SECONDARY_SUBMIT_TIMEOUT_MS=750
CSD_POOL_PARALLEL_NODE_HEALTH_REFRESH_MS=1000
CSD_POOL_PARALLEL_NODE_HEALTH_MAX_AGE_MS=3000
```

The feature flag defaults to disabled. The primary and secondary URLs must be
distinct. In live mode, the primary submit URL must still match the template
node because mining jobs are node-local. This canary release rejects any
parallel-submit budget other than exactly `1`.

## Result Semantics

- `both_accepted`: both nodes accepted; node-a response remains authoritative.
- `authority_accepted_secondary_failed`: node-a accepted and node-b failed,
  rejected, or timed out.
- `authority_accepted_secondary_skipped`: node-a accepted and node-b was
  skipped by the health safety gate.
- `secondary_accepted_authority_failed`: node-b accepted, but the pool result
  remains failed because node-a is authoritative.
- `both_failed`: neither node accepted.
- `authority_failed_secondary_skipped`: node-a failed and node-b was not safe
  to use.

## Required Gates

Before any production enablement:

1. Unit fault injection must cover both accepted, node-a accepted/node-b
   failed, node-a failed/node-b accepted, both timeout, chain mismatch, and
   two concurrent candidates competing for the one-shot budget.
2. Workspace tests, strict clippy, release check, formatting, and diff checks
   must pass.
3. An isolated daemon and independent database must replay candidate submits
   against two controllable node adapters.
4. The canary must verify that the combined audit record contains both node
   outcomes and that primary-only behavior is byte-for-byte unchanged while
   the feature flag is disabled.
5. A later production canary must be limited to a single real candidate and
   must not share a window with miner expansion. Its audit record must show
   `secondary_candidate_budget.claimed=true` and `remaining=0`; every later
   candidate must show `candidate_budget_exhausted` and remain node-a only.

No wallet, signer, payout, miner, PRL, BTX, or DIL behavior is part of this
change.

## Local Gate Results

The code-only candidate passed:

- `cargo fmt --all -- --check`;
- `git diff --check`;
- `cargo test --workspace`: 191 tests passed;
- `cargo clippy --workspace --all-targets -- -D warnings`;
- `ops/bin/csd-pool-release-check.sh`: pass 1793, fail 0;
- an in-process two-node HTTP replay using the real `CsdNodeClient`;
- fault injection for both accepted, authority accepted/secondary failed,
  authority failed/secondary accepted, both timed out, and chain mismatch;
- a concurrent two-candidate regression proving node-a receives both
  candidates while node-b receives exactly one;
- a conservative skip regression proving a chain-state mismatch still
  consumes the budget and the next candidate remains node-a only.

No production feature flag was enabled by these checks.

## Linux Isolated Canary

The pre-one-shot source archive was built offline on Linux with Rust 1.97.1.
Its now-superseded candidate daemon SHA256 was
`9478f2fc0b44cd20c958d764344a15c0c9adf8a34689dbe7b2def84f2d6ac2fa`.

At `2026-07-21 05:35:38 CST`, an isolated daemon used loopback Stratum/API
ports `3336/18083`, two controllable loopback node adapters, and the independent
database `csd_pool_parallel_canary_6826f6e`. Four distinct easy-target
candidates produced these durable audit results:

- both nodes accepted: node-a `1.627 ms`, node-b `1.438 ms`;
- node-a accepted while node-b returned an injected HTTP error: node-a
  remained authoritative and the miner submit response remained accepted;
- node tips differed: node-b was skipped with
  `skip_reason=chain_state_mismatch`, and its submit request count did not
  change;
- node-a accepted while node-b exceeded its strict `750 ms` timeout: the
  durable result was `authority_accepted_secondary_failed`, and the miner
  submit response remained accepted.

All four candidates were present in the isolated `blocks` table with complete
primary/secondary health and submit audit objects. The canary daemon had
`NRestarts=0`. After evidence capture, all isolated units were stopped and all
four loopback ports were closed. The production pool daemon, node-a, node-b,
and signer retained their original PID and `NRestarts=0`.

This was an isolated functional PASS for the fanout behavior, not production
authorization. The one-shot revision requires a new Linux artifact and
isolated replay before it can replace this superseded binary. Neither result
proves a lower orphan rate on the public network.

## Residual Miner Liveness Evidence

The production observability gate also isolated a separate miner issue that is
not caused by candidate fanout:

- `V100-JN-20260719-10-CSD` initiated watchdog reconnects at 03:59:18,
  04:03:18, and 04:07:18 CST;
- `V100-JN-20260719-25-CSD` initiated a watchdog reconnect at 04:04:39 CST
  and required five failed dial attempts before recovery;
- pool-side PoW matching showed that local `submit OK latency_ms=0` messages
  during a half-open socket were not present in accepted shares;
- delayed `sessions.ended_at` values explain apparent old/new session overlap
  and do not indicate duplicate miner processes.

This remains a miner-side liveness and response-accounting risk. It does not
change the parallel-submit implementation or authorize a miner rollout.

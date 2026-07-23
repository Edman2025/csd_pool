# Candidate Parallel Submit Canary

Status: stateless full-candidate fanout is code and isolated validation only.
Production remains A-only. The legacy job-cache-affine path is blocked by
`BLOCKED_JOB_CACHE_AFFINITY` and must never be enabled.

## Scope

The current candidate submitter can fan a solved block out only when
`CSD_POOL_STATELESS_PARALLEL_CANDIDATE_SUBMIT_ENABLED=true` and the separately
approved one-shot gate has passed.

- templates are fetched only from node-a;
- node-a receives its cache-affine submit while node-b receives a complete
  stateless candidate containing the raw template material needed to perform
  independent consensus validation with an empty job cache;
- node-b reconstructs, validates, durably stores, applies, and broadcasts the
  candidate through the official adapter without consulting node-a state;
- a persistent `O_EXCL` latch limits node-b to exactly one candidate across
  daemon restarts; the latch file is `0600`, file and parent are synced, and
  unsafe writable parents or broad existing-latch permissions fail closed;
- pre-claim health must show the same height, tip, and chainwork on both nodes,
  node-b must report at least one relay peer, and that tip must match the
  candidate parent; a mismatch or zero-peer node-b before claim leaves the
  one-shot available, while drift after a durable claim consumes it and skips
  node-b;
- missing or stale health, latch errors, node timeouts, malformed material, or
  chain-state differences never delay or disable the node-a request;
- either node's successful consensus-and-relay result makes the aggregate
  result accepted; secondary-only acceptance is persisted as
  `submitted_secondary` and creates an operator alert;
- both node attempts, health snapshots, latch state, timing, local-canonical
  state, and relay outcome are stored under `submit_response.parallel_submit`.

The official adapter does not treat local insertion or queue admission as
network success. Every successful response requires a real gossipsub publish
acknowledgement. A P2P-first or duplicate local-canonical candidate retries a
fresh relay acknowledgement rather than trusting cached relay state.

## Configuration

```dotenv
CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_ENABLED=false
CSD_POOL_STATELESS_PARALLEL_CANDIDATE_SUBMIT_ENABLED=false
CSD_POOL_STATELESS_PARALLEL_CANDIDATE_LATCH_PATH=/var/lib/csd-pool/stateless-candidate-canary.latch
CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_BUDGET=1
CSD_POOL_PRIMARY_SUBMIT_NODE_NAME=node-a
CSD_POOL_SECONDARY_SUBMIT_NODE_NAME=node-b
CSD_POOL_SECONDARY_SUBMIT_NODE_URL=http://127.0.0.1:8791
CSD_POOL_PRIMARY_SUBMIT_TIMEOUT_MS=2000
CSD_POOL_SECONDARY_SUBMIT_TIMEOUT_MS=750
CSD_POOL_PARALLEL_NODE_HEALTH_REFRESH_MS=1000
CSD_POOL_PARALLEL_NODE_HEALTH_MAX_AGE_MS=3000
```

Both feature flags default to disabled. The legacy flag is a hard error. The
primary and secondary URLs must be distinct, the primary submit URL must match
the template node, the latch path must be absolute, and any budget other than
exactly `1` is rejected.

## Result Semantics

- `both_accepted`: both nodes independently accepted and relayed.
- `authority_accepted_secondary_failed`: node-a accepted and node-b failed,
  rejected, or timed out.
- `authority_accepted_secondary_skipped`: node-a accepted and node-b was
  skipped by the health safety gate.
- `secondary_accepted_authority_failed`: node-b independently accepted and
  relayed while node-a failed; aggregate `ok=true`, DB status is
  `submitted_secondary`, and an operator alert is mandatory.
- `secondary_accepted_authority_local_canonical_relay_failed`: node-b relayed
  while node-a inserted locally but failed to obtain a relay acknowledgement.
- `authority_accepted_secondary_local_canonical_relay_failed`: node-a relayed
  while node-b inserted locally but failed to obtain a relay acknowledgement.
- `both_local_canonical_relay_failed`: both inserted locally but neither
  obtained a relay acknowledgement; aggregate `ok=false` and DB status is
  `relay_failed`.
- any primary or secondary timeout/transport error is outcome-ambiguous and
  remains `submitted` for reconciliation; it is never prematurely classified
  as `orphaned`.
- `both_failed`: neither node accepted.
- `authority_failed_secondary_skipped`: node-a failed and node-b was not safe
  to use.

## Required Gates

Before any production enablement:

1. Two official-adapter isolated replays must pass: direct official-handler
   replay and two-listener HTTP replay. Both use only node-a template material;
   node-b starts with an empty job cache and a divergent mempool.
2. Replays must cover malformed material, duplicate/idempotent submission,
   P2P-first arrival, fresh relay ACK recovery, secondary timeout, A/B health
   drift, and local-canonical relay failure.
3. Aggregate and DB integration must prove secondary-only success is accepted,
   persisted as `submitted_secondary`, reconciled, and alerted.
4. The one-shot latch must survive daemon reconstruction, reject unsafe parent
   or file modes, and fail closed on pre-existing latch and persistence error.
5. `ops/bin/csd-pool-stateless-candidate-canary-gate.py` must verify locked
   source hashes, test counts, an at-most-five-minute-old strict-UTC A-only
   baseline, JN12/3335 frozen state, A/B convergence and positive peer counts,
   CPU/RSS/FD/tasks/PG limits, previous-release and config SHA, latch
   preservation, and the rollback verification chain. The migrated daemon must
   retain `ProtectSystem=strict`, explicitly include `/var/lib/csd-pool` in its
   effective `ReadWritePaths`, and pass the write-access probe from the
   daemon's mount namespace. A host-namespace `runuser ... test -w` result is
   insufficient.
6. Workspace tests, strict clippy, release check, formatting, and diff checks
   must pass. The feature flags remain false after every local gate.
7. Any future production canary requires a fresh, independent authorization
   and exactly one real candidate. Local PASS never authorizes production.

No wallet, signer, payout, miner, PRL, BTX, or DIL behavior is part of this
change.

## Local Gate Results

The current code-only candidate passed:

- `cargo fmt --all -- --check`;
- `git diff --check`;
- official adapter `pool_candidate_tests`: 12/12, including a P2P-first
  candidate that remains idempotently accepted after it becomes a canonical
  ancestor rather than the current tip;
- official adapter `pool_header_publish_tests`: 6/6, including an in-memory
  two-peer replay that requires the published consensus header bytes to arrive;
- official adapter `dial_backoff_tests`: 5/5;
- pool bridge: 68/68, DB: 26/26, node client: 15/15;
- stateless canary evidence gate matrix: 44/44, including the migrated-unit
  missing-`ReadWritePaths` regression, daemon mount-namespace probe enforcement,
  rollback
  config content, duplicate-control, unknown-syntax/control-byte rejection,
  permissions, and baseline freshness;
- `ops/bin/csd-pool-release-check.sh`: pass 1861, fail 0;
- direct and HTTP two-official-adapter replays with node-b empty cache and
  divergent mempool;
- secondary-only aggregate-to-DB-to-alert integration;
- restart-persistent latch, permission, timeout, health drift, malformed,
  duplicate, P2P-first, relay recovery, rollback, and resource gates.

No production connection, build, deployment, restart, or feature flag was
performed by these checks.

## Historical Superseded Fanout Evidence

The remaining dated sections document earlier job-cache-affine and
process-local one-shot experiments. They explain prior incidents but are not
valid implementation, artifact, replay, rollback, or production-authorization
evidence for the current stateless path.

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

## Atomic One-Shot Revision

The production gray-release latch was added in source commit
`0a173bee94c878b9ea98316e20690c5610feab48`, tree
`eb516d8bf87edc8e83aa3436cff1b545065d4776`. The fixed source archive SHA256 is
`6f849cc74c41a9987ad1cc70e935c749088bb0b7d189f9324029d70c3d666cd9`.
It was built locked and offline on Linux with Rust 1.97.1. The resulting
daemon SHA256 is
`651376fce0cdf02137cef7bad4fd8e25e62b39ab4b1707db0b19f79f78c5b84d`.

The isolated daemon used loopback Stratum/API ports `3336/18083`, two
controllable loopback node adapters, and an independent database. Four
candidate records at `2026-07-21 06:02:01-06:02:11 CST` produced this exact
budget-claim sequence:

1. `one-shot-first`: claimed `true`, node-b accepted, remaining `0`;
2. `one-shot-second`: claimed `false`, node-b skipped with
   `candidate_budget_exhausted`;
3. `mismatch-consumes-budget`: claimed `true`, node-b skipped with
   `chain_state_mismatch`, remaining `0`;
4. `aligned-after-mismatch`: claimed `false`, node-b skipped with
   `candidate_budget_exhausted`.

Node-b received exactly one submit request in the first process and none in
the fresh mismatch process. The combined claim sequence was
`true,false,true,false`. This proves both that concurrent or later candidates
cannot reuse a consumed budget and that a safety-gate skip does not pass the
budget to a later candidate.

Two preliminary harness runs stopped safely before PASS: one reused a
deterministic candidate hash across process restarts, and one changed the
static probe template outside its accepted fixture shape. The final run
allocated distinct session extranonces while preserving the exact template,
then passed all DB, node audit, response, and cleanup assertions. No production
component changed during any run; all isolated listeners were closed after
the final capture.

Durable evidence is stored under
`/data/csd-pool/parallel-canary/0a173be-one-shot/`. Its evidence archive SHA256
is `49c2d6d74f46142c7c5a2d58bb876792c51d15381d81823c04b7d3d120c865fa`.

## Production One-Shot Result And Parent Repair

The one-shot production canary started at `2026-07-21 06:13:04 CST`. Its
process-local budget behaved safely, but the intended node-b submit was not
exercised:

- height `58427`, hash
  `00000000000010bd7b85f08688ee8322818820686b6726426ce861dd851469fd`,
  claimed the `1` budget and left `0`; node-a accepted it while node-b was
  skipped with `candidate_parent_mismatch`;
- height `58437`, hash
  `0000000000001287afc7245997b6de859c53576a833f0583fc8902df6d2cbed2`,
  saw the exhausted budget and remained node-a only;
- both blocks reached the pool's `10` confirmation threshold, remained
  non-orphaned, and paid `5,000,000,000` base units to the configured mining
  address; the public block feed independently reported both hashes as final;
- node-b logs showed only later P2P receipt of each block and no local submit
  call.

The first candidate's serialized header contains parent
`00000000000013c6d2db4280dd807f75740a3300e84c53d4114b944b9012d56b`,
exactly matching both node health tips. The gate incorrectly reversed the
already canonical header bytes before comparing them with the display hash,
creating a false mismatch. Production was rolled back to the A-only
observability daemon; node-a, node-b, signer, wallet, and miners were not
restarted or changed.

The repair represents node health tips and candidate parents as one
`CanonicalBlockHash([u8; 32])` type. Display hashes are decoded once, candidate
parents are copied directly from serialized header bytes `4..36`, and the
eligibility gate compares the structured values without an implicit reverse.
The real height-58427 header is a regression fixture. Additional tests prove:

- matching display/header forms, including prefix and hex case differences,
  call node-b exactly once;
- the reversed byte order is not accepted as the same parent;
- a genuine parent mismatch still consumes the one-shot budget;
- the next candidate reports `candidate_budget_exhausted`;
- two concurrent candidates still issue at most one node-b call;
- malformed display hashes and malformed header lengths fail closed.

The repaired local tree passed formatting and diff checks, workspace tests
(`195` passed), strict clippy, and release check (`1793` passed, `0` failed).
It is not authorized for production. A fresh Linux artifact and isolated
two-node replay are required, followed by a separately approved production
window after the miner expansion finishes.

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

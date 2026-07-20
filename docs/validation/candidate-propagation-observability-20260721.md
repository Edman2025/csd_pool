# Candidate propagation observability

Date: 2026-07-21

## Scope

This release candidate adds timing evidence to the existing single-node block
candidate path. It does not change:

- share or block validation;
- accepted-share persistence order;
- template or submit-node selection;
- candidate submission order;
- consensus, reorg, signing, reward, payout, or wallet behavior;
- the node's mined-header broadcast mechanism.

Parallel node submission, candidate fast-path reordering, and an off-host relay
are explicitly excluded from this release.

## Evidence fields

The pool starts a monotonic timer after a verified share is identified as a
block candidate and before the accepted share is persisted. It stores the
following under `blocks.submit_response_json.pool_observability`:

- `candidate_detected_unix_ms`: wall-clock correlation timestamp;
- `candidate_detected_to_submit_start_us`: candidate detection through the
  start of the existing node request, including accepted-share persistence;
- `node_roundtrip_us`: complete adapter HTTP request/response time.

The compatible official-node adapter stores:

- `request_received_unix_ms`: adapter wall-clock receive time;
- `accept_elapsed_us`: adapter request start through consensus acceptance;
- `relay_enqueue_elapsed_us`: adapter request start through local
  mined-header queue admission;
- `relay_queued`: whether the local mined-header queue accepted the event.

`relay_enqueue_elapsed_us` is not peer-receipt or first-relay latency. It proves
only local queue admission. A future peer acknowledgement or remote relay probe
is required to measure network receipt directly.

Prometheus exports sum, count, and max for:

- `detected_to_submit_start`;
- `node_roundtrip`;
- `candidate_record`;
- `candidate_total`;
- optional `node_accept`;
- optional `relay_enqueue`.

Structured logs carry the same pool-side phase values and the node
observability JSON. Transport failures remain retryable and retain both the
candidate record and available timing evidence.

## Fixed source and build

Pool source base:

- commit: `10ed88fa3967b27ad28683f4f67b34eef0479b15`;
- branch: `codex/csd-candidate-propagation-observability`.

Official node source:

- upstream: `https://github.com/compute-substrate/compute-substrate`;
- commit: `d2884dd7d8dbcdb6322af66afa0f0f833a9ab98c`;
- adapter patch SHA256:
  `264dae8654edb876c8abab07d3dc5fb01e1d0ef463d4e6ea28a29f6ef960fd44`;
- P2P backoff patch SHA256:
  `cb56d3625876cff5fc2f8ad4833405631b47f073d89217e9b93647f99a394bb3`.

The fixed official-node source was copied from an existing clean checkout
instead of waiting indefinitely for a new GitHub download. The source commit
and clean tree were verified before applying either patch. A codeload fetch is
only an optional, time-bounded cross-check. The codeload cross-check was stopped
after its 30-second limit without a complete archive; it was not used as build
input.

Isolated Linux node build:

- path:
  `/tmp/csd-observability-build-20260721T013353/source/target/release/csd`;
- SHA256:
  `e423a789eb20f1335206d4545b2d59587167528b9e10e018bb38bf9de66d4b2f`;
- `cargo test --lib dial_backoff_tests`: 5 passed;
- `cargo check --lib --bin csd`: passed;
- `cargo build --release --bin csd`: passed.

The node candidate was built but not started or installed.

The pool workspace was cross-compiled for Linux x86_64/glibc 2.27. Candidate
artifacts:

- `csd-pool-daemon` SHA256:
  `b0345ac81f3aa1e5456ced9b7164c62fe6a2707477eb843fc25c1cfeb707fad8`;
- `csd-pool-mock-node` SHA256:
  `d183b8ecffde643ac91c81827bfd95a2514cbd48388e467e1822e2c427bc3688`;
- final replay `csd-pool-workers` SHA256:
  `45cb7696d3e9921ce5d95d8bd46b560273efcee091fdfcf5ce06de86c2d51461`.

The daemon binary is unchanged by the later accepted-share probe corrections;
those corrections affect only the isolated worker test tool.

## Code gates

The following gates passed before the isolated replay and must pass once more
after this final documentation update:

```text
cargo fmt --all -- --check
git diff --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
ops/bin/csd-pool-release-check.sh
```

The pool Linux release SHA and isolated replay results are recorded below only
after those commands complete against the final source tree.

## Isolated replay gate

Before any production deployment, a real daemon plus controlled candidate
endpoint must demonstrate:

1. one accepted block candidate follows the existing single submit path;
2. the candidate record retains the original node response fields;
3. `pool_observability` and `node_observability` are persisted;
4. Prometheus count increments exactly once and all available phases are
   non-negative;
5. adapter `relay_queued=true` is reported only after local queue admission;
6. transport failure persists a retryable candidate and timing evidence;
7. no second node request, consensus change, payout action, or wallet action
   occurs.

The complete local E2E ran against an isolated PostgreSQL database, loopback
mock node, loopback signer, and private daemon ports. It passed 40 checks with
zero failures. During that run the accepted-share probe exposed and then
verified a test-only byte-order correction: Stratum `extranonce1` is now
decoded as its four wire bytes instead of being parsed as a big-endian integer.
The probe additionally rejects any notification that does not exactly match
the known static/easy template shape.

The dedicated candidate replay then submitted one valid share through the real
daemon Stratum path:

- accepted shares: 1;
- block candidates: 1;
- candidate database rows: 1;
- candidate hash:
  `1268a27cf901cffe493e4cc9d9a331208fbd5807da055c3822e5b5357d6b8e8d`;
- duplicate or replacement submit: 0;
- wallet, payout, signer, and production node actions: 0.

Persisted timing evidence:

```text
candidate_detected_to_submit_start_us=3581
node_roundtrip_us=491
candidate_record_us=3089
candidate_total_us=7165
node_accept_us=3
relay_enqueue_us=3
relay_queued=true
```

Every Prometheus timing phase had count 1. The database response retained
`ok=true`, the original mock source, `pool_observability`, and
`node_observability`. All replay listeners were closed afterward. The legacy
services on the isolated host remained inactive before and after the replay.

Evidence archive:

- local path:
  `/tmp/csd-candidate-observability-replay-d466dbb.tar.gz`;
- SHA256:
  `7d3b33621237495a7d7b06a46aa9f9a8c75abab6ddbf848611f2bbf7c798cb5c`.

## Production monitoring gate

Production deployment remains a separate, reversible monitoring-only window:

1. Freeze miner changes and capture 5m/15m/1h Stratum, reject, service, node,
   signer, CPU, FD, PostgreSQL, and health baselines.
2. Back up the exact node-a and daemon binaries, configs, systemd drop-ins, and
   schema metadata.
3. Upgrade only node-a to the fixed monitoring adapter while node-b remains
   unchanged. Verify binary SHA, release, height, tip, chainwork, peers,
   template/submit API, and `NRestarts=0`.
4. After the node-a gate passes, upgrade only the pool daemon. Confirm release,
   revision, server instance, metrics, and database persistence.
5. Observe 15 minutes and 60 minutes. Require continuous worker coverage,
   accepted-difficulty hashrate without material regression, no reject or
   reconnect storm, operational health, unchanged node/signer state, and no
   candidate behavior change.
6. If a real candidate occurs, reconcile its pool timing, adapter timing, node
   acceptance, canonical status, and final reward. Do not treat queue admission
   as remote propagation.

Any failed gate rolls back only the component introduced in that step. No
parallel submit experiment starts in this release.

## Parallel submit follow-up

The current adapter reconstructs a candidate from a node-local template cache.
Therefore a candidate created from node-a cannot be blindly posted to node-b:
node-b may correctly reject the foreign job as unknown. Parallel submission
requires a separately tested stateless full-candidate payload or audited
template-cache replication. That change must be feature-gated, isolated first,
and deployed only after this monitoring release completes its 60-minute gate.

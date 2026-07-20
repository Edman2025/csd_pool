# CSD orphan attribution (2026-07-20)

This audit is based on read-only production database, journal, source, and
public-chain observations. Timestamps are Asia/Shanghai (CST). The pool, nodes,
signer, and miners were not changed while collecting the evidence.

## Baseline after the A100 admission

The baseline was locked after 15:26 CST, when the eight
`B-CSD-A100-187-43-GPU0` through `GPU7` workers were already connected:

- Stratum connections: 316 on production port 3333 and 2 on private canary
  port 3334.
- Distinct workers over 15 minutes and one hour: 318.
- Accepted-difficulty hashrate at 15:26:49: 965.093 GH/s over 5 minutes,
  950.161 GH/s over 15 minutes, and 934.335 GH/s over one hour.
- The eight A100 workers had 126 accepted shares and an early aggregate
  estimate of 33.470 GH/s, with 2 low-difficulty, 1 stale/unknown-job, and 0
  other rejects. This short window is not a stable per-GPU benchmark.

## Candidate attribution

Heights 57264, 57321, and 57342 were inferred from the canonical height of the
job's previous block because the early candidate rows did not persist height.
Candidate receive time is the accepted candidate-share persistence timestamp;
the daemon did not yet record a separate socket-receive timestamp.

| Height | Pool candidate | Canonical block | Finder | Job created / age at submit | Receive -> recorded submit | Node result | Attribution |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 57264 | `000000000000059d1b9ef8d4c4b36302a74218a214d754c5d2ae8489d89ed278` | `0000000000000277f2f410514d17dd45d12b93b457f2fb59035d6718154d941c` | `V100-JN-20260719-21-CSD` | 15:33:33.095 / 25.353 s | 15:33:58.445729 -> 15:33:58.447922 (2.193 ms) | HTTP 409 was surfaced as a retryable transport error; the old client discarded the response body | `unknown/evicted same-tip`, probably stale-prevhash. The next job appeared 5 seconds later, but local acceptance cannot be proven. |
| 57321 | `0000000000001935c4f48790aae2458694abd9d96ff707698fcb8abf93e06615` | `0000000000002d5dd8d228f2b07d366f45d8c0cf323d79c2a2d0eac025c9b1a3` | `V100-ECS-aly-041-CSD` | 17:52:23.404 / 10.956 s | 17:52:34.357664 -> 17:52:34.359896 (2.232 ms) | HTTP 409 `solved pool job is stale` | Definite stale-prevhash. The next job was not created until 17:55:23.410. |
| 57342 | `0000000000000215f5aa38c9980b88ee31bc6a48c521388ca781c9967f2e2967` | `0000000000000a6a233b405e293d8a3887dac77529d9638be3dc371175eb392c` | `V100-CQ-20260719-21-CSD` | 18:20:08.395 / 94.967 s | 18:21:43.360151 -> 18:21:43.362325 (2.174 ms) | HTTP 409 `solved pool job is stale` | Definite stale-prevhash. The next job appeared at 18:22:08.400. |
| 57479 | `0000000000000fd11abfeb57bbf55b1aef903a4268ccf2dd9667f6d3addb0dd8` | `000000000000093c51df6278b15dc203996e68a6b158dbbf5c9d3960f8d6809c` | `V100-CQ-20260719-01-CSD` | 22:55:55.615 / 30.607 s | 22:56:26.212070 -> 22:56:26.221500 (9.430 ms) | accepted; seen at 22:56:40.014; orphaned at 22:57:12.122 | Local accepted, then lost a same-prevhash race. Peer and relay journals are no longer retained, so propagation attribution is insufficient. |
| 57939 | `0000000000002778f4e8043790de01b381f70e8bfd60b4f01a1b7f0d3fa17116` | `000000000000040fa805646e0ed5a4f1deca73238fefbd0b1df47ef4d5684fc3` | `V100-ECS-aly-111-CSD` | 14:27:06.360 / 99.254 s | 14:28:45.605269 -> 14:28:45.614231 (8.962 ms) | node-a accepted immediately; seen at 14:28:55.005; orphaned at 14:29:30.099 | Local accepted, then lost a same-prevhash race with actionable propagation evidence. |

For height 57939, node-a installed the pool candidate as its tip at 14:28:45.
Node-b did not receive that candidate. It downloaded the competing canonical
block from a peer at 14:29:24. Node-a downloaded the competing branch at
14:29:26 and reorganized with `undo=1, apply=2`. This proves that the pool
candidate reached node-a but does not prove it reached another independent
relay. It does not prove that a faster relay would certainly have won.

The canonical blocks in all five rows reference the same previous block as the
corresponding pool candidate. There is no malformed-header or wrong-prevhash
construction evidence.

## Avoidability assessment

- Definitely stale and avoidable at the time: 2. Both occurred while job
  replacement lagged the live tip by 120 to 180 seconds. The current production
  daemon polls the live tip every 2 seconds and discards templates behind it, so
  this failure mode has already been addressed.
- Probably stale, but response evidence is incomplete: 1.
- Local accepted and then lost a race: 2.
- Of the two races, one has direct evidence that node-b never saw the local
  candidate. It is a candidate for a dual-node/relay experiment, not proof of a
  guaranteed recoverable reward.
- Definitely unavoidable: 0. The retained evidence is not strong enough to
  label either race unavoidable.

The safe lower bound is two historically avoidable stale candidates. A third
candidate has a concrete propagation deficiency worth testing. The other two
must remain `insufficient evidence` rather than being counted as recoverable
rewards.

## Current candidate hot path

The daemon currently performs the following operations synchronously:

1. Validate the share.
2. Persist the accepted share.
3. Submit the candidate to one `CsdNodeClient`.
4. Persist the candidate and node response.
5. Return the Stratum submit response.

Production uses node-a (`127.0.0.1:18789`) for template, submit, and watch.
Although node-b is declared with `submit,watch`, the bridge constructs only one
submitter from `CSD_POOL_SUBMIT_NODE_URL`, and template/submit affinity requires
that URL to match the template node. Candidate submission is therefore neither
parallel nor broadcast to node-b. Both nodes are on the same host and external
network path.

The retained candidates show 2.2 to 9.4 milliseconds between accepted-share
persistence and recorded submission completion. This database ordering is
suboptimal for a block fast path, but it does not explain the 41-second
height-57939 propagation gap by itself.

## Decision

Do not deploy a candidate fast path before the VarDiff/session production
60-minute gate. After that gate, a code-only change may be prepared to submit a
validated candidate concurrently to two same-tip nodes before nonessential
database work, while preserving the authoritative candidate/share audit record.
It must record pool receive, node request, node accept, and first-relay times.
Any remote relay should be tested as a separate, non-signing, broadcast-only
canary on a different host and network path.

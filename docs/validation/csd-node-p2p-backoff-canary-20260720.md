# CSD node P2P backoff canary - 2026-07-20

## Scope

This change only affects outbound P2P dial scheduling and failure logging in
the pinned official node source at
`d2884dd7d8dbcdb6322af66afa0f0f833a9ab98c`. It does not change consensus,
mining templates, block submission, Stratum, signer, or payout behavior.

The production pool continued using node-a. The candidate ran only on the
isolated node-b RPC endpoint and was not added to the template or submit path.

## Root cause

The upstream retry record was deleted as soon as its retry deadline expired.
The next failure therefore restarted at the 30-second delay instead of
progressing through the intended backoff sequence.

The peer discovery loop could also start another dial for the same peer
address every 15 seconds while an earlier TCP attempt was still pending. A
long network timeout consequently produced several queued attempts and a
burst of duplicate error records.

## Change

- Retain dial failure history for 24 hours after the last failure.
- Progress through 30, 120, 300, 900, 1800, and 3600 second delays.
- Clear retained failure state after a successful connection.
- Allow at most one pending outbound dial per peer address.
- Compact expected timeout, refused, unreachable, and no-address failures.
- Preserve detailed diagnostics for unexpected, malformed, identity, and sync
  failures.

Patch SHA256:
`cb56d3625876cff5fc2f8ad4833405631b47f073d89217e9b93647f99a394bb3`

## Verification

- P2P unit tests: 5 passed, 0 failed.
- Official-source `cargo check --lib --bin csd`: passed.
- Repository release checks: 1786 passed, 0 failed.
- Server-side P2P unit tests: 5 passed, 0 failed.
- Server release candidate SHA256:
  `f1ff858fa544104f26eb441441c63bcb62477c4e6471abaebe9f3dd796c89930`

The initial 20-second health gate rolled back cleanly while the node was still
replaying local chain state. The unchanged binary also required about 43
seconds after rollback. The corrected 90-second gate accepted the candidate
after 58 seconds.

## Canary result

Observation window: `2026-07-20 08:59:37` through `09:15:06` CST.

| Metric | node-a original | node-b candidate |
| --- | ---: | ---: |
| Detailed `OutgoingConnectionError` | 2168 | 3 |
| Periodic redial attempts | 1486 | 293 |
| Structured tracked dial failures | n/a | 333 |
| Structured untracked expected failures | n/a | 12 |
| Failure level 1 / 30s | n/a | 109 |
| Failure level 2 / 120s | n/a | 111 |
| Failure level 3 / 300s | n/a | 111 |
| Failure level 4 / 900s | n/a | 2 |

Detailed log noise fell 99.86%, and periodic redial attempts fell 80.28%.
The three remaining detailed records were startup `Aborted` events.

At the end of the window, node-a and node-b had the same height and tip,
both had five connected peers, node-b had `NRestarts=0`, and node-a, node-b,
pool daemon, and signer were active. The node-a and pool daemon PIDs were
unchanged throughout the candidate rollout.

## Production disposition

Node-a was promoted in the controlled maintenance window at
`2026-07-20 10:17:14 CST`. Both node-a and node-b now run
`/data/csd-pool/node/bin/csd-p2p-backoff-v2-f1ff858f`, SHA256
`f1ff858fa544104f26eb441441c63bcb62477c4e6471abaebe9f3dd796c89930`.

The final read-only check at `10:48 CST` found both nodes at height `57837`
with identical tip and chainwork, zero warning-level records after settling,
and `NRestarts=0`. Node-a remains the template and submit backend; node-b
remains the independent watch/canary node.

Node replay delayed the bound pool daemon during the promotion window. Future
node maintenance must therefore use the 90-second node health gate and must
not start the daemon until the template endpoint is ready.

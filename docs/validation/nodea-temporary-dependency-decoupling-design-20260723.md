# Node A temporary dependency decoupling design

Status: local design only. This document does not authorize a production
connection, unit write, daemon reload, service restart, or Node A retry.

## Confirmed trigger

The current production graph binds `csd-pool-migrated-daemon.service` to Node A
with `BindsTo` and binds `csd-pool-migrated-signer.service` with `Requires`.
Restarting Node A therefore stops both dependents. The daemon then follows its
`Restart=always` policy, while the switch payload still holds its pre-restart
PID. The stale PID assertion triggers the A-only rollback, whose second Node A
restart repeats the propagation.

## Temporary drop-ins

The design uses two new, uniquely named production drop-ins installed as
root-owned regular files with mode `0600`. The repository files are source
artifacts; the deployment must use `install -o root -g root -m 0600`, verify the
installed SHA, fsync the containing directory, and reject any existing path or
symlink.

Daemon:

```ini
[Unit]
BindsTo=
Wants=csd-pool-migrated-node.service
After=csd-pool-migrated-node.service
```

Signer:

```ini
[Unit]
Requires=
Wants=csd-pool-migrated-node.service
After=csd-pool-migrated-node.service
```

The empty list assignments remove the strong stop-propagating edge. `Wants`
retains a weak startup relationship and `After` retains ordering. No service,
environment, launcher, or executable directive is changed.

## Commit protocol

1. Fresh-lock Node A, Node B, daemon, and signer PID, InvocationID, NRestarts,
   running SHA, launcher and effective non-generation configuration. Lock
   production count, accepted freshness, A-only flags, resources, endpoints,
   and A/B peer and chain state.
2. Create a new root-owned mode `0700` staging directory on the same filesystem.
   Materialize both reviewed drop-ins there as mode `0600`, verify bytes and
   SHA, and fsync files and directory.
3. Atomically rename both files into new, non-existing names below the daemon
   and signer drop-in directories. If either rename fails, remove the other
   before any `daemon-reload`; the effective manager graph is still unchanged.
4. Run one `systemctl daemon-reload`. Verify that daemon `BindsTo` no longer
   contains Node A, signer `Requires` no longer contains Node A, both `Wants`
   and `After` contain Node A, Node A `BoundBy` no longer contains the daemon,
   and Node A `RequiredBy` no longer contains the signer. Other `RequiredBy`
   units are allowed and must not be erased.
5. Verify daemon, signer, Node B, and the still-old Node A PID, InvocationID,
   NRestarts, SHA and active state are unchanged. A generation drift is a
   pre-restart hold; dependent drift is a core redline.
6. Restart only Node A once into the target selector. Do not restart or
   try-restart the daemon, signer, Node B, a target, or a wildcard.
7. Require target SHA, active/running and NRestarts zero. Require daemon,
   signer, and Node B generations unchanged; production/accepted and resources
   healthy; and A/B height, tip, and chainwork converged.
8. With target A stable and active, remove both exact temporary drop-ins and
   run one `daemon-reload`. Verify the original daemon `BindsTo`, signer
   `Requires`, and Node A reverse edges are restored while every running
   generation remains unchanged.

`daemon-reload` changes the manager graph only. Any daemon, signer, or Node B
generation change during either reload is a redline and is never normalized
away.

## Rollback protocol

If target A is not active, has the wrong SHA, restarts, or fails a critical
health gate, keep the temporary decoupling loaded and restart only Node A once
to the locked old selector. First prove old A is active/running with the exact
old SHA and prove production, accepted, resources, daemon, signer, Node B and
the A/B chain are healthy. Only then remove the two temporary drop-ins, reload
the manager graph, and verify restoration without any dependent generation
change.

At every interruption point the recovery order is:

1. ensure Node A is active on either the accepted target or locked old SHA;
2. ensure daemon and signer were not restarted;
3. restore both dependency edges;
4. verify the graph and all running generations.

The rollback artifact is the exact pair of drop-in paths plus their SHA values.
It removes only those paths. It never edits the base unit or existing drop-ins.

## Availability budget

The daemon remains A-only while the temporary graph is active. During the Node
A restart it can keep miner sessions and accepted-share handling alive, but it
cannot safely obtain a new authoritative template or submit a candidate while
the primary adapter is unavailable. This is a real-output risk, so the future
executor must enforce:

- at most 30 seconds of continuous Node A API unavailability per restart;
- at most 60 seconds cumulative unavailability including one rollback restart;
- at most 180 seconds from target process start to A/B health and chain
  convergence;
- at most 300 seconds total with the strong dependencies removed.

Exceeding a timer follows the rollback protocol. Before the first restart,
accepted shares must be fresh, no candidate may be in flight, Node B must be
healthy and on the same chain, and the old-A rollback selector must be locally
verified. These limits bound exposure but do not eliminate the short interval
in which a newly found block could not be submitted through authority Node A.

## Local verification

`ops/bin/csd-pool-nodea-dependency-decoupling-self-test.py` implements an
execution-level manager model with systemd list-reset semantics. It covers:

- reset of `BindsTo` and `Requires` while retaining `Wants` and `After`;
- `daemon-reload` preserving every running generation;
- restart of A without propagation in the temporary graph;
- successful target path and restoration of the original graph;
- target failure, old-A rollback, then graph restoration;
- interruption before reload, after reload, and during graph restoration;
- PID/generation drift before Node A restart;
- reproduction of the original incident when the strong graph is left intact.

The model intentionally does not claim production execution. A future
production payload must independently verify the current effective graph and
fresh process anchors before using these source artifacts.

# Log and disk governance

The production pool keeps high-volume operational data off the root filesystem
and bounds local logs so a peer or miner retry storm cannot fill the host.

## Policy

- Ordinary accepted shares are debug events. Block candidates and lifecycle
  events remain at info level.
- Persistent journals are capped at 1 GiB, keep at least 6 GiB free on the root
  filesystem, and retain at most seven days.
- PostgreSQL pool tables use a tablespace on the `/data` filesystem.
- Accepted shares and rejected/stale share events are retained for 14 days.
- Node health samples are retained for 90 days.
- Resolved alerts are retained for 180 days. Active alerts are never pruned.
- History deletion runs in 50,000-row batches and relies on autovacuum to reuse
  freed table space.

## Production checks

Before moving PostgreSQL relations, create and verify a current custom-format
backup. Create the tablespace directory as `postgres:postgres` with mode 0700,
then create a PostgreSQL tablespace owned by the pool database role. Move pool
tables and indexes with `ALTER TABLE/INDEX ... SET TABLESPACE`, and set the
database default tablespace so future relations follow the same policy.

After deployment, verify:

- the pool, both nodes, signer, PostgreSQL, and HAProxy are active;
- the two node tips and chainwork match;
- Stratum established connections and accepted-difficulty hashrate recover;
- block candidate submission remains enabled;
- journal usage is below the configured cap;
- pool relations report the `/data` tablespace;
- the retention service succeeds in dry-run and normal mode.

Do not use `VACUUM FULL` on the live shares table. Batched deletion plus ordinary
autovacuum avoids a long exclusive lock; physical growth is bounded by reuse.

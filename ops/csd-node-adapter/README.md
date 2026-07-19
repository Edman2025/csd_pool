# Official CSD Node Mining Adapter

This directory turns the pinned official `compute-substrate` node into the
template, candidate submission, and block status backend required by the pool.
The patch is pinned to commit
`d2884dd7d8dbcdb6322af66afa0f0f833a9ab98c` and was compiled against that exact
source revision.

## Build

```bash
git clone https://github.com/compute-substrate/compute-substrate.git
git -C compute-substrate checkout d2884dd7d8dbcdb6322af66afa0f0f833a9ab98c
ops/csd-node-adapter/apply-and-build.sh "$PWD/compute-substrate"
```

Run the resulting official node with a fresh token of at least 32 characters:

```bash
export CSD_POOL_ADAPTER_TOKEN='<same value as pool CSD_POOL_NODE_TOKEN>'
compute-substrate/target/release/csd node --help
```

The adapter adds authenticated endpoints at `/api/network`,
`/api/rpc/mining/template`, `/api/rpc/block/submit`, and
`/api/rpc/block/status`. Existing official endpoints, including `/health` and
`/tx/submit`, remain available. Keep the adapter RPC on a private network;
do not expose it directly to miners or the public internet.

Pool templates use a dedicated non-blocking timestamp selector. It chooses the
later of wall clock and the parent's minimum consensus time, then enforces the
official future-drift bound. The endpoint must never call the regular miner's
blocking `choose_block_time`, because waiting there can return a template whose
parent is already stale.

The release also includes `ops/bin/csd-pool-node-adapter-run.sh` and
`ops/systemd/csd-pool-node-adapter.service`. Install the compiled official node
at `/opt/csd-node/bin/csd`, provision the canonical network genesis at
`/etc/csd-node/genesis.bin`, and set node data/P2P overrides in the pool env as
needed. Copy `ops/env/csd-pool-node.env.example` to
`/etc/csd-pool/node.env`, set a fresh adapter token, and restrict it to
`root:csd-node` mode `0640`. Keeping the node environment separate prevents the
node service account from reading database, operator, or signer credentials.
The launcher maps `CSD_POOL_NODE_TOKEN` to the adapter process.
The RPC defaults to `127.0.0.1:8789`; use a private RFC1918 address only when a
separate pool host must connect, and never bind the adapter directly to a public
interface.

The patch also repairs a one-variable compile error present in the pinned
upstream commit (`_target_hi` versus `hi`). The mining implementation itself
uses the official node's template construction, consensus codec, PoW check,
header index, reorg path, mempool removal, and mined-header broadcast.

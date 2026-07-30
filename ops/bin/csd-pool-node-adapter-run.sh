#!/usr/bin/env bash
set -euo pipefail

: "${CSD_POOL_NODE_TOKEN:?CSD_POOL_NODE_TOKEN is required}"

NODE_BINARY="${CSD_POOL_NODE_BINARY:-/opt/csd-node/bin/csd}"
NODE_DATADIR="${CSD_POOL_NODE_DATADIR:-/var/lib/csd-node}"
NODE_GENESIS="${CSD_POOL_NODE_GENESIS:-/etc/csd-node/genesis.bin}"
NODE_RPC="${CSD_POOL_NODE_RPC:-127.0.0.1:8789}"
NODE_P2P_LISTEN="${CSD_POOL_NODE_P2P_LISTEN:-/ip4/0.0.0.0/tcp/17999}"
NODE_BOOTNODES="${CSD_POOL_NODE_BOOTNODES:-}"
NODE_HEADER_RECEIPT_ACK_ENABLED="${CSD_POOL_NODE_HEADER_RECEIPT_ACK_ENABLED:-false}"

[[ -x "$NODE_BINARY" ]] || { printf 'CSD node binary missing: %s\n' "$NODE_BINARY" >&2; exit 1; }
[[ -f "$NODE_GENESIS" ]] || { printf 'CSD genesis missing: %s\n' "$NODE_GENESIS" >&2; exit 1; }

export CSD_POOL_ADAPTER_TOKEN="$CSD_POOL_NODE_TOKEN"
export CSD_HEADER_RECEIPT_ACK_ENABLED="$NODE_HEADER_RECEIPT_ACK_ENABLED"
exec "$NODE_BINARY" node \
  --datadir "$NODE_DATADIR" \
  --genesis "$NODE_GENESIS" \
  --rpc "$NODE_RPC" \
  --p2p-listen "$NODE_P2P_LISTEN" \
  --bootnodes "$NODE_BOOTNODES"

#!/usr/bin/env bash
set -euo pipefail

: "${CSD_RELAY_OBSERVER_TOKEN:?CSD_RELAY_OBSERVER_TOKEN is required}"
: "${CSD_RELAY_OBSERVER_BOOTNODES:?CSD_RELAY_OBSERVER_BOOTNODES is required}"

NODE_BINARY="${CSD_RELAY_OBSERVER_BINARY:-/opt/csd-relay-observer/bin/csd}"
NODE_DATADIR="${CSD_RELAY_OBSERVER_DATADIR:-/var/lib/csd-relay-observer}"
NODE_GENESIS="${CSD_RELAY_OBSERVER_GENESIS:-/etc/csd-relay-observer/genesis.bin}"
NODE_RPC="${CSD_RELAY_OBSERVER_RPC:-127.0.0.1:18789}"
NODE_P2P_LISTEN="${CSD_RELAY_OBSERVER_P2P_LISTEN:-/ip4/127.0.0.1/tcp/0}"
DISK_BUDGET_BYTES="${CSD_RELAY_OBSERVER_DISK_BUDGET_BYTES:-0}"

[[ -x "$NODE_BINARY" ]] || {
  printf 'CSD relay observer binary missing: %s\n' "$NODE_BINARY" >&2
  exit 1
}
[[ -f "$NODE_GENESIS" ]] || {
  printf 'CSD relay observer genesis missing: %s\n' "$NODE_GENESIS" >&2
  exit 1
}
[[ "$NODE_RPC" == 127.0.0.1:* || "$NODE_RPC" == "[::1]:"* ]] || {
  printf 'CSD relay observer RPC must bind loopback: %s\n' "$NODE_RPC" >&2
  exit 1
}
[[ "$NODE_P2P_LISTEN" == /ip4/127.0.0.1/* || "$NODE_P2P_LISTEN" == /ip6/::1/* ]] || {
  printf 'CSD relay observer P2P listener must bind loopback: %s\n' "$NODE_P2P_LISTEN" >&2
  exit 1
}
[[ "$DISK_BUDGET_BYTES" =~ ^[1-9][0-9]*$ ]] || {
  printf 'CSD relay observer disk budget must be a positive byte count\n' >&2
  exit 1
}

export CSD_POOL_ADAPTER_TOKEN="$CSD_RELAY_OBSERVER_TOKEN"
export CSD_HEADER_RECEIPT_ACK_ENABLED=true
"$NODE_BINARY" node \
  --datadir "$NODE_DATADIR" \
  --genesis "$NODE_GENESIS" \
  --rpc "$NODE_RPC" \
  --p2p-listen "$NODE_P2P_LISTEN" \
  --bootnodes "$CSD_RELAY_OBSERVER_BOOTNODES" &
node_pid=$!

stop_node() {
  kill -TERM "$node_pid" 2>/dev/null || true
  wait "$node_pid" 2>/dev/null || true
  if [[ -n "${guard_pid:-}" ]]; then
    kill -TERM "$guard_pid" 2>/dev/null || true
    wait "$guard_pid" 2>/dev/null || true
  fi
}
trap stop_node INT TERM

budget_kib=$((DISK_BUDGET_BYTES / 1024))
(
  while kill -0 "$node_pid" 2>/dev/null; do
    state_kib="$(du -sk "$NODE_DATADIR" | awk '{print $1}')" || {
      printf 'CSD relay observer disk usage probe failed\n' >&2
      kill -TERM "$node_pid" 2>/dev/null || true
      exit 75
    }
    if ((state_kib > budget_kib)); then
      printf 'CSD relay observer disk budget exceeded: %s KiB > %s KiB\n' \
        "$state_kib" "$budget_kib" >&2
      kill -TERM "$node_pid" 2>/dev/null || true
      exit 75
    fi
    sleep 60
  done
) &
guard_pid=$!

set +e
wait "$node_pid"
node_rc=$?
set -e
kill -TERM "$guard_pid" 2>/dev/null || true
wait "$guard_pid" 2>/dev/null || true
exit "$node_rc"

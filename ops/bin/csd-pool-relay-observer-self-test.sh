#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
RUNNER="$ROOT_DIR/ops/bin/csd-pool-relay-observer-run.sh"
UNIT="$ROOT_DIR/ops/systemd/csd-pool-relay-observer.service.in"
CANARY_UNIT="$ROOT_DIR/ops/systemd/csd-pool-relay-observer-canary.service"
TMP_DIR="$(mktemp -d)"
miner_sentinel_pid=""
cleanup() {
  if [[ -n "$miner_sentinel_pid" ]]; then
    kill -TERM "$miner_sentinel_pid" 2>/dev/null || true
    wait "$miner_sentinel_pid" 2>/dev/null || true
  fi
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

FAKE_BINARY="$TMP_DIR/csd"
GENESIS="$TMP_DIR/genesis.bin"
CAPTURE="$TMP_DIR/capture"
DATADIR="$TMP_DIR/state"
mkdir -p "$DATADIR"
printf 'genesis\n' >"$GENESIS"
cat >"$FAKE_BINARY" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
{
  printf 'ack=%s\n' "${CSD_HEADER_RECEIPT_ACK_ENABLED:-}"
  printf 'token=%s\n' "${CSD_POOL_ADAPTER_TOKEN:+present}"
  printf 'argv='
  printf '%q ' "$@"
  printf '\n'
} >"$CSD_RELAY_TEST_CAPTURE"
if [[ "${CSD_RELAY_TEST_SLEEP:-0}" == "1" ]]; then
  sleep 30
fi
SH
chmod 0755 "$FAKE_BINARY"

CSD_RELAY_OBSERVER_TOKEN=test-token \
CSD_RELAY_OBSERVER_BOOTNODES=/ip4/203.0.113.10/tcp/17999 \
CSD_RELAY_OBSERVER_BINARY="$FAKE_BINARY" \
CSD_RELAY_OBSERVER_DATADIR="$DATADIR" \
CSD_RELAY_OBSERVER_GENESIS="$GENESIS" \
CSD_RELAY_OBSERVER_DISK_BUDGET_BYTES=17179869184 \
CSD_RELAY_TEST_CAPTURE="$CAPTURE" \
"$RUNNER"

grep -Fxq 'ack=true' "$CAPTURE"
grep -Fxq 'token=present' "$CAPTURE"
grep -Fq -- '--rpc 127.0.0.1:18789' "$CAPTURE"
grep -Fq -- '--p2p-listen /ip4/127.0.0.1/tcp/0' "$CAPTURE"

if CSD_RELAY_OBSERVER_TOKEN=test-token \
  CSD_RELAY_OBSERVER_BOOTNODES=/ip4/203.0.113.10/tcp/17999 \
  CSD_RELAY_OBSERVER_BINARY="$FAKE_BINARY" \
  CSD_RELAY_OBSERVER_DATADIR="$DATADIR" \
  CSD_RELAY_OBSERVER_GENESIS="$GENESIS" \
  CSD_RELAY_OBSERVER_DISK_BUDGET_BYTES=17179869184 \
  CSD_RELAY_OBSERVER_RPC=0.0.0.0:18789 \
  CSD_RELAY_TEST_CAPTURE="$CAPTURE" \
  "$RUNNER" >/dev/null 2>&1; then
  printf 'non-loopback observer RPC was accepted\n' >&2
  exit 1
fi

if CSD_RELAY_OBSERVER_TOKEN=test-token \
  CSD_RELAY_OBSERVER_BOOTNODES=/ip4/203.0.113.10/tcp/17999 \
  CSD_RELAY_OBSERVER_BINARY="$FAKE_BINARY" \
  CSD_RELAY_OBSERVER_DATADIR="$DATADIR" \
  CSD_RELAY_OBSERVER_GENESIS="$GENESIS" \
  CSD_RELAY_OBSERVER_DISK_BUDGET_BYTES=17179869184 \
  CSD_RELAY_OBSERVER_P2P_LISTEN=/ip4/0.0.0.0/tcp/17999 \
  CSD_RELAY_TEST_CAPTURE="$CAPTURE" \
  "$RUNNER" >/dev/null 2>&1; then
  printf 'non-loopback observer P2P listener was accepted\n' >&2
  exit 1
fi

grep -Fxq 'PrivateDevices=true' "$UNIT"
grep -Fxq 'DevicePolicy=closed' "$UNIT"
grep -Fxq 'CPUWeight=10' "$UNIT"
grep -Fxq 'IOWeight=10' "$UNIT"
grep -Fxq 'MemoryHigh=@MEMORY_HIGH_BYTES@' "$UNIT"
grep -Fxq 'MemoryMax=@MEMORY_MAX_BYTES@' "$UNIT"
grep -Fxq 'ReadWritePaths=/var/lib/csd-relay-observer /var/log/csd-relay-observer' "$UNIT"
grep -Fxq 'CPUQuota=50%' "$CANARY_UNIT"
grep -Fxq 'MemoryHigh=536870912' "$CANARY_UNIT"
grep -Fxq 'MemoryMax=805306368' "$CANARY_UNIT"
grep -Fxq 'RuntimeMaxSec=1800' "$CANARY_UNIT"
grep -Fxq 'Restart=no' "$CANARY_UNIT"
if grep -Eiq 'nvidia|miner|csd-v100|csd-gpu' "$UNIT" "$CANARY_UNIT" "$RUNNER"; then
  printf 'observer artifact references GPU or miner services\n' >&2
  exit 1
fi

sleep 300 &
miner_sentinel_pid=$!
printf 'occupied\n' >"$DATADIR/occupied"
set +e
CSD_RELAY_OBSERVER_TOKEN=test-token \
CSD_RELAY_OBSERVER_BOOTNODES=/ip4/203.0.113.10/tcp/17999 \
CSD_RELAY_OBSERVER_BINARY="$FAKE_BINARY" \
CSD_RELAY_OBSERVER_DATADIR="$DATADIR" \
CSD_RELAY_OBSERVER_GENESIS="$GENESIS" \
CSD_RELAY_OBSERVER_DISK_BUDGET_BYTES=1 \
CSD_RELAY_TEST_CAPTURE="$CAPTURE" \
CSD_RELAY_TEST_SLEEP=1 \
"$RUNNER" >/dev/null 2>&1
disk_guard_rc=$?
set -e
[[ "$disk_guard_rc" -ne 0 ]] || {
  printf 'observer disk budget failure did not stop the observer\n' >&2
  exit 1
}
kill -0 "$miner_sentinel_pid" || {
  printf 'observer disk guard affected the miner sentinel\n' >&2
  exit 1
}

set +e
python3 - <<'PY' >/dev/null 2>&1
import resource

limit = 32 * 1024 * 1024
resource.setrlimit(resource.RLIMIT_AS, (limit, limit))
bytearray(128 * 1024 * 1024)
PY
oom_rc=$?
set -e
[[ "$oom_rc" -ne 0 ]] || {
  printf 'observer OOM fixture unexpectedly succeeded\n' >&2
  exit 1
}
kill -0 "$miner_sentinel_pid" || {
  printf 'observer OOM fixture affected the miner sentinel\n' >&2
  exit 1
}

set +e
python3 - <<'PY' >/dev/null 2>&1
import resource

resource.setrlimit(resource.RLIMIT_CPU, (1, 1))
while True:
    pass
PY
cpu_rc=$?
set -e
[[ "$cpu_rc" -ne 0 ]] || {
  printf 'observer CPU limit fixture unexpectedly succeeded\n' >&2
  exit 1
}
kill -0 "$miner_sentinel_pid" || {
  printf 'observer CPU limit fixture affected the miner sentinel\n' >&2
  exit 1
}

printf 'observer relay self-test PASS\n'

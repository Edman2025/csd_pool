#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
BIN_DIR="${CSD_POOL_BIN_DIR:-$ROOT_DIR/target/release}"
DAEMON_BIN="${CSD_POOL_DAEMON_BIN:-$BIN_DIR/csd-pool-daemon}"
WORKERS_BIN="${CSD_POOL_WORKERS_BIN:-$BIN_DIR/csd-pool-workers}"
MOCK_NODE_BIN="${CSD_POOL_MOCK_NODE_BIN:-$BIN_DIR/csd-pool-mock-node}"
SIGNER_BIN="${CSD_POOL_SIGNER_BIN:-$BIN_DIR/csd-pool-signer}"

API_ADDR="${CSD_POOL_E2E_API_ADDR:-127.0.0.1:18080}"
STRATUM_ADDR="${CSD_POOL_E2E_STRATUM_ADDR:-127.0.0.1:13333}"
STATIC_API_ADDR="${CSD_POOL_E2E_STATIC_API_ADDR:-127.0.0.1:18081}"
STATIC_STRATUM_ADDR="${CSD_POOL_E2E_STATIC_STRATUM_ADDR:-127.0.0.1:13334}"
MOCK_NODE_ADDR="${CSD_POOL_E2E_MOCK_NODE_ADDR:-127.0.0.1:18791}"
SIGNER_ADDR="${CSD_POOL_E2E_SIGNER_ADDR:-127.0.0.1:18890}"
API_URL="http://$API_ADDR"
STATIC_API_URL="http://$STATIC_API_ADDR"
MOCK_NODE_URL="http://$MOCK_NODE_ADDR"
SIGNER_URL="http://$SIGNER_ADDR"
POOL_ADDRESS="${CSD_POOL_E2E_POOL_ADDRESS:-0123456789abcdef0123456789abcdef01234567}"
ACCEPTED_SHARE_MINER="${CSD_POOL_E2E_ACCEPTED_SHARE_MINER:-000000000123456789abcdef0123456789abcdf0}"
SMOKE_CLIENTS="${CSD_POOL_E2E_SMOKE_CLIENTS:-5}"
DATABASE_URL="${CSD_POOL_E2E_DATABASE_URL:-}"

MOCK_LOG="${CSD_POOL_E2E_MOCK_LOG:-/tmp/csd-pool-local-e2e-mock-node.log}"
SIGNER_LOG="${CSD_POOL_E2E_SIGNER_LOG:-/tmp/csd-pool-local-e2e-signer.log}"
DAEMON_LOG="${CSD_POOL_E2E_DAEMON_LOG:-/tmp/csd-pool-local-e2e-daemon.log}"
STATIC_DAEMON_LOG="${CSD_POOL_E2E_STATIC_DAEMON_LOG:-/tmp/csd-pool-local-e2e-static-daemon.log}"
TEMPLATE_REPORT="${CSD_POOL_E2E_TEMPLATE_REPORT:-/tmp/csd-pool-local-e2e-template-check.json}"
CANDIDATE_CANARY_REPORT="${CSD_POOL_E2E_CANDIDATE_CANARY_REPORT:-/tmp/csd-pool-local-e2e-candidate-canary.json}"
SIGNER_REPORT="${CSD_POOL_E2E_SIGNER_REPORT:-/tmp/csd-pool-local-e2e-signer-check.json}"
REWARD_REPORT="${CSD_POOL_E2E_REWARD_REPORT:-/tmp/csd-pool-local-e2e-reward-dry-run.json}"
PAYOUT_REPORT="${CSD_POOL_E2E_PAYOUT_REPORT:-/tmp/csd-pool-local-e2e-payout-dry-run.json}"
SMOKE_REPORT="${CSD_POOL_E2E_SMOKE_REPORT:-/tmp/csd-pool-local-e2e-stratum-smoke.json}"
SUBMIT_PROBE_REPORT="${CSD_POOL_E2E_SUBMIT_PROBE_REPORT:-/tmp/csd-pool-local-e2e-stratum-submit-probe.json}"
ACCEPTED_SHARE_REPORT="${CSD_POOL_E2E_ACCEPTED_SHARE_REPORT:-/tmp/csd-pool-local-e2e-accepted-share-probe.json}"
ACCEPTED_MINER_REPORT="${CSD_POOL_E2E_ACCEPTED_MINER_REPORT:-/tmp/csd-pool-local-e2e-accepted-miner.json}"
HTTP_REPORT="${CSD_POOL_E2E_HTTP_REPORT:-/tmp/csd-pool-local-e2e-http.txt}"

PASS=0
FAIL=0
PIDS=()

ok() {
  PASS=$((PASS + 1))
  printf 'ok: %s\n' "$1"
}

fail() {
  FAIL=$((FAIL + 1))
  printf 'fail: %s\n' "$1" >&2
}

cleanup() {
  local pid
  for pid in "${PIDS[@]:-}"; do
    kill "$pid" >/dev/null 2>&1 || true
  done
  for pid in "${PIDS[@]:-}"; do
    wait "$pid" >/dev/null 2>&1 || true
  done
}
trap cleanup EXIT

run_workers_command() {
  if [[ -x "$WORKERS_BIN" ]]; then
    env -u CSD_POOL_CONFIG -u CSD_POOL_DATABASE_URL "$WORKERS_BIN" "$@"
  elif command -v cargo >/dev/null 2>&1; then
    env -u CSD_POOL_CONFIG -u CSD_POOL_DATABASE_URL \
      cargo run --quiet --manifest-path "$ROOT_DIR/Cargo.toml" -p csd-pool-workers -- "$@"
  else
    return 127
  fi
}

start_mock_node() {
  if [[ -x "$MOCK_NODE_BIN" ]]; then
    env -u CSD_POOL_CONFIG -u CSD_POOL_DATABASE_URL \
      CSD_POOL_MOCK_NODE_LISTEN="$MOCK_NODE_ADDR" \
      CSD_POOL_MOCK_NODE_EASY_CANDIDATES=true \
      "$MOCK_NODE_BIN" >"$MOCK_LOG" 2>&1 &
  elif command -v cargo >/dev/null 2>&1; then
    env -u CSD_POOL_CONFIG -u CSD_POOL_DATABASE_URL \
      CSD_POOL_MOCK_NODE_LISTEN="$MOCK_NODE_ADDR" \
      CSD_POOL_MOCK_NODE_EASY_CANDIDATES=true \
      cargo run --quiet --manifest-path "$ROOT_DIR/Cargo.toml" -p csd-pool-mock-node >"$MOCK_LOG" 2>&1 &
  else
    return 127
  fi
  PIDS+=("$!")
}

start_signer() {
  if [[ -x "$SIGNER_BIN" ]]; then
    env -u CSD_POOL_CONFIG -u CSD_POOL_DATABASE_URL -u CSD_POOL_SIGNER_TOKEN \
      CSD_POOL_SIGNER_LISTEN="$SIGNER_ADDR" "$SIGNER_BIN" >"$SIGNER_LOG" 2>&1 &
  elif command -v cargo >/dev/null 2>&1; then
    env -u CSD_POOL_CONFIG -u CSD_POOL_DATABASE_URL -u CSD_POOL_SIGNER_TOKEN \
      CSD_POOL_SIGNER_LISTEN="$SIGNER_ADDR" \
      cargo run --quiet --manifest-path "$ROOT_DIR/Cargo.toml" -p csd-pool-signer >"$SIGNER_LOG" 2>&1 &
  else
    return 127
  fi
  PIDS+=("$!")
}

start_daemon() {
  if [[ -z "$DATABASE_URL" ]]; then
    printf 'CSD_POOL_E2E_DATABASE_URL is required for the live-template daemon\n' >&2
    return 1
  fi
  if [[ -x "$DAEMON_BIN" ]]; then
    env -u CSD_POOL_CONFIG \
      CSD_POOL_DATABASE_URL="$DATABASE_URL" \
      CSD_POOL_API_LISTEN="$API_ADDR" \
      CSD_POOL_STRATUM_LISTEN="$STRATUM_ADDR" \
      CSD_POOL_TEMPLATE_MODE=live \
      CSD_POOL_SUBMIT_CANDIDATES=true \
      CSD_POOL_NODE_URL="$MOCK_NODE_URL" \
      CSD_POOL_MINING_ADDRESS="$POOL_ADDRESS" \
      "$DAEMON_BIN" >"$DAEMON_LOG" 2>&1 &
  elif command -v cargo >/dev/null 2>&1; then
    env -u CSD_POOL_CONFIG \
      CSD_POOL_DATABASE_URL="$DATABASE_URL" \
      CSD_POOL_API_LISTEN="$API_ADDR" \
      CSD_POOL_STRATUM_LISTEN="$STRATUM_ADDR" \
      CSD_POOL_TEMPLATE_MODE=live \
      CSD_POOL_SUBMIT_CANDIDATES=true \
      CSD_POOL_NODE_URL="$MOCK_NODE_URL" \
      CSD_POOL_MINING_ADDRESS="$POOL_ADDRESS" \
      cargo run --quiet --manifest-path "$ROOT_DIR/Cargo.toml" -p csd-pool-daemon >"$DAEMON_LOG" 2>&1 &
  else
    return 127
  fi
  PIDS+=("$!")
}

start_static_daemon() {
  if [[ -x "$DAEMON_BIN" ]]; then
    env -u CSD_POOL_CONFIG -u CSD_POOL_DATABASE_URL \
      CSD_POOL_API_LISTEN="$STATIC_API_ADDR" \
      CSD_POOL_STRATUM_LISTEN="$STATIC_STRATUM_ADDR" \
      CSD_POOL_TEMPLATE_MODE=static \
      CSD_POOL_MINING_ADDRESS="$POOL_ADDRESS" \
      "$DAEMON_BIN" >"$STATIC_DAEMON_LOG" 2>&1 &
  elif command -v cargo >/dev/null 2>&1; then
    env -u CSD_POOL_CONFIG -u CSD_POOL_DATABASE_URL \
      CSD_POOL_API_LISTEN="$STATIC_API_ADDR" \
      CSD_POOL_STRATUM_LISTEN="$STATIC_STRATUM_ADDR" \
      CSD_POOL_TEMPLATE_MODE=static \
      CSD_POOL_MINING_ADDRESS="$POOL_ADDRESS" \
      cargo run --quiet --manifest-path "$ROOT_DIR/Cargo.toml" -p csd-pool-daemon >"$STATIC_DAEMON_LOG" 2>&1 &
  else
    return 127
  fi
  PIDS+=("$!")
}

wait_for_http() {
  local url="$1"
  local label="$2"
  local attempts="${3:-80}"
  local i
  for ((i = 1; i <= attempts; i++)); do
    if curl --fail --silent --show-error --max-time 1 "$url" >/dev/null 2>&1; then
      ok "$label"
      return 0
    fi
    sleep 0.25
  done
  fail "$label"
  return 1
}

check_http_endpoint() {
  local path="$1"
  local label="$2"
  if curl --fail --silent --show-error --max-time 5 "$API_URL$path" >>"$HTTP_REPORT" 2>&1; then
    printf '\n--- %s ---\n' "$path" >>"$HTTP_REPORT"
    ok "$label"
  else
    fail "$label; see $HTTP_REPORT"
  fi
}

require_report_text() {
  local path="$1"
  local pattern="$2"
  local label="$3"
  if grep -Fq "$pattern" "$path"; then
    ok "$label"
  else
    fail "$label; see $path"
  fi
}

printf 'CSD Pool local e2e\n'
printf 'root=%s\n' "$ROOT_DIR"
printf 'api=%s\n' "$API_URL"
printf 'stratum=%s\n' "$STRATUM_ADDR"
printf 'static_api=%s\n' "$STATIC_API_URL"
printf 'static_stratum=%s\n' "$STATIC_STRATUM_ADDR"
printf 'mock_node=%s\n' "$MOCK_NODE_URL"
printf 'signer=%s\n' "$SIGNER_URL"
printf 'database=%s\n' "${DATABASE_URL:+configured}"

: >"$HTTP_REPORT"

if ! command -v curl >/dev/null 2>&1; then
  fail "curl is required"
  printf 'summary: pass=%s fail=%s\n' "$PASS" "$FAIL"
  exit 1
fi

if ! start_mock_node; then
  fail "cannot start mock CSD node; build release binary or install cargo"
  printf 'summary: pass=%s fail=%s\n' "$PASS" "$FAIL"
  exit 1
fi
wait_for_http "$MOCK_NODE_URL/health" "mock CSD node health"

if ! start_signer; then
  fail "cannot start signer; build release binary or install cargo"
  printf 'summary: pass=%s fail=%s\n' "$PASS" "$FAIL"
  exit 1
fi
wait_for_http "$SIGNER_URL/health" "signer health"

if CSD_POOL_NODE_URL="$MOCK_NODE_URL" \
  CSD_POOL_MINING_ADDRESS="$POOL_ADDRESS" \
  run_workers_command check-node-template >"$TEMPLATE_REPORT"; then
  ok "template contract against mock CSD node"
  require_report_text "$TEMPLATE_REPORT" '"passed": true' "template contract report passed"
else
  fail "template contract against mock CSD node; see $TEMPLATE_REPORT and $MOCK_LOG"
fi

if CSD_POOL_TEMPLATE_NODE_URL="$MOCK_NODE_URL" \
  CSD_POOL_SUBMIT_NODE_URL="$MOCK_NODE_URL" \
  CSD_POOL_MINING_ADDRESS="$POOL_ADDRESS" \
  CSD_POOL_NODE_CANDIDATE_CANARY_CONFIRM=mine-and-submit \
  CSD_POOL_NODE_CANDIDATE_CANARY_MAX_ATTEMPTS=100 \
  CSD_POOL_NODE_CANDIDATE_CANARY_THREADS=2 \
  run_workers_command mine-node-candidate-canary >"$CANDIDATE_CANARY_REPORT"; then
  ok "candidate canary against easy mock CSD node"
  require_report_text "$CANDIDATE_CANARY_REPORT" '"passed": true' "candidate canary report passed"
  require_report_text "$CANDIDATE_CANARY_REPORT" '"confirmations": 12' "candidate canary reached confirmed status"
else
  fail "candidate canary against easy mock CSD node; see $CANDIDATE_CANARY_REPORT and $MOCK_LOG"
fi

if CSD_POOL_SIGNER_URL="$SIGNER_URL" \
  run_workers_command check-signer >"$SIGNER_REPORT"; then
  ok "signer contract check"
  require_report_text "$SIGNER_REPORT" '"passed": true' "signer contract report passed"
  require_report_text "$SIGNER_REPORT" '"sign_ok": true' "signer contract sign step passed"
else
  fail "signer contract check; see $SIGNER_REPORT and $SIGNER_LOG"
fi

if run_workers_command reward-dry-run >"$REWARD_REPORT"; then
  ok "reward dry-run"
  require_report_text "$REWARD_REPORT" '"repository": "memory"' "reward dry-run uses memory repository"
  require_report_text "$REWARD_REPORT" '"miner_total_base_units": 4950000000' "reward dry-run preserves miner total"
  require_report_text "$REWARD_REPORT" '"kind": "pool_fee"' "reward dry-run records pool fee"
else
  fail "reward dry-run; see $REWARD_REPORT"
fi

if run_workers_command payout-dry-run >"$PAYOUT_REPORT"; then
  ok "payout dry-run"
  require_report_text "$PAYOUT_REPORT" '"repository": "memory"' "payout dry-run uses memory repository"
  require_report_text "$PAYOUT_REPORT" '"total_base_units": 250000000' "payout dry-run selects expected total"
  require_report_text "$PAYOUT_REPORT" '"kind": "payout_lock"' "payout dry-run records payout lock"
else
  fail "payout dry-run; see $PAYOUT_REPORT"
fi

if ! start_daemon; then
  fail "cannot start daemon; build release binary or install cargo"
  printf 'summary: pass=%s fail=%s\n' "$PASS" "$FAIL"
  exit 1
fi
wait_for_http "$API_URL/health" "daemon API health"

check_http_endpoint "/api/status" "public status endpoint"
check_http_endpoint "/api/pool" "public pool endpoint"
check_http_endpoint "/getting-started" "getting started page"
check_http_endpoint "/api/getting-started" "getting started endpoint"
check_http_endpoint "/api/history?range=12h" "public history endpoint"

if CSD_POOL_SMOKE_CLIENTS="$SMOKE_CLIENTS" \
  CSD_POOL_SMOKE_TIMEOUT_SECS=10 \
  run_workers_command stratum-smoke "$STRATUM_ADDR" >"$SMOKE_REPORT"; then
  ok "stratum smoke against live-template daemon"
  require_report_text "$SMOKE_REPORT" "\"requested_clients\": $SMOKE_CLIENTS" "stratum smoke requested client count"
  require_report_text "$SMOKE_REPORT" "\"succeeded_clients\": $SMOKE_CLIENTS" "stratum smoke succeeded client count"
  require_report_text "$SMOKE_REPORT" '"failed_clients": 0' "stratum smoke has no failed clients"
else
  fail "stratum smoke against live-template daemon; see $SMOKE_REPORT and $DAEMON_LOG"
fi

if CSD_POOL_SMOKE_TIMEOUT_SECS=10 \
  run_workers_command stratum-submit-probe "$STRATUM_ADDR" >"$SUBMIT_PROBE_REPORT"; then
  ok "stratum submit probe against live-template daemon"
  require_report_text "$SUBMIT_PROBE_REPORT" '"passed": true' "stratum submit probe passed"
  require_report_text "$SUBMIT_PROBE_REPORT" '"submit_response_received": true' "stratum submit probe received response"
  require_report_text "$SUBMIT_PROBE_REPORT" '"submit_response_standard": true' "stratum submit probe standard response"
else
  fail "stratum submit probe against live-template daemon; see $SUBMIT_PROBE_REPORT and $DAEMON_LOG"
fi

if ! start_static_daemon; then
  fail "cannot start static daemon; build release binary or install cargo"
else
  wait_for_http "$STATIC_API_URL/health" "static daemon API health"
  if CSD_POOL_SMOKE_TIMEOUT_SECS=10 \
    run_workers_command stratum-accepted-share-probe "$STATIC_STRATUM_ADDR" >"$ACCEPTED_SHARE_REPORT"; then
    ok "stratum accepted share probe against static daemon"
    require_report_text "$ACCEPTED_SHARE_REPORT" '"passed": true' "stratum accepted share probe passed"
    require_report_text "$ACCEPTED_SHARE_REPORT" '"submit_result": true' "stratum accepted share submit accepted"
  else
    fail "stratum accepted share probe against static daemon; see $ACCEPTED_SHARE_REPORT and $STATIC_DAEMON_LOG"
  fi
  if curl --fail --silent --show-error --max-time 5 "$STATIC_API_URL/api/miner/$ACCEPTED_SHARE_MINER" -o "$ACCEPTED_MINER_REPORT"; then
    ok "accepted share miner profile fetched"
    require_report_text "$ACCEPTED_MINER_REPORT" '"online":true' "accepted share miner online"
    require_report_text "$ACCEPTED_MINER_REPORT" '"shares_accepted":1' "accepted share appears in miner API"
  else
    fail "accepted share miner profile fetch failed; see $ACCEPTED_MINER_REPORT and $STATIC_DAEMON_LOG"
  fi
fi

check_http_endpoint "/api/metrics" "public metrics endpoint"

printf 'reports:\n'
printf '  mock_log=%s\n' "$MOCK_LOG"
printf '  signer_log=%s\n' "$SIGNER_LOG"
printf '  daemon_log=%s\n' "$DAEMON_LOG"
printf '  static_daemon_log=%s\n' "$STATIC_DAEMON_LOG"
printf '  template_report=%s\n' "$TEMPLATE_REPORT"
printf '  candidate_canary_report=%s\n' "$CANDIDATE_CANARY_REPORT"
printf '  signer_report=%s\n' "$SIGNER_REPORT"
printf '  reward_report=%s\n' "$REWARD_REPORT"
printf '  payout_report=%s\n' "$PAYOUT_REPORT"
printf '  smoke_report=%s\n' "$SMOKE_REPORT"
printf '  submit_probe_report=%s\n' "$SUBMIT_PROBE_REPORT"
printf '  accepted_share_report=%s\n' "$ACCEPTED_SHARE_REPORT"
printf '  accepted_miner_report=%s\n' "$ACCEPTED_MINER_REPORT"
printf '  http_report=%s\n' "$HTTP_REPORT"
printf 'summary: pass=%s fail=%s\n' "$PASS" "$FAIL"

if [[ "$FAIL" -gt 0 ]]; then
  exit 1
fi

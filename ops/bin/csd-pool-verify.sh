#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
BIN_DIR="${CSD_POOL_BIN_DIR:-$ROOT_DIR/target/release}"
WORKERS_BIN="${CSD_POOL_WORKERS_BIN:-$BIN_DIR/csd-pool-workers}"
DAEMON_BIN="${CSD_POOL_DAEMON_BIN:-$BIN_DIR/csd-pool-daemon}"
SIGNER_BIN="${CSD_POOL_SIGNER_BIN:-$BIN_DIR/csd-pool-signer}"
MOCK_NODE_BIN="${CSD_POOL_MOCK_NODE_BIN:-$BIN_DIR/csd-pool-mock-node}"
API_BIN="${CSD_POOL_API_BIN:-$BIN_DIR/csd-pool-api}"
BRIDGE_BIN="${CSD_POOL_BRIDGE_BIN:-$BIN_DIR/csd-pool-bridge}"
API_URL="${CSD_POOL_VERIFY_API_URL:-http://127.0.0.1:8080}"
STRATUM_ADDR="${CSD_POOL_VERIFY_STRATUM_ADDR:-127.0.0.1:3333}"
MOCK_NODE_ADDR="${CSD_POOL_VERIFY_MOCK_NODE_ADDR:-127.0.0.1:18790}"
MOCK_NODE_URL="${CSD_POOL_VERIFY_MOCK_NODE_URL:-http://$MOCK_NODE_ADDR}"
MOCK_NODE_POOL_ADDRESS="${CSD_POOL_VERIFY_MOCK_NODE_POOL_ADDRESS:-0123456789abcdef0123456789abcdef01234567}"
CONFIG_PATH="${CSD_POOL_CONFIG:-$ROOT_DIR/ops/config.private-beta.toml}"
ENV_PATH="${CSD_POOL_ENV_FILE:-$ROOT_DIR/ops/env/csd-pool.env.example}"
HAPROXY_CONFIG="${CSD_POOL_HAPROXY_CONFIG:-$ROOT_DIR/ops/haproxy/haproxy.cfg}"
BACKUP_DIR="${CSD_POOL_BACKUP_DIR:-/var/backups/csd-pool}"
BACKUP_MAX_AGE_DAYS="${CSD_POOL_BACKUP_MAX_AGE_DAYS:-2}"
BACKUP_MIN_BYTES="${CSD_POOL_BACKUP_MIN_BYTES:-1024}"

PASS=0
FAIL=0
SKIP=0

ok() {
  PASS=$((PASS + 1))
  printf 'ok: %s\n' "$1"
}

fail() {
  FAIL=$((FAIL + 1))
  printf 'fail: %s\n' "$1" >&2
}

skip() {
  SKIP=$((SKIP + 1))
  printf 'skip: %s\n' "$1"
}

require_file() {
  local path="$1"
  if [[ -f "$path" ]]; then
    ok "file exists: $path"
  else
    fail "missing file: $path"
  fi
}

require_executable() {
  local path="$1"
  if [[ -x "$path" ]]; then
    ok "executable exists: $path"
  else
    fail "missing executable: $path"
  fi
}

require_text_in_file() {
  local path="$1"
  local pattern="$2"
  local label="$3"
  if grep -Fq "$pattern" "$path"; then
    ok "$label"
  else
    fail "$label"
  fi
}

verify_sha256_manifest() {
  local manifest="$1"
  local manifest_dir manifest_file
  manifest_dir="$(cd "$(dirname "$manifest")" && pwd)"
  manifest_file="$(basename "$manifest")"

  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$manifest_dir" && sha256sum -c "$manifest_file")
  elif command -v shasum >/dev/null 2>&1; then
    (cd "$manifest_dir" && shasum -a 256 -c "$manifest_file")
  else
    printf 'sha256 verification tool missing\n' >&2
    return 127
  fi
}

check_docs_redaction_scan() {
  local base_dir="$1"
  local label="$2"
  local pattern='postgres(ql)?://[^[:space:]`]+:[^<][^@[:space:]`]+@|CSD_POOL_(OPERATOR|SIGNER)_TOKEN=(dev-secret|change-me[^[:space:]]*)|Authorization:[[:space:]]*Bearer[[:space:]]+[A-Za-z0-9._~+/=-]{8,}'
  local findings=""
  local doc

  for doc in \
    "$base_dir/README.md" \
    "$base_dir/ops/README.md" \
    "$base_dir/ops/INCIDENT-RUNBOOK.md" \
    "$base_dir/docs/architecture.md" \
    "$base_dir/docs/api-spec.md" \
    "$base_dir/docs/product-design.md"; do
    if [[ -f "$doc" ]]; then
      findings="$findings$(grep -EIn "$pattern" "$doc" || true)"
    fi
  done

  if [[ -n "$findings" ]]; then
    printf '%s\n' "$findings" >/tmp/csd-pool-doc-redaction-findings.log
    fail "$label: doc redaction scan failed; see /tmp/csd-pool-doc-redaction-findings.log"
  else
    ok "$label: doc redaction scan passed"
  fi
}

check_release_archive() {
  local archive="$1"
  local tmp_dir root_count release_root
  local entry entry_name entry_path

  require_file "$archive"
  if [[ ! -f "$archive" ]]; then
    return
  fi

  if tar -tzf "$archive" | grep -Eq '(^/|(^|/)\.\.(/|$))'; then
    fail "release archive contains unsafe paths"
    return
  fi
  ok "release archive paths are safe"

  tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-release-verify.XXXXXX")"
  if ! tar -xzf "$archive" -C "$tmp_dir"; then
    rm -rf "$tmp_dir"
    fail "release archive extracts"
    return
  fi
  ok "release archive extracts"

  root_count="$(find "$tmp_dir" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')"
  if [[ "$root_count" != "1" ]]; then
    rm -rf "$tmp_dir"
    fail "release archive has exactly one root directory"
    return
  fi
  release_root="$(find "$tmp_dir" -mindepth 1 -maxdepth 1 -type d | head -n 1)"
  ok "release archive has exactly one root directory"

  require_file "$release_root/RELEASE-MANIFEST.txt"
  require_file "$release_root/SHA256SUMS"
  require_file "$release_root/.github/workflows/ci.yml"
  require_executable "$release_root/ops/wallet-signer/signer.mjs"
  require_file "$release_root/ops/wallet-signer/package-lock.json"
  require_file "$release_root/ops/wallet-signer/node_modules/@inversealtruism/csd-client/package.json"
  require_file "$release_root/ops/wallet-signer/node_modules/@inversealtruism/csd-crypto/package.json"
  require_file "$release_root/ops/wallet-signer/node_modules/@inversealtruism/csd-tx/package.json"
  require_file "$release_root/ops/csd-node-adapter/compute-substrate-pool-adapter.patch"
  require_file "$release_root/ops/csd-node-adapter/MANIFEST.txt"
  require_executable "$release_root/ops/csd-node-adapter/apply-and-build.sh"
  require_executable "$release_root/ops/bin/csd-pool-launch-gaps-self-test.sh"
  if [[ -f "$release_root/SHA256SUMS" ]]; then
    if verify_sha256_manifest "$release_root/SHA256SUMS" >/tmp/csd-pool-release-sha256sum.log 2>&1; then
      ok "release archive SHA256SUMS verifies"
    else
      fail "release archive SHA256SUMS verifies; see /tmp/csd-pool-release-sha256sum.log"
    fi
  fi

  require_text_in_file "$release_root/RELEASE-MANIFEST.txt" "release_check=ops/bin/csd-pool-release-check.sh" "release manifest records release check"
  require_text_in_file "$release_root/RELEASE-MANIFEST.txt" "ci_workflow=.github/workflows/ci.yml" "release manifest records CI workflow"
  require_text_in_file "$release_root/RELEASE-MANIFEST.txt" "wallet_signer=ops/wallet-signer/signer.mjs" "release manifest records wallet signer"
  require_text_in_file "$release_root/RELEASE-MANIFEST.txt" "launch_gaps_self_test=ops/bin/csd-pool-launch-gaps-self-test.sh" "release manifest records launch gaps self-test"
  for entry in \
    "verify=ops/bin/csd-pool-verify.sh" \
    "real_env_doctor=ops/bin/csd-pool-real-env-doctor.sh" \
    "real_env_doctor_self_test=ops/bin/csd-pool-real-env-doctor-self-test.sh" \
    "live_startup_policy_self_test=ops/bin/csd-pool-live-startup-policy-self-test.sh" \
    "go_live=ops/bin/csd-pool-go-live-check.sh" \
    "real_go_live=ops/bin/csd-pool-real-go-live.sh" \
    "verify_go_live_evidence=ops/bin/csd-pool-verify-go-live-evidence.sh" \
    "verify_real_go_live_summary=ops/bin/csd-pool-verify-real-go-live-summary.sh" \
    "export_real_go_live_receipt=ops/bin/csd-pool-export-real-go-live-receipt.sh" \
    "verify_real_go_live_receipt=ops/bin/csd-pool-verify-real-go-live-receipt.sh" \
    "public_acceptance=ops/bin/csd-pool-public-acceptance.sh" \
    "verify_public_acceptance_evidence=ops/bin/csd-pool-verify-public-acceptance-evidence.sh" \
    "public_acceptance_self_test=ops/bin/csd-pool-public-acceptance-self-test.sh" \
    "verify_launch_handoff=ops/bin/csd-pool-verify-launch-handoff.sh" \
    "launch_handoff_self_test=ops/bin/csd-pool-launch-handoff-self-test.sh" \
    "export_launch_handoff=ops/bin/csd-pool-export-launch-handoff.sh" \
    "verify_launch_handoff_package=ops/bin/csd-pool-verify-launch-handoff-package.sh" \
    "audit_launch_readiness=ops/bin/csd-pool-audit-launch-readiness.sh" \
    "export_launch_dossier=ops/bin/csd-pool-export-launch-dossier.sh" \
    "verify_launch_dossier=ops/bin/csd-pool-verify-launch-dossier.sh" \
    "launch_dossier_self_test=ops/bin/csd-pool-launch-dossier-self-test.sh" \
    "finalize_launch=ops/bin/csd-pool-finalize-launch.sh" \
    "explain_launch_gaps=ops/bin/csd-pool-explain-launch-gaps.sh" \
    "export_final_review=ops/bin/csd-pool-export-final-review.sh" \
    "verify_final_review=ops/bin/csd-pool-verify-final-review.sh" \
    "final_review_self_test=ops/bin/csd-pool-final-review-self-test.sh" \
    "evidence_redaction_self_test=ops/bin/csd-pool-evidence-redaction-self-test.sh" \
    "release_archive_self_test=ops/bin/csd-pool-release-archive-self-test.sh" \
    "generate_signoff=ops/bin/csd-pool-generate-signoff.sh" \
    "install_release=ops/bin/csd-pool-install-release.sh" \
    "rollback_release=ops/bin/csd-pool-rollback-release.sh" \
    "install_release_self_test=ops/bin/csd-pool-install-release-self-test.sh" \
    "payout_serialization_self_test=ops/bin/csd-pool-payout-serialization-self-test.sh" \
    "local_e2e=ops/bin/csd-pool-local-e2e.sh" \
    "node_adapter_run=ops/bin/csd-pool-node-adapter-run.sh" \
    "dev_env=ops/bin/csd-pool-dev-env.sh"; do
    entry_name="${entry%%=*}"
    entry_path="${entry#*=}"
    require_text_in_file "$release_root/RELEASE-MANIFEST.txt" "$entry" "release manifest records $entry_name"
    require_executable "$release_root/$entry_path"
  done
  require_text_in_file "$release_root/.github/workflows/ci.yml" "Launch gaps self-test" "release archive CI workflow records launch gaps self-test"
  require_text_in_file "$release_root/.github/workflows/ci.yml" "ops/bin/csd-pool-launch-gaps-self-test.sh" "release archive CI workflow runs launch gaps self-test"
  require_text_in_file "$release_root/.github/workflows/ci.yml" "Launch handoff self-test" "release archive CI workflow records launch handoff self-test"
  require_text_in_file "$release_root/.github/workflows/ci.yml" "ops/bin/csd-pool-launch-handoff-self-test.sh" "release archive CI workflow runs launch handoff self-test"
  require_text_in_file "$release_root/.github/workflows/ci.yml" "Launch dossier self-test" "release archive CI workflow records launch dossier self-test"
  require_text_in_file "$release_root/.github/workflows/ci.yml" "ops/bin/csd-pool-launch-dossier-self-test.sh" "release archive CI workflow runs launch dossier self-test"
  require_text_in_file "$release_root/.github/workflows/ci.yml" "ops/bin/csd-pool-live-startup-policy-self-test.sh" "release archive CI workflow runs live startup policy self-test"
  require_text_in_file "$release_root/ops/bin/csd-pool-release-check.sh" "doc has no unredacted database URL password" "release check carries doc redaction gate"
  require_text_in_file "$release_root/ops/bin/csd-pool-release-check.sh" "check_release_manifest_coverage" "release check carries manifest coverage gate"
  require_text_in_file "$release_root/ops/bin/csd-pool-release-check.sh" "release verifier covers every release manifest ops/bin and workflow entry" "release check reports manifest coverage gate"
  require_text_in_file "$release_root/ops/bin/csd-pool-release-check.sh" "missing release verifier manifest coverage" "release check reports manifest coverage failure"
  require_text_in_file "$release_root/ops/bin/csd-pool-verify.sh" "verify_sha256_manifest" "release verifier carries SHA256 manifest helper"
  require_text_in_file "$release_root/ops/bin/csd-pool-verify.sh" "shasum -a 256 -c" "release verifier carries shasum fallback"
  require_text_in_file "$release_root/ops/bin/csd-pool-verify.sh" "sha256 verification tool missing" "release verifier reports missing SHA256 tool"
  check_docs_redaction_scan "$release_root" "release archive"
  if CSD_POOL_ROOT="$release_root" "$release_root/ops/bin/csd-pool-launch-gaps-self-test.sh" >/tmp/csd-pool-launch-gaps-self-test.log 2>&1; then
    ok "release archive launch gaps self-test passed"
  else
    fail "release archive launch gaps self-test failed; see /tmp/csd-pool-launch-gaps-self-test.log"
  fi
  if CSD_POOL_ROOT="$release_root" "$release_root/ops/bin/csd-pool-launch-handoff-self-test.sh" >/tmp/csd-pool-launch-handoff-self-test.log 2>&1; then
    ok "release archive launch handoff self-test passed"
  else
    fail "release archive launch handoff self-test failed; see /tmp/csd-pool-launch-handoff-self-test.log"
  fi
  if CSD_POOL_ROOT="$release_root" "$release_root/ops/bin/csd-pool-launch-dossier-self-test.sh" >/tmp/csd-pool-launch-dossier-self-test.log 2>&1; then
    ok "release archive launch dossier self-test passed"
  else
    fail "release archive launch dossier self-test failed; see /tmp/csd-pool-launch-dossier-self-test.log"
  fi
  rm -rf "$tmp_dir"
}

check_systemd_service_hardening() {
  local service="$1"
  local environment_file="EnvironmentFile=/etc/csd-pool/csd-pool.env"
  if [[ "$(basename "$service")" == "csd-pool-node-adapter.service" ]]; then
    environment_file="EnvironmentFile=/etc/csd-pool/node.env"
  fi
  require_text_in_file "$service" "User=" "systemd hardening $(basename "$service"): User"
  require_text_in_file "$service" "WorkingDirectory=/opt/csd-pool" "systemd hardening $(basename "$service"): WorkingDirectory"
  require_text_in_file "$service" "$environment_file" "systemd hardening $(basename "$service"): EnvironmentFile"
  require_text_in_file "$service" "NoNewPrivileges=true" "systemd hardening $(basename "$service"): NoNewPrivileges"
  require_text_in_file "$service" "PrivateTmp=true" "systemd hardening $(basename "$service"): PrivateTmp"
  require_text_in_file "$service" "ProtectHome=true" "systemd hardening $(basename "$service"): ProtectHome"
  require_text_in_file "$service" "ProtectSystem=strict" "systemd hardening $(basename "$service"): ProtectSystem"
}

check_haproxy_template() {
  local config="$1"
  require_file "$config"
  if [[ ! -f "$config" ]]; then
    return
  fi
  require_text_in_file "$config" "frontend csd_stratum_3333" "haproxy stratum frontend"
  require_text_in_file "$config" "bind :3333" "haproxy stratum public bind"
  require_text_in_file "$config" "stick-table type ip" "haproxy stratum stick table"
  require_text_in_file "$config" "tcp-request connection reject if { sc0_conn_cur gt 64 }" "haproxy stratum connection cap"
  require_text_in_file "$config" "server local_pool 127.0.0.1:33330 check" "haproxy stratum local backend"
  require_text_in_file "$config" "frontend csd_http" "haproxy http frontend"
  require_text_in_file "$config" "bind :80" "haproxy http public bind"
  require_text_in_file "$config" "option httpchk GET /health" "haproxy api health check"
  require_text_in_file "$config" "server local_api 127.0.0.1:8080 check" "haproxy api local backend"
  if command -v haproxy >/dev/null 2>&1; then
    if haproxy -c -f "$config" >/tmp/csd-pool-haproxy-verify.log 2>&1; then
      ok "haproxy config validates"
    else
      fail "haproxy config validates; see /tmp/csd-pool-haproxy-verify.log"
    fi
  else
    skip "haproxy config validation skipped; haproxy not installed"
  fi
}

check_no_placeholder_secrets() {
  local path="$1"
  local label="$2"
  if [[ ! -f "$path" ]]; then
    fail "$label: file missing"
    return
  fi
  case "$path" in
    *.example|*.sample|*.template)
      skip "$label: example/template file"
      return
      ;;
  esac
  if grep -Eiq '(change-me|dev-secret|example-secret|placeholder|replace-me)' "$path"; then
    fail "$label: placeholder secret found"
  else
    ok "$label: no placeholder secrets"
  fi
}

check_recent_backup() {
  local dir="$1"
  local max_age_days="$2"
  local min_bytes="$3"
  if [[ ! "$max_age_days" =~ ^[0-9]+$ ]] || [[ "$max_age_days" -lt 1 ]]; then
    fail "backup freshness: CSD_POOL_BACKUP_MAX_AGE_DAYS must be a positive integer"
    return
  fi
  if [[ ! "$min_bytes" =~ ^[0-9]+$ ]]; then
    fail "backup freshness: CSD_POOL_BACKUP_MIN_BYTES must be a non-negative integer"
    return
  fi
  if [[ ! -d "$dir" ]]; then
    fail "backup freshness: directory missing: $dir"
    return
  fi

  local recent_backup
  recent_backup="$(find "$dir" -type f -name '*.dump' -mtime "-$max_age_days" -size +"$min_bytes"c -print 2>/dev/null | sort | tail -n 1)"
  if [[ -z "$recent_backup" ]]; then
    fail "backup freshness: no .dump newer than ${max_age_days}d and larger than ${min_bytes} bytes in $dir"
  else
    ok "backup freshness: $(basename "$recent_backup")"
  fi
}

check_http() {
  local url="$1"
  local label="$2"
  if command -v curl >/dev/null 2>&1; then
    if curl --fail --silent --show-error --max-time 5 "$url" >/dev/null; then
      ok "$label"
    else
      fail "$label"
    fi
  else
    skip "$label: curl not installed"
  fi
}

run_workers_command() {
  if [[ -x "$WORKERS_BIN" ]]; then
    "$WORKERS_BIN" "$@"
  elif command -v cargo >/dev/null 2>&1; then
    cargo run --quiet --manifest-path "$ROOT_DIR/Cargo.toml" -p csd-pool-workers -- "$@"
  else
    return 127
  fi
}

start_mock_node() {
  if [[ -x "$MOCK_NODE_BIN" ]]; then
    CSD_POOL_MOCK_NODE_LISTEN="$MOCK_NODE_ADDR" "$MOCK_NODE_BIN" >/tmp/csd-pool-mock-node.log 2>&1 &
  elif command -v cargo >/dev/null 2>&1; then
    CSD_POOL_MOCK_NODE_LISTEN="$MOCK_NODE_ADDR" cargo run --quiet --manifest-path "$ROOT_DIR/Cargo.toml" -p csd-pool-mock-node >/tmp/csd-pool-mock-node.log 2>&1 &
  else
    return 127
  fi
  printf '%s\n' "$!"
}

wait_for_mock_node() {
  local attempts=40
  local delay=0.25
  local i
  for ((i = 1; i <= attempts; i++)); do
    if curl --fail --silent --show-error --max-time 1 "$MOCK_NODE_URL/health" >/dev/null 2>&1; then
      return 0
    fi
    sleep "$delay"
  done
  return 1
}

check_mock_node_contract() {
  if ! command -v curl >/dev/null 2>&1; then
    skip "mock CSD node contract disabled; curl not installed"
    return
  fi

  local pid=""
  if ! pid="$(start_mock_node)"; then
    fail "mock CSD node contract: cannot start mock node without binary or cargo"
    return
  fi

  if ! wait_for_mock_node; then
    kill "$pid" >/dev/null 2>&1 || true
    wait "$pid" >/dev/null 2>&1 || true
    fail "mock CSD node contract: mock node did not become healthy; see /tmp/csd-pool-mock-node.log"
    return
  fi

  if CSD_POOL_NODE_URL="$MOCK_NODE_URL" \
    CSD_POOL_MINING_ADDRESS="$MOCK_NODE_POOL_ADDRESS" \
    run_workers_command check-node-template >/tmp/csd-pool-mock-node-template-check.json; then
    ok "mock CSD node contract check completed"
  else
    fail "mock CSD node contract check failed; see /tmp/csd-pool-mock-node-template-check.json and /tmp/csd-pool-mock-node.log"
  fi
  kill "$pid" >/dev/null 2>&1 || true
  wait "$pid" >/dev/null 2>&1 || true
}

printf 'CSD Pool verification\n'
printf 'root=%s\n' "$ROOT_DIR"
printf 'bin_dir=%s\n' "$BIN_DIR"

require_file "$CONFIG_PATH"
require_file "$ENV_PATH"
check_haproxy_template "$HAPROXY_CONFIG"
require_executable "$ROOT_DIR/ops/bin/csd-pool-verify.sh"
require_executable "$ROOT_DIR/ops/bin/csd-pool-preflight.sh"
require_executable "$ROOT_DIR/ops/bin/csd-pool-go-live-check.sh"
require_executable "$ROOT_DIR/ops/bin/csd-pool-verify-go-live-evidence.sh"
require_executable "$ROOT_DIR/ops/bin/csd-pool-local-e2e.sh"
require_executable "$ROOT_DIR/ops/bin/csd-pool-restore-drill.sh"
require_executable "$ROOT_DIR/ops/bin/csd-pool-release-check.sh"
require_file "$ROOT_DIR/ops/RELEASE-CHECKLIST.md"
check_no_placeholder_secrets "$ENV_PATH" "environment file"

if run_workers_command check-config "$CONFIG_PATH" >/tmp/csd-pool-check-config.json; then
  ok "config check completed"
else
  fail "config check failed; see /tmp/csd-pool-check-config.json"
fi

for unit in "$ROOT_DIR"/ops/systemd/*.service "$ROOT_DIR"/ops/systemd/*.timer; do
  require_file "$unit"
done

for service in "$ROOT_DIR"/ops/systemd/*.service; do
  check_systemd_service_hardening "$service"
done

for expected_unit in \
  csd-pool-daemon.service \
  csd-pool-signer.service \
  csd-pool-reconcile-blocks.timer \
  csd-pool-rewards.timer \
  csd-pool-payout-create.timer \
  csd-pool-payout-sign.timer \
  csd-pool-payout-submit.timer \
  csd-pool-payout-reconcile.timer \
  csd-pool-monitoring.timer \
  csd-pool-backup.timer; do
  require_file "$ROOT_DIR/ops/systemd/$expected_unit"
done

if command -v systemd-analyze >/dev/null 2>&1; then
  if systemd-analyze verify "$ROOT_DIR"/ops/systemd/*.service "$ROOT_DIR"/ops/systemd/*.timer >/tmp/csd-pool-systemd-verify.log 2>&1; then
    ok "systemd units verify"
  else
    fail "systemd units verify; see /tmp/csd-pool-systemd-verify.log"
  fi
else
  skip "systemd-analyze not installed"
fi

if [[ "${CSD_POOL_VERIFY_RELEASE:-0}" == "1" ]]; then
  require_executable "$WORKERS_BIN"
  require_executable "$DAEMON_BIN"
  require_executable "$SIGNER_BIN"
  require_executable "$API_BIN"
  require_executable "$BRIDGE_BIN"
  require_executable "$MOCK_NODE_BIN"
  if [[ -n "${CSD_POOL_VERIFY_RELEASE_ARCHIVE:-}" ]]; then
    check_release_archive "$CSD_POOL_VERIFY_RELEASE_ARCHIVE"
  else
    skip "release archive verification disabled; set CSD_POOL_VERIFY_RELEASE_ARCHIVE"
  fi
else
  skip "release binary checks disabled; set CSD_POOL_VERIFY_RELEASE=1"
fi

if [[ "${CSD_POOL_VERIFY_BACKUP:-0}" == "1" ]]; then
  check_recent_backup "$BACKUP_DIR" "$BACKUP_MAX_AGE_DAYS" "$BACKUP_MIN_BYTES"
else
  skip "backup freshness check disabled; set CSD_POOL_VERIFY_BACKUP=1"
fi

if [[ "${CSD_POOL_VERIFY_MIGRATE:-0}" == "1" ]]; then
  if run_workers_command migrate >/tmp/csd-pool-migrate.log; then
    ok "database migrations applied"
  else
    fail "database migrations failed; see /tmp/csd-pool-migrate.log"
  fi
else
  skip "database migration check disabled; set CSD_POOL_VERIFY_MIGRATE=1"
fi

if [[ "${CSD_POOL_VERIFY_MOCK_NODE:-0}" == "1" ]]; then
  check_mock_node_contract
else
  skip "mock CSD node contract disabled; set CSD_POOL_VERIFY_MOCK_NODE=1"
fi

if [[ "${CSD_POOL_VERIFY_LOCAL_E2E:-0}" == "1" ]]; then
  if "$ROOT_DIR/ops/bin/csd-pool-local-e2e.sh" >/tmp/csd-pool-local-e2e.log; then
    ok "local e2e completed"
  else
    fail "local e2e failed; see /tmp/csd-pool-local-e2e.log"
  fi
else
  skip "local e2e disabled; set CSD_POOL_VERIFY_LOCAL_E2E=1"
fi

if [[ "${CSD_POOL_VERIFY_HTTP:-1}" == "1" ]]; then
  check_http "$API_URL/health" "api health endpoint"
  check_http "$API_URL/status" "public status page"
  check_http "$API_URL/getting-started" "getting started page"
  check_http "$API_URL/api/status" "public status endpoint"
  check_http "$API_URL/api/pool" "api pool endpoint"
  check_http "$API_URL/api/getting-started" "getting started endpoint"
  if [[ -n "${CSD_POOL_OPERATOR_TOKEN:-}" ]]; then
    if command -v curl >/dev/null 2>&1; then
      if curl --fail --silent --show-error --max-time 5 \
        -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
        "$API_URL/api/operator/payouts/status" >/dev/null; then
        ok "operator payout status endpoint"
      else
        fail "operator payout status endpoint"
      fi
    else
      skip "operator payout status endpoint: curl not installed"
    fi
  else
    skip "operator endpoint check disabled; set CSD_POOL_OPERATOR_TOKEN"
  fi
else
  skip "http checks disabled; set CSD_POOL_VERIFY_HTTP=1"
fi

if [[ "${CSD_POOL_VERIFY_SMOKE:-0}" == "1" ]]; then
  if run_workers_command stratum-smoke "$STRATUM_ADDR" >/tmp/csd-pool-stratum-smoke.json; then
    ok "stratum smoke completed"
  else
    fail "stratum smoke failed; see /tmp/csd-pool-stratum-smoke.json"
  fi
else
  skip "stratum smoke disabled; set CSD_POOL_VERIFY_SMOKE=1"
fi

if [[ "${CSD_POOL_VERIFY_LOAD:-0}" == "1" ]]; then
  if run_workers_command stratum-load-test "$STRATUM_ADDR" >/tmp/csd-pool-stratum-load-test.json; then
    ok "stratum load test completed"
  else
    fail "stratum load test failed; see /tmp/csd-pool-stratum-load-test.json"
  fi
else
  skip "stratum load test disabled; set CSD_POOL_VERIFY_LOAD=1"
fi

printf 'summary: pass=%s fail=%s skip=%s\n' "$PASS" "$FAIL" "$SKIP"
if [[ "$FAIL" -gt 0 ]]; then
  exit 1
fi

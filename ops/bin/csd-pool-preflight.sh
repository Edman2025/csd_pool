#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
BIN_DIR="${CSD_POOL_BIN_DIR:-$ROOT_DIR/target/release}"
WORKERS_BIN="${CSD_POOL_WORKERS_BIN:-$BIN_DIR/csd-pool-workers}"
ENV_PATH="${CSD_POOL_ENV_FILE:-/etc/csd-pool/csd-pool.env}"
if [[ ! -f "$ENV_PATH" && -f "$ROOT_DIR/ops/env/csd-pool.env.example" ]]; then
  ENV_PATH="$ROOT_DIR/ops/env/csd-pool.env.example"
fi
CONFIG_PATH="${CSD_POOL_PREFLIGHT_CONFIG:-${CSD_POOL_CONFIG:-/etc/csd-pool/config.toml}}"
if [[ ! -f "$CONFIG_PATH" && -f "$ROOT_DIR/ops/config.private-beta.toml" ]]; then
  CONFIG_PATH="$ROOT_DIR/ops/config.private-beta.toml"
fi
REPORT_DIR="${CSD_POOL_PREFLIGHT_REPORT_DIR:-/tmp}"

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

run_workers_command() {
  if [[ -x "$WORKERS_BIN" ]]; then
    "$WORKERS_BIN" "$@"
  elif command -v cargo >/dev/null 2>&1; then
    cargo run --quiet --manifest-path "$ROOT_DIR/Cargo.toml" -p csd-pool-workers -- "$@"
  else
    return 127
  fi
}

file_mode() {
  stat -f '%Lp' "$1" 2>/dev/null || stat -c '%a' "$1" 2>/dev/null || true
}

check_env_permissions() {
  local path="$1"
  case "$path" in
    *.example|*.sample|*.template)
      skip "env permission check skipped for example/template file"
      return
      ;;
  esac
  local mode
  mode="$(file_mode "$path")"
  if [[ -z "$mode" ]]; then
    skip "env permission check skipped; stat mode unavailable"
    return
  fi
  local world_digit="${mode: -1}"
  if [[ "$world_digit" =~ ^[0-7]$ && "$world_digit" -eq 0 ]]; then
    ok "env file is not world-readable: mode $mode"
  else
    fail "env file must not be world-readable: mode $mode"
  fi
}

load_env() {
  local path="$1"
  if [[ ! -f "$path" ]]; then
    fail "env file missing: $path"
    return 1
  fi
  set -a
  # shellcheck disable=SC1090
  source "$path"
  set +a
  ok "env file loaded: $path"
}

printf 'CSD Pool preflight\n'
printf 'root=%s\n' "$ROOT_DIR"
printf 'env=%s\n' "$ENV_PATH"
printf 'config=%s\n' "$CONFIG_PATH"

mkdir -p "$REPORT_DIR"

if load_env "$ENV_PATH"; then
  check_env_permissions "$ENV_PATH"
fi

export CSD_POOL_CONFIG="$CONFIG_PATH"
if [[ -f "$CONFIG_PATH" ]]; then
  ok "config file exists: $CONFIG_PATH"
else
  fail "config file missing: $CONFIG_PATH"
fi

if CSD_POOL_CHECK_CONFIG_REQUIRE_ENV=1 \
  run_workers_command check-config "$CONFIG_PATH" >"$REPORT_DIR/csd-pool-check-config.json"; then
  ok "config check passed"
else
  fail "config check failed; see $REPORT_DIR/csd-pool-check-config.json"
fi

if [[ "${CSD_POOL_PREFLIGHT_NODE:-0}" == "1" ]]; then
  if run_workers_command check-node-template >"$REPORT_DIR/csd-pool-check-node-template.json"; then
    ok "node template contract passed"
  else
    fail "node template contract failed; see $REPORT_DIR/csd-pool-check-node-template.json"
  fi
else
  skip "node template contract disabled; set CSD_POOL_PREFLIGHT_NODE=1"
fi

if [[ "${CSD_POOL_PREFLIGHT_SIGNER:-0}" == "1" ]]; then
  if run_workers_command check-signer >"$REPORT_DIR/csd-pool-check-signer.json"; then
    ok "signer contract passed"
  else
    fail "signer contract failed; see $REPORT_DIR/csd-pool-check-signer.json"
  fi
else
  skip "signer contract disabled; set CSD_POOL_PREFLIGHT_SIGNER=1"
fi

if [[ "${CSD_POOL_PREFLIGHT_MIGRATE:-0}" == "1" ]]; then
  if run_workers_command migrate >"$REPORT_DIR/csd-pool-migrate.json"; then
    ok "database migrations applied"
  else
    fail "database migrations failed; see $REPORT_DIR/csd-pool-migrate.json"
  fi
else
  skip "database migration disabled; set CSD_POOL_PREFLIGHT_MIGRATE=1"
fi

if [[ "${CSD_POOL_PREFLIGHT_VERIFY:-0}" == "1" ]]; then
  if CSD_POOL_VERIFY_HTTP="${CSD_POOL_PREFLIGHT_VERIFY_HTTP:-0}" \
    CSD_POOL_ENV_FILE="$ENV_PATH" \
    CSD_POOL_CONFIG="$CONFIG_PATH" \
    "$ROOT_DIR/ops/bin/csd-pool-verify.sh" >"$REPORT_DIR/csd-pool-verify.log"; then
    ok "deployment verification passed"
  else
    fail "deployment verification failed; see $REPORT_DIR/csd-pool-verify.log"
  fi
else
  skip "deployment verification disabled; set CSD_POOL_PREFLIGHT_VERIFY=1"
fi

printf 'reports=%s\n' "$REPORT_DIR"
printf 'summary: pass=%s fail=%s skip=%s\n' "$PASS" "$FAIL" "$SKIP"
if [[ "$FAIL" -gt 0 ]]; then
  exit 1
fi

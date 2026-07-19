#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
BIN_DIR="${CSD_POOL_BIN_DIR:-$ROOT_DIR/target/release}"
WORKERS_BIN="${CSD_POOL_WORKERS_BIN:-$BIN_DIR/csd-pool-workers}"
BACKUP_PATH="${CSD_POOL_BACKUP_PATH:-${1:-}}"
RESTORE_DATABASE_URL="${CSD_POOL_RESTORE_DATABASE_URL:-}"
RESTORE_API_URL="${CSD_POOL_RESTORE_API_URL:-}"
RESTORE_REPORT_DIR="${CSD_POOL_RESTORE_REPORT_DIR:-}"
CONFIRM="${CSD_POOL_RESTORE_DRILL_CONFIRM:-}"

redact_url() {
  local value="$1"
  printf '%s' "$value" | sed -E 's#(postgres(ql)?://[^:/@]+):[^@]*@#\1:<redacted>@#'
}

require() {
  local name="$1"
  local value="$2"
  if [[ -z "$value" ]]; then
    printf 'missing required %s\n' "$name" >&2
    exit 2
  fi
}

run_or_print() {
  if [[ "$CONFIRM" == "restore-drill" ]]; then
    "$@"
  else
    printf 'dry-run:'
    for arg in "$@"; do
      if [[ "$arg" == CSD_POOL_DATABASE_URL=* ]]; then
        printf ' %q' "CSD_POOL_DATABASE_URL=$(redact_url "${arg#CSD_POOL_DATABASE_URL=}")"
      else
        printf ' %q' "$arg"
      fi
    done
    printf '\n'
  fi
}

check_http() {
  local url="$1"
  local output_path
  if [[ -z "$url" ]]; then
    printf 'skip: restore API checks disabled; set CSD_POOL_RESTORE_API_URL\n'
    return 0
  fi
  if ! command -v curl >/dev/null 2>&1; then
    printf 'skip: curl not installed\n'
    return 0
  fi
  if [[ -n "$RESTORE_REPORT_DIR" ]]; then
    mkdir -p "$RESTORE_REPORT_DIR"
  fi
  for item in \
    "/health:restore-http-health.json" \
    "/api/pool:restore-http-pool.json" \
    "/api/blocks:restore-http-blocks.json" \
    "/api/payments:restore-http-payments.json"; do
    path="${item%%:*}"
    output_path="${item#*:}"
    if [[ -n "$RESTORE_REPORT_DIR" ]]; then
      curl --fail --silent --show-error --max-time 5 "$url$path" >"$RESTORE_REPORT_DIR/$output_path"
      printf 'ok: restore API %s report=%s\n' "$path" "$RESTORE_REPORT_DIR/$output_path"
    else
      curl --fail --silent --show-error --max-time 5 "$url$path" >/dev/null
      printf 'ok: restore API %s\n' "$path"
    fi
  done
  if [[ -n "${CSD_POOL_OPERATOR_TOKEN:-}" ]]; then
    if [[ -n "$RESTORE_REPORT_DIR" ]]; then
      curl --fail --silent --show-error --max-time 5 \
        -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
        "$url/api/operator/payouts/status" >"$RESTORE_REPORT_DIR/restore-http-operator-payout-status.json"
      printf 'ok: restore API operator payout status report=%s\n' "$RESTORE_REPORT_DIR/restore-http-operator-payout-status.json"
    else
      curl --fail --silent --show-error --max-time 5 \
        -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
        "$url/api/operator/payouts/status" >/dev/null
      printf 'ok: restore API operator payout status\n'
    fi
  else
    printf 'skip: restore operator API check disabled; set CSD_POOL_OPERATOR_TOKEN\n'
  fi
}

require "CSD_POOL_BACKUP_PATH or path argument" "$BACKUP_PATH"
require "CSD_POOL_RESTORE_DATABASE_URL" "$RESTORE_DATABASE_URL"

if [[ ! -f "$BACKUP_PATH" ]]; then
  printf 'backup file does not exist: %s\n' "$BACKUP_PATH" >&2
  exit 2
fi

printf 'CSD Pool restore drill\n'
printf 'backup=%s\n' "$BACKUP_PATH"
printf 'restore_database_url=%s\n' "$(redact_url "$RESTORE_DATABASE_URL")"

if [[ "$CONFIRM" != "restore-drill" ]]; then
  printf 'dry-run mode: set CSD_POOL_RESTORE_DRILL_CONFIRM=restore-drill to execute\n'
fi

run_or_print env \
  "CSD_POOL_DATABASE_URL=$RESTORE_DATABASE_URL" \
  "CSD_POOL_RESTORE_CONFIRM=restore" \
  "$WORKERS_BIN" restore-db "$BACKUP_PATH"

run_or_print env \
  "CSD_POOL_DATABASE_URL=$RESTORE_DATABASE_URL" \
  "$WORKERS_BIN" migrate

if [[ "$CONFIRM" == "restore-drill" ]]; then
  check_http "$RESTORE_API_URL"
  printf 'restore drill complete\n'
else
  printf 'restore drill plan complete\n'
fi

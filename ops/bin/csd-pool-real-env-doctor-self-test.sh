#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
DOCTOR_SCRIPT="${CSD_POOL_REAL_ENV_DOCTOR_SELF_TEST_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-real-env-doctor.sh}"
OUTPUT_DIR="${CSD_POOL_REAL_ENV_DOCTOR_SELF_TEST_DIR:-}"
KEEP_TMP="${CSD_POOL_REAL_ENV_DOCTOR_SELF_TEST_KEEP_DIR:-0}"
OWN_TMP_DIR=0

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
}

require_failed_check() {
  local summary_path="$1"
  local key="$2"
  python3 - "$summary_path" "$key" <<'PY'
import json
import sys

summary_path, key = sys.argv[1:3]
with open(summary_path, "r", encoding="utf-8") as handle:
    summary = json.load(handle)
matches = [check for check in summary.get("checks", []) if check.get("key") == key]
if len(matches) != 1 or matches[0].get("passed") is not False:
    raise SystemExit(f"expected exactly one failed check for {key}")
PY
}

cleanup() {
  if [[ "$KEEP_TMP" != "1" && "$OWN_TMP_DIR" == "1" && -n "${OUTPUT_DIR:-}" && -d "$OUTPUT_DIR" ]]; then
    rm -rf "$OUTPUT_DIR"
  fi
}
trap cleanup EXIT

[[ -x "$DOCTOR_SCRIPT" ]] || fail "real environment doctor not executable: $DOCTOR_SCRIPT"

if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-real-env-doctor-self-test.XXXXXX")"
  OWN_TMP_DIR=1
else
  mkdir -p "$OUTPUT_DIR"
  OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
fi

printf 'CSD Pool real environment doctor self-test\n'
printf 'output_dir=%s\n' "$OUTPUT_DIR"

GOOD_ENV="$OUTPUT_DIR/good.env"
GOOD_CONFIG="$OUTPUT_DIR/good-config.toml"
GOOD_OUT="$OUTPUT_DIR/good"
GOOD_KEY="$OUTPUT_DIR/good-wallet.key"
printf '%s\n' '0101010101010101010101010101010101010101010101010101010101010101' >"$GOOD_KEY"
chmod 0600 "$GOOD_KEY"
cat >"$GOOD_ENV" <<ENV
CSD_POOL_DATABASE_URL=postgres://csd_pool:ProdDbPassword0123456789@10.1.0.10:5432/csd_pool
CSD_POOL_RESTORE_DATABASE_URL=postgres://csd_pool:RestoreDbPassword0123456789@10.1.0.10:5432/csd_pool_restore
CSD_POOL_TEMPLATE_MODE=live
CSD_POOL_SUBMIT_CANDIDATES=true
CSD_POOL_NODE_TOKEN=00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff
CSD_POOL_OPERATOR_TOKEN=0123456789abcdef0123456789abcdef
CSD_POOL_SIGNER_URL=http://10.1.0.20:8890
CSD_POOL_SIGNER_TOKEN=abcdef0123456789abcdef0123456789
CSD_POOL_SIGNER_WALLET_ADDRESS=abcdef0123456789abcdef0123456789abcdef01
CSD_POOL_SIGNER_PRIVATE_KEY_FILE=$GOOD_KEY
CSD_POOL_SIGNER_NODE_URL=http://10.1.0.30:8789
CSD_POOL_WATCH_NODE_URL=https://1.1.1.1:8790
CSD_POOL_SUBMIT_NODE_URL=https://8.8.8.8:8790
CSD_POOL_PAYOUT_NODE_URL=http://10.1.0.30:8789
CSD_POOL_PUBLIC_API_URL=https://1.0.0.1
CSD_POOL_PUBLIC_STRATUM_ADDR=8.8.4.4:3333
ENV
chmod 0640 "$GOOD_ENV"
cat >"$GOOD_CONFIG" <<'TOML'
[pool]
mining_address = "123456789abcdef0123456789abcdef012345678"
minimum_payout_csd = "1.0"
manual_payout_approval_csd = "250.0"
max_payout_batch_csd = "1000.0"
max_daily_payout_csd = "5000.0"

[stratum]
listen = "127.0.0.1:3333"

[api]
listen = "127.0.0.1:8080"

[signer]
listen = "127.0.0.1:8890"
TOML

CSD_POOL_DOCTOR_OUTPUT_DIR="$GOOD_OUT" \
CSD_POOL_GO_LIVE_TARGET=production \
  "$DOCTOR_SCRIPT" "$GOOD_ENV" "$GOOD_CONFIG" >"$OUTPUT_DIR/good-doctor.log" 2>&1 \
  || { cat "$OUTPUT_DIR/good-doctor.log" >&2; fail "doctor rejected production-shaped inputs"; }

if grep -Fq '"status": "ready_for_real_go_live"' "$GOOD_OUT/real-environment-doctor-summary.json"; then
  printf 'ok: doctor accepts production-shaped inputs\n'
else
  cat "$GOOD_OUT/real-environment-doctor-summary.json" >&2
  fail "doctor positive summary is not ready_for_real_go_live"
fi

BAD_ENV="$OUTPUT_DIR/bad.env"
BAD_CONFIG="$OUTPUT_DIR/bad-config.toml"
BAD_OUT="$OUTPUT_DIR/bad"
cat >"$BAD_ENV" <<'ENV'
CSD_POOL_DATABASE_URL=postgres://csd_pool:secret-password@127.0.0.1:5432/csd_pool
CSD_POOL_RESTORE_DATABASE_URL=postgres://csd_pool:secret-password@127.0.0.1:5432/csd_pool
CSD_POOL_NODE_TOKEN=short-node-token
CSD_POOL_OPERATOR_TOKEN=short-token
CSD_POOL_SIGNER_URL=http://127.0.0.1:8890
CSD_POOL_SIGNER_TOKEN=dev-secret
CSD_POOL_SIGNER_WALLET_ADDRESS=pool.example
CSD_POOL_WATCH_NODE_URL=http://127.0.0.1:8790
CSD_POOL_SUBMIT_NODE_URL=http://127.0.0.1:8790
CSD_POOL_PUBLIC_API_URL=http://pool.example.com
CSD_POOL_PUBLIC_STRATUM_ADDR=127.0.0.1:3333
ENV
chmod 0644 "$BAD_ENV"
cat >"$BAD_CONFIG" <<'TOML'
[pool]
name = "pool.example"

[stratum]
listen = "0.0.0.0:3333"

[api]
listen = "0.0.0.0:8080"

[signer]
listen = "0.0.0.0:8890"
TOML

if CSD_POOL_DOCTOR_OUTPUT_DIR="$BAD_OUT" \
  CSD_POOL_GO_LIVE_TARGET=production \
    "$DOCTOR_SCRIPT" "$BAD_ENV" "$BAD_CONFIG" >"$OUTPUT_DIR/bad-doctor.log" 2>&1; then
  cat "$OUTPUT_DIR/bad-doctor.log" >&2
  fail "doctor accepted loopback, placeholder, and weak-secret inputs"
fi

if grep -Fq '"status": "needs_real_inputs"' "$BAD_OUT/real-environment-doctor-summary.json"; then
  printf 'ok: doctor rejects non-real inputs\n'
else
  cat "$BAD_OUT/real-environment-doctor-summary.json" >&2
  fail "doctor negative summary did not report needs_real_inputs"
fi

for key in \
  env_file_not_world_readable \
  env_has_no_placeholder_values \
  config_has_no_placeholder_values \
  operator_token_production_length \
  signer_token_production_length \
  node_token_production_length \
  database_url_has_password \
  restore_database_url_has_password \
  config_stratum_listen_loopback \
  config_api_listen_loopback \
  config_signer_listen_loopback \
  template_mode_live \
  submit_candidates_enabled \
  config_mining_address_valid \
  config_mining_address_not_example \
  config_payout_limits_valid \
  restore_database_is_separate \
  public_api_is_https \
  public_api_dns_global \
  public_stratum_addr_global \
  watch_node_url_real_non_loopback \
  submit_node_url_real_non_loopback \
  payout_node_url_internal_not_public \
  signer_node_url_internal_not_public \
  signer_private_key_file_exists \
  signer_private_key_permissions_restricted \
  signer_private_key_shape_valid \
  signer_node_runtime_supported \
  signer_wallet_address_shape; do
  if grep -Fq "\"key\": \"$key\"" "$BAD_OUT/real-environment-doctor-summary.json"; then
    printf 'ok: negative summary includes %s\n' "$key"
  else
    cat "$BAD_OUT/real-environment-doctor-summary.json" >&2
    fail "negative summary missing expected check: $key"
  fi
done

PUBLIC_SIGNER_ENV="$OUTPUT_DIR/public-signer.env"
PUBLIC_SIGNER_CONFIG="$OUTPUT_DIR/public-signer-config.toml"
PUBLIC_SIGNER_OUT="$OUTPUT_DIR/public-signer"
cat >"$PUBLIC_SIGNER_ENV" <<ENV
CSD_POOL_DATABASE_URL=postgres://csd_pool:ProdDbPassword0123456789@10.1.0.10:5432/csd_pool
CSD_POOL_RESTORE_DATABASE_URL=postgres://csd_pool:RestoreDbPassword0123456789@10.1.0.10:5432/csd_pool_restore
CSD_POOL_TEMPLATE_MODE=live
CSD_POOL_SUBMIT_CANDIDATES=true
CSD_POOL_NODE_TOKEN=00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff
CSD_POOL_OPERATOR_TOKEN=0123456789abcdef0123456789abcdef
CSD_POOL_SIGNER_URL=https://8.8.8.8:8890
CSD_POOL_SIGNER_TOKEN=abcdef0123456789abcdef0123456789
CSD_POOL_SIGNER_WALLET_ADDRESS=abcdef0123456789abcdef0123456789abcdef01
CSD_POOL_SIGNER_PRIVATE_KEY_FILE=$GOOD_KEY
CSD_POOL_SIGNER_NODE_URL=http://10.1.0.30:8789
CSD_POOL_WATCH_NODE_URL=https://1.1.1.1:8790
CSD_POOL_SUBMIT_NODE_URL=https://8.8.4.4:8790
CSD_POOL_PAYOUT_NODE_URL=http://10.1.0.30:8789
CSD_POOL_PUBLIC_API_URL=https://1.0.0.1
CSD_POOL_PUBLIC_STRATUM_ADDR=8.8.4.4:3333
ENV
chmod 0640 "$PUBLIC_SIGNER_ENV"
cat >"$PUBLIC_SIGNER_CONFIG" <<'TOML'
[pool]
mining_address = "123456789abcdef0123456789abcdef012345678"
minimum_payout_csd = "1.0"
manual_payout_approval_csd = "250.0"
max_payout_batch_csd = "1000.0"
max_daily_payout_csd = "5000.0"

[stratum]
listen = "127.0.0.1:3333"

[api]
listen = "127.0.0.1:8080"

[signer]
listen = "127.0.0.1:8890"
TOML

if CSD_POOL_DOCTOR_OUTPUT_DIR="$PUBLIC_SIGNER_OUT" \
  CSD_POOL_GO_LIVE_TARGET=production \
    "$DOCTOR_SCRIPT" "$PUBLIC_SIGNER_ENV" "$PUBLIC_SIGNER_CONFIG" >"$OUTPUT_DIR/public-signer-doctor.log" 2>&1; then
  cat "$OUTPUT_DIR/public-signer-doctor.log" >&2
  fail "doctor accepted public signer URL"
fi

UNSAFE_CONFIG="$OUTPUT_DIR/unsafe-config.toml"
UNSAFE_CONFIG_OUT="$OUTPUT_DIR/unsafe-config"
cat >"$UNSAFE_CONFIG" <<'TOML'
[pool]
mining_address = "0123456789abcdef0123456789abcdef01234567"
minimum_payout_csd = "1.0"
manual_payout_approval_csd = "1000.0"
max_payout_batch_csd = "250.0"
max_daily_payout_csd = "5000.0"

[stratum]
listen = "127.0.0.1:3333"

[api]
listen = "127.0.0.1:8080"

[signer]
listen = "127.0.0.1:8890"
TOML
if CSD_POOL_DOCTOR_OUTPUT_DIR="$UNSAFE_CONFIG_OUT" \
  CSD_POOL_GO_LIVE_TARGET=production \
    "$DOCTOR_SCRIPT" "$GOOD_ENV" "$UNSAFE_CONFIG" >"$OUTPUT_DIR/unsafe-config-doctor.log" 2>&1; then
  cat "$OUTPUT_DIR/unsafe-config-doctor.log" >&2
  fail "doctor accepted example mining address and unordered payout limits"
fi
for key in config_mining_address_not_example config_payout_limits_valid; do
  if ! require_failed_check "$UNSAFE_CONFIG_OUT/real-environment-doctor-summary.json" "$key"; then
    cat "$UNSAFE_CONFIG_OUT/real-environment-doctor-summary.json" >&2
    fail "unsafe config summary did not fail expected check: $key"
  fi
done
printf 'ok: doctor rejects example mining address and unordered payout limits\n'
if require_failed_check "$PUBLIC_SIGNER_OUT/real-environment-doctor-summary.json" signer_url_internal_not_public; then
  printf 'ok: doctor rejects public signer URL\n'
else
  cat "$PUBLIC_SIGNER_OUT/real-environment-doctor-summary.json" >&2
  fail "public signer summary missing signer_url_internal_not_public check"
fi

if grep -R "secret-password" "$BAD_OUT" "$OUTPUT_DIR/bad-doctor.log" >/dev/null 2>&1; then
  grep -R "secret-password" "$BAD_OUT" "$OUTPUT_DIR/bad-doctor.log" >&2 || true
  fail "doctor leaked a database password in reports"
else
  printf 'ok: doctor reports redact database passwords\n'
fi

printf 'good_summary=%s\n' "$GOOD_OUT/real-environment-doctor-summary.json"
printf 'bad_summary=%s\n' "$BAD_OUT/real-environment-doctor-summary.json"
printf 'public_signer_summary=%s\n' "$PUBLIC_SIGNER_OUT/real-environment-doctor-summary.json"
printf 'unsafe_config_summary=%s\n' "$UNSAFE_CONFIG_OUT/real-environment-doctor-summary.json"
printf 'summary: real environment doctor self-test passed\n'

#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
RELEASE_ARCHIVE="${1:-${CSD_POOL_FINAL_RELEASE_ARCHIVE:-}}"
RECEIPT_ARCHIVE="${2:-${CSD_POOL_FINAL_REAL_GO_LIVE_RECEIPT:-}}"
ACCEPTANCE_ARCHIVE="${3:-${CSD_POOL_FINAL_PUBLIC_ACCEPTANCE_EVIDENCE:-}}"
OUTPUT_DIR="${CSD_POOL_FINAL_OUTPUT_DIR:-}"
TIMESTAMP="${CSD_POOL_FINAL_TIMESTAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
ALLOW_NON_LAUNCHABLE="${CSD_POOL_FINAL_ALLOW_NON_LAUNCHABLE:-0}"
REQUIRE_PUBLIC_ACCEPTED_SHARE="${CSD_POOL_FINAL_REQUIRE_PUBLIC_ACCEPTED_SHARE:-1}"

VERIFY_HANDOFF_SCRIPT="${CSD_POOL_VERIFY_LAUNCH_HANDOFF_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-launch-handoff.sh}"
EXPORT_HANDOFF_SCRIPT="${CSD_POOL_EXPORT_LAUNCH_HANDOFF_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-export-launch-handoff.sh}"
VERIFY_HANDOFF_PACKAGE_SCRIPT="${CSD_POOL_VERIFY_LAUNCH_HANDOFF_PACKAGE_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-launch-handoff-package.sh}"
EXPORT_DOSSIER_SCRIPT="${CSD_POOL_EXPORT_LAUNCH_DOSSIER_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-export-launch-dossier.sh}"
VERIFY_DOSSIER_SCRIPT="${CSD_POOL_VERIFY_LAUNCH_DOSSIER_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-launch-dossier.sh}"

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
}

usage() {
  printf 'usage: %s /path/to/release.tar.gz /path/to/real-go-live-receipt.tar.gz /path/to/public-acceptance-evidence.tar.gz\n' "$(basename "$0")" >&2
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1"
  else
    fail "sha256 tool missing"
  fi
}

sha256_value() {
  sha256_file "$1" | awk '{print $1}'
}

json_escape() {
  sed 's/\\/\\\\/g; s/"/\\"/g'
}

json_string() {
  printf '"%s"' "$(printf '%s' "$1" | json_escape)"
}

require_executable() {
  local path="$1"
  [[ -x "$path" ]] || fail "required script is not executable: $path"
}

run_step() {
  local label="$1"
  local log_path="$2"
  shift 2
  printf 'running: %s\n' "$label"
  if "$@" >"$log_path" 2>&1; then
    printf 'ok: %s\n' "$label"
  else
    cat "$log_path" >&2
    fail "$label failed; see $log_path"
  fi
}

if [[ -z "$RELEASE_ARCHIVE" || -z "$RECEIPT_ARCHIVE" || -z "$ACCEPTANCE_ARCHIVE" ]]; then
  usage
  exit 2
fi

[[ -f "$RELEASE_ARCHIVE" ]] || fail "release archive not found: $RELEASE_ARCHIVE"
[[ -f "$RECEIPT_ARCHIVE" ]] || fail "real go-live receipt not found: $RECEIPT_ARCHIVE"
[[ -f "$ACCEPTANCE_ARCHIVE" ]] || fail "public acceptance evidence not found: $ACCEPTANCE_ARCHIVE"
require_executable "$VERIFY_HANDOFF_SCRIPT"
require_executable "$EXPORT_HANDOFF_SCRIPT"
require_executable "$VERIFY_HANDOFF_PACKAGE_SCRIPT"
require_executable "$EXPORT_DOSSIER_SCRIPT"
require_executable "$VERIFY_DOSSIER_SCRIPT"

if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$(dirname "$ACCEPTANCE_ARCHIVE")/final-launch-$TIMESTAMP"
fi
mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

printf 'CSD Pool final launch\n'
printf 'output_dir=%s\n' "$OUTPUT_DIR"
printf 'allow_non_launchable=%s\n' "$ALLOW_NON_LAUNCHABLE"
printf 'require_public_accepted_share=%s\n' "$REQUIRE_PUBLIC_ACCEPTED_SHARE"

run_step "verify launch handoff inputs" "$OUTPUT_DIR/01-verify-launch-handoff.log" \
  "$VERIFY_HANDOFF_SCRIPT" "$RELEASE_ARCHIVE" "$RECEIPT_ARCHIVE" "$ACCEPTANCE_ARCHIVE"

run_step "export launch handoff package" "$OUTPUT_DIR/02-export-launch-handoff.log" \
  env CSD_POOL_HANDOFF_OUTPUT_DIR="$OUTPUT_DIR" CSD_POOL_HANDOFF_TIMESTAMP="$TIMESTAMP" \
  "$EXPORT_HANDOFF_SCRIPT" "$RELEASE_ARCHIVE" "$RECEIPT_ARCHIVE" "$ACCEPTANCE_ARCHIVE"

HANDOFF_PACKAGE="$(awk -F= '/^launch_handoff_package=/{print $2}' "$OUTPUT_DIR/02-export-launch-handoff.log")"
[[ -n "$HANDOFF_PACKAGE" && -f "$HANDOFF_PACKAGE" ]] || fail "launch handoff package path missing from export log"

run_step "verify launch handoff package" "$OUTPUT_DIR/03-verify-launch-handoff-package.log" \
  "$VERIFY_HANDOFF_PACKAGE_SCRIPT" "$HANDOFF_PACKAGE"

DOSSIER_ENV=(
  CSD_POOL_DOSSIER_OUTPUT_DIR="$OUTPUT_DIR"
  CSD_POOL_DOSSIER_TIMESTAMP="$TIMESTAMP"
  CSD_POOL_DOSSIER_REQUIRE_PUBLIC_ACCEPTED_SHARE="$REQUIRE_PUBLIC_ACCEPTED_SHARE"
)
if [[ "$ALLOW_NON_LAUNCHABLE" == "1" ]]; then
  DOSSIER_ENV+=(CSD_POOL_DOSSIER_ALLOW_NON_LAUNCHABLE=1)
fi
run_step "export launch dossier" "$OUTPUT_DIR/04-export-launch-dossier.log" \
  env "${DOSSIER_ENV[@]}" "$EXPORT_DOSSIER_SCRIPT" "$HANDOFF_PACKAGE"

DOSSIER_PACKAGE="$(awk -F= '/^launch_dossier_package=/{print $2}' "$OUTPUT_DIR/04-export-launch-dossier.log")"
READINESS_STATUS="$(awk -F= '/^readiness_status=/{print $2}' "$OUTPUT_DIR/04-export-launch-dossier.log")"
[[ -n "$DOSSIER_PACKAGE" && -f "$DOSSIER_PACKAGE" ]] || fail "launch dossier package path missing from export log"
[[ -n "$READINESS_STATUS" ]] || fail "readiness status missing from dossier export log"
DOSSIER_DIR="$OUTPUT_DIR/$(basename "$DOSSIER_PACKAGE" .tar.gz)"
READINESS_SUMMARY="$DOSSIER_DIR/readiness/launch-readiness-summary.json"
[[ -f "$READINESS_SUMMARY" ]] || fail "launch readiness summary missing from dossier staging dir: $READINESS_SUMMARY"

if [[ "$ALLOW_NON_LAUNCHABLE" == "1" ]]; then
  run_step "verify launch dossier package" "$OUTPUT_DIR/05-verify-launch-dossier.log" \
    env CSD_POOL_DOSSIER_ALLOW_NON_LAUNCHABLE=1 "$VERIFY_DOSSIER_SCRIPT" "$DOSSIER_PACKAGE"
else
  run_step "verify launch dossier package" "$OUTPUT_DIR/05-verify-launch-dossier.log" \
    "$VERIFY_DOSSIER_SCRIPT" "$DOSSIER_PACKAGE"
fi

FINAL_STATUS="launch_ready"
if [[ "$READINESS_STATUS" != "launch_ready" ]]; then
  FINAL_STATUS="needs_real_environment_evidence"
fi

READINESS_FRAGMENT="$(python3 - "$READINESS_SUMMARY" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    summary = json.load(handle)
checks = {
    item.get("key"): item.get("passed")
    for item in summary.get("checks") or []
    if isinstance(item, dict) and item.get("key")
}
payload = {
    "status": summary.get("status"),
    "hard_failures": summary.get("hard_failures"),
    "public_accepted_share_required": summary.get("public_accepted_share_required"),
    "public_accepted_share_observed": summary.get("public_accepted_share_observed"),
    "public_accepted_share_minimum": summary.get("public_accepted_share_minimum"),
    "public_canary_accepted_share_minimum": summary.get("public_canary_accepted_share_minimum"),
    "public_canary_shares_accepted": summary.get("public_canary_shares_accepted"),
    "public_canary_miner_recently_seen": checks.get("public_canary_miner_recently_seen"),
    "public_canary_source_configured_when_required": checks.get("public_canary_source_configured_when_required"),
    "public_status_release_identity_matches_receipt": checks.get("public_status_release_identity_matches_receipt"),
    "public_acceptance_toolchain_manifest_verified": checks.get("public_acceptance_toolchain_manifest_verified"),
}
print(json.dumps(payload, sort_keys=True))
PY
)"

cat >"$OUTPUT_DIR/final-launch-summary.json" <<JSON
{
  "status": $(json_string "$FINAL_STATUS"),
  "target": "final-launch",
  "timestamp_utc": $(json_string "$TIMESTAMP"),
  "allow_non_launchable": $(if [[ "$ALLOW_NON_LAUNCHABLE" == "1" ]]; then printf 'true'; else printf 'false'; fi),
  "require_public_accepted_share": $(if [[ "$REQUIRE_PUBLIC_ACCEPTED_SHARE" == "1" ]]; then printf 'true'; else printf 'false'; fi),
  "release_archive": $(json_string "$RELEASE_ARCHIVE"),
  "release_archive_sha256": $(json_string "$(sha256_value "$RELEASE_ARCHIVE")"),
  "real_go_live_receipt": $(json_string "$RECEIPT_ARCHIVE"),
  "real_go_live_receipt_sha256": $(json_string "$(sha256_value "$RECEIPT_ARCHIVE")"),
  "public_acceptance_evidence": $(json_string "$ACCEPTANCE_ARCHIVE"),
  "public_acceptance_evidence_sha256": $(json_string "$(sha256_value "$ACCEPTANCE_ARCHIVE")"),
  "launch_handoff_package": $(json_string "$HANDOFF_PACKAGE"),
  "launch_handoff_package_sha256": $(json_string "$(sha256_value "$HANDOFF_PACKAGE")"),
  "launch_dossier_package": $(json_string "$DOSSIER_PACKAGE"),
  "launch_dossier_package_sha256": $(json_string "$(sha256_value "$DOSSIER_PACKAGE")"),
  "readiness_status": $(json_string "$READINESS_STATUS"),
  "readiness": $READINESS_FRAGMENT
}
JSON

{
  printf 'CSD Pool Final Launch\n'
  printf 'status=%s\n' "$FINAL_STATUS"
  printf 'readiness_status=%s\n' "$READINESS_STATUS"
  printf 'readiness_summary=%s\n' "$READINESS_SUMMARY"
  printf 'readiness_hard_failures=%s\n' "$(python3 - "$READINESS_SUMMARY" <<'PY'
import json
import sys
with open(sys.argv[1], "r", encoding="utf-8") as handle:
    print(json.load(handle).get("hard_failures", "unknown"))
PY
)"
  printf 'allow_non_launchable=%s\n' "$ALLOW_NON_LAUNCHABLE"
  printf 'require_public_accepted_share=%s\n' "$REQUIRE_PUBLIC_ACCEPTED_SHARE"
  printf 'release_archive=%s\n' "$RELEASE_ARCHIVE"
  printf 'real_go_live_receipt=%s\n' "$RECEIPT_ARCHIVE"
  printf 'public_acceptance_evidence=%s\n' "$ACCEPTANCE_ARCHIVE"
  printf 'launch_handoff_package=%s\n' "$HANDOFF_PACKAGE"
  printf 'launch_handoff_package_sha256=%s\n' "$(sha256_value "$HANDOFF_PACKAGE")"
  printf 'launch_dossier_package=%s\n' "$DOSSIER_PACKAGE"
  printf 'launch_dossier_package_sha256=%s\n' "$(sha256_value "$DOSSIER_PACKAGE")"
  printf 'summary_json=%s\n' "$OUTPUT_DIR/final-launch-summary.json"
} >"$OUTPUT_DIR/FINAL-LAUNCH-REPORT.txt"

printf 'final_launch_report=%s\n' "$OUTPUT_DIR/FINAL-LAUNCH-REPORT.txt"
printf 'final_launch_summary=%s\n' "$OUTPUT_DIR/final-launch-summary.json"
printf 'launch_handoff_package=%s\n' "$HANDOFF_PACKAGE"
printf 'launch_dossier_package=%s\n' "$DOSSIER_PACKAGE"
printf 'readiness_status=%s\n' "$READINESS_STATUS"
printf 'status=%s\n' "$FINAL_STATUS"
printf 'summary: final launch package set exported\n'

if [[ "$FINAL_STATUS" != "launch_ready" && "$ALLOW_NON_LAUNCHABLE" != "1" ]]; then
  exit 1
fi

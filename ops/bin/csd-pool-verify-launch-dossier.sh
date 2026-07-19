#!/usr/bin/env bash
set -euo pipefail

DOSSIER_ARCHIVE="${1:-${CSD_POOL_DOSSIER_PACKAGE:-}}"
DOSSIER_SHA256="${2:-${CSD_POOL_DOSSIER_PACKAGE_SHA256:-}}"
ALLOW_NON_LAUNCHABLE="${CSD_POOL_DOSSIER_ALLOW_NON_LAUNCHABLE:-0}"
TMP_ROOT="${CSD_POOL_DOSSIER_TMP_DIR:-}"
KEEP_TMP="${CSD_POOL_DOSSIER_KEEP_DIR:-0}"
OWN_TMP_DIR=0
PASS=0
FAIL=0

ok() {
  PASS=$((PASS + 1))
  printf 'ok: %s\n' "$1"
}

fail_check() {
  FAIL=$((FAIL + 1))
  printf 'fail: %s\n' "$1" >&2
}

fatal() {
  fail_check "$1"
  printf 'summary: pass=%s fail=%s\n' "$PASS" "$FAIL" >&2
  exit 1
}

usage() {
  printf 'usage: %s /path/to/csd-pool-*-launch-dossier-*.tar.gz [/path/to/.sha256]\n' "$(basename "$0")" >&2
}

sha256_value() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
  else
    fatal "sha256 tool missing"
  fi
}

json_query() {
  local path="$1"
  local expr="$2"
  python3 - "$path" "$expr" <<'PY'
import json
import sys

path, expr = sys.argv[1:3]
with open(path, "r", encoding="utf-8") as handle:
    value = json.load(handle)
for part in expr.split("."):
    if isinstance(value, dict) and part in value:
        value = value[part]
    else:
        sys.exit(1)
if isinstance(value, bool):
    print("true" if value else "false")
else:
    print(value)
PY
}

manifest_value() {
  local key="$1"
  awk -F= -v key="$key" '$1 == key {sub($1 FS, ""); print; exit}' "$MANIFEST"
}

require_file() {
  local path="$1"
  local label="$2"
  [[ -f "$path" ]] && ok "$label exists" || fatal "$label missing"
}

require_text() {
  local path="$1"
  local pattern="$2"
  local label="$3"
  grep -Fq "$pattern" "$path" && ok "$label" || fatal "$label missing"
}

validate_json_file() {
  local path="$1"
  local label="$2"
  if python3 -m json.tool "$path" >/dev/null 2>&1; then
    ok "$label JSON valid"
  else
    fatal "$label JSON invalid"
  fi
}

check_required_readiness_checks() {
  local summary="$1"
  if python3 - "$summary" <<'PY'
import json
import sys

summary_path = sys.argv[1]
required = [
    "handoff_package_verified",
    "handoff_summary_passed",
    "real_go_live_summary_passed",
    "real_go_live_non_dry_run",
    "real_go_live_target_launch_mode",
    "real_go_live_zero_skips",
    "real_go_live_public_api_https",
    "real_go_live_public_stratum_present",
    "real_go_live_no_fixture_markers",
    "real_go_live_inputs_verified",
    "real_go_live_postcheck_verified",
    "public_acceptance_summary_passed",
    "public_acceptance_public_api_https",
    "public_acceptance_endpoint_matches_receipt",
    "public_acceptance_no_fixture_markers",
    "public_acceptance_endpoint_routability_global",
    "public_status_release_identity_matches_receipt",
    "public_acceptance_toolchain_manifest_verified",
    "public_stratum_submit_response_standard",
    "public_stratum_accepted_share_observed",
    "public_canary_accepted_share_minimum_matches_summary",
    "public_canary_accepted_share_minimum_met",
    "public_canary_miner_recently_seen",
    "public_canary_source_configured_when_required",
    "public_canary_miner_visible",
]
with open(summary_path, "r", encoding="utf-8") as handle:
    summary = json.load(handle)
checks = {
    item.get("key"): item
    for item in summary.get("checks") or []
    if isinstance(item, dict) and item.get("key")
}
missing = []
failed = []
wrong_severity = []
for key in required:
    item = checks.get(key)
    if item is None:
        missing.append(key)
        continue
    if item.get("passed") is not True:
        failed.append(key)
    if item.get("severity") != "hard":
        wrong_severity.append(key)
if missing:
    print("missing_required_readiness_checks=" + ",".join(missing))
if failed:
    print("failed_required_readiness_checks=" + ",".join(failed))
if wrong_severity:
    print("non_hard_required_readiness_checks=" + ",".join(wrong_severity))
if missing or failed or wrong_severity:
    sys.exit(1)
print("required_readiness_checks_present=True")
print("required_readiness_checks_passed=True")
PY
  then
    ok "required readiness checks present and passed"
  else
    fatal "required readiness checks missing or failed"
  fi
}

cleanup() {
  if [[ "$KEEP_TMP" != "1" && "$OWN_TMP_DIR" == "1" && -n "${TMP_DIR:-}" && -d "$TMP_DIR" ]]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

if [[ -z "$DOSSIER_ARCHIVE" ]]; then
  usage
  exit 2
fi

[[ -f "$DOSSIER_ARCHIVE" ]] || fatal "launch dossier package not found: $DOSSIER_ARCHIVE"
ok "launch dossier package exists"

if [[ -z "$DOSSIER_SHA256" && -f "$DOSSIER_ARCHIVE.sha256" ]]; then
  DOSSIER_SHA256="$DOSSIER_ARCHIVE.sha256"
fi

if [[ -n "$DOSSIER_SHA256" ]]; then
  [[ -f "$DOSSIER_SHA256" ]] || fatal "launch dossier package .sha256 not found: $DOSSIER_SHA256"
  dossier_hash="$(sha256_value "$DOSSIER_ARCHIVE")"
  dossier_sha_line="$(sed -n '1p' "$DOSSIER_SHA256" 2>/dev/null || true)"
  if [[ "$dossier_sha_line" == "$dossier_hash "* && "$dossier_sha_line" == *"$(basename "$DOSSIER_ARCHIVE")"* ]]; then
    ok "launch dossier package sha256 verified"
  else
    fatal "launch dossier package sha256 mismatch"
  fi
fi

if tar -tzf "$DOSSIER_ARCHIVE" >/dev/null; then
  ok "launch dossier package can be listed"
else
  fatal "launch dossier package cannot be listed"
fi

if ! tar -tzf "$DOSSIER_ARCHIVE" | grep -E '(^/|(^|/)\.\.($|/))' >/dev/null; then
  ok "launch dossier package paths are relative and safe"
else
  fatal "launch dossier package contains unsafe paths"
fi

if [[ -z "$TMP_ROOT" ]]; then
  TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-dossier.XXXXXX")"
  OWN_TMP_DIR=1
else
  mkdir -p "$TMP_ROOT"
  TMP_DIR="$(mktemp -d "$TMP_ROOT/csd-pool-dossier.XXXXXX")"
  OWN_TMP_DIR=1
fi

tar -xzf "$DOSSIER_ARCHIVE" -C "$TMP_DIR"
ok "launch dossier package extracted"

top_count="$(find "$TMP_DIR" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')"
[[ "$top_count" == "1" ]] || fatal "launch dossier package must contain exactly one top-level directory"
DOSSIER_DIR="$(find "$TMP_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
MANIFEST="$DOSSIER_DIR/DOSSIER-MANIFEST.txt"

require_file "$MANIFEST" "DOSSIER-MANIFEST.txt"
require_file "$DOSSIER_DIR/DOSSIER-README.txt" "DOSSIER-README.txt"
require_file "$DOSSIER_DIR/DOSSIER-SHA256SUMS" "DOSSIER-SHA256SUMS"
require_file "$DOSSIER_DIR/launch-dossier-summary.json" "launch-dossier-summary.json"
require_file "$DOSSIER_DIR/launch-readiness.audit.log" "launch-readiness.audit.log"
require_file "$DOSSIER_DIR/readiness/LAUNCH-READINESS-REPORT.txt" "LAUNCH-READINESS-REPORT.txt"
require_file "$DOSSIER_DIR/readiness/launch-readiness-summary.json" "launch-readiness-summary.json"
require_file "$DOSSIER_DIR/readiness/launch-readiness-package-verify.log" "launch-readiness-package-verify.log"
require_text "$MANIFEST" "included_files:" "dossier manifest lists included files"
require_text "$MANIFEST" "verify_launch_dossier=ops/bin/csd-pool-verify-launch-dossier.sh" "dossier manifest records verifier"
require_text "$DOSSIER_DIR/DOSSIER-README.txt" "CSD Pool Launch Dossier" "dossier README title present"

(
  cd "$DOSSIER_DIR"
  if shasum -a 256 -c DOSSIER-SHA256SUMS >"$TMP_DIR/dossier-sha256.log" 2>&1; then
    :
  else
    cat "$TMP_DIR/dossier-sha256.log" >&2
    exit 1
  fi
) && ok "dossier internal SHA256SUMS verified" || fatal "dossier internal SHA256SUMS failed"

handoff_name="$(manifest_value handoff_package)"
handoff_sha="$(manifest_value handoff_package_sha256)"
[[ -n "$handoff_name" && -n "$handoff_sha" ]] || fatal "handoff metadata missing from dossier manifest"
require_file "$DOSSIER_DIR/$handoff_name" "embedded handoff package"
require_file "$DOSSIER_DIR/$handoff_name.sha256" "embedded handoff package .sha256"
[[ "$(sha256_value "$DOSSIER_DIR/$handoff_name")" == "$handoff_sha" ]] && ok "embedded handoff sha256 matches manifest" || fatal "embedded handoff sha256 mismatch"

validate_json_file "$DOSSIER_DIR/launch-dossier-summary.json" "launch dossier summary"
validate_json_file "$DOSSIER_DIR/readiness/launch-readiness-summary.json" "launch readiness summary"
dossier_status="$(json_query "$DOSSIER_DIR/launch-dossier-summary.json" status)"
dossier_target="$(json_query "$DOSSIER_DIR/launch-dossier-summary.json" target)"
summary_handoff_name="$(json_query "$DOSSIER_DIR/launch-dossier-summary.json" handoff_package)"
summary_handoff_sha="$(json_query "$DOSSIER_DIR/launch-dossier-summary.json" handoff_package_sha256)"
summary_require_public_accepted_share="$(json_query "$DOSSIER_DIR/launch-dossier-summary.json" require_public_accepted_share)"
summary_readiness_report="$(json_query "$DOSSIER_DIR/launch-dossier-summary.json" readiness_report)"
summary_readiness_summary="$(json_query "$DOSSIER_DIR/launch-dossier-summary.json" readiness_summary)"
summary_readiness_audit_log="$(json_query "$DOSSIER_DIR/launch-dossier-summary.json" readiness_audit_log)"
readiness_handoff_package="$(json_query "$DOSSIER_DIR/readiness/launch-readiness-summary.json" handoff_package)"
readiness_handoff_sha="$(json_query "$DOSSIER_DIR/readiness/launch-readiness-summary.json" handoff_package_sha256)"
manifest_require_public_accepted_share="$(manifest_value require_public_accepted_share)"
if [[ "$manifest_require_public_accepted_share" == "1" ]]; then
  manifest_require_public_accepted_share_bool="true"
else
  manifest_require_public_accepted_share_bool="false"
fi
readiness_status="$(json_query "$DOSSIER_DIR/readiness/launch-readiness-summary.json" status)"
hard_failures="$(json_query "$DOSSIER_DIR/readiness/launch-readiness-summary.json" hard_failures)"
[[ "$dossier_target" == "launch-dossier" ]] && ok "dossier summary target launch-dossier" || fatal "dossier summary target mismatch"
[[ "$dossier_status" == "$readiness_status" ]] && ok "dossier status matches readiness status" || fatal "dossier status mismatch"
[[ "$summary_handoff_name" == "$handoff_name" ]] && ok "dossier summary handoff package matches manifest" || fatal "dossier summary handoff package mismatch"
[[ "$summary_handoff_sha" == "$handoff_sha" ]] && ok "dossier summary handoff sha256 matches manifest" || fatal "dossier summary handoff sha256 mismatch"
[[ "$(basename "$readiness_handoff_package")" == "$handoff_name" ]] && ok "readiness summary handoff package matches manifest" || fatal "readiness summary handoff package mismatch"
[[ "$readiness_handoff_sha" == "$handoff_sha" ]] && ok "readiness summary handoff sha256 matches manifest" || fatal "readiness summary handoff sha256 mismatch"
[[ "$summary_require_public_accepted_share" == "$manifest_require_public_accepted_share_bool" ]] && ok "dossier summary accepted-share requirement matches manifest" || fatal "dossier summary accepted-share requirement mismatch"
[[ "$summary_readiness_report" == "$(manifest_value readiness_report)" ]] && ok "dossier summary readiness report path matches manifest" || fatal "dossier summary readiness report path mismatch"
[[ "$summary_readiness_summary" == "$(manifest_value readiness_summary)" ]] && ok "dossier summary readiness summary path matches manifest" || fatal "dossier summary readiness summary path mismatch"
[[ "$summary_readiness_audit_log" == "$(manifest_value readiness_audit_log)" ]] && ok "dossier summary readiness audit log path matches manifest" || fatal "dossier summary readiness audit log path mismatch"

if [[ "$ALLOW_NON_LAUNCHABLE" == "1" ]]; then
  ok "non-launchable dossier allowed by CSD_POOL_DOSSIER_ALLOW_NON_LAUNCHABLE=1"
else
  [[ "$readiness_status" == "launch_ready" ]] && ok "readiness status launch_ready" || fatal "readiness status is not launch_ready"
  [[ "$hard_failures" == "0" ]] && ok "readiness hard failures zero" || fatal "readiness hard failures are not zero"
  check_required_readiness_checks "$DOSSIER_DIR/readiness/launch-readiness-summary.json"
fi

require_text "$DOSSIER_DIR/readiness/LAUNCH-READINESS-REPORT.txt" "status=$readiness_status" "readiness report status matches summary"
require_text "$DOSSIER_DIR/launch-readiness.audit.log" "readiness_summary=" "readiness audit output recorded"

printf 'extracted_dir=%s\n' "$DOSSIER_DIR"
printf 'summary: pass=%s fail=%s\n' "$PASS" "$FAIL"
if [[ "$FAIL" -ne 0 ]]; then
  exit 1
fi

#!/usr/bin/env bash
set -euo pipefail

ARCHIVE="${1:-${CSD_POOL_FINAL_REVIEW_PACKAGE:-}}"
SHA_FILE="${2:-${CSD_POOL_FINAL_REVIEW_PACKAGE_SHA256:-}}"
KEEP_TMP="${CSD_POOL_FINAL_REVIEW_KEEP_DIR:-0}"
ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
VERIFY_HANDOFF_PACKAGE_SCRIPT="${CSD_POOL_VERIFY_LAUNCH_HANDOFF_PACKAGE_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-launch-handoff-package.sh}"
VERIFY_DOSSIER_SCRIPT="${CSD_POOL_VERIFY_LAUNCH_DOSSIER_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-launch-dossier.sh}"
OWN_TMP_DIR=0
PASS=0
FAIL=0

ok() {
  PASS=$((PASS + 1))
  printf 'ok: %s\n' "$1"
}

fatal() {
  FAIL=$((FAIL + 1))
  printf 'fail: %s\n' "$1" >&2
  printf 'summary: pass=%s fail=%s\n' "$PASS" "$FAIL" >&2
  exit 1
}

usage() {
  printf 'usage: %s /path/to/csd-pool-final-review-*.tar.gz [/path/to/.sha256]\n' "$(basename "$0")" >&2
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1"
  else
    fatal "sha256 tool missing"
  fi
}

sha256_value() {
  sha256_file "$1" | awk '{print $1}'
}

json_value() {
  local path="$1"
  local key="$2"
  python3 - "$path" "$key" <<'PY'
import json
import sys
path, key = sys.argv[1:3]
with open(path, "r", encoding="utf-8") as handle:
    value = json.load(handle)
for part in key.split("."):
    if isinstance(value, dict):
        value = value.get(part)
    else:
        value = None
if isinstance(value, bool):
    print("true" if value else "false")
elif value is not None:
    print(value)
PY
}

manifest_value() {
  local key="$1"
  awk -F= -v key="$key" '$1 == key {sub($1 FS, ""); print; exit}' "$MANIFEST"
}

key_value() {
  local path="$1"
  local key="$2"
  awk -F= -v key="$key" '$1 == key {sub($1 FS, ""); print; exit}' "$path"
}

require_file() {
  local path="$1"
  local label="$2"
  [[ -f "$path" ]] && ok "$label exists" || fatal "$label missing"
}

require_executable() {
  local path="$1"
  local label="$2"
  [[ -x "$path" ]] && ok "$label executable" || fatal "$label not executable: $path"
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

check_final_review_redaction_safety() {
  local review_dir="$1"
  local log_path="$TMP_DIR/final-review-redaction-safety.log"
  if python3 - "$review_dir" >"$log_path" 2>&1 <<'PY'
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
skip_suffixes = {".gz", ".tgz", ".zip", ".xz", ".bz2", ".zst"}
patterns = [
    ("authorization_bearer", re.compile(r"Authorization:\s*Bearer\s+(?!<redacted>|redacted\b)[A-Za-z0-9._~+/=-]{8,}", re.I)),
    ("secret_env_assignment", re.compile(r"\b(CSD_POOL_[A-Z0-9_]*(?:TOKEN|SECRET|PASSWORD|KEY)[A-Z0-9_]*)=(?!<redacted>|redacted\b)[^\s]+", re.I)),
    ("postgres_password_url", re.compile(r"postgres(?:ql)?://[^:/@\s]+:(?!<redacted>@|redacted@)[^@\s]+@", re.I)),
    ("url_basic_auth_password", re.compile(r"\b[a-z][a-z0-9+.-]*://[^:/@\s]+:(?!<redacted>@|redacted@)[^@\s]+@", re.I)),
]
findings = []
checked = 0
for path in sorted(p for p in root.rglob("*") if p.is_file()):
    if any(str(path).endswith(suffix) for suffix in skip_suffixes):
        continue
    try:
        data = path.read_bytes()
    except OSError as exc:
        findings.append((str(path.relative_to(root)), "read_error", str(exc)))
        continue
    text = data.decode("utf-8", errors="ignore")
    checked += 1
    for name, pattern in patterns:
        match = pattern.search(text)
        if match:
            findings.append((str(path.relative_to(root)), name, match.group(0)[:160]))
if findings:
    for rel, name, sample in findings[:50]:
        print(f"finding={name} file={rel} sample={sample}")
print(f"final_review_redaction_checked_files={checked}")
print(f"final_review_redaction_findings={len(findings)}")
if findings:
    sys.exit(1)
PY
  then
    ok "final review redaction scan passed"
  else
    cat "$log_path" >&2
    fatal "final review redaction scan failed"
  fi
}

cleanup() {
  if [[ "$KEEP_TMP" != "1" && "$OWN_TMP_DIR" == "1" && -n "${TMP_DIR:-}" && -d "$TMP_DIR" ]]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

if [[ -z "$ARCHIVE" ]]; then
  usage
  exit 2
fi
[[ -f "$ARCHIVE" ]] || fatal "final review package not found: $ARCHIVE"
ok "final review package exists"

if [[ -z "$SHA_FILE" && -f "$ARCHIVE.sha256" ]]; then
  SHA_FILE="$ARCHIVE.sha256"
fi
if [[ -n "$SHA_FILE" ]]; then
  [[ -f "$SHA_FILE" ]] || fatal "final review package .sha256 not found: $SHA_FILE"
  hash="$(sha256_value "$ARCHIVE")"
  line="$(sed -n '1p' "$SHA_FILE" 2>/dev/null || true)"
  [[ "$line" == "$hash "* && "$line" == *"$(basename "$ARCHIVE")"* ]] && ok "final review package sha256 verified" || fatal "final review package sha256 mismatch"
fi

tar -tzf "$ARCHIVE" >/dev/null && ok "final review package can be listed" || fatal "final review package cannot be listed"
if tar -tzf "$ARCHIVE" | grep -E '(^/|(^|/)\.\.($|/))' >/dev/null; then
  fatal "final review package contains unsafe paths"
else
  ok "final review package paths are relative and safe"
fi

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-final-review.XXXXXX")"
OWN_TMP_DIR=1
tar -xzf "$ARCHIVE" -C "$TMP_DIR"
ok "final review package extracted"
top_count="$(find "$TMP_DIR" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')"
[[ "$top_count" == "1" ]] || fatal "final review package must contain exactly one top-level directory"
REVIEW_DIR="$(find "$TMP_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
MANIFEST="$REVIEW_DIR/FINAL-REVIEW-MANIFEST.txt"

require_file "$MANIFEST" "FINAL-REVIEW-MANIFEST.txt"
require_file "$REVIEW_DIR/FINAL-REVIEW-README.txt" "FINAL-REVIEW-README.txt"
require_file "$REVIEW_DIR/FINAL-REVIEW-SHA256SUMS" "FINAL-REVIEW-SHA256SUMS"
require_file "$REVIEW_DIR/final/final-launch-summary.json" "final-launch-summary.json"
require_file "$REVIEW_DIR/final/FINAL-LAUNCH-REPORT.txt" "FINAL-LAUNCH-REPORT.txt"
validate_json_file "$REVIEW_DIR/final/final-launch-summary.json" "final-launch-summary"
check_final_review_redaction_safety "$REVIEW_DIR"

(
  cd "$REVIEW_DIR"
  shasum -a 256 -c FINAL-REVIEW-SHA256SUMS >/tmp/csd-pool-final-review-sha.log 2>&1
) && ok "final review internal SHA256SUMS verified" || { cat /tmp/csd-pool-final-review-sha.log >&2; fatal "final review internal SHA256SUMS failed"; }

manifest_status="$(manifest_value status)"
summary_status="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" status)"
[[ -n "$manifest_status" && "$manifest_status" == "$summary_status" ]] && ok "manifest status matches final summary" || fatal "manifest status mismatch"

handoff_path="$(manifest_value handoff_package)"
dossier_path="$(manifest_value dossier_package)"
handoff_sha="$(manifest_value handoff_package_sha256)"
dossier_sha="$(manifest_value dossier_package_sha256)"
summary_handoff_sha="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" launch_handoff_package_sha256)"
summary_dossier_sha="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" launch_dossier_package_sha256)"
summary_handoff_package="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" launch_handoff_package)"
summary_dossier_package="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" launch_dossier_package)"
summary_release_archive="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" release_archive)"
summary_receipt_archive="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" real_go_live_receipt)"
summary_acceptance_archive="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" public_acceptance_evidence)"
summary_release_sha="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" release_archive_sha256)"
summary_receipt_sha="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" real_go_live_receipt_sha256)"
summary_acceptance_sha="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" public_acceptance_evidence_sha256)"
summary_require_public_accepted_share="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" require_public_accepted_share)"
summary_readiness_status="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" readiness.status)"
summary_readiness_hard_failures="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" readiness.hard_failures)"
summary_readiness_required="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" readiness.public_accepted_share_required)"
summary_readiness_observed="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" readiness.public_accepted_share_observed)"
summary_readiness_minimum="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" readiness.public_accepted_share_minimum)"
summary_readiness_canary_minimum="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" readiness.public_canary_accepted_share_minimum)"
summary_readiness_canary_shares="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" readiness.public_canary_shares_accepted)"
summary_readiness_canary_recent="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" readiness.public_canary_miner_recently_seen)"
summary_readiness_canary_source="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" readiness.public_canary_source_configured_when_required)"
summary_readiness_public_status_release="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" readiness.public_status_release_identity_matches_receipt)"
summary_readiness_acceptance_toolchain="$(json_value "$REVIEW_DIR/final/final-launch-summary.json" readiness.public_acceptance_toolchain_manifest_verified)"
require_file "$REVIEW_DIR/$handoff_path" "embedded handoff package"
require_file "$REVIEW_DIR/$dossier_path" "embedded dossier package"
[[ "$(sha256_value "$REVIEW_DIR/$handoff_path")" == "$handoff_sha" ]] && ok "embedded handoff sha256 matches manifest" || fatal "embedded handoff sha256 mismatch"
[[ "$(sha256_value "$REVIEW_DIR/$dossier_path")" == "$dossier_sha" ]] && ok "embedded dossier sha256 matches manifest" || fatal "embedded dossier sha256 mismatch"
[[ -n "$summary_handoff_sha" && "$summary_handoff_sha" == "$handoff_sha" ]] && ok "final summary handoff sha256 matches manifest" || fatal "final summary handoff sha256 mismatch"
[[ -n "$summary_dossier_sha" && "$summary_dossier_sha" == "$dossier_sha" ]] && ok "final summary dossier sha256 matches manifest" || fatal "final summary dossier sha256 mismatch"
[[ -n "$summary_handoff_package" && "$(basename "$summary_handoff_package")" == "$(basename "$handoff_path")" ]] && ok "final summary handoff package path matches manifest" || fatal "final summary handoff package path mismatch"
[[ -n "$summary_dossier_package" && "$(basename "$summary_dossier_package")" == "$(basename "$dossier_path")" ]] && ok "final summary dossier package path matches manifest" || fatal "final summary dossier package path mismatch"
[[ -n "$summary_release_archive" && "$summary_release_archive" == "$(manifest_value release_archive)" ]] && ok "final summary release archive path matches manifest" || fatal "final summary release archive path mismatch"
[[ -n "$summary_receipt_archive" && "$summary_receipt_archive" == "$(manifest_value real_go_live_receipt)" ]] && ok "final summary receipt archive path matches manifest" || fatal "final summary receipt archive path mismatch"
[[ -n "$summary_acceptance_archive" && "$summary_acceptance_archive" == "$(manifest_value public_acceptance_evidence)" ]] && ok "final summary public acceptance archive path matches manifest" || fatal "final summary public acceptance archive path mismatch"
[[ -n "$summary_release_sha" && "$summary_release_sha" == "$(manifest_value release_archive_sha256)" ]] && ok "final summary release sha256 matches manifest" || fatal "final summary release sha256 mismatch"
[[ -n "$summary_receipt_sha" && "$summary_receipt_sha" == "$(manifest_value real_go_live_receipt_sha256)" ]] && ok "final summary receipt sha256 matches manifest" || fatal "final summary receipt sha256 mismatch"
[[ -n "$summary_acceptance_sha" && "$summary_acceptance_sha" == "$(manifest_value public_acceptance_evidence_sha256)" ]] && ok "final summary public acceptance sha256 matches manifest" || fatal "final summary public acceptance sha256 mismatch"

HANDOFF_EXTRACT_DIR="$TMP_DIR/final-review-handoff-package"
mkdir -p "$HANDOFF_EXTRACT_DIR"
tar -xzf "$REVIEW_DIR/$handoff_path" -C "$HANDOFF_EXTRACT_DIR"
HANDOFF_MANIFEST="$(find "$HANDOFF_EXTRACT_DIR" -type f -name HANDOFF-MANIFEST.txt | head -n1)"
require_file "$HANDOFF_MANIFEST" "embedded handoff manifest"
HANDOFF_DIR="$(cd "$(dirname "$HANDOFF_MANIFEST")" && pwd)"
handoff_release_name="$(key_value "$HANDOFF_MANIFEST" release_archive)"
handoff_receipt_name="$(key_value "$HANDOFF_MANIFEST" real_go_live_receipt)"
handoff_acceptance_name="$(key_value "$HANDOFF_MANIFEST" public_acceptance_evidence)"
handoff_release_sha="$(key_value "$HANDOFF_MANIFEST" release_archive_sha256)"
handoff_receipt_sha="$(key_value "$HANDOFF_MANIFEST" real_go_live_receipt_sha256)"
handoff_acceptance_sha="$(key_value "$HANDOFF_MANIFEST" public_acceptance_evidence_sha256)"
HANDOFF_RECEIPT_ARCHIVE="$HANDOFF_DIR/$handoff_receipt_name"
HANDOFF_ACCEPTANCE_ARCHIVE="$HANDOFF_DIR/$handoff_acceptance_name"
require_file "$HANDOFF_RECEIPT_ARCHIVE" "embedded handoff receipt archive"
require_file "$HANDOFF_ACCEPTANCE_ARCHIVE" "embedded handoff public acceptance archive"
if tar -tzf "$HANDOFF_RECEIPT_ARCHIVE" >/dev/null; then
  ok "embedded handoff receipt archive can be listed"
else
  fatal "embedded handoff receipt archive cannot be listed"
fi
if tar -tzf "$HANDOFF_RECEIPT_ARCHIVE" | grep -E '(^/|(^|/)\.\.($|/))' >/dev/null; then
  fatal "embedded handoff receipt archive contains unsafe paths"
else
  ok "embedded handoff receipt archive paths are relative and safe"
fi
RECEIPT_EXTRACT_DIR="$TMP_DIR/final-review-receipt"
mkdir -p "$RECEIPT_EXTRACT_DIR"
tar -xzf "$HANDOFF_RECEIPT_ARCHIVE" -C "$RECEIPT_EXTRACT_DIR"
RECEIPT_GO_LIVE_SUMMARY="$(find "$RECEIPT_EXTRACT_DIR" -type f -name go-live-summary.json | head -n1)"
RECEIPT_DOCTOR_SUMMARY="$(find "$RECEIPT_EXTRACT_DIR" -type f -name real-environment-doctor-summary.json | head -n1)"
ACCEPTANCE_EXTRACT_DIR="$TMP_DIR/final-review-public-acceptance"
mkdir -p "$ACCEPTANCE_EXTRACT_DIR"
tar -xzf "$HANDOFF_ACCEPTANCE_ARCHIVE" -C "$ACCEPTANCE_EXTRACT_DIR"
ACCEPTANCE_SUMMARY="$(find "$ACCEPTANCE_EXTRACT_DIR" -type f -name public-acceptance-summary.json | head -n1)"
ACCEPTANCE_STATUS="$(find "$ACCEPTANCE_EXTRACT_DIR" -type f -name http-public-status.json | head -n1)"
[[ -n "$summary_release_archive" && "$(basename "$summary_release_archive")" == "$handoff_release_name" ]] && ok "final summary release archive matches embedded handoff manifest" || fatal "final summary release archive mismatch"
[[ -n "$summary_receipt_archive" && "$(basename "$summary_receipt_archive")" == "$handoff_receipt_name" ]] && ok "final summary receipt archive matches embedded handoff manifest" || fatal "final summary receipt archive mismatch"
[[ -n "$summary_acceptance_archive" && "$(basename "$summary_acceptance_archive")" == "$handoff_acceptance_name" ]] && ok "final summary public acceptance archive matches embedded handoff manifest" || fatal "final summary public acceptance archive mismatch"
[[ "$summary_release_sha" == "$handoff_release_sha" ]] && ok "final summary release sha256 matches embedded handoff manifest" || fatal "final summary release sha256 does not match embedded handoff manifest"
[[ "$summary_receipt_sha" == "$handoff_receipt_sha" ]] && ok "final summary receipt sha256 matches embedded handoff manifest" || fatal "final summary receipt sha256 does not match embedded handoff manifest"
[[ "$summary_acceptance_sha" == "$handoff_acceptance_sha" ]] && ok "final summary public acceptance sha256 matches embedded handoff manifest" || fatal "final summary public acceptance sha256 does not match embedded handoff manifest"
require_file "$RECEIPT_GO_LIVE_SUMMARY" "embedded receipt go-live summary"
require_file "$ACCEPTANCE_SUMMARY" "embedded public acceptance summary"
require_file "$ACCEPTANCE_STATUS" "embedded public acceptance status response"
validate_json_file "$ACCEPTANCE_SUMMARY" "embedded public acceptance summary"
validate_json_file "$ACCEPTANCE_STATUS" "embedded public acceptance status response"

if python3 - "$RECEIPT_GO_LIVE_SUMMARY" "$ACCEPTANCE_SUMMARY" "$ACCEPTANCE_STATUS" >"$TMP_DIR/final-review-public-status-release-binding.log" 2>&1 <<'PY'
import json
import sys

receipt_path, acceptance_path, status_path = sys.argv[1:4]
with open(receipt_path, "r", encoding="utf-8") as handle:
    receipt = json.load(handle)
with open(acceptance_path, "r", encoding="utf-8") as handle:
    acceptance = json.load(handle)
with open(status_path, "r", encoding="utf-8") as handle:
    status = json.load(handle)

receipt_release = receipt.get("release") or {}
acceptance_release = acceptance.get("public_status_release") or {}
status_release = status.get("release") or {}

def release_time(payload):
    return payload.get("built_at") or payload.get("timestamp_utc")

checks = {
    "public_acceptance_release_name_matches_status": bool(acceptance_release.get("name")) and acceptance_release.get("name") == status_release.get("name"),
    "public_acceptance_release_revision_matches_status": bool(acceptance_release.get("revision")) and acceptance_release.get("revision") == status_release.get("revision"),
    "public_acceptance_release_time_matches_status": bool(release_time(acceptance_release)) and release_time(acceptance_release) == release_time(status_release),
    "public_acceptance_release_version_matches_status": bool(acceptance_release.get("version")) and acceptance_release.get("version") == status_release.get("version"),
    "public_status_release_name_matches_receipt": bool(receipt_release.get("name")) and status_release.get("name") == receipt_release.get("name"),
    "public_status_release_revision_matches_receipt": bool(receipt_release.get("revision")) and status_release.get("revision") == receipt_release.get("revision"),
    "public_status_release_time_matches_receipt": bool(release_time(receipt_release)) and release_time(status_release) == release_time(receipt_release),
}
for key, value in checks.items():
    print(f"{key}={value}")
if not all(checks.values()):
    raise SystemExit(1)
PY
then
  ok "final review public status release identity matches receipt and acceptance summary"
else
  cat "$TMP_DIR/final-review-public-status-release-binding.log" >&2
  fatal "final review public status release identity mismatch"
fi

require_executable "$VERIFY_HANDOFF_PACKAGE_SCRIPT" "launch handoff package verifier"
if "$VERIFY_HANDOFF_PACKAGE_SCRIPT" "$REVIEW_DIR/$handoff_path" >"$TMP_DIR/final-review-handoff-verify.log" 2>&1; then
  ok "embedded handoff package verifier passed"
else
  cat "$TMP_DIR/final-review-handoff-verify.log" >&2
  fatal "embedded handoff package verifier failed"
fi

if [[ "$summary_status" == "launch_ready" ]]; then
  [[ "$summary_require_public_accepted_share" == "true" ]] && ok "final summary requires public accepted share for launch_ready review" || fatal "launch_ready final review requires public accepted share"
  require_executable "$VERIFY_DOSSIER_SCRIPT" "launch dossier verifier"
  if "$VERIFY_DOSSIER_SCRIPT" "$REVIEW_DIR/$dossier_path" >"$TMP_DIR/final-review-dossier-verify.log" 2>&1; then
    ok "embedded launch dossier verifier passed"
  else
    cat "$TMP_DIR/final-review-dossier-verify.log" >&2
    fatal "embedded launch dossier verifier failed"
  fi
else
  require_executable "$VERIFY_DOSSIER_SCRIPT" "launch dossier verifier"
  if CSD_POOL_DOSSIER_ALLOW_NON_LAUNCHABLE=1 "$VERIFY_DOSSIER_SCRIPT" "$REVIEW_DIR/$dossier_path" >"$TMP_DIR/final-review-dossier-verify.log" 2>&1; then
    ok "embedded non-launch-ready dossier verifier passed with explicit allowance"
  else
    cat "$TMP_DIR/final-review-dossier-verify.log" >&2
    fatal "embedded non-launch-ready dossier verifier failed"
  fi
fi

DOSSIER_EXTRACT_DIR="$TMP_DIR/final-review-dossier"
mkdir -p "$DOSSIER_EXTRACT_DIR"
tar -xzf "$REVIEW_DIR/$dossier_path" -C "$DOSSIER_EXTRACT_DIR"
DOSSIER_READINESS_SUMMARY="$(find "$DOSSIER_EXTRACT_DIR" -type f -path '*/readiness/launch-readiness-summary.json' | head -n1)"
require_file "$DOSSIER_READINESS_SUMMARY" "embedded dossier readiness summary"
validate_json_file "$DOSSIER_READINESS_SUMMARY" "embedded dossier readiness summary"
if python3 - "$REVIEW_DIR/final/final-launch-summary.json" "$DOSSIER_READINESS_SUMMARY" >"$TMP_DIR/final-review-readiness-binding.log" 2>&1 <<'PY'
import json
import sys

final_path, readiness_path = sys.argv[1:3]
with open(final_path, "r", encoding="utf-8") as handle:
    final = json.load(handle)
with open(readiness_path, "r", encoding="utf-8") as handle:
    readiness = json.load(handle)
checks = {
    item.get("key"): item.get("passed")
    for item in readiness.get("checks") or []
    if isinstance(item, dict) and item.get("key")
}
expected = {
    "status": readiness.get("status"),
    "hard_failures": readiness.get("hard_failures"),
    "public_accepted_share_required": readiness.get("public_accepted_share_required"),
    "public_accepted_share_observed": readiness.get("public_accepted_share_observed"),
    "public_accepted_share_minimum": readiness.get("public_accepted_share_minimum"),
    "public_canary_accepted_share_minimum": readiness.get("public_canary_accepted_share_minimum"),
    "public_canary_shares_accepted": readiness.get("public_canary_shares_accepted"),
    "public_canary_miner_recently_seen": checks.get("public_canary_miner_recently_seen"),
    "public_canary_source_configured_when_required": checks.get("public_canary_source_configured_when_required"),
    "public_status_release_identity_matches_receipt": checks.get("public_status_release_identity_matches_receipt"),
    "public_acceptance_toolchain_manifest_verified": checks.get("public_acceptance_toolchain_manifest_verified"),
}
actual = final.get("readiness") if isinstance(final.get("readiness"), dict) else {}
ok = True
for key, expected_value in expected.items():
    actual_value = actual.get(key)
    passed = actual_value == expected_value
    print(f"readiness_{key}_matches={passed}")
    if not passed:
        ok = False
if not ok:
    raise SystemExit(1)
PY
then
  ok "final summary readiness fields match embedded dossier readiness summary"
else
  cat "$TMP_DIR/final-review-readiness-binding.log" >&2
  fatal "final summary readiness fields mismatch embedded dossier readiness summary"
fi

doctor_included="$(manifest_value doctor_included)"
gaps_included="$(manifest_value gaps_included)"
if [[ "$doctor_included" == "true" ]]; then
  require_file "$REVIEW_DIR/doctor/REAL-ENVIRONMENT-DOCTOR.txt" "REAL-ENVIRONMENT-DOCTOR.txt"
  require_file "$REVIEW_DIR/doctor/real-environment-doctor-summary.json" "real-environment-doctor-summary.json"
  validate_json_file "$REVIEW_DIR/doctor/real-environment-doctor-summary.json" "real-environment-doctor-summary"
  doctor_status="$(json_value "$REVIEW_DIR/doctor/real-environment-doctor-summary.json" status)"
  doctor_hard_failures="$(json_value "$REVIEW_DIR/doctor/real-environment-doctor-summary.json" hard_failures)"
  if [[ "$summary_status" == "launch_ready" ]]; then
    [[ "$doctor_status" == "ready_for_real_go_live" ]] && ok "doctor status ready_for_real_go_live for launch_ready review" || fatal "launch_ready final review requires doctor status ready_for_real_go_live"
    [[ "$doctor_hard_failures" == "0" ]] && ok "doctor hard failures zero for launch_ready review" || fatal "launch_ready final review requires doctor hard_failures=0"
  else
    [[ -n "$doctor_status" ]] && ok "doctor status present for non-launch-ready review" || fatal "doctor status missing"
  fi
  require_file "$RECEIPT_DOCTOR_SUMMARY" "embedded receipt doctor summary"
  [[ "$(sha256_value "$REVIEW_DIR/doctor/real-environment-doctor-summary.json")" == "$(sha256_value "$RECEIPT_DOCTOR_SUMMARY")" ]] \
    && ok "final review doctor summary matches embedded receipt doctor summary" \
    || fatal "final review doctor summary mismatch with embedded receipt"
elif [[ "$summary_status" == "launch_ready" ]]; then
  fatal "launch_ready final review requires doctor reports"
fi
if [[ "$summary_status" != "launch_ready" ]]; then
  [[ "$gaps_included" == "true" ]] || fatal "non-launch-ready final review must include gaps"
  require_file "$REVIEW_DIR/gaps/LAUNCH-GAPS-REPORT.txt" "LAUNCH-GAPS-REPORT.txt"
  require_file "$REVIEW_DIR/gaps/launch-gaps-summary.json" "launch-gaps-summary.json"
  validate_json_file "$REVIEW_DIR/gaps/launch-gaps-summary.json" "launch-gaps-summary"
  gaps_status="$(json_value "$REVIEW_DIR/gaps/launch-gaps-summary.json" status)"
  [[ "$gaps_status" == "$summary_status" ]] && ok "gaps status matches final summary" || fatal "gaps status mismatch"
else
  ok "launch_ready final review does not require gaps"
fi

printf 'extracted_dir=%s\n' "$REVIEW_DIR"
printf 'summary: pass=%s fail=%s\n' "$PASS" "$FAIL"

#!/usr/bin/env bash
set -euo pipefail

FINAL_DIR="${1:-${CSD_POOL_FINAL_REVIEW_FINAL_DIR:-}}"
DOCTOR_DIR="${2:-${CSD_POOL_FINAL_REVIEW_DOCTOR_DIR:-}}"
GAPS_DIR="${3:-${CSD_POOL_FINAL_REVIEW_GAPS_DIR:-}}"
OUTPUT_DIR="${CSD_POOL_FINAL_REVIEW_OUTPUT_DIR:-}"
TIMESTAMP="${CSD_POOL_FINAL_REVIEW_TIMESTAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
}

usage() {
  printf 'usage: %s /path/to/final-output-dir [/path/to/doctor-dir] [/path/to/gaps-dir]\n' "$(basename "$0")" >&2
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

copy_artifact() {
  local src="$1"
  local dst="$2"
  local label="$3"
  [[ -f "$src" ]] || fail "$label missing: $src"
  install -m 0644 "$src" "$dst"
}

if [[ -z "$FINAL_DIR" ]]; then
  usage
  exit 2
fi
[[ -d "$FINAL_DIR" ]] || fail "final output directory not found: $FINAL_DIR"
FINAL_DIR="$(cd "$FINAL_DIR" && pwd)"
FINAL_SUMMARY="$FINAL_DIR/final-launch-summary.json"
FINAL_REPORT="$FINAL_DIR/FINAL-LAUNCH-REPORT.txt"
[[ -f "$FINAL_SUMMARY" ]] || fail "final-launch-summary.json missing from $FINAL_DIR"
[[ -f "$FINAL_REPORT" ]] || fail "FINAL-LAUNCH-REPORT.txt missing from $FINAL_DIR"

FINAL_STATUS="$(json_value "$FINAL_SUMMARY" status)"
DOSSIER_PACKAGE="$(json_value "$FINAL_SUMMARY" launch_dossier_package)"
HANDOFF_PACKAGE="$(json_value "$FINAL_SUMMARY" launch_handoff_package)"
RELEASE_ARCHIVE="$(json_value "$FINAL_SUMMARY" release_archive)"
RECEIPT_ARCHIVE="$(json_value "$FINAL_SUMMARY" real_go_live_receipt)"
ACCEPTANCE_ARCHIVE="$(json_value "$FINAL_SUMMARY" public_acceptance_evidence)"
RELEASE_ARCHIVE_SHA="$(json_value "$FINAL_SUMMARY" release_archive_sha256)"
RECEIPT_ARCHIVE_SHA="$(json_value "$FINAL_SUMMARY" real_go_live_receipt_sha256)"
ACCEPTANCE_ARCHIVE_SHA="$(json_value "$FINAL_SUMMARY" public_acceptance_evidence_sha256)"
[[ -n "$FINAL_STATUS" ]] || fail "final summary status missing"
[[ -n "$DOSSIER_PACKAGE" && -f "$DOSSIER_PACKAGE" ]] || fail "launch dossier package missing from final summary"
[[ -n "$HANDOFF_PACKAGE" && -f "$HANDOFF_PACKAGE" ]] || fail "launch handoff package missing from final summary"
[[ -n "$RELEASE_ARCHIVE_SHA" ]] || fail "release archive sha256 missing from final summary"
[[ -n "$RECEIPT_ARCHIVE_SHA" ]] || fail "real go-live receipt sha256 missing from final summary"
[[ -n "$ACCEPTANCE_ARCHIVE_SHA" ]] || fail "public acceptance evidence sha256 missing from final summary"

if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$FINAL_DIR/final-review-$TIMESTAMP"
fi
mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
PACKAGE_NAME="csd-pool-final-review-$TIMESTAMP"
STAGING_DIR="$OUTPUT_DIR/$PACKAGE_NAME"
ARCHIVE_PATH="$OUTPUT_DIR/$PACKAGE_NAME.tar.gz"
rm -rf "$STAGING_DIR" "$ARCHIVE_PATH" "$ARCHIVE_PATH.sha256"
mkdir -p "$STAGING_DIR/final" "$STAGING_DIR/artifacts"

copy_artifact "$FINAL_SUMMARY" "$STAGING_DIR/final/final-launch-summary.json" "final launch summary"
copy_artifact "$FINAL_REPORT" "$STAGING_DIR/final/FINAL-LAUNCH-REPORT.txt" "final launch report"
copy_artifact "$HANDOFF_PACKAGE" "$STAGING_DIR/artifacts/$(basename "$HANDOFF_PACKAGE")" "handoff package"
copy_artifact "$DOSSIER_PACKAGE" "$STAGING_DIR/artifacts/$(basename "$DOSSIER_PACKAGE")" "dossier package"

if [[ -n "$DOCTOR_DIR" ]]; then
  [[ -d "$DOCTOR_DIR" ]] || fail "doctor directory not found: $DOCTOR_DIR"
  mkdir -p "$STAGING_DIR/doctor"
  copy_artifact "$DOCTOR_DIR/REAL-ENVIRONMENT-DOCTOR.txt" "$STAGING_DIR/doctor/REAL-ENVIRONMENT-DOCTOR.txt" "real environment doctor report"
  copy_artifact "$DOCTOR_DIR/real-environment-doctor-summary.json" "$STAGING_DIR/doctor/real-environment-doctor-summary.json" "real environment doctor summary"
fi

if [[ -n "$GAPS_DIR" ]]; then
  [[ -d "$GAPS_DIR" ]] || fail "gaps directory not found: $GAPS_DIR"
  mkdir -p "$STAGING_DIR/gaps"
  copy_artifact "$GAPS_DIR/LAUNCH-GAPS-REPORT.txt" "$STAGING_DIR/gaps/LAUNCH-GAPS-REPORT.txt" "launch gaps report"
  copy_artifact "$GAPS_DIR/launch-gaps-summary.json" "$STAGING_DIR/gaps/launch-gaps-summary.json" "launch gaps summary"
elif [[ "$FINAL_STATUS" != "launch_ready" ]]; then
  fail "non-launch-ready final review requires a gaps directory"
fi

cat >"$STAGING_DIR/FINAL-REVIEW-README.txt" <<README
CSD Pool Final Review

Verify this package with:

  ops/bin/csd-pool-verify-final-review.sh $PACKAGE_NAME.tar.gz

The package contains the final launch report, final machine-readable summary,
handoff package, launch dossier package, optional pre-go-live doctor reports,
and gap reports when the final status is not launch_ready.
README

cat >"$STAGING_DIR/FINAL-REVIEW-MANIFEST.txt" <<MANIFEST
name=$PACKAGE_NAME
timestamp_utc=$TIMESTAMP
status=$FINAL_STATUS
final_summary=final/final-launch-summary.json
final_report=final/FINAL-LAUNCH-REPORT.txt
handoff_package=artifacts/$(basename "$HANDOFF_PACKAGE")
handoff_package_sha256=$(sha256_value "$HANDOFF_PACKAGE")
dossier_package=artifacts/$(basename "$DOSSIER_PACKAGE")
dossier_package_sha256=$(sha256_value "$DOSSIER_PACKAGE")
release_archive=$RELEASE_ARCHIVE
release_archive_sha256=$RELEASE_ARCHIVE_SHA
real_go_live_receipt=$RECEIPT_ARCHIVE
real_go_live_receipt_sha256=$RECEIPT_ARCHIVE_SHA
public_acceptance_evidence=$ACCEPTANCE_ARCHIVE
public_acceptance_evidence_sha256=$ACCEPTANCE_ARCHIVE_SHA
doctor_included=$([[ -n "$DOCTOR_DIR" ]] && printf 'true' || printf 'false')
gaps_included=$([[ -n "$GAPS_DIR" ]] && printf 'true' || printf 'false')
verify_final_review=ops/bin/csd-pool-verify-final-review.sh
MANIFEST

(
  cd "$STAGING_DIR"
  find . -type f ! -name FINAL-REVIEW-SHA256SUMS | sort | while read -r file; do
    sha256_file "$file"
  done >FINAL-REVIEW-SHA256SUMS
)

(
  cd "$OUTPUT_DIR"
  tar -czf "$ARCHIVE_PATH" "$PACKAGE_NAME"
)
sha256_file "$ARCHIVE_PATH" >"$ARCHIVE_PATH.sha256"

printf 'final_review_package=%s\n' "$ARCHIVE_PATH"
printf 'final_review_package_sha256=%s\n' "$ARCHIVE_PATH.sha256"
printf 'status=%s\n' "$FINAL_STATUS"
printf 'summary: final review package exported\n'

#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
SUMMARY_PATH="${1:-${CSD_POOL_REAL_GO_LIVE_SUMMARY:-}}"
OUTPUT_DIR="${2:-${CSD_POOL_REAL_GO_LIVE_RECEIPT_DIR:-}}"
VERIFY_REAL_SCRIPT="${CSD_POOL_VERIFY_REAL_GO_LIVE_SUMMARY_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-real-go-live-summary.sh}"
TIMESTAMP="${CSD_POOL_REAL_GO_LIVE_RECEIPT_TIMESTAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
RECEIPT_NAME="${CSD_POOL_REAL_GO_LIVE_RECEIPT_NAME:-}"

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
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

summary_value() {
  local key="$1"
  awk -F= -v key="$key" '$1 == key {sub($1 FS, ""); print; exit}' "$SUMMARY_PATH"
}

copy_required() {
  local src="$1"
  local dst="$2"
  local label="$3"
  [[ -n "$src" ]] || fail "$label path missing from REAL-GO-LIVE-SUMMARY.txt"
  [[ -f "$src" ]] || fail "$label not found: $src"
  install -m 0644 "$src" "$dst"
}

usage() {
  printf 'usage: %s /path/to/REAL-GO-LIVE-SUMMARY.txt [output-dir]\n' "$(basename "$0")" >&2
}

if [[ -z "$SUMMARY_PATH" ]]; then
  usage
  exit 2
fi

[[ -f "$SUMMARY_PATH" ]] || fail "REAL-GO-LIVE-SUMMARY.txt not found: $SUMMARY_PATH"
[[ -x "$VERIFY_REAL_SCRIPT" ]] || fail "real go-live summary verifier not executable: $VERIFY_REAL_SCRIPT"

SUMMARY_PATH="$(cd "$(dirname "$SUMMARY_PATH")" && pwd)/$(basename "$SUMMARY_PATH")"
if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$(dirname "$SUMMARY_PATH")"
fi
mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

target="$(summary_value target)"
if [[ -z "$RECEIPT_NAME" ]]; then
  RECEIPT_NAME="csd-pool-${target:-target}-real-go-live-receipt-${TIMESTAMP}"
fi

STAGING_DIR="$OUTPUT_DIR/$RECEIPT_NAME"
RECEIPT_ARCHIVE="$OUTPUT_DIR/$RECEIPT_NAME.tar.gz"
RECEIPT_SHA256="$RECEIPT_ARCHIVE.sha256"
VERIFY_LOG="$OUTPUT_DIR/$RECEIPT_NAME.verify-real-go-live-summary.log"

if ! "$VERIFY_REAL_SCRIPT" "$SUMMARY_PATH" >"$VERIFY_LOG" 2>&1; then
  fail "real go-live summary verification failed before receipt export; see $VERIFY_LOG"
fi

inputs_path="$(summary_value real_go_live_inputs)"
toolchain_path="$(summary_value launch_toolchain_manifest)"
doctor_report_path="$(summary_value real_environment_doctor_report)"
doctor_summary_path="$(summary_value real_environment_doctor_summary)"
report_path="$(summary_value go_live_report)"
go_live_summary_path="$(summary_value go_live_summary)"
signoff_path="$(summary_value go_live_signoff)"
postcheck_path="$(summary_value real_go_live_postcheck)"
evidence_archive="$(summary_value evidence_archive)"
evidence_sha256_path="$(summary_value evidence_sha256)"

rm -rf "$STAGING_DIR" "$RECEIPT_ARCHIVE" "$RECEIPT_SHA256"
install -d -m 0755 "$STAGING_DIR"

copy_required "$SUMMARY_PATH" "$STAGING_DIR/REAL-GO-LIVE-SUMMARY.txt" "real go-live summary"
copy_required "$inputs_path" "$STAGING_DIR/real-go-live-inputs.log" "real go-live inputs"
copy_required "$toolchain_path" "$STAGING_DIR/launch-toolchain-manifest.json" "launch toolchain manifest"
copy_required "$doctor_report_path" "$STAGING_DIR/REAL-ENVIRONMENT-DOCTOR.txt" "real environment doctor report"
copy_required "$doctor_summary_path" "$STAGING_DIR/real-environment-doctor-summary.json" "real environment doctor summary"
copy_required "$report_path" "$STAGING_DIR/GO-LIVE-REPORT.txt" "go-live report"
copy_required "$go_live_summary_path" "$STAGING_DIR/go-live-summary.json" "go-live summary"
copy_required "$signoff_path" "$STAGING_DIR/GO-LIVE-SIGNOFF.md" "go-live signoff"
copy_required "$postcheck_path" "$STAGING_DIR/real-go-live-postcheck.log" "real go-live postcheck"
copy_required "$evidence_archive" "$STAGING_DIR/$(basename "$evidence_archive")" "go-live evidence archive"
copy_required "$evidence_sha256_path" "$STAGING_DIR/$(basename "$evidence_sha256_path")" "go-live evidence sha256"

{
  printf 'name=%s\n' "$RECEIPT_NAME"
  printf 'created_at_utc=%s\n' "$TIMESTAMP"
  printf 'target=%s\n' "${target:-missing}"
  printf 'source_summary=%s\n' "$SUMMARY_PATH"
  printf 'verify_real_go_live_summary=%s\n' "$VERIFY_REAL_SCRIPT"
  printf 'included_files:\n'
  (
    cd "$STAGING_DIR"
    find . -type f ! -name RECEIPT-SHA256SUMS -print | sort | while read -r file; do
      printf '  %s\n' "${file#./}"
    done
  )
} >"$STAGING_DIR/RECEIPT-MANIFEST.txt"

(
  cd "$STAGING_DIR"
  find . -type f ! -name RECEIPT-SHA256SUMS -print | sort | while read -r file; do
    sha256_file "$file"
  done >RECEIPT-SHA256SUMS
)

(
  cd "$OUTPUT_DIR"
  tar -czf "$RECEIPT_ARCHIVE" "$RECEIPT_NAME"
)
sha256_file "$RECEIPT_ARCHIVE" >"$RECEIPT_SHA256"

printf 'real_go_live_receipt=%s\n' "$RECEIPT_ARCHIVE"
printf 'real_go_live_receipt_sha256=%s\n' "$(sha256_value "$RECEIPT_ARCHIVE")"
printf 'summary: real go-live receipt exported\n'

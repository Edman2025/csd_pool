#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
VERIFY_HANDOFF_SCRIPT="${CSD_POOL_VERIFY_LAUNCH_HANDOFF_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-launch-handoff.sh}"
RELEASE_ARCHIVE="${1:-${CSD_POOL_HANDOFF_RELEASE_ARCHIVE:-}}"
RECEIPT_ARCHIVE="${2:-${CSD_POOL_HANDOFF_REAL_GO_LIVE_RECEIPT:-}}"
ACCEPTANCE_ARCHIVE="${3:-${CSD_POOL_HANDOFF_PUBLIC_ACCEPTANCE_EVIDENCE:-}}"
OUTPUT_DIR="${CSD_POOL_HANDOFF_OUTPUT_DIR:-}"
TIMESTAMP="${CSD_POOL_HANDOFF_TIMESTAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
PACKAGE_NAME="${CSD_POOL_HANDOFF_PACKAGE_NAME:-}"

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

copy_artifact_and_sha() {
  local src="$1"
  local dst_dir="$2"
  local dst="$dst_dir/$(basename "$src")"
  cp "$src" "$dst"
  if [[ -f "$src.sha256" ]]; then
    cp "$src.sha256" "$dst.sha256"
  else
    sha256_file "$dst" >"$dst.sha256"
  fi
}

if [[ -z "$RELEASE_ARCHIVE" || -z "$RECEIPT_ARCHIVE" || -z "$ACCEPTANCE_ARCHIVE" ]]; then
  usage
  exit 2
fi

[[ -x "$VERIFY_HANDOFF_SCRIPT" ]] || fail "launch handoff verifier not executable: $VERIFY_HANDOFF_SCRIPT"
[[ -f "$RELEASE_ARCHIVE" ]] || fail "release archive not found: $RELEASE_ARCHIVE"
[[ -f "$RECEIPT_ARCHIVE" ]] || fail "real go-live receipt archive not found: $RECEIPT_ARCHIVE"
[[ -f "$ACCEPTANCE_ARCHIVE" ]] || fail "public acceptance evidence archive not found: $ACCEPTANCE_ARCHIVE"

if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$(dirname "$ACCEPTANCE_ARCHIVE")"
fi
mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

verify_log="$OUTPUT_DIR/launch-handoff.verify.log"
if "$VERIFY_HANDOFF_SCRIPT" "$RELEASE_ARCHIVE" "$RECEIPT_ARCHIVE" "$ACCEPTANCE_ARCHIVE" >"$verify_log" 2>&1; then
  :
else
  cat "$verify_log" >&2
  fail "launch handoff verification failed before export; see $verify_log"
fi

if [[ -z "$PACKAGE_NAME" ]]; then
  release_base="$(basename "$RELEASE_ARCHIVE" .tar.gz)"
  PACKAGE_NAME="${release_base}-launch-handoff-${TIMESTAMP}"
fi

STAGING_DIR="$OUTPUT_DIR/$PACKAGE_NAME"
PACKAGE_ARCHIVE="$OUTPUT_DIR/$PACKAGE_NAME.tar.gz"
rm -rf "$STAGING_DIR" "$PACKAGE_ARCHIVE" "$PACKAGE_ARCHIVE.sha256"
mkdir -p "$STAGING_DIR"

copy_artifact_and_sha "$RELEASE_ARCHIVE" "$STAGING_DIR"
copy_artifact_and_sha "$RECEIPT_ARCHIVE" "$STAGING_DIR"
copy_artifact_and_sha "$ACCEPTANCE_ARCHIVE" "$STAGING_DIR"
cp "$verify_log" "$STAGING_DIR/launch-handoff.verify.log"

cat >"$STAGING_DIR/HANDOFF-README.txt" <<README
CSD Pool Launch Handoff

This package is the portable final delivery artifact for a CSD Pool launch.
It contains the release archive, the real go-live receipt, the public
acceptance evidence archive, each archive's .sha256 file, the export-time
handoff verification log, HANDOFF-MANIFEST.txt, HANDOFF-SHA256SUMS, and a
machine-readable handoff-summary.json.

Verification:

  ops/bin/csd-pool-verify-launch-handoff-package.sh $(basename "$PACKAGE_ARCHIVE")

The verifier checks this package's checksum, internal HANDOFF-SHA256SUMS,
artifact hashes recorded in HANDOFF-MANIFEST.txt, and then re-runs
ops/bin/csd-pool-verify-launch-handoff.sh against the embedded release archive,
real go-live receipt, and public acceptance evidence archive.

Artifacts:

  release_archive=$(basename "$RELEASE_ARCHIVE")
  real_go_live_receipt=$(basename "$RECEIPT_ARCHIVE")
  public_acceptance_evidence=$(basename "$ACCEPTANCE_ARCHIVE")
README

cat >"$STAGING_DIR/handoff-summary.json" <<JSON
{
  "status": "passed",
  "target": "launch-handoff",
  "name": $(json_string "$PACKAGE_NAME"),
  "timestamp_utc": $(json_string "$TIMESTAMP"),
  "release_archive": $(json_string "$(basename "$RELEASE_ARCHIVE")"),
  "release_archive_sha256": $(json_string "$(sha256_value "$RELEASE_ARCHIVE")"),
  "real_go_live_receipt": $(json_string "$(basename "$RECEIPT_ARCHIVE")"),
  "real_go_live_receipt_sha256": $(json_string "$(sha256_value "$RECEIPT_ARCHIVE")"),
  "public_acceptance_evidence": $(json_string "$(basename "$ACCEPTANCE_ARCHIVE")"),
  "public_acceptance_evidence_sha256": $(json_string "$(sha256_value "$ACCEPTANCE_ARCHIVE")"),
  "verify_log": "launch-handoff.verify.log"
}
JSON

cat >"$STAGING_DIR/HANDOFF-MANIFEST.txt" <<MANIFEST
name=$PACKAGE_NAME
timestamp_utc=$TIMESTAMP
release_archive=$(basename "$RELEASE_ARCHIVE")
release_archive_sha256=$(sha256_value "$RELEASE_ARCHIVE")
real_go_live_receipt=$(basename "$RECEIPT_ARCHIVE")
real_go_live_receipt_sha256=$(sha256_value "$RECEIPT_ARCHIVE")
public_acceptance_evidence=$(basename "$ACCEPTANCE_ARCHIVE")
public_acceptance_evidence_sha256=$(sha256_value "$ACCEPTANCE_ARCHIVE")
verify_launch_handoff=ops/bin/csd-pool-verify-launch-handoff.sh
verify_public_acceptance_evidence=ops/bin/csd-pool-verify-public-acceptance-evidence.sh
verify_real_go_live_receipt=ops/bin/csd-pool-verify-real-go-live-receipt.sh
included_files:
  HANDOFF-MANIFEST.txt
  HANDOFF-README.txt
  HANDOFF-SHA256SUMS
  handoff-summary.json
  launch-handoff.verify.log
  $(basename "$RELEASE_ARCHIVE")
  $(basename "$RELEASE_ARCHIVE").sha256
  $(basename "$RECEIPT_ARCHIVE")
  $(basename "$RECEIPT_ARCHIVE").sha256
  $(basename "$ACCEPTANCE_ARCHIVE")
  $(basename "$ACCEPTANCE_ARCHIVE").sha256
MANIFEST

(
  cd "$STAGING_DIR"
  find . -type f | sort | while read -r file; do
    [[ "$file" == "./HANDOFF-SHA256SUMS" ]] && continue
    sha256_file "$file"
  done >HANDOFF-SHA256SUMS
)

(
  cd "$OUTPUT_DIR"
  tar -czf "$PACKAGE_ARCHIVE" "$PACKAGE_NAME"
)
sha256_file "$PACKAGE_ARCHIVE" >"$PACKAGE_ARCHIVE.sha256"

printf 'launch_handoff_package=%s\n' "$PACKAGE_ARCHIVE"
printf 'launch_handoff_package_sha256=%s\n' "$(sha256_value "$PACKAGE_ARCHIVE")"
printf 'summary: launch handoff package exported\n'

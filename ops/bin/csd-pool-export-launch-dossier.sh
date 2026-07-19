#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
HANDOFF_PACKAGE="${1:-${CSD_POOL_DOSSIER_HANDOFF_PACKAGE:-}}"
HANDOFF_SHA256="${2:-${CSD_POOL_DOSSIER_HANDOFF_SHA256:-}}"
AUDIT_SCRIPT="${CSD_POOL_AUDIT_LAUNCH_READINESS_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-audit-launch-readiness.sh}"
OUTPUT_DIR="${CSD_POOL_DOSSIER_OUTPUT_DIR:-}"
TIMESTAMP="${CSD_POOL_DOSSIER_TIMESTAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
DOSSIER_NAME="${CSD_POOL_DOSSIER_NAME:-}"
ALLOW_NON_LAUNCHABLE="${CSD_POOL_DOSSIER_ALLOW_NON_LAUNCHABLE:-0}"
REQUIRE_PUBLIC_ACCEPTED_SHARE="${CSD_POOL_DOSSIER_REQUIRE_PUBLIC_ACCEPTED_SHARE:-0}"

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
}

usage() {
  printf 'usage: %s /path/to/csd-pool-*-launch-handoff-*.tar.gz [/path/to/.sha256]\n' "$(basename "$0")" >&2
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

if [[ -z "$HANDOFF_PACKAGE" ]]; then
  usage
  exit 2
fi

[[ -f "$HANDOFF_PACKAGE" ]] || fail "launch handoff package not found: $HANDOFF_PACKAGE"
[[ -x "$AUDIT_SCRIPT" ]] || fail "launch readiness audit script not executable: $AUDIT_SCRIPT"

if [[ -z "$HANDOFF_SHA256" && -f "$HANDOFF_PACKAGE.sha256" ]]; then
  HANDOFF_SHA256="$HANDOFF_PACKAGE.sha256"
fi

if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$(dirname "$HANDOFF_PACKAGE")"
fi
mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

if [[ -z "$DOSSIER_NAME" ]]; then
  handoff_base="$(basename "$HANDOFF_PACKAGE" .tar.gz)"
  DOSSIER_NAME="${handoff_base}-launch-dossier-${TIMESTAMP}"
fi

STAGING_DIR="$OUTPUT_DIR/$DOSSIER_NAME"
DOSSIER_ARCHIVE="$OUTPUT_DIR/$DOSSIER_NAME.tar.gz"
rm -rf "$STAGING_DIR" "$DOSSIER_ARCHIVE" "$DOSSIER_ARCHIVE.sha256"
mkdir -p "$STAGING_DIR"

cp "$HANDOFF_PACKAGE" "$STAGING_DIR/$(basename "$HANDOFF_PACKAGE")"
if [[ -n "$HANDOFF_SHA256" && -f "$HANDOFF_SHA256" ]]; then
  cp "$HANDOFF_SHA256" "$STAGING_DIR/$(basename "$HANDOFF_PACKAGE").sha256"
else
  sha256_file "$STAGING_DIR/$(basename "$HANDOFF_PACKAGE")" >"$STAGING_DIR/$(basename "$HANDOFF_PACKAGE").sha256"
fi

READINESS_DIR="$STAGING_DIR/readiness"
mkdir -p "$READINESS_DIR"
READINESS_ENV=(
  CSD_POOL_READINESS_REPORT_DIR="$READINESS_DIR"
  CSD_POOL_READINESS_REQUIRE_PUBLIC_ACCEPTED_SHARE="$REQUIRE_PUBLIC_ACCEPTED_SHARE"
)
if env "${READINESS_ENV[@]}" "$AUDIT_SCRIPT" "$HANDOFF_PACKAGE" "${HANDOFF_SHA256:-}" >"$STAGING_DIR/launch-readiness.audit.log" 2>&1; then
  :
else
  if [[ "$ALLOW_NON_LAUNCHABLE" == "1" ]]; then
    printf 'warning: readiness audit reported non-launchable status; exporting gap dossier because CSD_POOL_DOSSIER_ALLOW_NON_LAUNCHABLE=1\n' >>"$STAGING_DIR/launch-readiness.audit.log"
  else
    cat "$STAGING_DIR/launch-readiness.audit.log" >&2
    fail "launch readiness audit failed; set CSD_POOL_DOSSIER_ALLOW_NON_LAUNCHABLE=1 only to export a gap dossier"
  fi
fi

READINESS_SUMMARY="$READINESS_DIR/launch-readiness-summary.json"
READINESS_REPORT="$READINESS_DIR/LAUNCH-READINESS-REPORT.txt"
[[ -f "$READINESS_SUMMARY" ]] || fail "readiness summary missing after audit"
[[ -f "$READINESS_REPORT" ]] || fail "readiness report missing after audit"
readiness_status="$(python3 - "$READINESS_SUMMARY" <<'PY'
import json
import sys
with open(sys.argv[1], "r", encoding="utf-8") as handle:
    print(json.load(handle).get("status", "unknown"))
PY
)"
if [[ "$readiness_status" != "launch_ready" && "$ALLOW_NON_LAUNCHABLE" != "1" ]]; then
  fail "readiness status is $readiness_status; refusing to export launch dossier"
fi

cat >"$STAGING_DIR/DOSSIER-README.txt" <<README
CSD Pool Launch Dossier

This package is the final launch review bundle. It contains the verified launch
handoff package and the launch readiness audit outputs derived from that package.

Verification:

  ops/bin/csd-pool-verify-launch-dossier.sh $(basename "$DOSSIER_ARCHIVE")

Readiness status:

  $readiness_status

Artifacts:

  handoff_package=$(basename "$HANDOFF_PACKAGE")
  readiness_report=readiness/LAUNCH-READINESS-REPORT.txt
  readiness_summary=readiness/launch-readiness-summary.json
README

cat >"$STAGING_DIR/launch-dossier-summary.json" <<JSON
{
  "status": $(json_string "$readiness_status"),
  "target": "launch-dossier",
  "name": $(json_string "$DOSSIER_NAME"),
  "timestamp_utc": $(json_string "$TIMESTAMP"),
  "launch_ready": $(if [[ "$readiness_status" == "launch_ready" ]]; then printf 'true'; else printf 'false'; fi),
  "require_public_accepted_share": $(if [[ "$REQUIRE_PUBLIC_ACCEPTED_SHARE" == "1" ]]; then printf 'true'; else printf 'false'; fi),
  "handoff_package": $(json_string "$(basename "$HANDOFF_PACKAGE")"),
  "handoff_package_sha256": $(json_string "$(sha256_value "$HANDOFF_PACKAGE")"),
  "readiness_report": "readiness/LAUNCH-READINESS-REPORT.txt",
  "readiness_summary": "readiness/launch-readiness-summary.json",
  "readiness_audit_log": "launch-readiness.audit.log"
}
JSON

cat >"$STAGING_DIR/DOSSIER-MANIFEST.txt" <<MANIFEST
name=$DOSSIER_NAME
timestamp_utc=$TIMESTAMP
status=$readiness_status
require_public_accepted_share=$REQUIRE_PUBLIC_ACCEPTED_SHARE
handoff_package=$(basename "$HANDOFF_PACKAGE")
handoff_package_sha256=$(sha256_value "$HANDOFF_PACKAGE")
readiness_report=readiness/LAUNCH-READINESS-REPORT.txt
readiness_summary=readiness/launch-readiness-summary.json
readiness_audit_log=launch-readiness.audit.log
verify_launch_dossier=ops/bin/csd-pool-verify-launch-dossier.sh
audit_launch_readiness=ops/bin/csd-pool-audit-launch-readiness.sh
included_files:
  DOSSIER-MANIFEST.txt
  DOSSIER-README.txt
  DOSSIER-SHA256SUMS
  launch-dossier-summary.json
  launch-readiness.audit.log
  readiness/LAUNCH-READINESS-REPORT.txt
  readiness/launch-readiness-summary.json
  readiness/launch-readiness-package-verify.log
  $(basename "$HANDOFF_PACKAGE")
  $(basename "$HANDOFF_PACKAGE").sha256
MANIFEST

(
  cd "$STAGING_DIR"
  find . -type f | sort | while read -r file; do
    [[ "$file" == "./DOSSIER-SHA256SUMS" ]] && continue
    sha256_file "$file"
  done >DOSSIER-SHA256SUMS
)

(
  cd "$OUTPUT_DIR"
  tar -czf "$DOSSIER_ARCHIVE" "$DOSSIER_NAME"
)
sha256_file "$DOSSIER_ARCHIVE" >"$DOSSIER_ARCHIVE.sha256"

printf 'launch_dossier_package=%s\n' "$DOSSIER_ARCHIVE"
printf 'launch_dossier_package_sha256=%s\n' "$(sha256_value "$DOSSIER_ARCHIVE")"
printf 'readiness_status=%s\n' "$readiness_status"
printf 'summary: launch dossier package exported\n'

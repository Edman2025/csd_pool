#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
VERIFY_DOSSIER_SCRIPT="${CSD_POOL_VERIFY_LAUNCH_DOSSIER_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-launch-dossier.sh}"
OUTPUT_DIR="${CSD_POOL_LAUNCH_DOSSIER_SELF_TEST_DIR:-}"
KEEP_TMP="${CSD_POOL_LAUNCH_DOSSIER_SELF_TEST_KEEP_DIR:-0}"
OWN_TMP_DIR=0

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

write_dossier_sums() {
  local dir="$1"
  (
    cd "$dir"
    find . -type f ! -name DOSSIER-SHA256SUMS | sort | while read -r file; do
      sha256_file "$file"
    done >DOSSIER-SHA256SUMS
  )
}

cleanup() {
  if [[ "$KEEP_TMP" != "1" && "$OWN_TMP_DIR" == "1" && -n "${OUTPUT_DIR:-}" && -d "$OUTPUT_DIR" ]]; then
    rm -rf "$OUTPUT_DIR"
  fi
}
trap cleanup EXIT

[[ -x "$VERIFY_DOSSIER_SCRIPT" ]] || fail "launch dossier verifier not executable: $VERIFY_DOSSIER_SCRIPT"

if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-launch-dossier-self-test.XXXXXX")"
  OWN_TMP_DIR=1
else
  mkdir -p "$OUTPUT_DIR"
  OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
fi

printf 'CSD Pool launch dossier self-test\n'
printf 'output_dir=%s\n' "$OUTPUT_DIR"

HANDOFF_ROOT="$OUTPUT_DIR/handoff"
HANDOFF_DIR="$HANDOFF_ROOT/csd-pool-self-test-launch-handoff-20260623T000000Z"
mkdir -p "$HANDOFF_DIR"
printf 'self-test handoff placeholder\n' >"$HANDOFF_DIR/HANDOFF-README.txt"
(
  cd "$HANDOFF_ROOT"
  tar -czf "$OUTPUT_DIR/self-test-launch-handoff.tar.gz" "$(basename "$HANDOFF_DIR")"
)
HANDOFF_SHA="$(sha256_value "$OUTPUT_DIR/self-test-launch-handoff.tar.gz")"

DOSSIER_ROOT="$OUTPUT_DIR/dossier"
DOSSIER_DIR="$DOSSIER_ROOT/csd-pool-self-test-launch-dossier-20260623T000000Z"
mkdir -p "$DOSSIER_DIR/readiness"
cp "$OUTPUT_DIR/self-test-launch-handoff.tar.gz" "$DOSSIER_DIR/self-test-launch-handoff.tar.gz"
sha256_file "$DOSSIER_DIR/self-test-launch-handoff.tar.gz" >"$DOSSIER_DIR/self-test-launch-handoff.tar.gz.sha256"
cat >"$DOSSIER_DIR/DOSSIER-README.txt" <<'TXT'
CSD Pool Launch Dossier
TXT
cat >"$DOSSIER_DIR/readiness/LAUNCH-READINESS-REPORT.txt" <<'TXT'
status=needs_real_environment_evidence
TXT
cat >"$DOSSIER_DIR/readiness/launch-readiness-summary.json" <<'JSON'
{
  "status": "needs_real_environment_evidence",
  "hard_failures": 1,
  "handoff_package": "__HANDOFF_PACKAGE__",
  "handoff_package_sha256": "__HANDOFF_SHA__",
  "checks": []
}
JSON
python3 - "$DOSSIER_DIR/readiness/launch-readiness-summary.json" "$HANDOFF_SHA" <<'PY'
import json
import sys

path, sha = sys.argv[1:3]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
data["handoff_package"] = "self-test-launch-handoff.tar.gz"
data["handoff_package_sha256"] = sha
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
printf 'summary: pass=1 fail=0\nreadiness_summary=readiness/launch-readiness-summary.json\n' >"$DOSSIER_DIR/launch-readiness.audit.log"
printf 'summary: pass=1 fail=0\n' >"$DOSSIER_DIR/readiness/launch-readiness-package-verify.log"
cat >"$DOSSIER_DIR/launch-dossier-summary.json" <<JSON
{
  "status": "needs_real_environment_evidence",
  "target": "launch-dossier",
  "name": "csd-pool-self-test-launch-dossier-20260623T000000Z",
  "timestamp_utc": "20260623T000000Z",
  "launch_ready": false,
  "require_public_accepted_share": false,
  "handoff_package": "self-test-launch-handoff.tar.gz",
  "handoff_package_sha256": "$HANDOFF_SHA",
  "readiness_report": "readiness/LAUNCH-READINESS-REPORT.txt",
  "readiness_summary": "readiness/launch-readiness-summary.json",
  "readiness_audit_log": "launch-readiness.audit.log"
}
JSON
cat >"$DOSSIER_DIR/DOSSIER-MANIFEST.txt" <<MANIFEST
name=csd-pool-self-test-launch-dossier-20260623T000000Z
timestamp_utc=20260623T000000Z
status=needs_real_environment_evidence
require_public_accepted_share=0
handoff_package=self-test-launch-handoff.tar.gz
handoff_package_sha256=$HANDOFF_SHA
readiness_report=readiness/LAUNCH-READINESS-REPORT.txt
readiness_summary=readiness/launch-readiness-summary.json
readiness_audit_log=launch-readiness.audit.log
verify_launch_dossier=ops/bin/csd-pool-verify-launch-dossier.sh
included_files:
  DOSSIER-MANIFEST.txt
  DOSSIER-README.txt
  DOSSIER-SHA256SUMS
  launch-dossier-summary.json
  launch-readiness.audit.log
  readiness/LAUNCH-READINESS-REPORT.txt
  readiness/launch-readiness-summary.json
  readiness/launch-readiness-package-verify.log
  self-test-launch-handoff.tar.gz
  self-test-launch-handoff.tar.gz.sha256
MANIFEST
write_dossier_sums "$DOSSIER_DIR"
(
  cd "$DOSSIER_ROOT"
  tar -czf "$OUTPUT_DIR/self-test-launch-dossier.tar.gz" "$(basename "$DOSSIER_DIR")"
)
sha256_file "$OUTPUT_DIR/self-test-launch-dossier.tar.gz" >"$OUTPUT_DIR/self-test-launch-dossier.tar.gz.sha256"

CSD_POOL_DOSSIER_ALLOW_NON_LAUNCHABLE=1 "$VERIFY_DOSSIER_SCRIPT" \
  "$OUTPUT_DIR/self-test-launch-dossier.tar.gz" \
  "$OUTPUT_DIR/self-test-launch-dossier.tar.gz.sha256" \
  >"$OUTPUT_DIR/positive-verify.log" 2>&1 \
  || { cat "$OUTPUT_DIR/positive-verify.log" >&2; fail "positive launch dossier verification failed"; }
printf 'ok: positive launch dossier verification passed\n'

TAMPER_ROOT="$OUTPUT_DIR/tamper"
mkdir -p "$TAMPER_ROOT"
tar -xzf "$OUTPUT_DIR/self-test-launch-dossier.tar.gz" -C "$TAMPER_ROOT"
TAMPER_DIR="$(find "$TAMPER_ROOT" -mindepth 1 -maxdepth 1 -type d | head -n1)"
python3 - "$TAMPER_DIR/launch-dossier-summary.json" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
value = data.get("handoff_package_sha256")
if not isinstance(value, str) or len(value) < 8:
    raise SystemExit("handoff_package_sha256 missing")
data["handoff_package_sha256"] = ("0" if value[0] != "0" else "1") + value[1:]
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
write_dossier_sums "$TAMPER_DIR"
(
  cd "$TAMPER_ROOT"
  tar -czf "$OUTPUT_DIR/tampered-launch-dossier-summary-sha.tar.gz" "$(basename "$TAMPER_DIR")"
)
sha256_file "$OUTPUT_DIR/tampered-launch-dossier-summary-sha.tar.gz" >"$OUTPUT_DIR/tampered-launch-dossier-summary-sha.tar.gz.sha256"

if CSD_POOL_DOSSIER_ALLOW_NON_LAUNCHABLE=1 "$VERIFY_DOSSIER_SCRIPT" \
  "$OUTPUT_DIR/tampered-launch-dossier-summary-sha.tar.gz" \
  "$OUTPUT_DIR/tampered-launch-dossier-summary-sha.tar.gz.sha256" \
  >"$OUTPUT_DIR/tampered-summary-sha-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/tampered-summary-sha-verify.log" >&2
  fail "tampered launch dossier package unexpectedly verified"
fi
if grep -Fq "dossier summary handoff sha256 mismatch" "$OUTPUT_DIR/tampered-summary-sha-verify.log"; then
  printf 'ok: tampered launch dossier package rejected by summary handoff sha binding\n'
else
  cat "$OUTPUT_DIR/tampered-summary-sha-verify.log" >&2
  fail "tampered launch dossier package failed for an unexpected reason"
fi

READINESS_TAMPER_ROOT="$OUTPUT_DIR/readiness-tamper"
mkdir -p "$READINESS_TAMPER_ROOT"
tar -xzf "$OUTPUT_DIR/self-test-launch-dossier.tar.gz" -C "$READINESS_TAMPER_ROOT"
READINESS_TAMPER_DIR="$(find "$READINESS_TAMPER_ROOT" -mindepth 1 -maxdepth 1 -type d | head -n1)"
python3 - "$READINESS_TAMPER_DIR/readiness/launch-readiness-summary.json" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
value = data.get("handoff_package_sha256")
if not isinstance(value, str) or len(value) < 8:
    raise SystemExit("handoff_package_sha256 missing")
data["handoff_package_sha256"] = ("0" if value[0] != "0" else "1") + value[1:]
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
write_dossier_sums "$READINESS_TAMPER_DIR"
(
  cd "$READINESS_TAMPER_ROOT"
  tar -czf "$OUTPUT_DIR/tampered-launch-dossier-readiness-sha.tar.gz" "$(basename "$READINESS_TAMPER_DIR")"
)
sha256_file "$OUTPUT_DIR/tampered-launch-dossier-readiness-sha.tar.gz" >"$OUTPUT_DIR/tampered-launch-dossier-readiness-sha.tar.gz.sha256"

if CSD_POOL_DOSSIER_ALLOW_NON_LAUNCHABLE=1 "$VERIFY_DOSSIER_SCRIPT" \
  "$OUTPUT_DIR/tampered-launch-dossier-readiness-sha.tar.gz" \
  "$OUTPUT_DIR/tampered-launch-dossier-readiness-sha.tar.gz.sha256" \
  >"$OUTPUT_DIR/tampered-readiness-sha-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/tampered-readiness-sha-verify.log" >&2
  fail "readiness-tampered launch dossier package unexpectedly verified"
fi
if grep -Fq "readiness summary handoff sha256 mismatch" "$OUTPUT_DIR/tampered-readiness-sha-verify.log"; then
  printf 'ok: tampered launch dossier package rejected by readiness handoff sha binding\n'
else
  cat "$OUTPUT_DIR/tampered-readiness-sha-verify.log" >&2
  fail "readiness-tampered launch dossier package failed for an unexpected reason"
fi

printf 'positive_verify_log=%s\n' "$OUTPUT_DIR/positive-verify.log"
printf 'tampered_summary_sha_log=%s\n' "$OUTPUT_DIR/tampered-summary-sha-verify.log"
printf 'tampered_readiness_sha_log=%s\n' "$OUTPUT_DIR/tampered-readiness-sha-verify.log"
printf 'summary: launch dossier self-test passed\n'

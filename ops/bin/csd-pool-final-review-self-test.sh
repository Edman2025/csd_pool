#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
PACKAGE="${1:-${CSD_POOL_FINAL_REVIEW_PACKAGE:-}}"
SHA_FILE="${2:-${CSD_POOL_FINAL_REVIEW_PACKAGE_SHA256:-}}"
VERIFY_SCRIPT="${CSD_POOL_VERIFY_FINAL_REVIEW_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-final-review.sh}"
OUTPUT_DIR="${CSD_POOL_FINAL_REVIEW_SELF_TEST_DIR:-}"
KEEP_TMP="${CSD_POOL_FINAL_REVIEW_SELF_TEST_KEEP_DIR:-0}"
OWN_TMP_DIR=0

fail() {
  printf 'fail: %s\n' "$1" >&2
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
    fail "sha256 tool missing"
  fi
}

sha256_value() {
  sha256_file "$1" | awk '{print $1}'
}

cleanup() {
  if [[ "$KEEP_TMP" != "1" && "$OWN_TMP_DIR" == "1" && -n "${TMP_DIR:-}" && -d "$TMP_DIR" ]]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

if [[ -z "$PACKAGE" ]]; then
  usage
  exit 2
fi
[[ -f "$PACKAGE" ]] || fail "final review package not found: $PACKAGE"
[[ -x "$VERIFY_SCRIPT" ]] || fail "final review verifier not executable: $VERIFY_SCRIPT"
if [[ -z "$SHA_FILE" && -f "$PACKAGE.sha256" ]]; then
  SHA_FILE="$PACKAGE.sha256"
fi

if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-final-review-self-test.XXXXXX")"
  OWN_TMP_DIR=1
else
  mkdir -p "$OUTPUT_DIR"
  OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
fi
TMP_DIR="$OUTPUT_DIR/work"
rm -rf "$TMP_DIR"
mkdir -p "$TMP_DIR"

printf 'CSD Pool final review self-test\n'
printf 'package=%s\n' "$PACKAGE"
printf 'output_dir=%s\n' "$OUTPUT_DIR"

if [[ -n "$SHA_FILE" ]]; then
  "$VERIFY_SCRIPT" "$PACKAGE" "$SHA_FILE" >"$OUTPUT_DIR/positive-verify.log" 2>&1 \
    || { cat "$OUTPUT_DIR/positive-verify.log" >&2; fail "positive final-review verification failed"; }
else
  "$VERIFY_SCRIPT" "$PACKAGE" >"$OUTPUT_DIR/positive-verify.log" 2>&1 \
    || { cat "$OUTPUT_DIR/positive-verify.log" >&2; fail "positive final-review verification failed"; }
fi
printf 'ok: positive final-review verification passed\n'

tar -xzf "$PACKAGE" -C "$TMP_DIR"
REVIEW_DIR="$(find "$TMP_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
[[ -n "$REVIEW_DIR" && -d "$REVIEW_DIR" ]] || fail "final review package did not extract to one top-level directory"
SUMMARY="$REVIEW_DIR/final/final-launch-summary.json"
[[ -f "$SUMMARY" ]] || fail "final-launch-summary.json missing from extracted package"
FINAL_STATUS="$(python3 - "$SUMMARY" <<'PY'
import json
import sys
with open(sys.argv[1], "r", encoding="utf-8") as handle:
    print(json.load(handle).get("status", "unknown"))
PY
)"

if [[ "$FINAL_STATUS" == "launch_ready" ]]; then
  ACCEPTED_SHARE_TMP="$OUTPUT_DIR/work-final-require-public-accepted-share"
  rm -rf "$ACCEPTED_SHARE_TMP"
  mkdir -p "$ACCEPTED_SHARE_TMP"
  tar -xzf "$PACKAGE" -C "$ACCEPTED_SHARE_TMP"
  ACCEPTED_SHARE_REVIEW_DIR="$(find "$ACCEPTED_SHARE_TMP" -mindepth 1 -maxdepth 1 -type d | head -n1)"
  ACCEPTED_SHARE_SUMMARY="$ACCEPTED_SHARE_REVIEW_DIR/final/final-launch-summary.json"
  [[ -f "$ACCEPTED_SHARE_SUMMARY" ]] || fail "final-launch-summary.json missing from accepted-share requirement tamper package"
  python3 - "$ACCEPTED_SHARE_SUMMARY" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
data["require_public_accepted_share"] = False
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
  (
    cd "$ACCEPTED_SHARE_REVIEW_DIR"
    find . -type f ! -name FINAL-REVIEW-SHA256SUMS | sort | while read -r file; do
      sha256_file "$file"
    done >FINAL-REVIEW-SHA256SUMS
  )
  ACCEPTED_SHARE_TAMPERED="$OUTPUT_DIR/$(basename "$PACKAGE" .tar.gz)-tampered-final-require-public-accepted-share.tar.gz"
  (
    cd "$ACCEPTED_SHARE_TMP"
    tar -czf "$ACCEPTED_SHARE_TAMPERED" "$(basename "$ACCEPTED_SHARE_REVIEW_DIR")"
  )
  sha256_file "$ACCEPTED_SHARE_TAMPERED" >"$ACCEPTED_SHARE_TAMPERED.sha256"
  if "$VERIFY_SCRIPT" "$ACCEPTED_SHARE_TAMPERED" "$ACCEPTED_SHARE_TAMPERED.sha256" >"$OUTPUT_DIR/tampered-final-require-public-accepted-share-verify.log" 2>&1; then
    cat "$OUTPUT_DIR/tampered-final-require-public-accepted-share-verify.log" >&2
    fail "accepted-share requirement tampered final-review package unexpectedly verified"
  fi
  if grep -Fq "launch_ready final review requires public accepted share" "$OUTPUT_DIR/tampered-final-require-public-accepted-share-verify.log"; then
    printf 'ok: tampered final-review package rejected by public accepted-share requirement\n'
  else
    cat "$OUTPUT_DIR/tampered-final-require-public-accepted-share-verify.log" >&2
    fail "accepted-share requirement tampered package failed for an unexpected reason"
  fi

  DOCTOR_HARD_TMP="$OUTPUT_DIR/work-final-doctor-hard-failures"
  rm -rf "$DOCTOR_HARD_TMP"
  mkdir -p "$DOCTOR_HARD_TMP"
  tar -xzf "$PACKAGE" -C "$DOCTOR_HARD_TMP"
  DOCTOR_HARD_REVIEW_DIR="$(find "$DOCTOR_HARD_TMP" -mindepth 1 -maxdepth 1 -type d | head -n1)"
  DOCTOR_HARD_SUMMARY="$DOCTOR_HARD_REVIEW_DIR/doctor/real-environment-doctor-summary.json"
  [[ -f "$DOCTOR_HARD_SUMMARY" ]] || fail "doctor summary missing from doctor hard failures tamper package"
  python3 - "$DOCTOR_HARD_SUMMARY" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
data["hard_failures"] = 1
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
  (
    cd "$DOCTOR_HARD_REVIEW_DIR"
    find . -type f ! -name FINAL-REVIEW-SHA256SUMS | sort | while read -r file; do
      sha256_file "$file"
    done >FINAL-REVIEW-SHA256SUMS
  )
  DOCTOR_HARD_TAMPERED="$OUTPUT_DIR/$(basename "$PACKAGE" .tar.gz)-tampered-final-doctor-hard-failures.tar.gz"
  (
    cd "$DOCTOR_HARD_TMP"
    tar -czf "$DOCTOR_HARD_TAMPERED" "$(basename "$DOCTOR_HARD_REVIEW_DIR")"
  )
  sha256_file "$DOCTOR_HARD_TAMPERED" >"$DOCTOR_HARD_TAMPERED.sha256"
  if "$VERIFY_SCRIPT" "$DOCTOR_HARD_TAMPERED" "$DOCTOR_HARD_TAMPERED.sha256" >"$OUTPUT_DIR/tampered-final-doctor-hard-failures-verify.log" 2>&1; then
    cat "$OUTPUT_DIR/tampered-final-doctor-hard-failures-verify.log" >&2
    fail "doctor hard failures tampered final-review package unexpectedly verified"
  fi
  if grep -Fq "launch_ready final review requires doctor hard_failures=0" "$OUTPUT_DIR/tampered-final-doctor-hard-failures-verify.log"; then
    printf 'ok: tampered final-review package rejected by doctor hard failures gate\n'
  else
    cat "$OUTPUT_DIR/tampered-final-doctor-hard-failures-verify.log" >&2
    fail "doctor hard failures tampered package failed for an unexpected reason"
  fi

  DOCTOR_BINDING_TMP="$OUTPUT_DIR/work-final-doctor-receipt-binding"
  rm -rf "$DOCTOR_BINDING_TMP"
  mkdir -p "$DOCTOR_BINDING_TMP"
  tar -xzf "$PACKAGE" -C "$DOCTOR_BINDING_TMP"
  DOCTOR_BINDING_REVIEW_DIR="$(find "$DOCTOR_BINDING_TMP" -mindepth 1 -maxdepth 1 -type d | head -n1)"
  DOCTOR_BINDING_SUMMARY="$DOCTOR_BINDING_REVIEW_DIR/doctor/real-environment-doctor-summary.json"
  [[ -f "$DOCTOR_BINDING_SUMMARY" ]] || fail "doctor summary missing from doctor receipt binding tamper package"
  python3 - "$DOCTOR_BINDING_SUMMARY" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
data["final_review_only_tamper"] = True
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
  (
    cd "$DOCTOR_BINDING_REVIEW_DIR"
    find . -type f ! -name FINAL-REVIEW-SHA256SUMS | sort | while read -r file; do
      sha256_file "$file"
    done >FINAL-REVIEW-SHA256SUMS
  )
  DOCTOR_BINDING_TAMPERED="$OUTPUT_DIR/$(basename "$PACKAGE" .tar.gz)-tampered-final-doctor-receipt-binding.tar.gz"
  (
    cd "$DOCTOR_BINDING_TMP"
    tar -czf "$DOCTOR_BINDING_TAMPERED" "$(basename "$DOCTOR_BINDING_REVIEW_DIR")"
  )
  sha256_file "$DOCTOR_BINDING_TAMPERED" >"$DOCTOR_BINDING_TAMPERED.sha256"
  if "$VERIFY_SCRIPT" "$DOCTOR_BINDING_TAMPERED" "$DOCTOR_BINDING_TAMPERED.sha256" >"$OUTPUT_DIR/tampered-final-doctor-receipt-binding-verify.log" 2>&1; then
    cat "$OUTPUT_DIR/tampered-final-doctor-receipt-binding-verify.log" >&2
    fail "doctor receipt binding tampered final-review package unexpectedly verified"
  fi
  if grep -Fq "final review doctor summary mismatch with embedded receipt" "$OUTPUT_DIR/tampered-final-doctor-receipt-binding-verify.log"; then
    printf 'ok: tampered final-review package rejected by doctor receipt binding\n'
  else
    cat "$OUTPUT_DIR/tampered-final-doctor-receipt-binding-verify.log" >&2
    fail "doctor receipt binding tampered package failed for an unexpected reason"
  fi
fi

python3 - "$SUMMARY" <<'PY'
import json
import sys
path = sys.argv[1]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
value = data.get("launch_handoff_package_sha256")
if not isinstance(value, str) or len(value) < 8:
    raise SystemExit("launch_handoff_package_sha256 missing or too short")
replacement = ("0" if value[0] != "0" else "1") + value[1:]
data["launch_handoff_package_sha256"] = replacement
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY

(
  cd "$REVIEW_DIR"
  find . -type f ! -name FINAL-REVIEW-SHA256SUMS | sort | while read -r file; do
    sha256_file "$file"
  done >FINAL-REVIEW-SHA256SUMS
)

TAMPERED="$OUTPUT_DIR/$(basename "$PACKAGE" .tar.gz)-tampered-summary-sha.tar.gz"
(
  cd "$TMP_DIR"
  tar -czf "$TAMPERED" "$(basename "$REVIEW_DIR")"
)
sha256_file "$TAMPERED" >"$TAMPERED.sha256"

if "$VERIFY_SCRIPT" "$TAMPERED" "$TAMPERED.sha256" >"$OUTPUT_DIR/tampered-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/tampered-verify.log" >&2
  fail "tampered final-review package unexpectedly verified"
fi

if grep -Fq "final summary handoff sha256 mismatch" "$OUTPUT_DIR/tampered-verify.log"; then
  printf 'ok: tampered final-review package rejected by summary handoff sha binding\n'
else
  cat "$OUTPUT_DIR/tampered-verify.log" >&2
  fail "tampered package failed for an unexpected reason"
fi

PROVENANCE_TMP="$OUTPUT_DIR/work-provenance"
rm -rf "$PROVENANCE_TMP"
mkdir -p "$PROVENANCE_TMP"
tar -xzf "$PACKAGE" -C "$PROVENANCE_TMP"
PROVENANCE_REVIEW_DIR="$(find "$PROVENANCE_TMP" -mindepth 1 -maxdepth 1 -type d | head -n1)"
PROVENANCE_SUMMARY="$PROVENANCE_REVIEW_DIR/final/final-launch-summary.json"
[[ -f "$PROVENANCE_SUMMARY" ]] || fail "final-launch-summary.json missing from provenance test package"
python3 - "$PROVENANCE_SUMMARY" <<'PY'
import json
import sys
path = sys.argv[1]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
value = data.get("release_archive_sha256")
if not isinstance(value, str) or len(value) < 8:
    raise SystemExit("release_archive_sha256 missing or too short")
data["release_archive_sha256"] = ("0" if value[0] != "0" else "1") + value[1:]
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
(
  cd "$PROVENANCE_REVIEW_DIR"
  find . -type f ! -name FINAL-REVIEW-SHA256SUMS | sort | while read -r file; do
    sha256_file "$file"
  done >FINAL-REVIEW-SHA256SUMS
)
PROVENANCE_TAMPERED="$OUTPUT_DIR/$(basename "$PACKAGE" .tar.gz)-tampered-release-sha.tar.gz"
(
  cd "$PROVENANCE_TMP"
  tar -czf "$PROVENANCE_TAMPERED" "$(basename "$PROVENANCE_REVIEW_DIR")"
)
sha256_file "$PROVENANCE_TAMPERED" >"$PROVENANCE_TAMPERED.sha256"
if "$VERIFY_SCRIPT" "$PROVENANCE_TAMPERED" "$PROVENANCE_TAMPERED.sha256" >"$OUTPUT_DIR/tampered-release-sha-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/tampered-release-sha-verify.log" >&2
  fail "release-sha tampered final-review package unexpectedly verified"
fi
if grep -Fq "final summary release sha256 mismatch" "$OUTPUT_DIR/tampered-release-sha-verify.log"; then
  printf 'ok: tampered final-review package rejected by release sha binding\n'
else
  cat "$OUTPUT_DIR/tampered-release-sha-verify.log" >&2
  fail "release-sha tampered package failed for an unexpected reason"
fi

PATH_TMP="$OUTPUT_DIR/work-provenance-path"
rm -rf "$PATH_TMP"
mkdir -p "$PATH_TMP"
tar -xzf "$PACKAGE" -C "$PATH_TMP"
PATH_REVIEW_DIR="$(find "$PATH_TMP" -mindepth 1 -maxdepth 1 -type d | head -n1)"
PATH_SUMMARY="$PATH_REVIEW_DIR/final/final-launch-summary.json"
[[ -f "$PATH_SUMMARY" ]] || fail "final-launch-summary.json missing from path provenance test package"
python3 - "$PATH_SUMMARY" <<'PY'
import json
import sys
path = sys.argv[1]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
value = data.get("release_archive")
if not isinstance(value, str) or not value:
    raise SystemExit("release_archive missing")
data["release_archive"] = value + ".misleading"
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
(
  cd "$PATH_REVIEW_DIR"
  find . -type f ! -name FINAL-REVIEW-SHA256SUMS | sort | while read -r file; do
    sha256_file "$file"
  done >FINAL-REVIEW-SHA256SUMS
)
PATH_TAMPERED="$OUTPUT_DIR/$(basename "$PACKAGE" .tar.gz)-tampered-release-path.tar.gz"
(
  cd "$PATH_TMP"
  tar -czf "$PATH_TAMPERED" "$(basename "$PATH_REVIEW_DIR")"
)
sha256_file "$PATH_TAMPERED" >"$PATH_TAMPERED.sha256"
if "$VERIFY_SCRIPT" "$PATH_TAMPERED" "$PATH_TAMPERED.sha256" >"$OUTPUT_DIR/tampered-release-path-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/tampered-release-path-verify.log" >&2
  fail "release-path tampered final-review package unexpectedly verified"
fi
if grep -Fq "final summary release archive path mismatch" "$OUTPUT_DIR/tampered-release-path-verify.log"; then
  printf 'ok: tampered final-review package rejected by release archive path binding\n'
else
  cat "$OUTPUT_DIR/tampered-release-path-verify.log" >&2
  fail "release-path tampered package failed for an unexpected reason"
fi

HANDOFF_PATH_TMP="$OUTPUT_DIR/work-handoff-package-path"
rm -rf "$HANDOFF_PATH_TMP"
mkdir -p "$HANDOFF_PATH_TMP"
tar -xzf "$PACKAGE" -C "$HANDOFF_PATH_TMP"
HANDOFF_PATH_REVIEW_DIR="$(find "$HANDOFF_PATH_TMP" -mindepth 1 -maxdepth 1 -type d | head -n1)"
HANDOFF_PATH_SUMMARY="$HANDOFF_PATH_REVIEW_DIR/final/final-launch-summary.json"
[[ -f "$HANDOFF_PATH_SUMMARY" ]] || fail "final-launch-summary.json missing from handoff package path test package"
python3 - "$HANDOFF_PATH_SUMMARY" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
value = data.get("launch_handoff_package")
if not isinstance(value, str) or not value:
    raise SystemExit("launch_handoff_package missing")
data["launch_handoff_package"] = value + ".misleading"
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
(
  cd "$HANDOFF_PATH_REVIEW_DIR"
  find . -type f ! -name FINAL-REVIEW-SHA256SUMS | sort | while read -r file; do
    sha256_file "$file"
  done >FINAL-REVIEW-SHA256SUMS
)
HANDOFF_PATH_TAMPERED="$OUTPUT_DIR/$(basename "$PACKAGE" .tar.gz)-tampered-handoff-package-path.tar.gz"
(
  cd "$HANDOFF_PATH_TMP"
  tar -czf "$HANDOFF_PATH_TAMPERED" "$(basename "$HANDOFF_PATH_REVIEW_DIR")"
)
sha256_file "$HANDOFF_PATH_TAMPERED" >"$HANDOFF_PATH_TAMPERED.sha256"
if "$VERIFY_SCRIPT" "$HANDOFF_PATH_TAMPERED" "$HANDOFF_PATH_TAMPERED.sha256" >"$OUTPUT_DIR/tampered-handoff-package-path-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/tampered-handoff-package-path-verify.log" >&2
  fail "handoff-package-path tampered final-review package unexpectedly verified"
fi
if grep -Fq "final summary handoff package path mismatch" "$OUTPUT_DIR/tampered-handoff-package-path-verify.log"; then
  printf 'ok: tampered final-review package rejected by handoff package path binding\n'
else
  cat "$OUTPUT_DIR/tampered-handoff-package-path-verify.log" >&2
  fail "handoff-package-path tampered package failed for an unexpected reason"
fi

READINESS_BINDING_TMP="$OUTPUT_DIR/work-final-readiness-binding"
rm -rf "$READINESS_BINDING_TMP"
mkdir -p "$READINESS_BINDING_TMP"
tar -xzf "$PACKAGE" -C "$READINESS_BINDING_TMP"
READINESS_BINDING_REVIEW_DIR="$(find "$READINESS_BINDING_TMP" -mindepth 1 -maxdepth 1 -type d | head -n1)"
READINESS_BINDING_SUMMARY="$READINESS_BINDING_REVIEW_DIR/final/final-launch-summary.json"
[[ -f "$READINESS_BINDING_SUMMARY" ]] || fail "final-launch-summary.json missing from readiness binding test package"
python3 - "$READINESS_BINDING_SUMMARY" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
readiness = data.get("readiness")
if not isinstance(readiness, dict):
    raise SystemExit("readiness summary fields missing")
readiness["public_canary_source_configured_when_required"] = not bool(
    readiness.get("public_canary_source_configured_when_required")
)
readiness["public_acceptance_toolchain_manifest_verified"] = not bool(
    readiness.get("public_acceptance_toolchain_manifest_verified")
)
readiness["public_status_release_identity_matches_receipt"] = not bool(
    readiness.get("public_status_release_identity_matches_receipt")
)
value = readiness.get("public_canary_shares_accepted")
readiness["public_canary_shares_accepted"] = 0 if value != 0 else 1
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
(
  cd "$READINESS_BINDING_REVIEW_DIR"
  find . -type f ! -name FINAL-REVIEW-SHA256SUMS | sort | while read -r file; do
    sha256_file "$file"
  done >FINAL-REVIEW-SHA256SUMS
)
READINESS_BINDING_TAMPERED="$OUTPUT_DIR/$(basename "$PACKAGE" .tar.gz)-tampered-final-readiness-binding.tar.gz"
(
  cd "$READINESS_BINDING_TMP"
  tar -czf "$READINESS_BINDING_TAMPERED" "$(basename "$READINESS_BINDING_REVIEW_DIR")"
)
sha256_file "$READINESS_BINDING_TAMPERED" >"$READINESS_BINDING_TAMPERED.sha256"
if "$VERIFY_SCRIPT" "$READINESS_BINDING_TAMPERED" "$READINESS_BINDING_TAMPERED.sha256" >"$OUTPUT_DIR/tampered-final-readiness-binding-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/tampered-final-readiness-binding-verify.log" >&2
  fail "readiness-binding tampered final-review package unexpectedly verified"
fi
if grep -Fq "final summary readiness fields mismatch embedded dossier readiness summary" "$OUTPUT_DIR/tampered-final-readiness-binding-verify.log"; then
  printf 'ok: tampered final-review package rejected by readiness summary binding\n'
else
  cat "$OUTPUT_DIR/tampered-final-readiness-binding-verify.log" >&2
  fail "readiness-binding tampered package failed for an unexpected reason"
fi

HANDOFF_SUMMARY_TMP="$OUTPUT_DIR/work-handoff-summary-sha"
rm -rf "$HANDOFF_SUMMARY_TMP"
mkdir -p "$HANDOFF_SUMMARY_TMP"
tar -xzf "$PACKAGE" -C "$HANDOFF_SUMMARY_TMP"
HANDOFF_SUMMARY_REVIEW_DIR="$(find "$HANDOFF_SUMMARY_TMP" -mindepth 1 -maxdepth 1 -type d | head -n1)"
HANDOFF_SUMMARY_FINAL="$HANDOFF_SUMMARY_REVIEW_DIR/final/final-launch-summary.json"
HANDOFF_SUMMARY_MANIFEST="$HANDOFF_SUMMARY_REVIEW_DIR/FINAL-REVIEW-MANIFEST.txt"
[[ -f "$HANDOFF_SUMMARY_FINAL" ]] || fail "final-launch-summary.json missing from handoff summary sha test package"
[[ -f "$HANDOFF_SUMMARY_MANIFEST" ]] || fail "FINAL-REVIEW-MANIFEST.txt missing from handoff summary sha test package"
HANDOFF_REL="$(awk -F= '$1 == "handoff_package" {sub($1 FS, ""); print; exit}' "$HANDOFF_SUMMARY_MANIFEST")"
[[ -n "$HANDOFF_REL" && -f "$HANDOFF_SUMMARY_REVIEW_DIR/$HANDOFF_REL" ]] || fail "embedded handoff package missing from handoff summary sha test package"
HANDOFF_ARCHIVE_PATH="$HANDOFF_SUMMARY_REVIEW_DIR/$HANDOFF_REL"
HANDOFF_EXTRACT_DIR="$HANDOFF_SUMMARY_TMP/handoff-extract"
mkdir -p "$HANDOFF_EXTRACT_DIR"
tar -xzf "$HANDOFF_ARCHIVE_PATH" -C "$HANDOFF_EXTRACT_DIR"
HANDOFF_ROOT="$(find "$HANDOFF_EXTRACT_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
HANDOFF_SUMMARY_JSON="$HANDOFF_ROOT/handoff-summary.json"
[[ -f "$HANDOFF_SUMMARY_JSON" ]] || fail "handoff-summary.json missing from embedded handoff package"
python3 - "$HANDOFF_SUMMARY_JSON" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
value = data.get("release_archive_sha256")
if not isinstance(value, str) or len(value) < 8:
    raise SystemExit("release_archive_sha256 missing or too short")
data["release_archive_sha256"] = ("0" if value[0] != "0" else "1") + value[1:]
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
(
  cd "$HANDOFF_ROOT"
  find . -type f ! -name HANDOFF-SHA256SUMS | sort | while read -r file; do
    sha256_file "$file"
  done >HANDOFF-SHA256SUMS
)
(
  cd "$HANDOFF_EXTRACT_DIR"
  tar -czf "$HANDOFF_ARCHIVE_PATH" "$(basename "$HANDOFF_ROOT")"
)
NEW_HANDOFF_SHA="$(sha256_value "$HANDOFF_ARCHIVE_PATH")"
python3 - "$HANDOFF_SUMMARY_MANIFEST" "$HANDOFF_SUMMARY_FINAL" "$NEW_HANDOFF_SHA" <<'PY'
import json
import sys

manifest_path, summary_path, new_sha = sys.argv[1:4]
lines = []
with open(manifest_path, "r", encoding="utf-8") as handle:
    for line in handle:
        if line.startswith("handoff_package_sha256="):
            lines.append(f"handoff_package_sha256={new_sha}\n")
        else:
            lines.append(line)
with open(manifest_path, "w", encoding="utf-8") as handle:
    handle.writelines(lines)
with open(summary_path, "r", encoding="utf-8") as handle:
    summary = json.load(handle)
summary["launch_handoff_package_sha256"] = new_sha
with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(summary, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
(
  cd "$HANDOFF_SUMMARY_REVIEW_DIR"
  find . -type f ! -name FINAL-REVIEW-SHA256SUMS | sort | while read -r file; do
    sha256_file "$file"
  done >FINAL-REVIEW-SHA256SUMS
)
HANDOFF_SUMMARY_TAMPERED="$OUTPUT_DIR/$(basename "$PACKAGE" .tar.gz)-tampered-handoff-summary-sha.tar.gz"
(
  cd "$HANDOFF_SUMMARY_TMP"
  tar -czf "$HANDOFF_SUMMARY_TAMPERED" "$(basename "$HANDOFF_SUMMARY_REVIEW_DIR")"
)
sha256_file "$HANDOFF_SUMMARY_TAMPERED" >"$HANDOFF_SUMMARY_TAMPERED.sha256"
if "$VERIFY_SCRIPT" "$HANDOFF_SUMMARY_TAMPERED" "$HANDOFF_SUMMARY_TAMPERED.sha256" >"$OUTPUT_DIR/tampered-handoff-summary-sha-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/tampered-handoff-summary-sha-verify.log" >&2
  fail "handoff-summary-sha tampered final-review package unexpectedly verified"
fi
if grep -Fq "handoff summary release sha256 mismatch" "$OUTPUT_DIR/tampered-handoff-summary-sha-verify.log"; then
  printf 'ok: tampered final-review package rejected by embedded handoff summary sha binding\n'
else
  cat "$OUTPUT_DIR/tampered-handoff-summary-sha-verify.log" >&2
  fail "handoff-summary-sha tampered package failed for an unexpected reason"
fi

REDACTION_TMP="$OUTPUT_DIR/work-redaction"
rm -rf "$REDACTION_TMP"
mkdir -p "$REDACTION_TMP"
tar -xzf "$PACKAGE" -C "$REDACTION_TMP"
REDACTION_REVIEW_DIR="$(find "$REDACTION_TMP" -mindepth 1 -maxdepth 1 -type d | head -n1)"
[[ -n "$REDACTION_REVIEW_DIR" && -d "$REDACTION_REVIEW_DIR" ]] || fail "redaction test package did not extract to one top-level directory"
printf '\nleaked_database_url=postgres://pool:secret-password@example.net/csd_pool\n' >>"$REDACTION_REVIEW_DIR/FINAL-REVIEW-README.txt"
(
  cd "$REDACTION_REVIEW_DIR"
  find . -type f ! -name FINAL-REVIEW-SHA256SUMS | sort | while read -r file; do
    sha256_file "$file"
  done >FINAL-REVIEW-SHA256SUMS
)
REDACTION_TAMPERED="$OUTPUT_DIR/$(basename "$PACKAGE" .tar.gz)-tampered-redaction.tar.gz"
(
  cd "$REDACTION_TMP"
  tar -czf "$REDACTION_TAMPERED" "$(basename "$REDACTION_REVIEW_DIR")"
)
sha256_file "$REDACTION_TAMPERED" >"$REDACTION_TAMPERED.sha256"
if "$VERIFY_SCRIPT" "$REDACTION_TAMPERED" "$REDACTION_TAMPERED.sha256" >"$OUTPUT_DIR/tampered-redaction-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/tampered-redaction-verify.log" >&2
  fail "redaction-tampered final-review package unexpectedly verified"
fi
if grep -Fq "final review redaction scan failed" "$OUTPUT_DIR/tampered-redaction-verify.log"; then
  printf 'ok: tampered final-review package rejected by redaction scan\n'
else
  cat "$OUTPUT_DIR/tampered-redaction-verify.log" >&2
  fail "redaction-tampered package failed for an unexpected reason"
fi

if [[ "$FINAL_STATUS" == "launch_ready" ]]; then
  DOSSIER_TAMPER_TMP="$OUTPUT_DIR/work-dossier-readiness"
  rm -rf "$DOSSIER_TAMPER_TMP"
  mkdir -p "$DOSSIER_TAMPER_TMP"
  tar -xzf "$PACKAGE" -C "$DOSSIER_TAMPER_TMP"
  DOSSIER_TAMPER_REVIEW_DIR="$(find "$DOSSIER_TAMPER_TMP" -mindepth 1 -maxdepth 1 -type d | head -n1)"
  DOSSIER_TAMPER_SUMMARY="$DOSSIER_TAMPER_REVIEW_DIR/final/final-launch-summary.json"
  DOSSIER_TAMPER_MANIFEST="$DOSSIER_TAMPER_REVIEW_DIR/FINAL-REVIEW-MANIFEST.txt"
  [[ -f "$DOSSIER_TAMPER_SUMMARY" ]] || fail "final-launch-summary.json missing from dossier readiness tamper package"
  [[ -f "$DOSSIER_TAMPER_MANIFEST" ]] || fail "FINAL-REVIEW-MANIFEST.txt missing from dossier readiness tamper package"
  DOSSIER_REL="$(awk -F= '$1 == "dossier_package" {sub($1 FS, ""); print; exit}' "$DOSSIER_TAMPER_MANIFEST")"
  [[ -n "$DOSSIER_REL" && -f "$DOSSIER_TAMPER_REVIEW_DIR/$DOSSIER_REL" ]] || fail "embedded dossier package missing from dossier readiness tamper package"
  DOSSIER_ARCHIVE_PATH="$DOSSIER_TAMPER_REVIEW_DIR/$DOSSIER_REL"
  DOSSIER_EXTRACT_DIR="$DOSSIER_TAMPER_TMP/dossier-extract"
  mkdir -p "$DOSSIER_EXTRACT_DIR"
  tar -xzf "$DOSSIER_ARCHIVE_PATH" -C "$DOSSIER_EXTRACT_DIR"
  DOSSIER_ROOT="$(find "$DOSSIER_EXTRACT_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
  READINESS_SUMMARY="$DOSSIER_ROOT/readiness/launch-readiness-summary.json"
  [[ -f "$READINESS_SUMMARY" ]] || fail "launch-readiness-summary.json missing from embedded dossier"
  python3 - "$READINESS_SUMMARY" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
checks = data.get("checks")
if not isinstance(checks, list):
    raise SystemExit("checks list missing")
original_len = len(checks)
removed_keys = {
    "public_stratum_accepted_share_observed",
    "public_canary_accepted_share_minimum_met",
    "public_canary_source_configured_when_required",
    "public_acceptance_toolchain_manifest_verified",
}
data["checks"] = [
    item for item in checks
    if not (isinstance(item, dict) and item.get("key") in removed_keys)
]
if len(data["checks"]) > original_len - len(removed_keys):
    raise SystemExit("public accepted-share readiness checks not found")
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
  (
    cd "$DOSSIER_ROOT"
    find . -type f ! -name DOSSIER-SHA256SUMS | sort | while read -r file; do
      sha256_file "$file"
    done >DOSSIER-SHA256SUMS
  )
  (
    cd "$DOSSIER_EXTRACT_DIR"
    tar -czf "$DOSSIER_ARCHIVE_PATH" "$(basename "$DOSSIER_ROOT")"
  )
  NEW_DOSSIER_SHA="$(sha256_value "$DOSSIER_ARCHIVE_PATH")"
  python3 - "$DOSSIER_TAMPER_MANIFEST" "$DOSSIER_TAMPER_SUMMARY" "$NEW_DOSSIER_SHA" <<'PY'
import json
import sys

manifest_path, summary_path, new_sha = sys.argv[1:4]
lines = []
with open(manifest_path, "r", encoding="utf-8") as handle:
    for line in handle:
        if line.startswith("dossier_package_sha256="):
            lines.append(f"dossier_package_sha256={new_sha}\n")
        else:
            lines.append(line)
with open(manifest_path, "w", encoding="utf-8") as handle:
    handle.writelines(lines)
with open(summary_path, "r", encoding="utf-8") as handle:
    summary = json.load(handle)
summary["launch_dossier_package_sha256"] = new_sha
with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(summary, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
  (
    cd "$DOSSIER_TAMPER_REVIEW_DIR"
    find . -type f ! -name FINAL-REVIEW-SHA256SUMS | sort | while read -r file; do
      sha256_file "$file"
    done >FINAL-REVIEW-SHA256SUMS
  )
  DOSSIER_READINESS_TAMPERED="$OUTPUT_DIR/$(basename "$PACKAGE" .tar.gz)-tampered-dossier-readiness.tar.gz"
  (
    cd "$DOSSIER_TAMPER_TMP"
    tar -czf "$DOSSIER_READINESS_TAMPERED" "$(basename "$DOSSIER_TAMPER_REVIEW_DIR")"
  )
  sha256_file "$DOSSIER_READINESS_TAMPERED" >"$DOSSIER_READINESS_TAMPERED.sha256"
  if "$VERIFY_SCRIPT" "$DOSSIER_READINESS_TAMPERED" "$DOSSIER_READINESS_TAMPERED.sha256" >"$OUTPUT_DIR/tampered-dossier-readiness-verify.log" 2>&1; then
    cat "$OUTPUT_DIR/tampered-dossier-readiness-verify.log" >&2
    fail "dossier-readiness tampered final-review package unexpectedly verified"
  fi
  if grep -Fq "required readiness checks missing or failed" "$OUTPUT_DIR/tampered-dossier-readiness-verify.log"; then
    printf 'ok: tampered final-review package rejected by embedded dossier required readiness checks\n'
  else
    cat "$OUTPUT_DIR/tampered-dossier-readiness-verify.log" >&2
    fail "dossier-readiness tampered package failed for an unexpected reason"
  fi
  printf 'tampered_dossier_readiness_package=%s\n' "$DOSSIER_READINESS_TAMPERED"
  printf 'tampered_dossier_readiness_package_sha256=%s\n' "$DOSSIER_READINESS_TAMPERED.sha256"
  printf 'tampered_dossier_readiness_log=%s\n' "$OUTPUT_DIR/tampered-dossier-readiness-verify.log"
fi

if [[ "$FINAL_STATUS" != "launch_ready" ]]; then
  GAP_TMP="$OUTPUT_DIR/work-gap-status"
  rm -rf "$GAP_TMP"
  mkdir -p "$GAP_TMP"
  tar -xzf "$PACKAGE" -C "$GAP_TMP"
  GAP_REVIEW_DIR="$(find "$GAP_TMP" -mindepth 1 -maxdepth 1 -type d | head -n1)"
  GAP_SUMMARY="$GAP_REVIEW_DIR/gaps/launch-gaps-summary.json"
  [[ -f "$GAP_SUMMARY" ]] || fail "launch-gaps-summary.json missing from non-launch-ready package"
  python3 - "$GAP_SUMMARY" <<'PY'
import json
import sys
path = sys.argv[1]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
data["status"] = "launch_ready"
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
  (
    cd "$GAP_REVIEW_DIR"
    find . -type f ! -name FINAL-REVIEW-SHA256SUMS | sort | while read -r file; do
      sha256_file "$file"
    done >FINAL-REVIEW-SHA256SUMS
  )
  GAP_TAMPERED="$OUTPUT_DIR/$(basename "$PACKAGE" .tar.gz)-tampered-gaps-status.tar.gz"
  (
    cd "$GAP_TMP"
    tar -czf "$GAP_TAMPERED" "$(basename "$GAP_REVIEW_DIR")"
  )
  sha256_file "$GAP_TAMPERED" >"$GAP_TAMPERED.sha256"
  if "$VERIFY_SCRIPT" "$GAP_TAMPERED" "$GAP_TAMPERED.sha256" >"$OUTPUT_DIR/tampered-gaps-status-verify.log" 2>&1; then
    cat "$OUTPUT_DIR/tampered-gaps-status-verify.log" >&2
    fail "gap-status tampered final-review package unexpectedly verified"
  fi
  if grep -Fq "gaps status mismatch" "$OUTPUT_DIR/tampered-gaps-status-verify.log"; then
    printf 'ok: tampered final-review package rejected by gaps status binding\n'
  else
    cat "$OUTPUT_DIR/tampered-gaps-status-verify.log" >&2
    fail "gap-status tampered package failed for an unexpected reason"
  fi
  printf 'tampered_gaps_status_package=%s\n' "$GAP_TAMPERED"
  printf 'tampered_gaps_status_package_sha256=%s\n' "$GAP_TAMPERED.sha256"
  printf 'tampered_gaps_status_log=%s\n' "$OUTPUT_DIR/tampered-gaps-status-verify.log"
fi

printf 'tampered_package=%s\n' "$TAMPERED"
printf 'tampered_package_sha256=%s\n' "$TAMPERED.sha256"
printf 'tampered_release_sha_package=%s\n' "$PROVENANCE_TAMPERED"
printf 'tampered_release_sha_package_sha256=%s\n' "$PROVENANCE_TAMPERED.sha256"
printf 'tampered_release_path_package=%s\n' "$PATH_TAMPERED"
printf 'tampered_release_path_package_sha256=%s\n' "$PATH_TAMPERED.sha256"
printf 'tampered_handoff_package_path_package=%s\n' "$HANDOFF_PATH_TAMPERED"
printf 'tampered_handoff_package_path_package_sha256=%s\n' "$HANDOFF_PATH_TAMPERED.sha256"
printf 'tampered_handoff_summary_sha_package=%s\n' "$HANDOFF_SUMMARY_TAMPERED"
printf 'tampered_handoff_summary_sha_package_sha256=%s\n' "$HANDOFF_SUMMARY_TAMPERED.sha256"
printf 'tampered_redaction_package=%s\n' "$REDACTION_TAMPERED"
printf 'tampered_redaction_package_sha256=%s\n' "$REDACTION_TAMPERED.sha256"
if [[ "${DOSSIER_READINESS_TAMPERED:-}" ]]; then
  printf 'tampered_dossier_readiness_package=%s\n' "$DOSSIER_READINESS_TAMPERED"
  printf 'tampered_dossier_readiness_package_sha256=%s\n' "$DOSSIER_READINESS_TAMPERED.sha256"
fi
printf 'positive_log=%s\n' "$OUTPUT_DIR/positive-verify.log"
printf 'tampered_log=%s\n' "$OUTPUT_DIR/tampered-verify.log"
printf 'tampered_release_sha_log=%s\n' "$OUTPUT_DIR/tampered-release-sha-verify.log"
printf 'tampered_release_path_log=%s\n' "$OUTPUT_DIR/tampered-release-path-verify.log"
printf 'tampered_handoff_package_path_log=%s\n' "$OUTPUT_DIR/tampered-handoff-package-path-verify.log"
printf 'tampered_handoff_summary_sha_log=%s\n' "$OUTPUT_DIR/tampered-handoff-summary-sha-verify.log"
printf 'tampered_redaction_log=%s\n' "$OUTPUT_DIR/tampered-redaction-verify.log"
if [[ "${DOSSIER_READINESS_TAMPERED:-}" ]]; then
  printf 'tampered_dossier_readiness_log=%s\n' "$OUTPUT_DIR/tampered-dossier-readiness-verify.log"
fi
printf 'summary: final review self-test passed\n'

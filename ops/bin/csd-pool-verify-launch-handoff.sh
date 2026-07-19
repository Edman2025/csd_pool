#!/usr/bin/env bash
set -euo pipefail

RELEASE_ARCHIVE="${1:-${CSD_POOL_HANDOFF_RELEASE_ARCHIVE:-}}"
RECEIPT_ARCHIVE="${2:-${CSD_POOL_HANDOFF_REAL_GO_LIVE_RECEIPT:-}}"
ACCEPTANCE_ARCHIVE="${3:-${CSD_POOL_HANDOFF_PUBLIC_ACCEPTANCE_EVIDENCE:-}}"
RELEASE_SHA256="${CSD_POOL_HANDOFF_RELEASE_SHA256:-}"
RECEIPT_SHA256="${CSD_POOL_HANDOFF_RECEIPT_SHA256:-}"
ACCEPTANCE_SHA256="${CSD_POOL_HANDOFF_ACCEPTANCE_SHA256:-}"
TMP_ROOT="${CSD_POOL_HANDOFF_TMP_DIR:-}"
KEEP_TMP="${CSD_POOL_HANDOFF_KEEP_DIR:-0}"
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
  printf 'usage: %s /path/to/csd-pool-release.tar.gz /path/to/real-go-live-receipt.tar.gz /path/to/public-acceptance-evidence.tar.gz\n' "$(basename "$0")" >&2
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

sha256_check_file() {
  local archive="$1"
  local checksum="$2"
  local label="$3"
  local expected line
  [[ -f "$checksum" ]] || fatal "$label .sha256 file missing: $checksum"
  expected="$(sha256_value "$archive")"
  line="$(sed -n '1p' "$checksum" 2>/dev/null || true)"
  if [[ "$line" == "$expected "* && "$line" == *"$(basename "$archive")"* ]]; then
    ok "$label archive sha256 verified"
  else
    fatal "$label archive sha256 mismatch"
  fi
}

safe_tar_check() {
  local archive="$1"
  local label="$2"
  if tar -tzf "$archive" >/dev/null; then
    ok "$label archive can be listed"
  else
    fatal "$label archive cannot be listed"
  fi
  if ! tar -tzf "$archive" | grep -E '(^/|(^|/)\.\.($|/))' >/dev/null; then
    ok "$label archive paths are relative and safe"
  else
    fatal "$label archive contains unsafe paths"
  fi
}

require_file() {
  local path="$1"
  local label="$2"
  [[ -f "$path" ]] && ok "$label exists" || fatal "$label missing"
}

require_executable() {
  local path="$1"
  local label="$2"
  [[ -x "$path" ]] && ok "$label executable" || fatal "$label not executable"
}

require_text() {
  local path="$1"
  local pattern="$2"
  local label="$3"
  grep -Fq "$pattern" "$path" && ok "$label" || fatal "$label missing"
}

json_query() {
  local path="$1"
  local expr="$2"
  python3 - "$path" "$expr" <<'PY'
import json
import sys

path, expr = sys.argv[1:3]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
value = data
for part in expr.split("."):
    if isinstance(value, dict) and part in value:
        value = value[part]
    else:
        sys.exit(1)
if isinstance(value, bool):
    print("true" if value else "false")
elif value is None:
    print("null")
else:
    print(value)
PY
}

cleanup() {
  if [[ "$KEEP_TMP" != "1" && "$OWN_TMP_DIR" == "1" && -n "${TMP_DIR:-}" && -d "$TMP_DIR" ]]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

if [[ -z "$RELEASE_ARCHIVE" || -z "$RECEIPT_ARCHIVE" || -z "$ACCEPTANCE_ARCHIVE" ]]; then
  usage
  exit 2
fi

[[ -f "$RELEASE_ARCHIVE" ]] || fatal "release archive not found: $RELEASE_ARCHIVE"
[[ -f "$RECEIPT_ARCHIVE" ]] || fatal "real go-live receipt archive not found: $RECEIPT_ARCHIVE"
[[ -f "$ACCEPTANCE_ARCHIVE" ]] || fatal "public acceptance evidence archive not found: $ACCEPTANCE_ARCHIVE"
ok "handoff input archives exist"

if [[ -z "$RELEASE_SHA256" && -f "$RELEASE_ARCHIVE.sha256" ]]; then
  RELEASE_SHA256="$RELEASE_ARCHIVE.sha256"
fi
if [[ -z "$RECEIPT_SHA256" && -f "$RECEIPT_ARCHIVE.sha256" ]]; then
  RECEIPT_SHA256="$RECEIPT_ARCHIVE.sha256"
fi
if [[ -z "$ACCEPTANCE_SHA256" && -f "$ACCEPTANCE_ARCHIVE.sha256" ]]; then
  ACCEPTANCE_SHA256="$ACCEPTANCE_ARCHIVE.sha256"
fi

[[ -n "$RELEASE_SHA256" ]] && sha256_check_file "$RELEASE_ARCHIVE" "$RELEASE_SHA256" "release"
[[ -n "$RECEIPT_SHA256" ]] && sha256_check_file "$RECEIPT_ARCHIVE" "$RECEIPT_SHA256" "real go-live receipt"
[[ -n "$ACCEPTANCE_SHA256" ]] && sha256_check_file "$ACCEPTANCE_ARCHIVE" "$ACCEPTANCE_SHA256" "public acceptance evidence"

safe_tar_check "$RELEASE_ARCHIVE" "release"
safe_tar_check "$RECEIPT_ARCHIVE" "real go-live receipt"
safe_tar_check "$ACCEPTANCE_ARCHIVE" "public acceptance evidence"

if [[ -z "$TMP_ROOT" ]]; then
  TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-launch-handoff.XXXXXX")"
  OWN_TMP_DIR=1
else
  mkdir -p "$TMP_ROOT"
  TMP_DIR="$(mktemp -d "$TMP_ROOT/csd-pool-launch-handoff.XXXXXX")"
  OWN_TMP_DIR=1
fi

mkdir -p "$TMP_DIR/acceptance" "$TMP_DIR/receipt"
tar -xzf "$RELEASE_ARCHIVE" -C "$TMP_DIR"
tar -xzf "$RECEIPT_ARCHIVE" -C "$TMP_DIR/receipt"
tar -xzf "$ACCEPTANCE_ARCHIVE" -C "$TMP_DIR/acceptance"
ok "handoff archives extracted for cross-checks"

release_top_count="$(find "$TMP_DIR" -mindepth 1 -maxdepth 1 -type d ! -name acceptance ! -name receipt | wc -l | tr -d ' ')"
[[ "$release_top_count" == "1" ]] || fatal "release archive must contain exactly one top-level release directory"
RELEASE_DIR="$(find "$TMP_DIR" -mindepth 1 -maxdepth 1 -type d ! -name acceptance ! -name receipt | head -n1)"

receipt_top_count="$(find "$TMP_DIR/receipt" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')"
[[ "$receipt_top_count" == "1" ]] || fatal "real go-live receipt archive must contain exactly one top-level directory"
RECEIPT_DIR="$(find "$TMP_DIR/receipt" -mindepth 1 -maxdepth 1 -type d | head -n1)"

acceptance_top_count="$(find "$TMP_DIR/acceptance" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')"
[[ "$acceptance_top_count" == "1" ]] || fatal "public acceptance archive must contain exactly one top-level directory"
ACCEPTANCE_DIR="$(find "$TMP_DIR/acceptance" -mindepth 1 -maxdepth 1 -type d | head -n1)"

require_file "$RELEASE_DIR/RELEASE-MANIFEST.txt" "release manifest"
require_file "$RELEASE_DIR/SHA256SUMS" "release SHA256SUMS"
require_file "$RECEIPT_DIR/go-live-summary.json" "real go-live receipt summary"
require_file "$ACCEPTANCE_DIR/http-public-status.json" "public acceptance status response"
require_executable "$RELEASE_DIR/ops/bin/csd-pool-verify.sh" "release verifier"
require_executable "$RELEASE_DIR/ops/bin/csd-pool-verify-real-go-live-receipt.sh" "release receipt verifier"
require_executable "$RELEASE_DIR/ops/bin/csd-pool-verify-public-acceptance-evidence.sh" "release public acceptance verifier"
require_executable "$RELEASE_DIR/ops/bin/csd-pool-evidence-redaction-self-test.sh" "release evidence redaction self-test"
require_executable "$RELEASE_DIR/ops/bin/csd-pool-release-archive-self-test.sh" "release archive self-test"
require_executable "$RELEASE_DIR/ops/bin/csd-pool-go-live-check.sh" "release go-live gate"
require_executable "$RELEASE_DIR/bin/csd-pool-workers" "release workers binary"
require_text "$RELEASE_DIR/RELEASE-MANIFEST.txt" "verify=ops/bin/csd-pool-verify.sh" "release manifest records release verifier"
require_text "$RELEASE_DIR/RELEASE-MANIFEST.txt" "verify_real_go_live_receipt=ops/bin/csd-pool-verify-real-go-live-receipt.sh" "release manifest records receipt verifier"
require_text "$RELEASE_DIR/RELEASE-MANIFEST.txt" "verify_public_acceptance_evidence=ops/bin/csd-pool-verify-public-acceptance-evidence.sh" "release manifest records public acceptance verifier"
require_text "$RELEASE_DIR/RELEASE-MANIFEST.txt" "evidence_redaction_self_test=ops/bin/csd-pool-evidence-redaction-self-test.sh" "release manifest records evidence redaction self-test"
require_text "$RELEASE_DIR/RELEASE-MANIFEST.txt" "release_archive_self_test=ops/bin/csd-pool-release-archive-self-test.sh" "release manifest records release archive self-test"
require_text "$RELEASE_DIR/RELEASE-MANIFEST.txt" "public_acceptance=ops/bin/csd-pool-public-acceptance.sh" "release manifest records public acceptance gate"

(
  cd "$RELEASE_DIR"
  if shasum -a 256 -c SHA256SUMS >"$TMP_DIR/release-sha256.log" 2>&1; then
    :
  else
    cat "$TMP_DIR/release-sha256.log" >&2
    exit 1
  fi
) && ok "release internal SHA256SUMS verified" || fatal "release internal SHA256SUMS failed"

CSD_POOL_ROOT="$RELEASE_DIR" \
CSD_POOL_BIN_DIR="$RELEASE_DIR/bin" \
CSD_POOL_VERIFY_HTTP=0 \
CSD_POOL_VERIFY_RELEASE=1 \
CSD_POOL_VERIFY_RELEASE_ARCHIVE="$RELEASE_ARCHIVE" \
  "$RELEASE_DIR/ops/bin/csd-pool-verify.sh" \
  >"$TMP_DIR/release-verify.log" 2>&1 \
  && ok "release archive verified by packaged release verifier" \
  || { cat "$TMP_DIR/release-verify.log" >&2; fatal "release archive failed packaged release verifier"; }

CSD_POOL_ROOT="$RELEASE_DIR" \
CSD_POOL_GO_LIVE_VERIFY_SCRIPT="$RELEASE_DIR/ops/bin/csd-pool-verify-go-live-evidence.sh" \
  "$RELEASE_DIR/ops/bin/csd-pool-verify-real-go-live-receipt.sh" "$RECEIPT_ARCHIVE" ${RECEIPT_SHA256:+"$RECEIPT_SHA256"} \
  >"$TMP_DIR/receipt-verify.log" 2>&1 \
  && ok "real go-live receipt verified by release verifier" \
  || { cat "$TMP_DIR/receipt-verify.log" >&2; fatal "real go-live receipt failed release verifier"; }

CSD_POOL_ROOT="$RELEASE_DIR" \
  "$RELEASE_DIR/ops/bin/csd-pool-verify-public-acceptance-evidence.sh" "$ACCEPTANCE_ARCHIVE" ${ACCEPTANCE_SHA256:+"$ACCEPTANCE_SHA256"} \
  >"$TMP_DIR/public-acceptance-verify.log" 2>&1 \
  && ok "public acceptance evidence verified by release verifier" \
  || { cat "$TMP_DIR/public-acceptance-verify.log" >&2; fatal "public acceptance evidence failed release verifier"; }

require_file "$ACCEPTANCE_DIR/public-acceptance-summary.json" "public acceptance summary"
acceptance_status="$(json_query "$ACCEPTANCE_DIR/public-acceptance-summary.json" status)"
[[ "$acceptance_status" == "passed" ]] && ok "public acceptance summary passed" || fatal "public acceptance summary is not passed"

acceptance_receipt="$(json_query "$ACCEPTANCE_DIR/public-acceptance-summary.json" receipt_archive)"
if [[ "$(basename "$acceptance_receipt")" == "$(basename "$RECEIPT_ARCHIVE")" ]]; then
  ok "public acceptance evidence references the supplied receipt"
else
  fatal "public acceptance evidence references a different receipt: $acceptance_receipt"
fi

acceptance_receipt_sha="$(json_query "$ACCEPTANCE_DIR/public-acceptance-summary.json" receipt_archive_sha256)"
if [[ "$acceptance_receipt_sha" == "$(sha256_value "$RECEIPT_ARCHIVE")" ]]; then
  ok "public acceptance evidence receipt sha256 matches supplied receipt"
else
  fatal "public acceptance evidence receipt sha256 mismatch"
fi

if python3 - "$RECEIPT_DIR/go-live-summary.json" "$ACCEPTANCE_DIR/http-public-status.json" >"$TMP_DIR/public-status-release-binding.log" 2>&1 <<'PY'
import json
import sys

summary_path, status_path = sys.argv[1:3]
with open(summary_path, "r", encoding="utf-8") as f:
    summary = json.load(f)
with open(status_path, "r", encoding="utf-8") as f:
    status = json.load(f)

expected = summary.get("release") or {}
actual = status.get("release") or {}
failed = False
for field in ("name", "revision", "timestamp_utc"):
    expected_value = expected.get(field)
    actual_value = actual.get(field)
    print(f"{field}: expected={expected_value} actual={actual_value}")
    if not expected_value or expected_value == "unknown" or actual_value != expected_value:
        failed = True
version = actual.get("version")
print(f"version: actual={version or 'missing'}")
if not version:
    failed = True
sys.exit(1 if failed else 0)
PY
then
  ok "public acceptance status release matches receipt"
else
  cat "$TMP_DIR/public-status-release-binding.log" >&2
  fatal "public acceptance status release mismatch with receipt"
fi

receipt_verify_summary="$(grep -F 'summary:' "$ACCEPTANCE_DIR/receipt-verify.log" | tail -n1 || true)"
if [[ "$receipt_verify_summary" == *"fail=0"* ]]; then
  ok "public acceptance embedded receipt verification passed"
else
  fatal "public acceptance embedded receipt verification did not pass"
fi

printf 'release_dir=%s\n' "$RELEASE_DIR"
printf 'acceptance_dir=%s\n' "$ACCEPTANCE_DIR"
printf 'summary: pass=%s fail=%s\n' "$PASS" "$FAIL"
if [[ "$FAIL" -ne 0 ]]; then
  exit 1
fi

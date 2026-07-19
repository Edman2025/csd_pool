#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
PACKAGE_ARCHIVE="${1:-${CSD_POOL_HANDOFF_PACKAGE:-}}"
PACKAGE_SHA256="${2:-${CSD_POOL_HANDOFF_PACKAGE_SHA256:-}}"
VERIFY_HANDOFF_SCRIPT="${CSD_POOL_VERIFY_LAUNCH_HANDOFF_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-launch-handoff.sh}"
TMP_ROOT="${CSD_POOL_HANDOFF_PACKAGE_TMP_DIR:-}"
KEEP_TMP="${CSD_POOL_HANDOFF_PACKAGE_KEEP_DIR:-0}"
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
  printf 'usage: %s /path/to/csd-pool-*-launch-handoff-*.tar.gz [/path/to/.sha256]\n' "$(basename "$0")" >&2
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

manifest_value() {
  local key="$1"
  awk -F= -v key="$key" '$1 == key {sub($1 FS, ""); print; exit}' "$MANIFEST"
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

validate_json_file() {
  local path="$1"
  local label="$2"
  if python3 -m json.tool "$path" >/dev/null 2>&1; then
    ok "$label JSON valid"
  else
    fatal "$label JSON invalid"
  fi
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

check_archive_sha_line() {
  local archive="$1"
  local checksum="$2"
  local expected="$3"
  local label="$4"
  local actual line
  [[ -f "$archive" ]] || fatal "$label archive missing"
  [[ -f "$checksum" ]] || fatal "$label .sha256 missing"
  actual="$(sha256_value "$archive")"
  [[ "$actual" == "$expected" ]] || fatal "$label sha256 does not match manifest"
  line="$(sed -n '1p' "$checksum" 2>/dev/null || true)"
  if [[ "$line" == "$expected "* && "$line" == *"$(basename "$archive")"* ]]; then
    ok "$label .sha256 line matches manifest"
  else
    fatal "$label .sha256 line mismatch"
  fi
}

cleanup() {
  if [[ "$KEEP_TMP" != "1" && "$OWN_TMP_DIR" == "1" && -n "${TMP_DIR:-}" && -d "$TMP_DIR" ]]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

if [[ -z "$PACKAGE_ARCHIVE" ]]; then
  usage
  exit 2
fi

[[ -f "$PACKAGE_ARCHIVE" ]] || fatal "launch handoff package not found: $PACKAGE_ARCHIVE"
ok "launch handoff package exists"
[[ -x "$VERIFY_HANDOFF_SCRIPT" ]] || fatal "launch handoff verifier not executable: $VERIFY_HANDOFF_SCRIPT"
ok "launch handoff verifier executable"

if [[ -z "$PACKAGE_SHA256" && -f "$PACKAGE_ARCHIVE.sha256" ]]; then
  PACKAGE_SHA256="$PACKAGE_ARCHIVE.sha256"
fi

if [[ -n "$PACKAGE_SHA256" ]]; then
  [[ -f "$PACKAGE_SHA256" ]] || fatal "launch handoff package .sha256 not found: $PACKAGE_SHA256"
  package_hash="$(sha256_value "$PACKAGE_ARCHIVE")"
  package_sha_line="$(sed -n '1p' "$PACKAGE_SHA256" 2>/dev/null || true)"
  if [[ "$package_sha_line" == "$package_hash "* && "$package_sha_line" == *"$(basename "$PACKAGE_ARCHIVE")"* ]]; then
    ok "launch handoff package sha256 verified"
  else
    fatal "launch handoff package sha256 mismatch"
  fi
fi

if tar -tzf "$PACKAGE_ARCHIVE" >/dev/null; then
  ok "launch handoff package can be listed"
else
  fatal "launch handoff package cannot be listed"
fi

if ! tar -tzf "$PACKAGE_ARCHIVE" | grep -E '(^/|(^|/)\.\.($|/))' >/dev/null; then
  ok "launch handoff package paths are relative and safe"
else
  fatal "launch handoff package contains unsafe paths"
fi

if [[ -z "$TMP_ROOT" ]]; then
  TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-handoff-package.XXXXXX")"
  OWN_TMP_DIR=1
else
  mkdir -p "$TMP_ROOT"
  TMP_DIR="$(mktemp -d "$TMP_ROOT/csd-pool-handoff-package.XXXXXX")"
  OWN_TMP_DIR=1
fi

tar -xzf "$PACKAGE_ARCHIVE" -C "$TMP_DIR"
ok "launch handoff package extracted"

top_count="$(find "$TMP_DIR" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')"
[[ "$top_count" == "1" ]] || fatal "launch handoff package must contain exactly one top-level directory"
PACKAGE_DIR="$(find "$TMP_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
MANIFEST="$PACKAGE_DIR/HANDOFF-MANIFEST.txt"

require_file "$MANIFEST" "HANDOFF-MANIFEST.txt"
require_file "$PACKAGE_DIR/HANDOFF-README.txt" "HANDOFF-README.txt"
require_file "$PACKAGE_DIR/HANDOFF-SHA256SUMS" "HANDOFF-SHA256SUMS"
require_file "$PACKAGE_DIR/handoff-summary.json" "handoff-summary.json"
require_file "$PACKAGE_DIR/launch-handoff.verify.log" "launch-handoff.verify.log"
require_text "$MANIFEST" "included_files:" "handoff manifest lists included files"
require_text "$MANIFEST" "HANDOFF-README.txt" "handoff manifest lists README"
require_text "$MANIFEST" "handoff-summary.json" "handoff manifest lists summary JSON"
require_text "$MANIFEST" "verify_launch_handoff=ops/bin/csd-pool-verify-launch-handoff.sh" "handoff manifest records launch verifier"
require_text "$PACKAGE_DIR/HANDOFF-README.txt" "CSD Pool Launch Handoff" "handoff README title present"
require_text "$PACKAGE_DIR/HANDOFF-README.txt" "ops/bin/csd-pool-verify-launch-handoff-package.sh" "handoff README includes verification command"

(
  cd "$PACKAGE_DIR"
  if shasum -a 256 -c HANDOFF-SHA256SUMS >"$TMP_DIR/handoff-sha256.log" 2>&1; then
    :
  else
    cat "$TMP_DIR/handoff-sha256.log" >&2
    exit 1
  fi
) && ok "handoff package internal SHA256SUMS verified" || fatal "handoff package internal SHA256SUMS failed"

release_name="$(manifest_value release_archive)"
release_sha="$(manifest_value release_archive_sha256)"
receipt_name="$(manifest_value real_go_live_receipt)"
receipt_sha="$(manifest_value real_go_live_receipt_sha256)"
acceptance_name="$(manifest_value public_acceptance_evidence)"
acceptance_sha="$(manifest_value public_acceptance_evidence_sha256)"

[[ -n "$release_name" && -n "$release_sha" ]] || fatal "release archive metadata missing from handoff manifest"
[[ -n "$receipt_name" && -n "$receipt_sha" ]] || fatal "receipt metadata missing from handoff manifest"
[[ -n "$acceptance_name" && -n "$acceptance_sha" ]] || fatal "public acceptance metadata missing from handoff manifest"
ok "handoff manifest artifact metadata present"

check_archive_sha_line "$PACKAGE_DIR/$release_name" "$PACKAGE_DIR/$release_name.sha256" "$release_sha" "release"
check_archive_sha_line "$PACKAGE_DIR/$receipt_name" "$PACKAGE_DIR/$receipt_name.sha256" "$receipt_sha" "real go-live receipt"
check_archive_sha_line "$PACKAGE_DIR/$acceptance_name" "$PACKAGE_DIR/$acceptance_name.sha256" "$acceptance_sha" "public acceptance evidence"

validate_json_file "$PACKAGE_DIR/handoff-summary.json" "handoff summary"
summary_status="$(json_query "$PACKAGE_DIR/handoff-summary.json" status)"
summary_target="$(json_query "$PACKAGE_DIR/handoff-summary.json" target)"
summary_release="$(json_query "$PACKAGE_DIR/handoff-summary.json" release_archive)"
summary_receipt="$(json_query "$PACKAGE_DIR/handoff-summary.json" real_go_live_receipt)"
summary_acceptance="$(json_query "$PACKAGE_DIR/handoff-summary.json" public_acceptance_evidence)"
summary_release_sha="$(json_query "$PACKAGE_DIR/handoff-summary.json" release_archive_sha256)"
summary_receipt_sha="$(json_query "$PACKAGE_DIR/handoff-summary.json" real_go_live_receipt_sha256)"
summary_acceptance_sha="$(json_query "$PACKAGE_DIR/handoff-summary.json" public_acceptance_evidence_sha256)"
[[ "$summary_status" == "passed" ]] && ok "handoff summary status passed" || fatal "handoff summary status is not passed"
[[ "$summary_target" == "launch-handoff" ]] && ok "handoff summary target launch-handoff" || fatal "handoff summary target mismatch"
[[ "$summary_release" == "$release_name" ]] && ok "handoff summary release matches manifest" || fatal "handoff summary release mismatch"
[[ "$summary_receipt" == "$receipt_name" ]] && ok "handoff summary receipt matches manifest" || fatal "handoff summary receipt mismatch"
[[ "$summary_acceptance" == "$acceptance_name" ]] && ok "handoff summary acceptance evidence matches manifest" || fatal "handoff summary acceptance evidence mismatch"
[[ "$summary_release_sha" == "$release_sha" ]] && ok "handoff summary release sha256 matches manifest" || fatal "handoff summary release sha256 mismatch"
[[ "$summary_receipt_sha" == "$receipt_sha" ]] && ok "handoff summary receipt sha256 matches manifest" || fatal "handoff summary receipt sha256 mismatch"
[[ "$summary_acceptance_sha" == "$acceptance_sha" ]] && ok "handoff summary acceptance sha256 matches manifest" || fatal "handoff summary acceptance sha256 mismatch"

"$VERIFY_HANDOFF_SCRIPT" "$PACKAGE_DIR/$release_name" "$PACKAGE_DIR/$receipt_name" "$PACKAGE_DIR/$acceptance_name" >"$TMP_DIR/launch-handoff-verify.log" 2>&1 \
  && ok "embedded handoff artifacts verified" \
  || { cat "$TMP_DIR/launch-handoff-verify.log" >&2; fatal "embedded handoff artifacts failed verification"; }

require_text "$PACKAGE_DIR/launch-handoff.verify.log" "summary: pass=" "export-time handoff verification summary present"
require_text "$PACKAGE_DIR/launch-handoff.verify.log" "fail=0" "export-time handoff verification passed"
require_text "$PACKAGE_DIR/launch-handoff.verify.log" "release archive verified by packaged release verifier" "export-time release archive verifier passed"
require_text "$PACKAGE_DIR/launch-handoff.verify.log" "release manifest records release archive self-test" "export-time release archive self-test manifest check passed"

printf 'extracted_dir=%s\n' "$PACKAGE_DIR"
printf 'summary: pass=%s fail=%s\n' "$PASS" "$FAIL"
if [[ "$FAIL" -ne 0 ]]; then
  exit 1
fi

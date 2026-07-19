#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
RECEIPT_ARCHIVE="${1:-${CSD_POOL_REAL_GO_LIVE_RECEIPT:-}}"
RECEIPT_SHA256="${2:-${CSD_POOL_REAL_GO_LIVE_RECEIPT_SHA256:-}}"
VERIFY_EVIDENCE_SCRIPT="${CSD_POOL_GO_LIVE_VERIFY_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-go-live-evidence.sh}"
TMP_ROOT="${CSD_POOL_RECEIPT_TMP_DIR:-}"
KEEP_TMP="${CSD_POOL_RECEIPT_KEEP_DIR:-0}"
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

summary_value() {
  local key="$1"
  awk -F= -v key="$key" '$1 == key {sub($1 FS, ""); print; exit}' "$SUMMARY_FILE"
}

manifest_value() {
  local key="$1"
  awk -F= -v key="$key" '$1 == key {sub($1 FS, ""); print; exit}' "$RECEIPT_DIR/RECEIPT-MANIFEST.txt"
}

check_receipt_redaction_safety() {
  local receipt_dir="$1"
  local log_path="$TMP_DIR/receipt-redaction-safety.log"
  if python3 - "$receipt_dir" >"$log_path" 2>&1 <<'PY'
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
        text = path.read_bytes().decode("utf-8", errors="ignore")
    except OSError as exc:
        findings.append((str(path.relative_to(root)), "read_error", str(exc)))
        continue
    checked += 1
    for name, pattern in patterns:
        match = pattern.search(text)
        if match:
            findings.append((str(path.relative_to(root)), name, match.group(0)[:160]))
if findings:
    for rel, name, sample in findings[:50]:
        print(f"finding={name} file={rel} sample={sample}")
print(f"receipt_redaction_checked_files={checked}")
print(f"receipt_redaction_findings={len(findings)}")
if findings:
    sys.exit(1)
PY
  then
    ok "receipt redaction scan passed"
  else
    cat "$log_path" >&2
    fatal "receipt redaction scan failed"
  fi
}

cleanup() {
  if [[ "$KEEP_TMP" != "1" && "$OWN_TMP_DIR" == "1" && -n "${TMP_DIR:-}" && -d "$TMP_DIR" ]]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

usage() {
  printf 'usage: %s /path/to/csd-pool-*-real-go-live-receipt-*.tar.gz [/path/to/.sha256]\n' "$(basename "$0")" >&2
}

if [[ -z "$RECEIPT_ARCHIVE" ]]; then
  usage
  exit 2
fi

[[ -f "$RECEIPT_ARCHIVE" ]] || fatal "receipt archive not found: $RECEIPT_ARCHIVE"
ok "receipt archive exists"
[[ -x "$VERIFY_EVIDENCE_SCRIPT" ]] || fatal "go-live evidence verifier not executable: $VERIFY_EVIDENCE_SCRIPT"
ok "go-live evidence verifier exists"

if [[ -z "$RECEIPT_SHA256" && -f "$RECEIPT_ARCHIVE.sha256" ]]; then
  RECEIPT_SHA256="$RECEIPT_ARCHIVE.sha256"
fi

if [[ -n "$RECEIPT_SHA256" ]]; then
  [[ -f "$RECEIPT_SHA256" ]] || fatal "receipt .sha256 not found: $RECEIPT_SHA256"
  receipt_hash="$(sha256_value "$RECEIPT_ARCHIVE")"
  receipt_sha_line="$(sed -n '1p' "$RECEIPT_SHA256" 2>/dev/null || true)"
  if [[ "$receipt_sha_line" == "$receipt_hash "* && "$receipt_sha_line" == *"$(basename "$RECEIPT_ARCHIVE")"* ]]; then
    ok "receipt archive sha256 verified"
  else
    fatal "receipt archive sha256 mismatch"
  fi
fi

if tar -tzf "$RECEIPT_ARCHIVE" >/dev/null; then
  ok "receipt archive can be listed"
else
  fatal "receipt archive cannot be listed"
fi

if ! tar -tzf "$RECEIPT_ARCHIVE" | grep -E '(^/|(^|/)\.\.($|/))' >/dev/null; then
  ok "receipt archive paths are relative and safe"
else
  fatal "receipt archive contains unsafe paths"
fi

if [[ -z "$TMP_ROOT" ]]; then
  TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-receipt-verify.XXXXXX")"
  OWN_TMP_DIR=1
else
  mkdir -p "$TMP_ROOT"
  TMP_DIR="$(mktemp -d "$TMP_ROOT/csd-pool-receipt-verify.XXXXXX")"
  OWN_TMP_DIR=1
fi

tar -xzf "$RECEIPT_ARCHIVE" -C "$TMP_DIR"
ok "receipt archive extracted"

top_count="$(find "$TMP_DIR" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')"
[[ "$top_count" == "1" ]] || fatal "receipt archive must contain exactly one top-level directory"
RECEIPT_DIR="$(find "$TMP_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"

for file in \
  RECEIPT-MANIFEST.txt \
  RECEIPT-SHA256SUMS \
  REAL-GO-LIVE-SUMMARY.txt \
  real-go-live-inputs.log \
  launch-toolchain-manifest.json \
  REAL-ENVIRONMENT-DOCTOR.txt \
  real-environment-doctor-summary.json \
  real-go-live-postcheck.log \
  GO-LIVE-SIGNOFF.md \
  GO-LIVE-REPORT.txt \
  go-live-summary.json; do
  if [[ -f "$RECEIPT_DIR/$file" ]]; then
    ok "$file exists"
  else
    fatal "$file missing from receipt"
  fi
done
check_receipt_redaction_safety "$RECEIPT_DIR"

(
  cd "$RECEIPT_DIR"
  if shasum -a 256 -c RECEIPT-SHA256SUMS >"$TMP_DIR/receipt-shasum.log" 2>&1; then
    :
  else
    cat "$TMP_DIR/receipt-shasum.log" >&2
    exit 1
  fi
) && ok "receipt internal sha256 manifest verified" || fatal "receipt internal sha256 manifest failed"

if grep -q 'included_files:' "$RECEIPT_DIR/RECEIPT-MANIFEST.txt" && \
   grep -q 'REAL-GO-LIVE-SUMMARY.txt' "$RECEIPT_DIR/RECEIPT-MANIFEST.txt" && \
   grep -q 'go-live-summary.json' "$RECEIPT_DIR/RECEIPT-MANIFEST.txt"; then
  ok "receipt manifest lists critical files"
else
  fatal "receipt manifest missing critical files"
fi

SUMMARY_FILE="$RECEIPT_DIR/REAL-GO-LIVE-SUMMARY.txt"
status="$(summary_value status)"
[[ "$status" == "passed" ]] && ok "real go-live summary status passed" || fatal "real go-live summary status is not passed"

target="$(summary_value target)"
manifest_target="$(manifest_value target)"
manifest_source_summary="$(manifest_value source_summary)"
manifest_verify_script="$(manifest_value verify_real_go_live_summary)"
[[ -n "$target" ]] && ok "real go-live summary target present" || fatal "real go-live summary target missing"
[[ -n "$manifest_target" && "$manifest_target" == "$target" ]] && ok "receipt manifest target matches summary" || fatal "receipt manifest target mismatch"
[[ -n "$manifest_source_summary" && "$(basename "$manifest_source_summary")" == "REAL-GO-LIVE-SUMMARY.txt" ]] && ok "receipt manifest source summary basename matches package summary" || fatal "receipt manifest source summary mismatch"
[[ -n "$manifest_verify_script" && "$(basename "$manifest_verify_script")" == "csd-pool-verify-real-go-live-summary.sh" ]] && ok "receipt manifest records real go-live summary verifier" || fatal "receipt manifest verifier mismatch"

inputs_sha="$(summary_value real_go_live_inputs_sha256)"
toolchain_sha="$(summary_value launch_toolchain_manifest_sha256)"
doctor_report_sha="$(summary_value real_environment_doctor_report_sha256)"
doctor_summary_sha="$(summary_value real_environment_doctor_summary_sha256)"
report_sha="$(summary_value go_live_report_sha256)"
go_live_summary_sha="$(summary_value go_live_summary_sha256)"
signoff_sha="$(summary_value go_live_signoff_sha256)"
evidence_archive_sha="$(summary_value evidence_archive_sha256)"
evidence_archive_original="$(summary_value evidence_archive)"
evidence_sha256_original="$(summary_value evidence_sha256)"

for item in \
  "$RECEIPT_DIR/real-go-live-inputs.log:$inputs_sha:real go-live inputs" \
  "$RECEIPT_DIR/launch-toolchain-manifest.json:$toolchain_sha:launch toolchain manifest" \
  "$RECEIPT_DIR/REAL-ENVIRONMENT-DOCTOR.txt:$doctor_report_sha:real environment doctor report" \
  "$RECEIPT_DIR/real-environment-doctor-summary.json:$doctor_summary_sha:real environment doctor summary" \
  "$RECEIPT_DIR/GO-LIVE-REPORT.txt:$report_sha:go-live report" \
  "$RECEIPT_DIR/go-live-summary.json:$go_live_summary_sha:go-live summary" \
  "$RECEIPT_DIR/GO-LIVE-SIGNOFF.md:$signoff_sha:go-live signoff"; do
  path="${item%%:*}"
  rest="${item#*:}"
  expected="${rest%%:*}"
  label="${rest#*:}"
  [[ -n "$expected" ]] || fatal "$label expected sha256 missing from REAL-GO-LIVE-SUMMARY.txt"
  actual="$(sha256_value "$path")"
  [[ "$actual" == "$expected" ]] && ok "$label sha256 matches summary" || fatal "$label sha256 mismatch"
done

evidence_name="$(basename "$evidence_archive_original")"
evidence_sha_name="$(basename "$evidence_sha256_original")"
[[ -n "$evidence_name" && -f "$RECEIPT_DIR/$evidence_name" ]] || fatal "evidence archive missing from receipt"
[[ -n "$evidence_sha_name" && -f "$RECEIPT_DIR/$evidence_sha_name" ]] || fatal "evidence .sha256 missing from receipt"
ok "evidence archive and .sha256 exist in receipt"

actual_evidence_sha="$(sha256_value "$RECEIPT_DIR/$evidence_name")"
[[ "$actual_evidence_sha" == "$evidence_archive_sha" ]] && ok "evidence archive sha256 matches summary" || fatal "evidence archive sha256 mismatch"

evidence_sha_line="$(sed -n '1p' "$RECEIPT_DIR/$evidence_sha_name" 2>/dev/null || true)"
if [[ "$evidence_sha_line" == "$evidence_archive_sha "* && "$evidence_sha_line" == *"$evidence_name"* ]]; then
  ok "evidence .sha256 line matches archive"
else
  fatal "evidence .sha256 line mismatch"
fi

if grep -q 'real_go_live_inputs_ok=True' "$RECEIPT_DIR/real-go-live-inputs.log" && \
   grep -q 'dry_run_env_false=True' "$RECEIPT_DIR/real-go-live-inputs.log" && \
   grep -q 'env_example_path=False' "$RECEIPT_DIR/real-go-live-inputs.log" && \
   grep -q 'config_example_path=False' "$RECEIPT_DIR/real-go-live-inputs.log"; then
  ok "real go-live inputs report proves real non-dry-run inputs"
else
  fatal "real go-live inputs report missing required proof"
fi

if python3 - "$RECEIPT_DIR/launch-toolchain-manifest.json" "$target" >"$TMP_DIR/launch-toolchain-manifest.log" 2>&1 <<'PY'
import json
import pathlib
import sys

path, target = sys.argv[1:3]
data = json.loads(pathlib.Path(path).read_text(encoding="utf-8"))
entries = data.get("entries") if isinstance(data, dict) else None
basenames = {entry.get("basename") for entry in entries or [] if isinstance(entry, dict)}
required = set(data.get("required_basenames") or [])
checks = {
    "launch_toolchain_target_matches": data.get("target") == target,
    "launch_toolchain_entries_present": isinstance(entries, list) and len(entries) >= 6,
    "launch_toolchain_required_entries_present": required.issubset(basenames),
    "launch_toolchain_entry_sha256_present": all(bool(entry.get("sha256")) for entry in entries or [] if isinstance(entry, dict)),
    "launch_toolchain_entries_executable": all(entry.get("executable") is True for entry in entries or [] if isinstance(entry, dict)),
    "launch_toolchain_workers_recorded": "csd-pool-workers" in basenames,
    "launch_toolchain_go_live_recorded": "csd-pool-go-live-check.sh" in basenames,
    "launch_toolchain_evidence_verifier_recorded": "csd-pool-verify-go-live-evidence.sh" in basenames,
    "launch_toolchain_signoff_recorded": "csd-pool-generate-signoff.sh" in basenames,
    "launch_toolchain_receipt_exporter_recorded": "csd-pool-export-real-go-live-receipt.sh" in basenames,
    "launch_toolchain_doctor_recorded": "csd-pool-real-env-doctor.sh" in basenames,
}
for name, passed in checks.items():
    print(f"{name}={passed}")
if not all(checks.values()):
    raise SystemExit(1)
PY
then
  ok "launch toolchain manifest proves real launch scripts"
else
  cat "$TMP_DIR/launch-toolchain-manifest.log" >&2
  fatal "launch toolchain manifest missing required proof"
fi

if grep -q 'status=ready_for_real_go_live' "$RECEIPT_DIR/REAL-ENVIRONMENT-DOCTOR.txt" && \
   grep -q '"status": "ready_for_real_go_live"' "$RECEIPT_DIR/real-environment-doctor-summary.json" && \
   grep -q '"hard_failures": 0' "$RECEIPT_DIR/real-environment-doctor-summary.json"; then
  ok "real environment doctor proves ready inputs"
else
  fatal "real environment doctor report missing ready proof"
fi

if grep -q 'real_go_live_postcheck_ok=True' "$RECEIPT_DIR/real-go-live-postcheck.log" && \
   grep -q 'summary_dry_run_false=True' "$RECEIPT_DIR/real-go-live-postcheck.log" && \
   grep -q 'sha256_line_matches_archive=True' "$RECEIPT_DIR/real-go-live-postcheck.log"; then
  ok "real go-live postcheck report proves accepted archive"
else
  fatal "real go-live postcheck report missing required proof"
fi

if grep -Fq '# CSD Pool Go-Live Signoff' "$RECEIPT_DIR/GO-LIVE-SIGNOFF.md" && \
   grep -Fq "archive_sha256: \`$evidence_archive_sha\`" "$RECEIPT_DIR/GO-LIVE-SIGNOFF.md" && \
   grep -Fq 'pool-endpoint-binding.log' "$RECEIPT_DIR/GO-LIVE-SIGNOFF.md" && \
   grep -Fq 'http-api-pool.json' "$RECEIPT_DIR/GO-LIVE-SIGNOFF.md" && \
   grep -Fq 'external-public-pool-binding.log' "$RECEIPT_DIR/GO-LIVE-SIGNOFF.md" && \
   grep -Fq 'http-public-api-pool.json' "$RECEIPT_DIR/GO-LIVE-SIGNOFF.md"; then
  ok "go-live signoff records archive sha and public pool evidence"
else
  fatal "go-live signoff missing archive sha or public pool evidence"
fi

"$VERIFY_EVIDENCE_SCRIPT" "$RECEIPT_DIR/$evidence_name" >"$TMP_DIR/evidence-verify.log" 2>&1 \
  && ok "embedded go-live evidence archive verified" \
  || { cat "$TMP_DIR/evidence-verify.log" >&2; fatal "embedded go-live evidence archive failed verification"; }

printf 'extracted_dir=%s\n' "$RECEIPT_DIR"
printf 'summary: pass=%s fail=%s\n' "$PASS" "$FAIL"
if [[ "$FAIL" -ne 0 ]]; then
  exit 1
fi

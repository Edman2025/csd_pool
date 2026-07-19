#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
EVIDENCE_ARCHIVE="${1:-${CSD_POOL_PUBLIC_ACCEPTANCE_EVIDENCE:-}}"
EVIDENCE_SHA256="${2:-${CSD_POOL_PUBLIC_ACCEPTANCE_EVIDENCE_SHA256:-}}"
TMP_ROOT="${CSD_POOL_PUBLIC_ACCEPTANCE_TMP_DIR:-}"
KEEP_TMP="${CSD_POOL_PUBLIC_ACCEPTANCE_KEEP_DIR:-0}"
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
  printf 'usage: %s /path/to/public-acceptance-evidence.tar.gz [/path/to/.sha256]\n' "$(basename "$0")" >&2
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

require_file() {
  local path="$1"
  local label="$2"
  if [[ -f "$path" ]]; then
    ok "$label exists"
  else
    fatal "$label missing"
  fi
}

require_text() {
  local path="$1"
  local pattern="$2"
  local label="$3"
  if grep -Fq "$pattern" "$path"; then
    ok "$label"
  else
    fatal "$label missing"
  fi
}

contains_fixture_marker() {
  local value
  value="$(printf '%s' "${1:-}" | tr '[:upper:]' '[:lower:]')"
  case "$value" in
    *fixture*|*example.com*|*example.net*|*example.org*|*pool.example*|*change-me*|*placeholder*)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
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

check_summary_report_bindings() {
  local summary="$1"
  if python3 - "$summary" >"$TMP_DIR/public-acceptance-summary-report-bindings.log" 2>&1 <<'PY'
import json
import os
import sys

summary_path = sys.argv[1]
expected = {
    "receipt_verify": "receipt-verify.log",
    "receipt_binding": "receipt-binding.log",
    "api_health": "http-public-health.json",
    "api_status": "http-public-status.json",
    "status_release_binding": "public-status-release-binding.log",
    "api_pool": "http-public-pool.json",
    "api_getting_started": "http-public-getting-started.json",
    "getting_started_binding": "getting-started-binding.log",
    "public_endpoint_routability": "public-endpoint-routability.log",
    "stratum_smoke": "public-stratum-smoke.json",
    "stratum_submit_probe": "public-stratum-submit-probe.json",
    "stratum_load": "public-stratum-load.json",
    "canary_miner": "public-canary-miner.json",
    "canary_miner_api": "http-public-canary-miner.json",
    "canary_miner_workers_api": "http-public-canary-miner-workers.json",
}
with open(summary_path, "r", encoding="utf-8") as handle:
    summary = json.load(handle)
reports = summary.get("reports")
if not isinstance(reports, dict):
    print("reports_object_present=False")
    raise SystemExit(1)
ok = True
for key, expected_name in expected.items():
    value = reports.get(key)
    matches = isinstance(value, str) and os.path.basename(value) == expected_name
    print(f"report_{key}_matches={matches}")
    if not matches:
        ok = False
if not ok:
    raise SystemExit(1)
PY
  then
    ok "public acceptance summary report paths match package files"
  else
    cat "$TMP_DIR/public-acceptance-summary-report-bindings.log" >&2
    fatal "public acceptance summary report path binding failed"
  fi
}

check_public_acceptance_redaction_safety() {
  local evidence_dir="$1"
  local log_path="$TMP_DIR/public-acceptance-redaction-safety.log"
  if python3 - "$evidence_dir" >"$log_path" 2>&1 <<'PY'
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
print(f"public_acceptance_redaction_checked_files={checked}")
print(f"public_acceptance_redaction_findings={len(findings)}")
if findings:
    sys.exit(1)
PY
  then
    ok "public acceptance redaction scan passed"
  else
    cat "$log_path" >&2
    fatal "public acceptance redaction scan failed"
  fi
}

cleanup() {
  if [[ "$KEEP_TMP" != "1" && "$OWN_TMP_DIR" == "1" && -n "${TMP_DIR:-}" && -d "$TMP_DIR" ]]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

if [[ -z "$EVIDENCE_ARCHIVE" ]]; then
  usage
  exit 2
fi

[[ -f "$EVIDENCE_ARCHIVE" ]] || fatal "public acceptance evidence archive not found: $EVIDENCE_ARCHIVE"
ok "public acceptance evidence archive exists"

if [[ -z "$EVIDENCE_SHA256" && -f "$EVIDENCE_ARCHIVE.sha256" ]]; then
  EVIDENCE_SHA256="$EVIDENCE_ARCHIVE.sha256"
fi

if [[ -n "$EVIDENCE_SHA256" ]]; then
  [[ -f "$EVIDENCE_SHA256" ]] || fatal "public acceptance evidence .sha256 not found: $EVIDENCE_SHA256"
  evidence_hash="$(sha256_value "$EVIDENCE_ARCHIVE")"
  evidence_sha_line="$(sed -n '1p' "$EVIDENCE_SHA256" 2>/dev/null || true)"
  if [[ "$evidence_sha_line" == "$evidence_hash "* && "$evidence_sha_line" == *"$(basename "$EVIDENCE_ARCHIVE")"* ]]; then
    ok "public acceptance evidence archive sha256 verified"
  else
    fatal "public acceptance evidence archive sha256 mismatch"
  fi
fi

if tar -tzf "$EVIDENCE_ARCHIVE" >/dev/null; then
  ok "public acceptance evidence archive can be listed"
else
  fatal "public acceptance evidence archive cannot be listed"
fi

if ! tar -tzf "$EVIDENCE_ARCHIVE" | grep -E '(^/|(^|/)\.\.($|/))' >/dev/null; then
  ok "public acceptance evidence archive paths are relative and safe"
else
  fatal "public acceptance evidence archive contains unsafe paths"
fi

if [[ -z "$TMP_ROOT" ]]; then
  TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-public-acceptance-verify.XXXXXX")"
  OWN_TMP_DIR=1
else
  mkdir -p "$TMP_ROOT"
  TMP_DIR="$(mktemp -d "$TMP_ROOT/csd-pool-public-acceptance-verify.XXXXXX")"
  OWN_TMP_DIR=1
fi

tar -xzf "$EVIDENCE_ARCHIVE" -C "$TMP_DIR"
ok "public acceptance evidence archive extracted"

top_count="$(find "$TMP_DIR" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' ')"
[[ "$top_count" == "1" ]] || fatal "public acceptance evidence archive must contain exactly one top-level directory"
EVIDENCE_DIR="$(find "$TMP_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"

require_file "$EVIDENCE_DIR/PUBLIC-ACCEPTANCE-SHA256SUMS" "PUBLIC-ACCEPTANCE-SHA256SUMS"
require_file "$EVIDENCE_DIR/PUBLIC-ACCEPTANCE-REPORT.txt" "PUBLIC-ACCEPTANCE-REPORT.txt"
require_file "$EVIDENCE_DIR/public-acceptance-summary.json" "public-acceptance-summary.json"
require_file "$EVIDENCE_DIR/acceptance-toolchain-manifest.json" "acceptance-toolchain-manifest.json"
require_file "$EVIDENCE_DIR/receipt-verify.log" "receipt-verify.log"
require_file "$EVIDENCE_DIR/receipt-binding.log" "receipt-binding.log"
require_file "$EVIDENCE_DIR/http-public-health.json" "http-public-health.json"
require_file "$EVIDENCE_DIR/http-public-status.json" "http-public-status.json"
require_file "$EVIDENCE_DIR/public-status-release-binding.log" "public-status-release-binding.log"
require_file "$EVIDENCE_DIR/http-public-pool.json" "http-public-pool.json"
require_file "$EVIDENCE_DIR/http-public-getting-started.json" "http-public-getting-started.json"
require_file "$EVIDENCE_DIR/getting-started-binding.log" "getting-started-binding.log"
require_file "$EVIDENCE_DIR/public-endpoint-routability.log" "public-endpoint-routability.log"
require_file "$EVIDENCE_DIR/public-stratum-smoke.json" "public-stratum-smoke.json"
require_file "$EVIDENCE_DIR/public-stratum-submit-probe.json" "public-stratum-submit-probe.json"
require_file "$EVIDENCE_DIR/public-stratum-load.json" "public-stratum-load.json"
require_file "$EVIDENCE_DIR/http-public-canary-miner.json" "http-public-canary-miner.json"
require_file "$EVIDENCE_DIR/http-public-canary-miner-workers.json" "http-public-canary-miner-workers.json"
require_file "$EVIDENCE_DIR/public-canary-miner.json" "public-canary-miner.json"
check_public_acceptance_redaction_safety "$EVIDENCE_DIR"

(
  cd "$EVIDENCE_DIR"
  if shasum -a 256 -c PUBLIC-ACCEPTANCE-SHA256SUMS >"$TMP_DIR/public-acceptance-shasum.log" 2>&1; then
    :
  else
    cat "$TMP_DIR/public-acceptance-shasum.log" >&2
    exit 1
  fi
) && ok "public acceptance internal sha256 manifest verified" || fatal "public acceptance internal sha256 manifest failed"

validate_json_file "$EVIDENCE_DIR/public-acceptance-summary.json" "public acceptance summary"
validate_json_file "$EVIDENCE_DIR/acceptance-toolchain-manifest.json" "public acceptance toolchain manifest"
validate_json_file "$EVIDENCE_DIR/http-public-health.json" "public /health"
validate_json_file "$EVIDENCE_DIR/http-public-status.json" "public /api/status"
validate_json_file "$EVIDENCE_DIR/http-public-pool.json" "public /api/pool"
validate_json_file "$EVIDENCE_DIR/http-public-getting-started.json" "public /api/getting-started"
validate_json_file "$EVIDENCE_DIR/public-stratum-smoke.json" "public Stratum smoke"
validate_json_file "$EVIDENCE_DIR/public-stratum-submit-probe.json" "public Stratum submit probe"
validate_json_file "$EVIDENCE_DIR/public-stratum-load.json" "public Stratum load"
validate_json_file "$EVIDENCE_DIR/http-public-canary-miner.json" "public canary miner profile"
validate_json_file "$EVIDENCE_DIR/http-public-canary-miner-workers.json" "public canary miner workers"
validate_json_file "$EVIDENCE_DIR/public-canary-miner.json" "public canary miner"
check_summary_report_bindings "$EVIDENCE_DIR/public-acceptance-summary.json"

status="$(json_query "$EVIDENCE_DIR/public-acceptance-summary.json" status)"
[[ "$status" == "passed" ]] && ok "public acceptance summary status passed" || fatal "public acceptance summary status is not passed"

summary_fail="$(json_query "$EVIDENCE_DIR/public-acceptance-summary.json" fail)"
[[ "$summary_fail" == "0" ]] && ok "public acceptance summary fail count is zero" || fatal "public acceptance summary fail count is not zero"
summary_pass="$(json_query "$EVIDENCE_DIR/public-acceptance-summary.json" pass)"
summary_skip="$(json_query "$EVIDENCE_DIR/public-acceptance-summary.json" skip)"

receipt_archive="$(json_query "$EVIDENCE_DIR/public-acceptance-summary.json" receipt_archive)"
receipt_archive_sha256="$(json_query "$EVIDENCE_DIR/public-acceptance-summary.json" receipt_archive_sha256)"
public_api_url="$(json_query "$EVIDENCE_DIR/public-acceptance-summary.json" public_api_url)"
public_stratum_addr="$(json_query "$EVIDENCE_DIR/public-acceptance-summary.json" public_stratum_addr)"
acceptance_toolchain_manifest="$(json_query "$EVIDENCE_DIR/public-acceptance-summary.json" acceptance_toolchain_manifest)"
public_status_release_name="$(json_query "$EVIDENCE_DIR/public-acceptance-summary.json" public_status_release.name)"
public_status_release_revision="$(json_query "$EVIDENCE_DIR/public-acceptance-summary.json" public_status_release.revision)"
public_status_release_built_at="$(json_query "$EVIDENCE_DIR/public-acceptance-summary.json" public_status_release.built_at)"
public_status_release_version="$(json_query "$EVIDENCE_DIR/public-acceptance-summary.json" public_status_release.version)"
[[ "$receipt_archive" != "missing" && -n "$receipt_archive" ]] && ok "public acceptance summary records receipt archive" || fatal "public acceptance summary receipt archive missing"
[[ "$receipt_archive_sha256" =~ ^[0-9a-f]{64}$ ]] && ok "public acceptance summary records receipt sha256" || fatal "public acceptance summary receipt sha256 missing or invalid"
[[ "$public_status_release_name" != "missing" && -n "$public_status_release_name" ]] && ok "public acceptance summary records public status release name" || fatal "public acceptance summary public status release name missing"
[[ "$public_status_release_revision" != "missing" && -n "$public_status_release_revision" ]] && ok "public acceptance summary records public status release revision" || fatal "public acceptance summary public status release revision missing"
[[ "$public_status_release_built_at" != "missing" && -n "$public_status_release_built_at" ]] && ok "public acceptance summary records public status release build time" || fatal "public acceptance summary public status release build time missing"
[[ "$public_status_release_version" != "missing" && -n "$public_status_release_version" ]] && ok "public acceptance summary records public status release version" || fatal "public acceptance summary public status release version missing"
[[ "$(basename "$acceptance_toolchain_manifest")" == "acceptance-toolchain-manifest.json" ]] && ok "public acceptance summary records toolchain manifest" || fatal "public acceptance summary toolchain manifest missing"

report_status="$(awk -F= '$1 == "status" {print $2; exit}' "$EVIDENCE_DIR/PUBLIC-ACCEPTANCE-REPORT.txt")"
report_fail="$(awk -F= '$1 == "fail" {print $2; exit}' "$EVIDENCE_DIR/PUBLIC-ACCEPTANCE-REPORT.txt")"
report_pass="$(awk -F= '$1 == "pass" {print $2; exit}' "$EVIDENCE_DIR/PUBLIC-ACCEPTANCE-REPORT.txt")"
report_skip="$(awk -F= '$1 == "skip" {print $2; exit}' "$EVIDENCE_DIR/PUBLIC-ACCEPTANCE-REPORT.txt")"
report_public_api_url="$(awk -F= '$1 == "public_api_url" {sub($1 FS, ""); print; exit}' "$EVIDENCE_DIR/PUBLIC-ACCEPTANCE-REPORT.txt")"
report_public_stratum_addr="$(awk -F= '$1 == "public_stratum_addr" {sub($1 FS, ""); print; exit}' "$EVIDENCE_DIR/PUBLIC-ACCEPTANCE-REPORT.txt")"
report_receipt_archive="$(awk -F= '$1 == "receipt_archive" {sub($1 FS, ""); print; exit}' "$EVIDENCE_DIR/PUBLIC-ACCEPTANCE-REPORT.txt")"
report_receipt_sha="$(awk -F= '$1 == "receipt_archive_sha256" {print $2; exit}' "$EVIDENCE_DIR/PUBLIC-ACCEPTANCE-REPORT.txt")"
report_release_name="$(awk -F= '$1 == "public_status_release_name" {sub($1 FS, ""); print; exit}' "$EVIDENCE_DIR/PUBLIC-ACCEPTANCE-REPORT.txt")"
report_release_revision="$(awk -F= '$1 == "public_status_release_revision" {sub($1 FS, ""); print; exit}' "$EVIDENCE_DIR/PUBLIC-ACCEPTANCE-REPORT.txt")"
report_release_built_at="$(awk -F= '$1 == "public_status_release_built_at" {sub($1 FS, ""); print; exit}' "$EVIDENCE_DIR/PUBLIC-ACCEPTANCE-REPORT.txt")"
report_release_version="$(awk -F= '$1 == "public_status_release_version" {sub($1 FS, ""); print; exit}' "$EVIDENCE_DIR/PUBLIC-ACCEPTANCE-REPORT.txt")"
[[ "$report_status" == "passed" ]] && ok "public acceptance report status passed" || fatal "public acceptance report status is not passed"
[[ "$report_fail" == "0" ]] && ok "public acceptance report fail count is zero" || fatal "public acceptance report fail count is not zero"
[[ "$report_fail" == "$summary_fail" ]] && ok "public acceptance report fail count matches summary" || fatal "public acceptance report fail count mismatch"
[[ "$report_pass" == "$summary_pass" ]] && ok "public acceptance report pass count matches summary" || fatal "public acceptance report pass count mismatch"
[[ "$report_skip" == "$summary_skip" ]] && ok "public acceptance report skip count matches summary" || fatal "public acceptance report skip count mismatch"
[[ "${report_public_api_url%/}" == "${public_api_url%/}" ]] && ok "public acceptance report API URL matches summary" || fatal "public acceptance report API URL mismatch"
[[ "$report_public_stratum_addr" == "$public_stratum_addr" ]] && ok "public acceptance report Stratum address matches summary" || fatal "public acceptance report Stratum address mismatch"
[[ "$report_receipt_archive" == "$receipt_archive" ]] && ok "public acceptance report receipt archive matches summary" || fatal "public acceptance report receipt archive mismatch"
[[ "$report_receipt_sha" == "$receipt_archive_sha256" ]] && ok "public acceptance report receipt sha256 matches summary" || fatal "public acceptance report receipt sha256 mismatch"
[[ "$report_release_name" == "$public_status_release_name" ]] && ok "public acceptance report release name matches summary" || fatal "public acceptance report release name mismatch"
[[ "$report_release_revision" == "$public_status_release_revision" ]] && ok "public acceptance report release revision matches summary" || fatal "public acceptance report release revision mismatch"
[[ "$report_release_built_at" == "$public_status_release_built_at" ]] && ok "public acceptance report release build time matches summary" || fatal "public acceptance report release build time mismatch"
[[ "$report_release_version" == "$public_status_release_version" ]] && ok "public acceptance report release version matches summary" || fatal "public acceptance report release version mismatch"

if python3 - "$EVIDENCE_DIR/public-acceptance-summary.json" "$EVIDENCE_DIR/http-public-status.json" >"$TMP_DIR/public-acceptance-status-release-summary.log" 2>&1 <<'PY'
import json
import sys

summary_path, status_path = sys.argv[1:3]
with open(summary_path, "r", encoding="utf-8") as handle:
    summary = json.load(handle)
with open(status_path, "r", encoding="utf-8") as handle:
    status = json.load(handle)
summary_release = summary.get("public_status_release") or {}
status_release = status.get("release") or {}
fields = ["name", "revision", "built_at", "version"]
checks = {
    f"public_status_release_{field}_summary_matches_http": bool(summary_release.get(field))
    and summary_release.get(field) != "missing"
    and summary_release.get(field) == status_release.get(field)
    for field in fields
}
for key, value in checks.items():
    print(f"{key}={value}")
if not all(checks.values()):
    raise SystemExit(1)
PY
then
  ok "public acceptance summary release identity matches public status"
else
  cat "$TMP_DIR/public-acceptance-status-release-summary.log" >&2
  fatal "public acceptance summary release identity mismatch"
fi

[[ "$public_api_url" == https://* ]] && ok "public acceptance API URL is HTTPS" || fatal "public acceptance API URL is not HTTPS"
[[ "$public_stratum_addr" == *:* && "$public_stratum_addr" != "missing" ]] && ok "public acceptance Stratum address present" || fatal "public acceptance Stratum address missing"
if contains_fixture_marker "$public_api_url" || contains_fixture_marker "$public_stratum_addr"; then
  fatal "public acceptance endpoints contain fixture markers"
else
  ok "public acceptance endpoints have no fixture markers"
fi

if python3 - "$EVIDENCE_DIR/acceptance-toolchain-manifest.json" "$public_api_url" "$public_stratum_addr" >"$TMP_DIR/public-acceptance-toolchain.log" 2>&1 <<'PY'
import json
import pathlib
import sys

manifest_path, public_api_url, public_stratum_addr = sys.argv[1:4]
data = json.loads(pathlib.Path(manifest_path).read_text(encoding="utf-8"))
entries = data.get("entries") if isinstance(data, dict) else None
basenames = {entry.get("basename") for entry in entries or [] if isinstance(entry, dict)}
required = set(data.get("required_basenames") or [])
checks = {
    "acceptance_toolchain_target_matches": data.get("target") == "public-acceptance",
    "acceptance_toolchain_public_api_matches": (data.get("public_api_url") or "").rstrip("/") == public_api_url.rstrip("/"),
    "acceptance_toolchain_public_stratum_matches": data.get("public_stratum_addr") == public_stratum_addr,
    "acceptance_toolchain_entries_present": isinstance(entries, list) and len(entries) >= 3,
    "acceptance_toolchain_required_entries_present": required.issubset(basenames),
    "acceptance_toolchain_entry_sha256_present": all(bool(entry.get("sha256")) and entry.get("sha256") != "missing" for entry in entries or [] if isinstance(entry, dict)),
    "acceptance_toolchain_entries_executable": all(entry.get("executable") is True for entry in entries or [] if isinstance(entry, dict)),
    "acceptance_toolchain_acceptance_script_recorded": "csd-pool-public-acceptance.sh" in basenames,
    "acceptance_toolchain_receipt_verifier_recorded": "csd-pool-verify-real-go-live-receipt.sh" in basenames,
    "acceptance_toolchain_workers_recorded": "csd-pool-workers" in basenames,
}
for key, value in checks.items():
    print(f"{key}={value}")
if not all(checks.values()):
    raise SystemExit(1)
PY
then
  ok "public acceptance toolchain manifest proves external verifier tools"
else
  cat "$TMP_DIR/public-acceptance-toolchain.log" >&2
  fatal "public acceptance toolchain manifest missing required proof"
fi

require_text "$EVIDENCE_DIR/receipt-verify.log" "summary: pass=" "receipt verification summary present"
require_text "$EVIDENCE_DIR/receipt-verify.log" "fail=0" "receipt verification passed"
require_text "$EVIDENCE_DIR/receipt-verify.log" "launch toolchain manifest proves real launch scripts" "receipt launch toolchain proof present"
require_text "$EVIDENCE_DIR/receipt-binding.log" "receipt_public_api_matches=True" "receipt public API binding matches"
require_text "$EVIDENCE_DIR/receipt-binding.log" "receipt_public_stratum_matches=True" "receipt public Stratum binding matches"
require_text "$EVIDENCE_DIR/public-status-release-binding.log" "release_name_matches=True" "public status release name matches receipt"
require_text "$EVIDENCE_DIR/public-status-release-binding.log" "release_revision_matches=True" "public status release revision matches receipt"
require_text "$EVIDENCE_DIR/public-status-release-binding.log" "release_built_at_matches=True" "public status release build timestamp matches receipt"
require_text "$EVIDENCE_DIR/public-status-release-binding.log" "release_version_present=True" "public status release version present"
require_text "$EVIDENCE_DIR/public-status-release-binding.log" "public_status_release_binding_ok=True" "public status release binding passed"
require_text "$EVIDENCE_DIR/public-endpoint-routability.log" "public_api_dns_all_global=True" "public API endpoint resolves globally"
require_text "$EVIDENCE_DIR/public-endpoint-routability.log" "public_stratum_dns_all_global=True" "public Stratum endpoint resolves globally"
require_text "$EVIDENCE_DIR/public-endpoint-routability.log" "public_endpoint_routability_ok=True" "public endpoint routability passed"
require_text "$EVIDENCE_DIR/getting-started-binding.log" "stratum_endpoint_matches=True" "getting-started Stratum endpoint matches"
require_text "$EVIDENCE_DIR/getting-started-binding.log" "commands_include_stratum_endpoint=True" "getting-started commands include Stratum endpoint"

if python3 - "$EVIDENCE_DIR/public-stratum-smoke.json" "$EVIDENCE_DIR/public-stratum-load.json" >"$TMP_DIR/public-acceptance-stratum.log" 2>&1 <<'PY'
import json
import sys

smoke_path, load_path = sys.argv[1:3]
with open(smoke_path, "r", encoding="utf-8") as handle:
    smoke = json.load(handle)
with open(load_path, "r", encoding="utf-8") as handle:
    load = json.load(handle)

checks = {
    "smoke_failed_clients_zero": smoke.get("failed_clients") == 0,
    "smoke_succeeded_requested": smoke.get("requested_clients", 0) > 0 and smoke.get("succeeded_clients") == smoke.get("requested_clients"),
}
if load.get("skipped") is True:
    checks["load_skipped_or_passed"] = True
else:
    min_success = load.get("min_success_clients", load.get("requested_clients", 0))
    checks["load_skipped_or_passed"] = (
        load.get("failed_clients") == 0
        and load.get("requested_clients", 0) > 0
        and load.get("succeeded_clients", 0) >= min_success
    )

for key, value in checks.items():
    print(f"{key}={value}")
if not all(checks.values()):
    sys.exit(1)
PY
then
  ok "public Stratum smoke/load acceptance reports passed"
else
  cat "$TMP_DIR/public-acceptance-stratum.log" >&2
  fatal "public Stratum smoke/load acceptance reports failed"
fi

if python3 - "$EVIDENCE_DIR/public-stratum-submit-probe.json" >"$TMP_DIR/public-stratum-submit-probe.log" 2>&1 <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    probe = json.load(handle)
checks = {
    "submit_probe_passed": probe.get("passed") is True,
    "submit_probe_saw_difficulty": probe.get("difficulty_seen") is True,
    "submit_probe_saw_notify": probe.get("notify_seen") is True,
    "submit_response_received": probe.get("submit_response_received") is True,
    "submit_response_standard": probe.get("submit_response_standard") is True,
    "submit_response_meaningful": probe.get("submit_result") is True or probe.get("submit_error_code") is not None,
}
for key, value in checks.items():
    print(f"{key}={value}")
if not all(checks.values()):
    sys.exit(1)
PY
then
  ok "public Stratum submit probe acceptance report passed"
else
  cat "$TMP_DIR/public-stratum-submit-probe.log" >&2
  fatal "public Stratum submit probe acceptance report failed"
fi

if python3 - "$EVIDENCE_DIR/public-acceptance-summary.json" "$EVIDENCE_DIR/public-stratum-smoke.json" "$EVIDENCE_DIR/http-public-canary-miner.json" "$EVIDENCE_DIR/http-public-canary-miner-workers.json" "$EVIDENCE_DIR/public-canary-miner.json" >"$TMP_DIR/public-canary-miner.log" 2>&1 <<'PY'
import json
import sys
import time

summary_path, smoke_path, miner_path, workers_path, canary_path = sys.argv[1:6]
with open(summary_path, "r", encoding="utf-8") as handle:
    summary = json.load(handle)
with open(smoke_path, "r", encoding="utf-8") as handle:
    smoke = json.load(handle)
with open(miner_path, "r", encoding="utf-8") as handle:
    miner = json.load(handle)
with open(workers_path, "r", encoding="utf-8") as handle:
    workers = json.load(handle)
with open(canary_path, "r", encoding="utf-8") as handle:
    canary = json.load(handle)

success_workers = [
    item.get("worker")
    for item in smoke.get("successes") or []
    if isinstance(item, dict) and isinstance(item.get("worker"), str)
]
canary_address = canary.get("canary_address")
canary_source = canary.get("canary_source", "smoke-success-worker")
worker_rows = workers.get("workers") if isinstance(workers, dict) else []
accepted_share_required = summary.get("accepted_share_required") in (True, "1", "true", "yes")
canary_declared_required = canary.get("accepted_share_required")
if canary_declared_required is None:
    canary_declared_required = False
accepted_share_minimum = int(summary.get("accepted_share_minimum") or 1)
canary_declared_minimum = int(canary.get("accepted_share_minimum") or 0)
canary_max_age_seconds = int(summary.get("canary_max_age_seconds") or canary.get("canary_max_age_seconds") or 600)
canary_declared_max_age_seconds = int(canary.get("canary_max_age_seconds") or 0)
shares_accepted = int(miner.get("shares_accepted") or 0)
try:
    last_seen_ts = int(miner.get("last_seen_ts") or 0)
except (TypeError, ValueError):
    last_seen_ts = 0
now_ts = int(time.time())
last_seen_age_seconds = now_ts - last_seen_ts if last_seen_ts > 0 else None
checks = {
    "smoke_success_worker_present": len(success_workers) > 0,
    "canary_status_passed": canary.get("status") == "passed",
    "canary_address_matches_smoke_success_or_configured": canary_source == "configured" or canary_address in success_workers,
    "accepted_share_canary_source_configured": (not accepted_share_required) or canary_source == "configured",
    "miner_address_matches_canary": miner.get("address") == canary_address,
    "miner_online": miner.get("online") is True,
    "workers_online_positive": int(miner.get("workers_online") or 0) >= 1,
    "worker_rows_present": isinstance(worker_rows, list) and len(worker_rows) >= 1,
    "accepted_share_requirement_declared_consistently": canary_declared_required == accepted_share_required,
    "accepted_share_minimum_declared_consistently": canary_declared_minimum == accepted_share_minimum,
    "canary_max_age_declared_consistently": canary_declared_max_age_seconds == canary_max_age_seconds,
    "last_seen_ts_present": last_seen_ts > 0,
    "last_seen_not_from_future": last_seen_ts > 0 and last_seen_age_seconds >= -30,
    "last_seen_within_max_age": last_seen_ts > 0 and last_seen_age_seconds <= canary_max_age_seconds,
}
embedded = canary.get("checks") if isinstance(canary.get("checks"), dict) else {}
for key in [
    "miner_address_matches",
    "miner_online",
    "workers_online_positive",
    "worker_rows_present",
    "last_seen_ts_present",
    "last_seen_not_from_future",
    "last_seen_within_max_age",
]:
    checks[f"embedded_{key}"] = embedded.get(key) is True
if accepted_share_required:
    checks["accepted_share_minimum_positive"] = accepted_share_minimum >= 1
    checks["accepted_share_minimum_met"] = shares_accepted >= accepted_share_minimum
    checks["embedded_accepted_share_minimum_met"] = embedded.get("accepted_share_minimum_met") is True

for key, value in checks.items():
    print(f"{key}={value}")
if not all(checks.values()):
    sys.exit(1)
PY
then
  ok "public canary miner acceptance report passed"
else
  cat "$TMP_DIR/public-canary-miner.log" >&2
  fatal "public canary miner acceptance report failed"
fi

printf 'extracted_dir=%s\n' "$EVIDENCE_DIR"
printf 'summary: pass=%s fail=%s\n' "$PASS" "$FAIL"
if [[ "$FAIL" -ne 0 ]]; then
  exit 1
fi

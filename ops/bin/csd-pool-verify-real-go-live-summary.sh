#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
SUMMARY_PATH="${1:-${CSD_POOL_REAL_GO_LIVE_SUMMARY:-}}"
VERIFY_SCRIPT="${CSD_POOL_GO_LIVE_VERIFY_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-go-live-evidence.sh}"

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
}

sha256_value() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
  else
    fail "sha256 tool missing"
  fi
}

summary_value() {
  local key="$1"
  awk -F= -v key="$key" '$1 == key {sub($1 FS, ""); print; exit}' "$SUMMARY_PATH"
}

usage() {
  printf 'usage: %s /path/to/REAL-GO-LIVE-SUMMARY.txt\n' "$(basename "$0")" >&2
}

if [[ -z "$SUMMARY_PATH" ]]; then
  usage
  exit 2
fi

[[ -f "$SUMMARY_PATH" ]] || fail "REAL-GO-LIVE-SUMMARY.txt not found: $SUMMARY_PATH"
[[ -x "$VERIFY_SCRIPT" ]] || fail "go-live evidence verifier not executable: $VERIFY_SCRIPT"

SUMMARY_PATH="$(cd "$(dirname "$SUMMARY_PATH")" && pwd)/$(basename "$SUMMARY_PATH")"
status="$(summary_value status)"
[[ "$status" == "passed" ]] || fail "real go-live summary status is not passed: ${status:-missing}"

target="$(summary_value target)"
report_path="$(summary_value go_live_report)"
report_sha="$(summary_value go_live_report_sha256)"
inputs_path="$(summary_value real_go_live_inputs)"
inputs_sha="$(summary_value real_go_live_inputs_sha256)"
toolchain_path="$(summary_value launch_toolchain_manifest)"
toolchain_sha="$(summary_value launch_toolchain_manifest_sha256)"
doctor_report_path="$(summary_value real_environment_doctor_report)"
doctor_report_sha="$(summary_value real_environment_doctor_report_sha256)"
doctor_summary_path="$(summary_value real_environment_doctor_summary)"
doctor_summary_sha="$(summary_value real_environment_doctor_summary_sha256)"
go_live_summary_path="$(summary_value go_live_summary)"
go_live_summary_sha="$(summary_value go_live_summary_sha256)"
signoff_path="$(summary_value go_live_signoff)"
signoff_sha="$(summary_value go_live_signoff_sha256)"
postcheck_path="$(summary_value real_go_live_postcheck)"
evidence_archive="$(summary_value evidence_archive)"
evidence_sha256_path="$(summary_value evidence_sha256)"
evidence_archive_sha="$(summary_value evidence_archive_sha256)"

python3 - \
  "$SUMMARY_PATH" \
  "$target" \
  "$inputs_path" "$inputs_sha" \
  "$toolchain_path" "$toolchain_sha" \
  "$doctor_report_path" "$doctor_report_sha" \
  "$doctor_summary_path" "$doctor_summary_sha" \
  "$report_path" "$report_sha" \
  "$go_live_summary_path" "$go_live_summary_sha" \
  "$signoff_path" "$signoff_sha" \
  "$postcheck_path" \
  "$evidence_archive" "$evidence_sha256_path" "$evidence_archive_sha" <<'PY'
import pathlib
import sys

(
    summary_path,
    target,
    inputs_path,
    inputs_sha,
    toolchain_path,
    toolchain_sha,
    doctor_report_path,
    doctor_report_sha,
    doctor_summary_path,
    doctor_summary_sha,
    report_path,
    report_sha,
    go_live_summary_path,
    go_live_summary_sha,
    signoff_path,
    signoff_sha,
    postcheck_path,
    evidence_archive,
    evidence_sha256_path,
    evidence_archive_sha,
) = sys.argv[1:21]

checks = {
    "real_go_live_summary_exists": pathlib.Path(summary_path).is_file(),
    "real_go_live_target_present": bool(target),
    "real_go_live_target_allowed": target in {"private-beta", "public-beta", "production"},
    "real_go_live_inputs_path_present": bool(inputs_path),
    "real_go_live_inputs_sha256_present": bool(inputs_sha),
    "launch_toolchain_manifest_path_present": bool(toolchain_path),
    "launch_toolchain_manifest_sha256_present": bool(toolchain_sha),
    "real_environment_doctor_report_path_present": bool(doctor_report_path),
    "real_environment_doctor_report_sha256_present": bool(doctor_report_sha),
    "real_environment_doctor_summary_path_present": bool(doctor_summary_path),
    "real_environment_doctor_summary_sha256_present": bool(doctor_summary_sha),
    "go_live_report_path_present": bool(report_path),
    "go_live_report_sha256_present": bool(report_sha),
    "go_live_summary_path_present": bool(go_live_summary_path),
    "go_live_summary_sha256_present": bool(go_live_summary_sha),
    "go_live_signoff_path_present": bool(signoff_path),
    "go_live_signoff_sha256_present": bool(signoff_sha),
    "real_go_live_postcheck_path_present": bool(postcheck_path),
    "evidence_archive_path_present": bool(evidence_archive),
    "evidence_sha256_path_present": bool(evidence_sha256_path),
    "evidence_archive_sha256_present": bool(evidence_archive_sha),
}
for label, raw_path in [
    ("real_go_live_inputs_exists", inputs_path),
    ("launch_toolchain_manifest_exists", toolchain_path),
    ("real_environment_doctor_report_exists", doctor_report_path),
    ("real_environment_doctor_summary_exists", doctor_summary_path),
    ("go_live_report_exists", report_path),
    ("go_live_summary_exists", go_live_summary_path),
    ("go_live_signoff_exists", signoff_path),
    ("real_go_live_postcheck_exists", postcheck_path),
    ("evidence_archive_exists", evidence_archive),
    ("evidence_sha256_exists", evidence_sha256_path),
]:
    checks[label] = pathlib.Path(raw_path).is_file() if raw_path else False

postcheck_text = pathlib.Path(postcheck_path).read_text(encoding="utf-8", errors="replace") if checks["real_go_live_postcheck_exists"] else ""
checks["postcheck_ok_recorded"] = "real_go_live_postcheck_ok=True" in postcheck_text
checks["postcheck_dry_run_false_recorded"] = "summary_dry_run_false=True" in postcheck_text
checks["postcheck_sha256_match_recorded"] = "sha256_line_matches_archive=True" in postcheck_text
checks["postcheck_target_match_recorded"] = "summary_target_matches=True" in postcheck_text

inputs_text = pathlib.Path(inputs_path).read_text(encoding="utf-8", errors="replace") if checks["real_go_live_inputs_exists"] else ""
checks["inputs_ok_recorded"] = "real_go_live_inputs_ok=True" in inputs_text
checks["inputs_target_matches_summary"] = f"target={target}" in inputs_text
checks["inputs_dry_run_false_recorded"] = "dry_run_env_false=True" in inputs_text
checks["inputs_env_not_example_recorded"] = "env_example_path=False" in inputs_text
checks["inputs_config_not_example_recorded"] = "config_example_path=False" in inputs_text
checks["inputs_workers_executable_recorded"] = "workers_bin_executable=True" in inputs_text
if "public_required=True" in inputs_text:
    checks["inputs_public_api_https_recorded"] = "public_api_https=True" in inputs_text
    checks["inputs_public_stratum_recorded"] = "public_stratum_addr=missing" not in inputs_text

doctor_report_text = pathlib.Path(doctor_report_path).read_text(encoding="utf-8", errors="replace") if checks["real_environment_doctor_report_exists"] else ""
doctor_summary_text = pathlib.Path(doctor_summary_path).read_text(encoding="utf-8", errors="replace") if checks["real_environment_doctor_summary_exists"] else ""
if doctor_summary_text:
    import json

    try:
        doctor_summary = json.loads(doctor_summary_text)
    except json.JSONDecodeError:
        doctor_summary = {}
    checks["doctor_summary_status_ready"] = doctor_summary.get("status") == "ready_for_real_go_live"
    checks["doctor_summary_target_matches"] = doctor_summary.get("go_live_target") == target
    checks["doctor_summary_hard_failures_zero"] = doctor_summary.get("hard_failures") == 0
checks["doctor_report_status_ready"] = "status=ready_for_real_go_live" in doctor_report_text

toolchain_text = pathlib.Path(toolchain_path).read_text(encoding="utf-8", errors="replace") if checks["launch_toolchain_manifest_exists"] else ""
if toolchain_text:
    import json

    try:
        toolchain = json.loads(toolchain_text)
    except json.JSONDecodeError:
        toolchain = {}
    entries = toolchain.get("entries") if isinstance(toolchain, dict) else None
    basenames = {entry.get("basename") for entry in entries or [] if isinstance(entry, dict)}
    sha_values_present = all(bool(entry.get("sha256")) for entry in entries or [] if isinstance(entry, dict))
    executable_values_present = all(entry.get("executable") is True for entry in entries or [] if isinstance(entry, dict))
    required = set(toolchain.get("required_basenames") or [])
    checks["launch_toolchain_target_matches"] = toolchain.get("target") == target
    checks["launch_toolchain_entries_present"] = isinstance(entries, list) and len(entries) >= 6
    checks["launch_toolchain_required_entries_present"] = required.issubset(basenames)
    checks["launch_toolchain_entry_sha256_present"] = sha_values_present
    checks["launch_toolchain_entries_executable"] = executable_values_present

summary_json_text = pathlib.Path(go_live_summary_path).read_text(encoding="utf-8", errors="replace") if checks["go_live_summary_exists"] else ""
if summary_json_text:
    import json

    try:
        go_live_summary = json.loads(summary_json_text)
    except json.JSONDecodeError:
        go_live_summary = {}
    checks["go_live_summary_target_matches"] = go_live_summary.get("target") == target
    checks["go_live_summary_dry_run_false"] = go_live_summary.get("dry_run") is False
    checks["go_live_summary_status_passed"] = (go_live_summary.get("summary") or {}).get("status") == "passed"

for name, passed in checks.items():
    print(f"{name}={passed}")
if not all(checks.values()):
    sys.exit(1)
PY

for item in \
  "$inputs_path:$inputs_sha:real go-live inputs" \
  "$toolchain_path:$toolchain_sha:launch toolchain manifest" \
  "$doctor_report_path:$doctor_report_sha:real environment doctor report" \
  "$doctor_summary_path:$doctor_summary_sha:real environment doctor summary" \
  "$report_path:$report_sha:go-live report" \
  "$go_live_summary_path:$go_live_summary_sha:go-live summary" \
  "$signoff_path:$signoff_sha:go-live signoff" \
  "$evidence_archive:$evidence_archive_sha:evidence archive"; do
  path="${item%%:*}"
  rest="${item#*:}"
  expected="${rest%%:*}"
  label="${rest#*:}"
  actual="$(sha256_value "$path")"
  if [[ "$actual" == "$expected" ]]; then
    printf 'ok: %s sha256 matches\n' "$label"
  else
    fail "$label sha256 mismatch: expected $expected got $actual"
  fi
done

sha_line="$(sed -n '1p' "$evidence_sha256_path" 2>/dev/null || true)"
if [[ "$sha_line" == "$evidence_archive_sha "* && "$sha_line" == *"$(basename "$evidence_archive")"* ]]; then
  printf 'ok: evidence .sha256 line matches archive\n'
else
  fail "evidence .sha256 line does not match archive"
fi

"$VERIFY_SCRIPT" "$evidence_archive"

printf 'real_go_live_summary=%s\n' "$SUMMARY_PATH"
printf 'summary: real go-live summary verified\n'

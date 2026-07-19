#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
PACKAGE_ARCHIVE="${1:-${CSD_POOL_HANDOFF_PACKAGE:-}}"
PACKAGE_SHA256="${2:-${CSD_POOL_HANDOFF_PACKAGE_SHA256:-}}"
VERIFY_PACKAGE_SCRIPT="${CSD_POOL_VERIFY_LAUNCH_HANDOFF_PACKAGE_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-launch-handoff-package.sh}"
OUTPUT_DIR="${CSD_POOL_READINESS_REPORT_DIR:-/tmp/csd-pool-launch-readiness}"
KEEP_TMP="${CSD_POOL_READINESS_KEEP_DIR:-0}"
ALLOW_NON_LAUNCHABLE="${CSD_POOL_READINESS_ALLOW_NON_LAUNCHABLE:-0}"
OWN_TMP_DIR=0

fail() {
  printf 'fail: %s\n' "$1" >&2
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
    fail "sha256 tool missing"
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

[[ -f "$PACKAGE_ARCHIVE" ]] || fail "launch handoff package not found: $PACKAGE_ARCHIVE"
[[ -x "$VERIFY_PACKAGE_SCRIPT" ]] || fail "launch handoff package verifier not executable: $VERIFY_PACKAGE_SCRIPT"

if [[ -z "$PACKAGE_SHA256" && -f "$PACKAGE_ARCHIVE.sha256" ]]; then
  PACKAGE_SHA256="$PACKAGE_ARCHIVE.sha256"
fi

mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-readiness.XXXXXX")"
OWN_TMP_DIR=1

VERIFY_LOG="$OUTPUT_DIR/launch-readiness-package-verify.log"
if [[ -n "$PACKAGE_SHA256" ]]; then
  "$VERIFY_PACKAGE_SCRIPT" "$PACKAGE_ARCHIVE" "$PACKAGE_SHA256" >"$VERIFY_LOG" 2>&1 \
    || { cat "$VERIFY_LOG" >&2; fail "launch handoff package verification failed"; }
else
  "$VERIFY_PACKAGE_SCRIPT" "$PACKAGE_ARCHIVE" >"$VERIFY_LOG" 2>&1 \
    || { cat "$VERIFY_LOG" >&2; fail "launch handoff package verification failed"; }
fi

tar -xzf "$PACKAGE_ARCHIVE" -C "$TMP_DIR"
PACKAGE_DIR="$(find "$TMP_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
[[ -n "$PACKAGE_DIR" && -d "$PACKAGE_DIR" ]] || fail "launch handoff package did not extract to one directory"

RECEIPT_ARCHIVE="$(find "$PACKAGE_DIR" -maxdepth 1 -type f -name 'csd-pool-*-real-go-live-receipt-*.tar.gz' | head -n1)"
ACCEPTANCE_ARCHIVE="$(find "$PACKAGE_DIR" -maxdepth 1 -type f -name 'public-acceptance-evidence.tar.gz' | head -n1)"
[[ -n "$RECEIPT_ARCHIVE" ]] || fail "real go-live receipt missing from handoff package"
[[ -n "$ACCEPTANCE_ARCHIVE" ]] || fail "public acceptance evidence missing from handoff package"

RECEIPT_DIR="$TMP_DIR/receipt"
ACCEPTANCE_DIR="$TMP_DIR/acceptance"
mkdir -p "$RECEIPT_DIR" "$ACCEPTANCE_DIR"
tar -xzf "$RECEIPT_ARCHIVE" -C "$RECEIPT_DIR"
tar -xzf "$ACCEPTANCE_ARCHIVE" -C "$ACCEPTANCE_DIR"

GO_LIVE_SUMMARY="$(find "$RECEIPT_DIR" -type f -name 'go-live-summary.json' | head -n1)"
REAL_INPUTS_LOG="$(find "$RECEIPT_DIR" -type f -name 'real-go-live-inputs.log' | head -n1)"
REAL_POSTCHECK_LOG="$(find "$RECEIPT_DIR" -type f -name 'real-go-live-postcheck.log' | head -n1)"
ACCEPTANCE_SUMMARY="$(find "$ACCEPTANCE_DIR" -type f -name 'public-acceptance-summary.json' | head -n1)"
ACCEPTANCE_REPORT="$(find "$ACCEPTANCE_DIR" -type f -name 'PUBLIC-ACCEPTANCE-REPORT.txt' | head -n1)"
ACCEPTANCE_STATUS_REPORT="$(find "$ACCEPTANCE_DIR" -type f -name 'http-public-status.json' | head -n1)"
ROUTABILITY_REPORT="$(find "$ACCEPTANCE_DIR" -type f -name 'public-endpoint-routability.log' | head -n1)"
SUBMIT_PROBE_REPORT="$(find "$ACCEPTANCE_DIR" -type f -name 'public-stratum-submit-probe.json' | head -n1)"
CANARY_REPORT="$(find "$ACCEPTANCE_DIR" -type f -name 'public-canary-miner.json' | head -n1)"
ACCEPTANCE_TOOLCHAIN_REPORT="$(find "$ACCEPTANCE_DIR" -type f -name 'acceptance-toolchain-manifest.json' | head -n1)"

[[ -n "$GO_LIVE_SUMMARY" ]] || fail "go-live-summary.json missing from receipt"
[[ -n "$ACCEPTANCE_SUMMARY" ]] || fail "public-acceptance-summary.json missing from acceptance evidence"

CSD_POOL_READINESS_HANDOFF_SHA256="$(sha256_value "$PACKAGE_ARCHIVE")" python3 - \
  "$PACKAGE_ARCHIVE" \
  "$VERIFY_LOG" \
  "$PACKAGE_DIR/handoff-summary.json" \
  "$GO_LIVE_SUMMARY" \
  "${REAL_INPUTS_LOG:-}" \
  "${REAL_POSTCHECK_LOG:-}" \
  "$ACCEPTANCE_SUMMARY" \
  "${ACCEPTANCE_REPORT:-}" \
  "${ACCEPTANCE_STATUS_REPORT:-}" \
  "${ROUTABILITY_REPORT:-}" \
  "${SUBMIT_PROBE_REPORT:-}" \
  "${CANARY_REPORT:-}" \
  "${ACCEPTANCE_TOOLCHAIN_REPORT:-}" \
  "$OUTPUT_DIR/launch-readiness-summary.json" \
  "$OUTPUT_DIR/LAUNCH-READINESS-REPORT.txt" <<'PY'
import json
import os
import pathlib
import sys
from datetime import datetime, timezone

(
    package_archive,
    verify_log,
    handoff_summary_path,
    go_live_summary_path,
    real_inputs_log,
    real_postcheck_log,
    acceptance_summary_path,
    acceptance_report_path,
    acceptance_status_path,
    routability_report_path,
    submit_probe_path,
    canary_report_path,
    acceptance_toolchain_path,
    summary_out,
    report_out,
) = sys.argv[1:16]


def load_json(path):
    if not path:
        return None
    with open(path, "r", encoding="utf-8") as handle:
        return json.load(handle)


def text(path):
    if not path:
        return ""
    try:
        return pathlib.Path(path).read_text(encoding="utf-8")
    except FileNotFoundError:
        return ""


def nested(data, *keys):
    value = data
    for key in keys:
        if not isinstance(value, dict):
            return None
        value = value.get(key)
    return value


def add_check(checks, key, passed, severity, evidence, detail=""):
    checks.append(
        {
            "key": key,
            "passed": bool(passed),
            "severity": severity,
            "evidence": evidence,
            "detail": detail,
        }
    )


def contains_fixture_marker(value):
    if value is None:
        return False
    lowered = str(value).lower()
    markers = (
        "fixture",
        "example.com",
        "example.net",
        "example.org",
        "pool.example",
        "change-me",
        "placeholder",
    )
    return any(marker in lowered for marker in markers)


def release_time(payload):
    if not isinstance(payload, dict):
        return None
    return payload.get("built_at") or payload.get("timestamp_utc")


handoff_summary = load_json(handoff_summary_path)
go_live = load_json(go_live_summary_path)
acceptance = load_json(acceptance_summary_path)
acceptance_status = load_json(acceptance_status_path) if acceptance_status_path else None
submit_probe = load_json(submit_probe_path) if submit_probe_path else None
canary = load_json(canary_report_path) if canary_report_path else None
acceptance_toolchain = load_json(acceptance_toolchain_path) if acceptance_toolchain_path else None
routability_text = text(routability_report_path)
inputs_text = text(real_inputs_log)
postcheck_text = text(real_postcheck_log)

checks = []
add_check(
    checks,
    "handoff_package_verified",
    "fail=0" in text(verify_log),
    "hard",
    verify_log,
    "launch handoff package verifier must pass with fail=0",
)
add_check(
    checks,
    "handoff_summary_passed",
    handoff_summary.get("status") == "passed" and handoff_summary.get("target") == "launch-handoff",
    "hard",
    handoff_summary_path,
    "handoff-summary.json must be passed and target launch-handoff",
)

go_live_summary = go_live.get("summary") or {}
go_live_endpoints = go_live.get("endpoints") or {}
receipt_public_api = go_live_endpoints.get("public_api_url")
receipt_public_stratum = go_live_endpoints.get("public_stratum_probe_addr") or go_live_endpoints.get("public_stratum_addr")
acceptance_public_api = acceptance.get("public_api_url")
acceptance_public_stratum = acceptance.get("public_stratum_addr")
accepted_share_minimum = int(acceptance.get("accepted_share_minimum") or 1)
accepted_share_required = (
    os.environ.get("CSD_POOL_READINESS_REQUIRE_PUBLIC_ACCEPTED_SHARE") == "1"
    or go_live.get("target") == "production"
    or acceptance.get("accepted_share_required") in (True, "1", "true", "yes")
)

add_check(
    checks,
    "real_go_live_summary_passed",
    go_live_summary.get("status") == "passed" and int(go_live_summary.get("fail", 1)) == 0,
    "hard",
    go_live_summary_path,
    "real go-live summary status must be passed with zero failures",
)
add_check(
    checks,
    "real_go_live_non_dry_run",
    go_live.get("dry_run") is False and "CSD_POOL_GO_LIVE_DRY_RUN=1" not in inputs_text,
    "hard",
    real_inputs_log or go_live_summary_path,
    "real launch evidence must be non-dry-run",
)
add_check(
    checks,
    "real_go_live_target_launch_mode",
    go_live.get("target") in {"public-beta", "production"},
    "hard",
    go_live_summary_path,
    "target must be public-beta or production",
)
add_check(
    checks,
    "real_go_live_zero_skips",
    int(go_live_summary.get("skip", 1)) == 0,
    "hard",
    go_live_summary_path,
    "real launch gate should not skip required production checks",
)
add_check(
    checks,
    "real_go_live_public_api_https",
    isinstance(receipt_public_api, str) and receipt_public_api.startswith("https://"),
    "hard",
    go_live_summary_path,
    "public API URL must be HTTPS",
)
add_check(
    checks,
    "real_go_live_public_stratum_present",
    isinstance(receipt_public_stratum, str) and ":" in receipt_public_stratum,
    "hard",
    go_live_summary_path,
    "public Stratum probe address must be present",
)

fixture_fields = {
    "host": go_live.get("host"),
    "kernel": go_live.get("kernel"),
    "release.revision": nested(go_live, "release", "revision"),
    "release.version": nested(go_live, "release", "version"),
    "config.sha256": nested(go_live, "config", "sha256"),
    "env.sha256": nested(go_live, "env", "sha256"),
    "workers.sha256": nested(go_live, "workers", "sha256"),
    "public_api_url": receipt_public_api,
    "public_stratum_addr": receipt_public_stratum,
}
fixture_hits = [key for key, value in fixture_fields.items() if contains_fixture_marker(value)]
add_check(
    checks,
    "real_go_live_no_fixture_markers",
    not fixture_hits,
    "hard",
    go_live_summary_path,
    "fixture/example/placeholder markers found: " + ", ".join(fixture_hits) if fixture_hits else "no fixture markers in launch identity fields",
)
add_check(
    checks,
    "real_go_live_inputs_verified",
    "real_go_live_inputs_ok=True" in inputs_text,
    "hard",
    real_inputs_log or "",
    "real-go-live-inputs.log must prove real inputs",
)
add_check(
    checks,
    "real_go_live_postcheck_verified",
    "real_go_live_postcheck_ok=True" in postcheck_text,
    "hard",
    real_postcheck_log or "",
    "real-go-live-postcheck.log must prove postcheck verification",
)

add_check(
    checks,
    "public_acceptance_summary_passed",
    acceptance.get("status") == "passed" and int(acceptance.get("fail", 1)) == 0,
    "hard",
    acceptance_summary_path,
    "public acceptance must pass with zero failures",
)
add_check(
    checks,
    "public_acceptance_public_api_https",
    isinstance(acceptance_public_api, str) and acceptance_public_api.startswith("https://"),
    "hard",
    acceptance_summary_path,
    "public acceptance API URL must be HTTPS",
)
add_check(
    checks,
    "public_acceptance_endpoint_matches_receipt",
    acceptance_public_api == receipt_public_api and acceptance_public_stratum == receipt_public_stratum,
    "hard",
    acceptance_summary_path,
    f"receipt=({receipt_public_api}, {receipt_public_stratum}) acceptance=({acceptance_public_api}, {acceptance_public_stratum})",
)
acceptance_fixture_hits = [
    key
    for key, value in {
        "public_api_url": acceptance_public_api,
        "public_stratum_addr": acceptance_public_stratum,
    }.items()
    if contains_fixture_marker(value)
]
add_check(
    checks,
    "public_acceptance_no_fixture_markers",
    not acceptance_fixture_hits,
    "hard",
    acceptance_summary_path,
    "fixture/example markers found: " + ", ".join(acceptance_fixture_hits) if acceptance_fixture_hits else "no fixture markers in public acceptance endpoints",
)
add_check(
    checks,
    "public_acceptance_endpoint_routability_global",
    "public_api_dns_all_global=True" in routability_text
    and "public_stratum_dns_all_global=True" in routability_text
    and "public_endpoint_routability_ok=True" in routability_text,
    "hard",
    routability_report_path or "",
    "public acceptance must prove API and Stratum DNS resolve to global public addresses",
)

receipt_release = go_live.get("release") or {}
acceptance_release = acceptance.get("public_status_release") or {}
status_release = (acceptance_status or {}).get("release") or {}
release_identity_ok = all(
    [
        bool(status_release.get("name")) and status_release.get("name") == receipt_release.get("name"),
        bool(status_release.get("revision")) and status_release.get("revision") == receipt_release.get("revision"),
        bool(release_time(status_release)) and release_time(status_release) == release_time(receipt_release),
        bool(acceptance_release.get("name")) and acceptance_release.get("name") == status_release.get("name"),
        bool(acceptance_release.get("revision")) and acceptance_release.get("revision") == status_release.get("revision"),
        bool(release_time(acceptance_release)) and release_time(acceptance_release) == release_time(status_release),
        bool(acceptance_release.get("version")) and acceptance_release.get("version") == status_release.get("version"),
    ]
)
add_check(
    checks,
    "public_status_release_identity_matches_receipt",
    release_identity_ok,
    "hard",
    acceptance_status_path or acceptance_summary_path,
    "public /api/status release identity must match public acceptance summary and real go-live receipt",
)

toolchain_entries = acceptance_toolchain.get("entries") if isinstance(acceptance_toolchain, dict) else []
toolchain_basenames = {
    entry.get("basename")
    for entry in toolchain_entries or []
    if isinstance(entry, dict)
}
toolchain_required = set((acceptance_toolchain or {}).get("required_basenames") or [])
toolchain_ok = bool(acceptance_toolchain) and all(
    [
        acceptance_toolchain.get("target") == "public-acceptance",
        (acceptance_toolchain.get("public_api_url") or "").rstrip("/") == (acceptance_public_api or "").rstrip("/"),
        acceptance_toolchain.get("public_stratum_addr") == acceptance_public_stratum,
        {"csd-pool-public-acceptance.sh", "csd-pool-verify-real-go-live-receipt.sh", "csd-pool-workers"}.issubset(toolchain_basenames),
        toolchain_required.issubset(toolchain_basenames),
        all(bool(entry.get("sha256")) and entry.get("sha256") != "missing" for entry in toolchain_entries or [] if isinstance(entry, dict)),
        all(entry.get("executable") is True for entry in toolchain_entries or [] if isinstance(entry, dict)),
    ]
)
add_check(
    checks,
    "public_acceptance_toolchain_manifest_verified",
    toolchain_ok,
    "hard",
    acceptance_toolchain_path or "",
    "public acceptance evidence must bind the external acceptance script, receipt verifier, and workers binary used by the reviewer",
)

canary_shares_accepted = 0
canary_accepted_share_met = False
canary_declared_minimum = 0
canary_recently_seen = False
canary_source_configured = False
if canary:
    try:
        canary_shares_accepted = int(((canary.get("miner") or {}).get("shares_accepted")) or 0)
    except (TypeError, ValueError):
        canary_shares_accepted = 0
    try:
        canary_declared_minimum = int(canary.get("accepted_share_minimum") or 0)
    except (TypeError, ValueError):
        canary_declared_minimum = 0
    canary_accepted_share_met = (
        canary_shares_accepted >= accepted_share_minimum
        and ((canary.get("checks") or {}).get("accepted_share_minimum_met") is True)
    )
    canary_recently_seen = (canary.get("checks") or {}).get("last_seen_within_max_age") is True
    canary_source_configured = canary.get("canary_source") == "configured"

submit_accepted = bool(submit_probe and submit_probe.get("submit_result") is True)
public_accepted_share_observed = (
    canary_accepted_share_met if accepted_share_required else (submit_accepted or canary_accepted_share_met)
)
accepted_share_detail = (
    f"submit_result={submit_accepted}, canary_shares_accepted={canary_shares_accepted}, "
    f"minimum={accepted_share_minimum}, required={accepted_share_required}"
)

if submit_probe:
    add_check(
        checks,
        "public_stratum_submit_response_standard",
        submit_probe.get("passed") is True
        and submit_probe.get("submit_response_received") is True
        and submit_probe.get("submit_response_standard") is True,
        "hard",
        submit_probe_path,
        "public Stratum must return a standard mining.submit response",
    )
    add_check(
        checks,
        "public_stratum_accepted_share_observed",
        public_accepted_share_observed,
        "hard" if accepted_share_required else "advisory",
        submit_probe_path,
        accepted_share_detail,
    )
else:
    add_check(
        checks,
        "public_stratum_submit_response_standard",
        False,
        "hard",
        "",
        "public-stratum-submit-probe.json missing",
    )
    add_check(
        checks,
        "public_stratum_accepted_share_observed",
        canary_accepted_share_met,
        "hard" if accepted_share_required else "advisory",
        "",
        accepted_share_detail,
    )

if canary:
    canary_checks = canary.get("checks") or {}
    add_check(
        checks,
        "public_canary_accepted_share_minimum_matches_summary",
        canary_declared_minimum == accepted_share_minimum,
        "hard",
        canary_report_path,
        f"summary_minimum={accepted_share_minimum}, canary_minimum={canary_declared_minimum}",
    )
    add_check(
        checks,
        "public_canary_accepted_share_minimum_met",
        canary_accepted_share_met,
        "hard" if accepted_share_required else "advisory",
        canary_report_path,
        accepted_share_detail,
    )
    add_check(
        checks,
        "public_canary_miner_recently_seen",
        canary_recently_seen,
        "hard",
        canary_report_path,
        "public canary miner last_seen_ts must be present and within the accepted max age",
    )
    add_check(
        checks,
        "public_canary_source_configured_when_required",
        (not accepted_share_required) or canary_source_configured,
        "hard" if accepted_share_required else "advisory",
        canary_report_path,
        "accepted-share launch evidence must use CSD_POOL_ACCEPTANCE_CANARY_ADDRESS, not the automatic smoke worker",
    )
    add_check(
        checks,
        "public_canary_miner_visible",
        canary.get("status") == "passed"
        and canary_checks.get("miner_online") is True
        and canary_checks.get("workers_online_positive") is True,
        "hard",
        canary_report_path,
        "public API must show the Stratum canary miner online",
    )
else:
    add_check(
        checks,
        "public_canary_accepted_share_minimum_met",
        False,
        "hard" if accepted_share_required else "advisory",
        "",
        "public-canary-miner.json missing",
    )
    add_check(checks, "public_canary_miner_recently_seen", False, "hard", "", "public-canary-miner.json missing")
    add_check(
        checks,
        "public_canary_source_configured_when_required",
        False,
        "hard" if accepted_share_required else "advisory",
        "",
        "public-canary-miner.json missing",
    )
    add_check(checks, "public_canary_miner_visible", False, "hard", "", "public-canary-miner.json missing")

hard_failures = [check for check in checks if check["severity"] == "hard" and not check["passed"]]
advisories = [check for check in checks if check["severity"] == "advisory" and not check["passed"]]
status = "launch_ready" if not hard_failures else "needs_real_environment_evidence"

summary = {
    "status": status,
    "target": "launch-readiness",
    "generated_at_utc": datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
    "handoff_package": package_archive,
    "handoff_package_sha256": os.environ.get("CSD_POOL_READINESS_HANDOFF_SHA256", ""),
    "launch_ready": status == "launch_ready",
    "hard_failures": len(hard_failures),
    "advisories": len(advisories),
    "receipt_public_api_url": receipt_public_api,
    "receipt_public_stratum_addr": receipt_public_stratum,
    "acceptance_public_api_url": acceptance_public_api,
    "acceptance_public_stratum_addr": acceptance_public_stratum,
    "public_accepted_share_required": accepted_share_required,
    "public_accepted_share_observed": public_accepted_share_observed,
    "public_accepted_share_minimum": accepted_share_minimum,
    "public_canary_accepted_share_minimum": canary_declared_minimum,
    "public_canary_shares_accepted": canary_shares_accepted,
    "checks": checks,
    "reports": {
        "package_verify_log": verify_log,
        "handoff_summary": handoff_summary_path,
        "go_live_summary": go_live_summary_path,
        "real_inputs_log": real_inputs_log,
        "real_postcheck_log": real_postcheck_log,
        "public_acceptance_summary": acceptance_summary_path,
        "public_acceptance_report": acceptance_report_path,
        "public_endpoint_routability": routability_report_path,
        "public_stratum_submit_probe": submit_probe_path,
        "public_canary_miner": canary_report_path,
        "public_acceptance_toolchain_manifest": acceptance_toolchain_path,
    },
}

pathlib.Path(summary_out).write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")

lines = [
    "CSD Pool Launch Readiness",
    f"status={status}",
    f"launch_ready={'true' if status == 'launch_ready' else 'false'}",
    f"hard_failures={len(hard_failures)}",
    f"advisories={len(advisories)}",
    f"handoff_package={package_archive}",
    f"handoff_package_sha256={os.environ.get('CSD_POOL_READINESS_HANDOFF_SHA256', 'missing')}",
    f"receipt_public_api_url={receipt_public_api or 'missing'}",
    f"receipt_public_stratum_addr={receipt_public_stratum or 'missing'}",
    f"acceptance_public_api_url={acceptance_public_api or 'missing'}",
    f"acceptance_public_stratum_addr={acceptance_public_stratum or 'missing'}",
    f"public_accepted_share_minimum={accepted_share_minimum}",
    f"public_canary_accepted_share_minimum={canary_declared_minimum}",
    f"public_canary_shares_accepted={canary_shares_accepted}",
    "",
    "Hard Checks",
]
for check in checks:
    if check["severity"] != "hard":
        continue
    prefix = "ok" if check["passed"] else "missing"
    lines.append(f"{prefix}: {check['key']} - {check['detail']}")
lines.append("")
lines.append("Advisory Checks")
for check in checks:
    if check["severity"] != "advisory":
        continue
    prefix = "ok" if check["passed"] else "advisory"
    lines.append(f"{prefix}: {check['key']} - {check['detail']}")
lines.append("")
lines.append(f"summary_json={summary_out}")
pathlib.Path(report_out).write_text("\n".join(lines) + "\n", encoding="utf-8")
PY

status="$(python3 - "$OUTPUT_DIR/launch-readiness-summary.json" <<'PY'
import json
import sys
with open(sys.argv[1], "r", encoding="utf-8") as handle:
    print(json.load(handle).get("status", "unknown"))
PY
)"

printf 'readiness_report=%s\n' "$OUTPUT_DIR/LAUNCH-READINESS-REPORT.txt"
printf 'readiness_summary=%s\n' "$OUTPUT_DIR/launch-readiness-summary.json"
printf 'status=%s\n' "$status"

if [[ "$status" != "launch_ready" && "$ALLOW_NON_LAUNCHABLE" != "1" ]]; then
  exit 1
fi

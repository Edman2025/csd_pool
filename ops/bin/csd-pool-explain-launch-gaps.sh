#!/usr/bin/env bash
set -euo pipefail

INPUT_PATH="${1:-${CSD_POOL_GAPS_INPUT:-}}"
OUTPUT_DIR="${CSD_POOL_GAPS_OUTPUT_DIR:-}"
ALLOW_OPEN="${CSD_POOL_GAPS_ALLOW_OPEN:-0}"
KEEP_TMP="${CSD_POOL_GAPS_KEEP_DIR:-0}"
OWN_TMP_DIR=0

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
}

usage() {
  printf 'usage: %s /path/to/launch-readiness-summary.json|final-launch-summary.json|final-output-dir|launch-dossier.tar.gz\n' "$(basename "$0")" >&2
}

cleanup() {
  if [[ "$KEEP_TMP" != "1" && "$OWN_TMP_DIR" == "1" && -n "${TMP_DIR:-}" && -d "$TMP_DIR" ]]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

if [[ -z "$INPUT_PATH" ]]; then
  usage
  exit 2
fi
[[ -e "$INPUT_PATH" ]] || fail "input path not found: $INPUT_PATH"

if [[ -z "$OUTPUT_DIR" ]]; then
  if [[ -d "$INPUT_PATH" ]]; then
    OUTPUT_DIR="$INPUT_PATH"
  else
    OUTPUT_DIR="$(dirname "$INPUT_PATH")/launch-gaps"
  fi
fi
mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-launch-gaps.XXXXXX")"
OWN_TMP_DIR=1

SUMMARY_PATH=""
case "$INPUT_PATH" in
  *.tar.gz|*.tgz)
    tar -xzf "$INPUT_PATH" -C "$TMP_DIR"
    SUMMARY_PATH="$(find "$TMP_DIR" -type f -path '*/readiness/launch-readiness-summary.json' -print | head -n1)"
    ;;
  *)
    if [[ -d "$INPUT_PATH" ]]; then
      SUMMARY_PATH="$(find "$INPUT_PATH" -type f -name 'launch-readiness-summary.json' -print | head -n1)"
      if [[ -z "$SUMMARY_PATH" && -f "$INPUT_PATH/final-launch-summary.json" ]]; then
        DOSSIER_PATH="$(python3 - "$INPUT_PATH/final-launch-summary.json" <<'PY'
import json
import sys
with open(sys.argv[1], "r", encoding="utf-8") as handle:
    print((json.load(handle).get("launch_dossier_package") or ""))
PY
)"
        [[ -n "$DOSSIER_PATH" && -f "$DOSSIER_PATH" ]] || fail "final output directory does not reference an existing launch dossier package"
        tar -xzf "$DOSSIER_PATH" -C "$TMP_DIR"
        SUMMARY_PATH="$(find "$TMP_DIR" -type f -path '*/readiness/launch-readiness-summary.json' -print | head -n1)"
      fi
    elif [[ "$(basename "$INPUT_PATH")" == "launch-readiness-summary.json" ]]; then
      SUMMARY_PATH="$INPUT_PATH"
    elif [[ "$(basename "$INPUT_PATH")" == "final-launch-summary.json" ]]; then
      DOSSIER_PATH="$(python3 - "$INPUT_PATH" <<'PY'
import json
import sys
with open(sys.argv[1], "r", encoding="utf-8") as handle:
    print((json.load(handle).get("launch_dossier_package") or ""))
PY
)"
      [[ -n "$DOSSIER_PATH" && -f "$DOSSIER_PATH" ]] || fail "final summary does not reference an existing launch dossier package"
      tar -xzf "$DOSSIER_PATH" -C "$TMP_DIR"
      SUMMARY_PATH="$(find "$TMP_DIR" -type f -path '*/readiness/launch-readiness-summary.json' -print | head -n1)"
    else
      fail "unsupported input; pass readiness summary, final summary, output directory, or dossier archive"
    fi
    ;;
esac

[[ -n "$SUMMARY_PATH" && -f "$SUMMARY_PATH" ]] || fail "launch-readiness-summary.json not found in input"

python3 - "$SUMMARY_PATH" "$OUTPUT_DIR/launch-gaps-summary.json" "$OUTPUT_DIR/LAUNCH-GAPS-REPORT.txt" <<'PY'
import json
import os
import pathlib
import sys
from datetime import datetime, timezone

summary_path, summary_out, report_out = sys.argv[1:4]
with open(summary_path, "r", encoding="utf-8") as handle:
    readiness = json.load(handle)

remediations = {
    "handoff_package_verified": "Re-run the handoff verifier with the release archive, real go-live receipt, and public acceptance evidence; replace any corrupt or mismatched artifact.",
    "handoff_summary_passed": "Regenerate the launch handoff package from passing release, receipt, and public acceptance artifacts.",
    "real_go_live_summary_passed": "Run ops/bin/csd-pool-real-go-live.sh on the target host until the real go-live gate reports status=passed and fail=0.",
    "real_go_live_non_dry_run": "Run the real go-live wrapper without CSD_POOL_GO_LIVE_DRY_RUN=1 and attach the new receipt.",
    "real_go_live_target_launch_mode": "Use CSD_POOL_GO_LIVE_TARGET=public-beta or production for launch evidence.",
    "real_go_live_zero_skips": "Enable every required go-live probe for the selected target so the summary records skip=0.",
    "real_go_live_public_api_https": "Expose the public API over HTTPS and set CSD_POOL_GO_LIVE_PUBLIC_API_URL to that URL.",
    "real_go_live_public_stratum_present": "Set CSD_POOL_GO_LIVE_PUBLIC_STRATUM_ADDR to the public Stratum host:port and prove TCP/protocol reachability.",
    "real_go_live_no_fixture_markers": "Replace fixture, example, placeholder, or change-me values in release identity, config, env, and public endpoint fields.",
    "real_go_live_inputs_verified": "Attach real-go-live-inputs.log from the target host showing real_go_live_inputs_ok=True.",
    "real_go_live_postcheck_verified": "Attach real-go-live-postcheck.log showing real_go_live_postcheck_ok=True and matching evidence checksums.",
    "public_acceptance_summary_passed": "Run ops/bin/csd-pool-public-acceptance.sh from outside the pool host until it passes with fail=0.",
    "public_acceptance_public_api_https": "Run public acceptance against the HTTPS public API URL.",
    "public_acceptance_endpoint_matches_receipt": "Use the same public API URL and Stratum address in go-live and public acceptance evidence.",
    "public_acceptance_no_fixture_markers": "Replace fixture or example public endpoints before collecting acceptance evidence.",
    "public_acceptance_endpoint_routability_global": "Run public acceptance against DNS names or IPs that resolve only to global public addresses and attach public-endpoint-routability.log.",
    "public_acceptance_toolchain_manifest_verified": "Regenerate public acceptance evidence with the current ops/bin/csd-pool-public-acceptance.sh so acceptance-toolchain-manifest.json binds the reviewer script, receipt verifier, and workers binary.",
    "public_stratum_submit_response_standard": "Fix public Stratum ingress or backend protocol handling until mining.submit returns a standard response.",
    "public_stratum_accepted_share_observed": "Point CSD_POOL_ACCEPTANCE_CANARY_ADDRESS at a real miner and require enough accepted shares through the public edge.",
    "public_canary_accepted_share_minimum_matches_summary": "Regenerate public acceptance evidence so public-canary-miner.json records the same accepted_share_minimum as public-acceptance-summary.json.",
    "public_canary_accepted_share_minimum_met": "Keep the public canary miner connected through the public Stratum endpoint until accepted shares meet CSD_POOL_ACCEPTED_SHARE_MINIMUM.",
    "public_canary_miner_recently_seen": "Regenerate public acceptance while the canary miner is actively connected so last_seen_ts is within CSD_POOL_ACCEPTANCE_CANARY_MAX_AGE_SECONDS.",
    "public_canary_source_configured_when_required": "Set CSD_POOL_ACCEPTANCE_CANARY_ADDRESS to the real miner before collecting required accepted-share evidence.",
    "public_canary_miner_visible": "Verify /api/miner/<addr20> and /api/miner/<addr20>/workers show the public canary miner online.",
}

checks = readiness.get("checks") or []


def evidence_label(value):
    if not value:
        return ""
    if os.path.exists(value):
        return value
    return f"original audit path, not present on this reviewer host: {value}"


hard_open = [
    {
        "key": check.get("key"),
        "severity": check.get("severity"),
        "evidence": evidence_label(check.get("evidence") or ""),
        "detail": check.get("detail") or "",
        "remediation": remediations.get(check.get("key"), "Collect stronger real-environment evidence for this failed readiness check and rerun finalization."),
    }
    for check in checks
    if check.get("severity") == "hard" and check.get("passed") is not True
]
advisory_open = [
    {
        "key": check.get("key"),
        "severity": check.get("severity"),
        "evidence": evidence_label(check.get("evidence") or ""),
        "detail": check.get("detail") or "",
        "remediation": remediations.get(check.get("key"), "Review this advisory and attach the optional evidence when required by the launch decision."),
    }
    for check in checks
    if check.get("severity") == "advisory" and check.get("passed") is not True
]

status = "launch_ready" if not hard_open else "needs_real_environment_evidence"
gap_summary = {
    "status": status,
    "target": "launch-gaps",
    "generated_at_utc": datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
    "readiness_summary": summary_path,
    "readiness_status": readiness.get("status"),
    "hard_gap_count": len(hard_open),
    "advisory_gap_count": len(advisory_open),
    "public_accepted_share_required": readiness.get("public_accepted_share_required"),
    "public_accepted_share_observed": readiness.get("public_accepted_share_observed"),
    "public_accepted_share_minimum": readiness.get("public_accepted_share_minimum"),
    "public_canary_accepted_share_minimum": readiness.get("public_canary_accepted_share_minimum"),
    "hard_gaps": hard_open,
    "advisory_gaps": advisory_open,
}
pathlib.Path(summary_out).write_text(json.dumps(gap_summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")

lines = [
    "CSD Pool Launch Gaps",
    f"status={status}",
    f"readiness_status={readiness.get('status', 'unknown')}",
    f"hard_gap_count={len(hard_open)}",
    f"advisory_gap_count={len(advisory_open)}",
    f"readiness_summary={summary_path}",
    "",
    "Hard Gaps",
]
if hard_open:
    for gap in hard_open:
        lines.extend(
            [
                f"- {gap['key']}",
                f"  evidence={gap['evidence'] or 'missing'}",
                f"  detail={gap['detail']}",
                f"  next={gap['remediation']}",
            ]
        )
else:
    lines.append("- none")
lines.extend(["", "Advisory Gaps"])
if advisory_open:
    for gap in advisory_open:
        lines.extend(
            [
                f"- {gap['key']}",
                f"  evidence={gap['evidence'] or 'missing'}",
                f"  detail={gap['detail']}",
                f"  next={gap['remediation']}",
            ]
        )
else:
    lines.append("- none")
lines.append("")
lines.append(f"summary_json={summary_out}")
pathlib.Path(report_out).write_text("\n".join(lines) + "\n", encoding="utf-8")
PY

status="$(python3 - "$OUTPUT_DIR/launch-gaps-summary.json" <<'PY'
import json
import sys
with open(sys.argv[1], "r", encoding="utf-8") as handle:
    print(json.load(handle).get("status", "unknown"))
PY
)"

printf 'launch_gaps_report=%s\n' "$OUTPUT_DIR/LAUNCH-GAPS-REPORT.txt"
printf 'launch_gaps_summary=%s\n' "$OUTPUT_DIR/launch-gaps-summary.json"
printf 'status=%s\n' "$status"
printf 'summary: launch gaps explained\n'

if [[ "$status" != "launch_ready" && "$ALLOW_OPEN" != "1" ]]; then
  exit 1
fi

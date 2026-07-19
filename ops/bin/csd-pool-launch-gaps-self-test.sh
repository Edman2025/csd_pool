#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
GAPS_SCRIPT="${CSD_POOL_LAUNCH_GAPS_SELF_TEST_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-explain-launch-gaps.sh}"
OUTPUT_DIR="${CSD_POOL_LAUNCH_GAPS_SELF_TEST_DIR:-}"
KEEP_TMP="${CSD_POOL_LAUNCH_GAPS_SELF_TEST_KEEP_DIR:-0}"
OWN_TMP_DIR=0

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
}

cleanup() {
  if [[ "$KEEP_TMP" != "1" && "$OWN_TMP_DIR" == "1" && -n "${OUTPUT_DIR:-}" && -d "$OUTPUT_DIR" ]]; then
    rm -rf "$OUTPUT_DIR"
  fi
}
trap cleanup EXIT

[[ -x "$GAPS_SCRIPT" ]] || fail "launch gaps script not executable: $GAPS_SCRIPT"

if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-launch-gaps-self-test.XXXXXX")"
  OWN_TMP_DIR=1
else
  mkdir -p "$OUTPUT_DIR"
  OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
fi

printf 'CSD Pool launch gaps self-test\n'
printf 'output_dir=%s\n' "$OUTPUT_DIR"

READINESS="$OUTPUT_DIR/launch-readiness-summary.json"
GAPS_DIR="$OUTPUT_DIR/gaps"
cat >"$READINESS" <<'JSON'
{
  "status": "needs_real_environment_evidence",
  "target": "launch-readiness",
  "public_accepted_share_required": true,
  "public_accepted_share_observed": false,
  "public_accepted_share_minimum": 2,
  "public_canary_accepted_share_minimum": 1,
  "checks": [
    {
      "key": "public_canary_accepted_share_minimum_matches_summary",
      "passed": false,
      "severity": "hard",
      "evidence": "/tmp/public-canary-miner.json",
      "detail": "summary_minimum=2, canary_minimum=1"
    }
  ]
}
JSON

if CSD_POOL_GAPS_OUTPUT_DIR="$GAPS_DIR" CSD_POOL_GAPS_ALLOW_OPEN=1 "$GAPS_SCRIPT" "$READINESS" >"$OUTPUT_DIR/gaps.log" 2>&1; then
  printf 'ok: launch gaps generated from readiness summary\n'
else
  cat "$OUTPUT_DIR/gaps.log" >&2
  fail "launch gaps script failed for canary minimum fixture"
fi

[[ -f "$GAPS_DIR/LAUNCH-GAPS-REPORT.txt" ]] || fail "LAUNCH-GAPS-REPORT.txt missing"
[[ -f "$GAPS_DIR/launch-gaps-summary.json" ]] || fail "launch-gaps-summary.json missing"

if grep -Fq "Regenerate public acceptance evidence" "$GAPS_DIR/LAUNCH-GAPS-REPORT.txt"; then
  printf 'ok: canary minimum remediation written to report\n'
else
  cat "$GAPS_DIR/LAUNCH-GAPS-REPORT.txt" >&2
  fail "canary minimum remediation missing from report"
fi

if python3 - "$GAPS_DIR/launch-gaps-summary.json" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    summary = json.load(handle)
hard = summary.get("hard_gaps") or []
checks = {
    "status_needs_evidence": summary.get("status") == "needs_real_environment_evidence",
    "accepted_share_minimum_preserved": summary.get("public_accepted_share_minimum") == 2,
    "canary_minimum_preserved": summary.get("public_canary_accepted_share_minimum") == 1,
    "canary_gap_present": any(
        item.get("key") == "public_canary_accepted_share_minimum_matches_summary"
        and "Regenerate public acceptance evidence" in item.get("remediation", "")
        for item in hard
    ),
}
for key, value in checks.items():
    print(f"{key}={value}")
if not all(checks.values()):
    sys.exit(1)
PY
then
  printf 'ok: canary minimum remediation written to summary\n'
else
  cat "$GAPS_DIR/launch-gaps-summary.json" >&2
  fail "canary minimum remediation missing from summary"
fi

printf 'gaps_report=%s\n' "$GAPS_DIR/LAUNCH-GAPS-REPORT.txt"
printf 'gaps_summary=%s\n' "$GAPS_DIR/launch-gaps-summary.json"
printf 'summary: launch gaps self-test passed\n'

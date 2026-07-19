#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
ARCHIVE_PATH="${1:-${CSD_POOL_GO_LIVE_EVIDENCE_ARCHIVE:-}}"
OUTPUT_PATH="${2:-${CSD_POOL_GO_LIVE_SIGNOFF_PATH:-}}"
VERIFY_SCRIPT="${CSD_POOL_GO_LIVE_VERIFY_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-go-live-evidence.sh}"
KEEP_DIR="${CSD_POOL_SIGNOFF_KEEP_DIR:-0}"
WORK_DIR=""

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
}

usage() {
  printf 'usage: %s /path/to/go-live-evidence.tar.gz [GO-LIVE-SIGNOFF.md]\n' "$(basename "$0")" >&2
}

cleanup() {
  if [[ -n "$WORK_DIR" && "$KEEP_DIR" != "1" ]]; then
    rm -rf "$WORK_DIR"
  fi
}
trap cleanup EXIT

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

if [[ -z "$ARCHIVE_PATH" ]]; then
  usage
  exit 2
fi

[[ -f "$ARCHIVE_PATH" ]] || fail "evidence archive not found: $ARCHIVE_PATH"
[[ -x "$VERIFY_SCRIPT" ]] || fail "go-live evidence verifier not executable: $VERIFY_SCRIPT"

ARCHIVE_PATH="$(cd "$(dirname "$ARCHIVE_PATH")" && pwd)/$(basename "$ARCHIVE_PATH")"
if [[ -z "$OUTPUT_PATH" ]]; then
  OUTPUT_PATH="$(dirname "$ARCHIVE_PATH")/GO-LIVE-SIGNOFF.md"
fi
mkdir -p "$(dirname "$OUTPUT_PATH")"
OUTPUT_PATH="$(cd "$(dirname "$OUTPUT_PATH")" && pwd)/$(basename "$OUTPUT_PATH")"

"$VERIFY_SCRIPT" "$ARCHIVE_PATH"

WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-signoff.XXXXXX")"
tar -tzf "$ARCHIVE_PATH" | while IFS= read -r item; do
  case "$item" in
    /*|*../*|../*) fail "unsafe evidence archive path: $item" ;;
  esac
done
tar -xzf "$ARCHIVE_PATH" -C "$WORK_DIR"

SUMMARY_JSON="$WORK_DIR/go-live-summary.json"
REPORT_TXT="$WORK_DIR/GO-LIVE-REPORT.txt"
MANIFEST_TXT="$WORK_DIR/EVIDENCE-MANIFEST.txt"
[[ -f "$SUMMARY_JSON" ]] || fail "go-live-summary.json missing from evidence archive"
[[ -f "$REPORT_TXT" ]] || fail "GO-LIVE-REPORT.txt missing from evidence archive"
[[ -f "$MANIFEST_TXT" ]] || fail "EVIDENCE-MANIFEST.txt missing from evidence archive"

archive_sha="$(sha256_value "$ARCHIVE_PATH")"
sha_file="$ARCHIVE_PATH.sha256"
sha_file_line="missing"
if [[ -f "$sha_file" ]]; then
  sha_file_line="$(sed -n '1p' "$sha_file")"
fi

python3 - "$SUMMARY_JSON" "$REPORT_TXT" "$MANIFEST_TXT" "$WORK_DIR" "$ARCHIVE_PATH" "$archive_sha" "$sha_file_line" >"$OUTPUT_PATH" <<'PY'
import json
import pathlib
import sys

summary_path, report_path, manifest_path, work_dir, archive_path, archive_sha, sha_file_line = sys.argv[1:8]
work = pathlib.Path(work_dir)
with open(summary_path, "r", encoding="utf-8") as f:
    summary = json.load(f)

def text_value(path, prefix, default="unknown"):
    p = pathlib.Path(path)
    if not p.exists():
        return default
    for line in p.read_text(encoding="utf-8", errors="replace").splitlines():
        if line.startswith(prefix):
            return line.split("=", 1)[1]
    return default

target = summary.get("target", "unknown")
dry_run = summary.get("dry_run", "unknown")
release = summary.get("release") or {}
endpoints = summary.get("endpoints") or {}
counts = summary.get("summary") or {}

critical_reports = [
    "real-env-readiness.log",
    "secrets-permissions-safety.log",
    "evidence-redaction-safety.log",
    "clock-safety.log",
    "disk-safety.log",
    "bind-safety.log",
    "edge-proxy-safety.log",
    "database-migration.json",
    "database-migration-safety.log",
    "database-runtime.json",
    "release-integrity.log",
    "systemd-runtime-safety.log",
    "runtime-hardening-safety.log",
    "resource-limit-safety.log",
    "service-provenance-safety.log",
    "backup-artifact-safety.log",
    "restore-drill.log",
    "restore-api-safety.log",
    "restore-http-health.json",
    "restore-http-pool.json",
    "restore-http-blocks.json",
    "restore-http-payments.json",
    "restore-http-operator-payout-status.json",
    "check-node-template.json",
    "node-runtime.json",
    "node-endpoint-safety.log",
    "check-signer.json",
    "signer-safety.log",
    "sample-health.json",
    "runtime-config-binding.log",
    "runtime-status-binding.log",
    "status-release-binding.log",
    "pool-endpoint-binding.log",
    "http-api-pool.json",
    "metrics-surface-safety.log",
    "http-prometheus-metrics.txt",
    "external-public-status-binding.log",
    "external-public-pool-binding.log",
    "http-public-api-pool.json",
    "external-public-config-binding.log",
    "getting-started-binding.log",
    "external-public-getting-started-binding.log",
    "public-dns-safety.log",
    "public-api-tls-safety.log",
    "public-api-headers-safety.log",
    "public-api-surface-safety.log",
    "public-operator-auth-boundary.log",
    "public-stratum-tcp.log",
    "public-port-tiers-safety.log",
    "public-port-tiers-smoke.json",
    "public-stratum-smoke.json",
    "public-stratum-load.json",
    "operator-readiness-safety.log",
    "payout-limit-safety.log",
    "payout-safety.log",
    "payout-controls-safety.log",
    "http-operator-payout-batches.json",
    "http-operator-payout-status.json",
    "EVIDENCE-SHA256SUMS",
]

def present_mark(name):
    return "present" if (work / name).exists() else "missing"

print("# CSD Pool Go-Live Signoff")
print()
print("## Decision Inputs")
print()
print(f"- target: `{target}`")
print(f"- dry_run: `{dry_run}`")
print(f"- summary_status: `{counts.get('status', 'unknown')}`")
print(f"- pass_fail_skip: `{counts.get('pass', 'unknown')}/{counts.get('fail', 'unknown')}/{counts.get('skip', 'unknown')}`")
print(f"- release_name: `{release.get('name', 'unknown')}`")
print(f"- release_revision: `{release.get('revision', 'unknown')}`")
print(f"- release_timestamp_utc: `{release.get('timestamp_utc', 'unknown')}`")
print(f"- release_manifest: `{release.get('manifest', 'unknown')}`")
print()
print("## Endpoints")
print()
for key in ["api_url", "stratum_addr", "public_api_url", "public_stratum_probe_addr", "public_stratum_addr", "public_port_tiers"]:
    print(f"- {key}: `{endpoints.get(key, 'unknown')}`")
print()
print("## Evidence Archive")
print()
print(f"- archive: `{archive_path}`")
print(f"- archive_sha256: `{archive_sha}`")
print(f"- archive_sha256_file: `{sha_file_line}`")
print(f"- report_status: `{text_value(report_path, 'status=')}`")
print(f"- manifest_status: `{text_value(manifest_path, 'status=')}`")
print()
print("## Critical Evidence Files")
print()
for name in critical_reports:
    print(f"- {name}: `{present_mark(name)}`")
print()
print("## Signoff Notes")
print()
print("- This report is generated only after `ops/bin/csd-pool-verify-go-live-evidence.sh` accepts the evidence archive.")
print("- A real launch signoff must have `dry_run: false`; dry-run reports are rehearsal evidence only.")
print("- Keep this file with `go-live-evidence.tar.gz`, its `.sha256`, `GO-LIVE-REPORT.txt`, and `go-live-summary.json`.")
PY

printf 'signoff_report=%s\n' "$OUTPUT_PATH"
printf 'summary: go-live signoff report generated\n'

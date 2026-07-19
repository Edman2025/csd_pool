#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
VERIFY_ACCEPTANCE_SCRIPT="${CSD_POOL_PUBLIC_ACCEPTANCE_SELF_TEST_VERIFY_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-public-acceptance-evidence.sh}"
OUTPUT_DIR="${CSD_POOL_PUBLIC_ACCEPTANCE_SELF_TEST_DIR:-}"
KEEP_TMP="${CSD_POOL_PUBLIC_ACCEPTANCE_SELF_TEST_KEEP_DIR:-0}"
OWN_TMP_DIR=0

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
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

write_sha_manifest() {
  local dir="$1"
  local manifest="$2"
  (
    cd "$dir"
    find . -type f ! -name "$manifest" | sort | while read -r file; do
      sha256_file "$file"
    done >"$manifest"
  )
}

write_acceptance_toolchain_manifest_fixture() {
  local dir="$1"
  local public_api="$2"
  local public_stratum="$3"
  cat >"$dir/acceptance-toolchain-manifest.json" <<JSON
{
  "target": "public-acceptance",
  "public_api_url": "$public_api",
  "public_stratum_addr": "$public_stratum",
  "entries": [
    {"path": "/opt/csd-pool/ops/bin/csd-pool-public-acceptance.sh", "basename": "csd-pool-public-acceptance.sh", "exists": true, "executable": true, "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"},
    {"path": "/opt/csd-pool/ops/bin/csd-pool-verify-real-go-live-receipt.sh", "basename": "csd-pool-verify-real-go-live-receipt.sh", "exists": true, "executable": true, "sha256": "1123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"},
    {"path": "/opt/csd-pool/bin/csd-pool-workers", "basename": "csd-pool-workers", "exists": true, "executable": true, "sha256": "2123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}
  ],
  "required_basenames": [
    "csd-pool-public-acceptance.sh",
    "csd-pool-verify-real-go-live-receipt.sh",
    "csd-pool-workers"
  ]
}
JSON
}

cleanup() {
  if [[ "$KEEP_TMP" != "1" && "$OWN_TMP_DIR" == "1" && -n "${OUTPUT_DIR:-}" && -d "$OUTPUT_DIR" ]]; then
    rm -rf "$OUTPUT_DIR"
  fi
}
trap cleanup EXIT

[[ -x "$VERIFY_ACCEPTANCE_SCRIPT" ]] || fail "public acceptance verifier not executable: $VERIFY_ACCEPTANCE_SCRIPT"

if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-public-acceptance-self-test.XXXXXX")"
  OWN_TMP_DIR=1
else
  mkdir -p "$OUTPUT_DIR"
  OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
fi

printf 'CSD Pool public acceptance self-test\n'
printf 'output_dir=%s\n' "$OUTPUT_DIR"

make_acceptance_package() {
  local root="$1"
  local public_api="$2"
  local public_stratum="$3"
  local archive="$4"
  local routability_ok="${5:-1}"

  local dir="$root/public-acceptance-evidence"
  local now_ts
  now_ts="$(date +%s)"
  mkdir -p "$dir"
  cat >"$dir/PUBLIC-ACCEPTANCE-REPORT.txt" <<TXT
CSD Pool Public Acceptance
status=passed
public_api_url=$public_api
public_stratum_addr=$public_stratum
receipt_archive=/tmp/csd-pool-self-test-real-go-live-receipt.tar.gz
receipt_archive_sha256=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
public_status_release_name=csd-pool-self-test
public_status_release_revision=abcdef0
public_status_release_built_at=2026-06-24T00:00:00Z
public_status_release_version=self-test
pass=12
fail=0
skip=1
TXT
  cat >"$dir/public-acceptance-summary.json" <<JSON
{
  "status":"passed",
  "pass":12,
  "fail":0,
  "skip":1,
  "public_api_url":"$public_api",
  "public_stratum_addr":"$public_stratum",
  "receipt_archive":"/tmp/csd-pool-self-test-real-go-live-receipt.tar.gz",
  "receipt_archive_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "public_status_release":{
    "name":"csd-pool-self-test",
    "revision":"abcdef0",
    "built_at":"2026-06-24T00:00:00Z",
    "version":"self-test"
  },
  "acceptance_toolchain_manifest":"$dir/acceptance-toolchain-manifest.json",
  "accepted_share_required":"0",
  "accepted_share_minimum":1,
  "canary_max_age_seconds":600,
  "reports":{
    "receipt_verify":"$dir/receipt-verify.log",
    "receipt_binding":"$dir/receipt-binding.log",
    "api_health":"$dir/http-public-health.json",
    "api_status":"$dir/http-public-status.json",
    "status_release_binding":"$dir/public-status-release-binding.log",
    "api_pool":"$dir/http-public-pool.json",
    "api_getting_started":"$dir/http-public-getting-started.json",
    "getting_started_binding":"$dir/getting-started-binding.log",
    "public_endpoint_routability":"$dir/public-endpoint-routability.log",
    "stratum_smoke":"$dir/public-stratum-smoke.json",
    "stratum_submit_probe":"$dir/public-stratum-submit-probe.json",
    "stratum_load":"$dir/public-stratum-load.json",
    "canary_miner":"$dir/public-canary-miner.json",
    "canary_miner_api":"$dir/http-public-canary-miner.json",
    "canary_miner_workers_api":"$dir/http-public-canary-miner-workers.json"
  }
}
JSON
  write_acceptance_toolchain_manifest_fixture "$dir" "$public_api" "$public_stratum"
  cat >"$dir/receipt-verify.log" <<'TXT'
ok: launch toolchain manifest proves real launch scripts
summary: pass=1 fail=0
TXT
  cat >"$dir/receipt-binding.log" <<'TXT'
receipt_public_api_matches=True
receipt_public_stratum_matches=True
TXT
  printf '{"ok":true}\n' >"$dir/http-public-health.json"
  printf '{"ok":true,"release":{"name":"csd-pool-self-test","revision":"abcdef0","built_at":"2026-06-24T00:00:00Z","version":"self-test"}}\n' >"$dir/http-public-status.json"
  cat >"$dir/public-status-release-binding.log" <<'TXT'
name: expected=csd-pool-self-test actual=csd-pool-self-test
revision: expected=abcdef0 actual=abcdef0
built_at: expected=2026-06-24T00:00:00Z actual=2026-06-24T00:00:00Z
version: actual=self-test
release_name_matches=True
release_revision_matches=True
release_built_at_matches=True
release_version_present=True
public_status_release_binding_ok=True
TXT
  printf '{"ok":true}\n' >"$dir/http-public-pool.json"
  printf '{"stratum_endpoint":"%s","commands":[{"command":"miner --pool %s"}]}\n' "$public_stratum" "$public_stratum" >"$dir/http-public-getting-started.json"
  cat >"$dir/getting-started-binding.log" <<'TXT'
stratum_endpoint_matches=True
commands_include_stratum_endpoint=True
TXT
  if [[ "$routability_ok" == "1" ]]; then
    cat >"$dir/public-endpoint-routability.log" <<'TXT'
public_api_dns_all_global=True
public_stratum_dns_all_global=True
public_endpoint_routability_ok=True
TXT
  else
    cat >"$dir/public-endpoint-routability.log" <<'TXT'
public_api_dns_all_global=False
public_stratum_dns_all_global=False
public_endpoint_routability_ok=False
TXT
  fi
  printf '{"requested_clients":1,"succeeded_clients":1,"failed_clients":0,"successes":[{"worker":"abc"}]}\n' >"$dir/public-stratum-smoke.json"
  printf '{"passed":true,"difficulty_seen":true,"notify_seen":true,"submit_response_received":true,"submit_response_standard":true,"submit_result":true}\n' >"$dir/public-stratum-submit-probe.json"
  printf '{"skipped":true}\n' >"$dir/public-stratum-load.json"
  printf '{"address":"abc","online":true,"workers_online":1,"shares_accepted":0,"last_seen_ts":%s}\n' "$now_ts" >"$dir/http-public-canary-miner.json"
  printf '{"workers":[{"worker":"abc"}]}\n' >"$dir/http-public-canary-miner-workers.json"
  printf '{"status":"passed","canary_address":"abc","canary_source":"smoke-success-worker","accepted_share_required":false,"accepted_share_minimum":1,"canary_max_age_seconds":600,"checks":{"miner_address_matches":true,"miner_online":true,"workers_online_positive":true,"worker_rows_present":true,"last_seen_ts_present":true,"last_seen_not_from_future":true,"last_seen_within_max_age":true},"miner":{"last_seen_ts":%s,"last_seen_age_seconds":0,"observed_at_ts":%s}}\n' "$now_ts" "$now_ts" >"$dir/public-canary-miner.json"
  write_sha_manifest "$dir" "PUBLIC-ACCEPTANCE-SHA256SUMS"
  (
    cd "$root"
    tar -czf "$archive" "$(basename "$dir")"
  )
  sha256_file "$archive" >"$archive.sha256"
}

GOOD_ROOT="$OUTPUT_DIR/good"
mkdir -p "$GOOD_ROOT"
GOOD_ARCHIVE="$OUTPUT_DIR/public-acceptance-good.tar.gz"
make_acceptance_package "$GOOD_ROOT" "https://1.1.1.1" "8.8.8.8:3333" "$GOOD_ARCHIVE" 1

"$VERIFY_ACCEPTANCE_SCRIPT" "$GOOD_ARCHIVE" "$GOOD_ARCHIVE.sha256" >"$OUTPUT_DIR/good-verify.log" 2>&1 \
  || { cat "$OUTPUT_DIR/good-verify.log" >&2; fail "public acceptance verifier rejected fixture-free package"; }
printf 'ok: fixture-free public acceptance package verified\n'

SUMMARY_BINDING_ROOT="$OUTPUT_DIR/summary-binding"
mkdir -p "$SUMMARY_BINDING_ROOT"
SUMMARY_BINDING_ARCHIVE="$OUTPUT_DIR/public-acceptance-summary-binding.tar.gz"
make_acceptance_package "$SUMMARY_BINDING_ROOT" "https://1.1.1.1" "8.8.8.8:3333" "$SUMMARY_BINDING_ARCHIVE" 1
SUMMARY_BINDING_DIR="$SUMMARY_BINDING_ROOT/public-acceptance-evidence"
python3 - "$SUMMARY_BINDING_DIR/public-acceptance-summary.json" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
data["reports"]["stratum_smoke"] = "/tmp/misleading-public-stratum-smoke.json"
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
write_sha_manifest "$SUMMARY_BINDING_DIR" "PUBLIC-ACCEPTANCE-SHA256SUMS"
(
  cd "$SUMMARY_BINDING_ROOT"
  tar -czf "$SUMMARY_BINDING_ARCHIVE" "$(basename "$SUMMARY_BINDING_DIR")"
)
sha256_file "$SUMMARY_BINDING_ARCHIVE" >"$SUMMARY_BINDING_ARCHIVE.sha256"

if "$VERIFY_ACCEPTANCE_SCRIPT" "$SUMMARY_BINDING_ARCHIVE" "$SUMMARY_BINDING_ARCHIVE.sha256" >"$OUTPUT_DIR/summary-binding-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/summary-binding-verify.log" >&2
  fail "public acceptance verifier accepted misleading summary report paths"
fi

if grep -Fq "public acceptance summary report path binding failed" "$OUTPUT_DIR/summary-binding-verify.log"; then
  printf 'ok: misleading public acceptance summary report paths rejected\n'
else
  cat "$OUTPUT_DIR/summary-binding-verify.log" >&2
  fail "summary report path binding package failed for an unexpected reason"
fi

RECEIPT_SHA_ROOT="$OUTPUT_DIR/receipt-sha-binding"
mkdir -p "$RECEIPT_SHA_ROOT"
RECEIPT_SHA_ARCHIVE="$OUTPUT_DIR/public-acceptance-receipt-sha-mismatch.tar.gz"
make_acceptance_package "$RECEIPT_SHA_ROOT" "https://1.1.1.1" "8.8.8.8:3333" "$RECEIPT_SHA_ARCHIVE" 1
RECEIPT_SHA_DIR="$RECEIPT_SHA_ROOT/public-acceptance-evidence"
python3 - "$RECEIPT_SHA_DIR/public-acceptance-summary.json" "$RECEIPT_SHA_DIR/PUBLIC-ACCEPTANCE-REPORT.txt" <<'PY'
import json
import sys

summary_path, report_path = sys.argv[1:3]
with open(summary_path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
data["receipt_archive_sha256"] = "not-a-sha256"
with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
lines = []
with open(report_path, "r", encoding="utf-8") as handle:
    for line in handle:
        if line.startswith("receipt_archive_sha256="):
            lines.append("receipt_archive_sha256=not-a-sha256\n")
        else:
            lines.append(line)
with open(report_path, "w", encoding="utf-8") as handle:
    handle.writelines(lines)
PY
write_sha_manifest "$RECEIPT_SHA_DIR" "PUBLIC-ACCEPTANCE-SHA256SUMS"
(
  cd "$RECEIPT_SHA_ROOT"
  tar -czf "$RECEIPT_SHA_ARCHIVE" "$(basename "$RECEIPT_SHA_DIR")"
)
sha256_file "$RECEIPT_SHA_ARCHIVE" >"$RECEIPT_SHA_ARCHIVE.sha256"

if "$VERIFY_ACCEPTANCE_SCRIPT" "$RECEIPT_SHA_ARCHIVE" "$RECEIPT_SHA_ARCHIVE.sha256" >"$OUTPUT_DIR/receipt-sha-binding-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/receipt-sha-binding-verify.log" >&2
  fail "public acceptance verifier accepted invalid receipt sha256 metadata"
fi

if grep -Fq "public acceptance summary receipt sha256 missing or invalid" "$OUTPUT_DIR/receipt-sha-binding-verify.log"; then
  printf 'ok: invalid public acceptance receipt sha rejected\n'
else
  cat "$OUTPUT_DIR/receipt-sha-binding-verify.log" >&2
  fail "receipt sha binding package failed for an unexpected reason"
fi

RECEIPT_TOOLCHAIN_ROOT="$OUTPUT_DIR/receipt-toolchain-proof"
mkdir -p "$RECEIPT_TOOLCHAIN_ROOT"
RECEIPT_TOOLCHAIN_ARCHIVE="$OUTPUT_DIR/public-acceptance-receipt-toolchain-missing.tar.gz"
make_acceptance_package "$RECEIPT_TOOLCHAIN_ROOT" "https://1.1.1.1" "8.8.8.8:3333" "$RECEIPT_TOOLCHAIN_ARCHIVE" 1
RECEIPT_TOOLCHAIN_DIR="$RECEIPT_TOOLCHAIN_ROOT/public-acceptance-evidence"
cat >"$RECEIPT_TOOLCHAIN_DIR/receipt-verify.log" <<'TXT'
summary: pass=1 fail=0
TXT
write_sha_manifest "$RECEIPT_TOOLCHAIN_DIR" "PUBLIC-ACCEPTANCE-SHA256SUMS"
(
  cd "$RECEIPT_TOOLCHAIN_ROOT"
  tar -czf "$RECEIPT_TOOLCHAIN_ARCHIVE" "$(basename "$RECEIPT_TOOLCHAIN_DIR")"
)
sha256_file "$RECEIPT_TOOLCHAIN_ARCHIVE" >"$RECEIPT_TOOLCHAIN_ARCHIVE.sha256"

if "$VERIFY_ACCEPTANCE_SCRIPT" "$RECEIPT_TOOLCHAIN_ARCHIVE" "$RECEIPT_TOOLCHAIN_ARCHIVE.sha256" >"$OUTPUT_DIR/receipt-toolchain-proof-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/receipt-toolchain-proof-verify.log" >&2
  fail "public acceptance verifier accepted receipt log without launch toolchain proof"
fi

if grep -Fq "receipt launch toolchain proof present missing" "$OUTPUT_DIR/receipt-toolchain-proof-verify.log"; then
  printf 'ok: public acceptance receipt toolchain proof enforced\n'
else
  cat "$OUTPUT_DIR/receipt-toolchain-proof-verify.log" >&2
  fail "receipt toolchain proof package failed for an unexpected reason"
fi

ACCEPTANCE_TOOLCHAIN_ROOT="$OUTPUT_DIR/acceptance-toolchain-binding"
mkdir -p "$ACCEPTANCE_TOOLCHAIN_ROOT"
ACCEPTANCE_TOOLCHAIN_ARCHIVE="$OUTPUT_DIR/public-acceptance-toolchain-mismatch.tar.gz"
make_acceptance_package "$ACCEPTANCE_TOOLCHAIN_ROOT" "https://1.1.1.1" "8.8.8.8:3333" "$ACCEPTANCE_TOOLCHAIN_ARCHIVE" 1
ACCEPTANCE_TOOLCHAIN_DIR="$ACCEPTANCE_TOOLCHAIN_ROOT/public-acceptance-evidence"
python3 - "$ACCEPTANCE_TOOLCHAIN_DIR/acceptance-toolchain-manifest.json" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
data["public_stratum_addr"] = "9.9.9.9:3333"
data["entries"] = [entry for entry in data.get("entries", []) if entry.get("basename") != "csd-pool-workers"]
with open(path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
write_sha_manifest "$ACCEPTANCE_TOOLCHAIN_DIR" "PUBLIC-ACCEPTANCE-SHA256SUMS"
(
  cd "$ACCEPTANCE_TOOLCHAIN_ROOT"
  tar -czf "$ACCEPTANCE_TOOLCHAIN_ARCHIVE" "$(basename "$ACCEPTANCE_TOOLCHAIN_DIR")"
)
sha256_file "$ACCEPTANCE_TOOLCHAIN_ARCHIVE" >"$ACCEPTANCE_TOOLCHAIN_ARCHIVE.sha256"

if "$VERIFY_ACCEPTANCE_SCRIPT" "$ACCEPTANCE_TOOLCHAIN_ARCHIVE" "$ACCEPTANCE_TOOLCHAIN_ARCHIVE.sha256" >"$OUTPUT_DIR/acceptance-toolchain-binding-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/acceptance-toolchain-binding-verify.log" >&2
  fail "public acceptance verifier accepted mismatched acceptance toolchain manifest"
fi

if grep -Fq "public acceptance toolchain manifest missing required proof" "$OUTPUT_DIR/acceptance-toolchain-binding-verify.log"; then
  printf 'ok: public acceptance toolchain manifest binding enforced\n'
else
  cat "$OUTPUT_DIR/acceptance-toolchain-binding-verify.log" >&2
  fail "acceptance toolchain binding package failed for an unexpected reason"
fi

REPORT_CONTEXT_ROOT="$OUTPUT_DIR/report-context-binding"
mkdir -p "$REPORT_CONTEXT_ROOT"
REPORT_CONTEXT_ARCHIVE="$OUTPUT_DIR/public-acceptance-report-context-mismatch.tar.gz"
make_acceptance_package "$REPORT_CONTEXT_ROOT" "https://1.1.1.1" "8.8.8.8:3333" "$REPORT_CONTEXT_ARCHIVE" 1
REPORT_CONTEXT_DIR="$REPORT_CONTEXT_ROOT/public-acceptance-evidence"
python3 - "$REPORT_CONTEXT_DIR/PUBLIC-ACCEPTANCE-REPORT.txt" <<'PY'
import sys

path = sys.argv[1]
lines = []
with open(path, "r", encoding="utf-8") as handle:
    for line in handle:
        if line.startswith("receipt_archive="):
            lines.append("receipt_archive=/tmp/another-real-go-live-receipt.tar.gz\n")
        else:
            lines.append(line)
with open(path, "w", encoding="utf-8") as handle:
    handle.writelines(lines)
PY
write_sha_manifest "$REPORT_CONTEXT_DIR" "PUBLIC-ACCEPTANCE-SHA256SUMS"
(
  cd "$REPORT_CONTEXT_ROOT"
  tar -czf "$REPORT_CONTEXT_ARCHIVE" "$(basename "$REPORT_CONTEXT_DIR")"
)
sha256_file "$REPORT_CONTEXT_ARCHIVE" >"$REPORT_CONTEXT_ARCHIVE.sha256"

if "$VERIFY_ACCEPTANCE_SCRIPT" "$REPORT_CONTEXT_ARCHIVE" "$REPORT_CONTEXT_ARCHIVE.sha256" >"$OUTPUT_DIR/report-context-binding-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/report-context-binding-verify.log" >&2
  fail "public acceptance verifier accepted mismatched report receipt archive"
fi

if grep -Fq "public acceptance report receipt archive mismatch" "$OUTPUT_DIR/report-context-binding-verify.log"; then
  printf 'ok: mismatched public acceptance report receipt archive rejected\n'
else
  cat "$OUTPUT_DIR/report-context-binding-verify.log" >&2
  fail "report context binding package failed for an unexpected reason"
fi

SUMMARY_RELEASE_ROOT="$OUTPUT_DIR/summary-release-binding"
mkdir -p "$SUMMARY_RELEASE_ROOT"
SUMMARY_RELEASE_ARCHIVE="$OUTPUT_DIR/public-acceptance-summary-release-mismatch.tar.gz"
make_acceptance_package "$SUMMARY_RELEASE_ROOT" "https://1.1.1.1" "8.8.8.8:3333" "$SUMMARY_RELEASE_ARCHIVE" 1
SUMMARY_RELEASE_DIR="$SUMMARY_RELEASE_ROOT/public-acceptance-evidence"
python3 - "$SUMMARY_RELEASE_DIR/public-acceptance-summary.json" "$SUMMARY_RELEASE_DIR/PUBLIC-ACCEPTANCE-REPORT.txt" <<'PY'
import json
import sys

summary_path, report_path = sys.argv[1:3]
with open(summary_path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
data["public_status_release"]["revision"] = "badrevision"
with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(data, handle, indent=2, sort_keys=True)
    handle.write("\n")
lines = []
with open(report_path, "r", encoding="utf-8") as handle:
    for line in handle:
        if line.startswith("public_status_release_revision="):
            lines.append("public_status_release_revision=badrevision\n")
        else:
            lines.append(line)
with open(report_path, "w", encoding="utf-8") as handle:
    handle.writelines(lines)
PY
write_sha_manifest "$SUMMARY_RELEASE_DIR" "PUBLIC-ACCEPTANCE-SHA256SUMS"
(
  cd "$SUMMARY_RELEASE_ROOT"
  tar -czf "$SUMMARY_RELEASE_ARCHIVE" "$(basename "$SUMMARY_RELEASE_DIR")"
)
sha256_file "$SUMMARY_RELEASE_ARCHIVE" >"$SUMMARY_RELEASE_ARCHIVE.sha256"

if "$VERIFY_ACCEPTANCE_SCRIPT" "$SUMMARY_RELEASE_ARCHIVE" "$SUMMARY_RELEASE_ARCHIVE.sha256" >"$OUTPUT_DIR/summary-release-binding-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/summary-release-binding-verify.log" >&2
  fail "public acceptance verifier accepted mismatched summary release identity"
fi

if grep -Fq "public acceptance summary release identity mismatch" "$OUTPUT_DIR/summary-release-binding-verify.log"; then
  printf 'ok: mismatched public acceptance summary release identity rejected\n'
else
  cat "$OUTPUT_DIR/summary-release-binding-verify.log" >&2
  fail "summary release binding package failed for an unexpected reason"
fi

STATUS_RELEASE_ROOT="$OUTPUT_DIR/status-release-binding"
mkdir -p "$STATUS_RELEASE_ROOT"
STATUS_RELEASE_ARCHIVE="$OUTPUT_DIR/public-acceptance-status-release-mismatch.tar.gz"
make_acceptance_package "$STATUS_RELEASE_ROOT" "https://1.1.1.1" "8.8.8.8:3333" "$STATUS_RELEASE_ARCHIVE" 1
STATUS_RELEASE_DIR="$STATUS_RELEASE_ROOT/public-acceptance-evidence"
python3 - "$STATUS_RELEASE_DIR/public-status-release-binding.log" <<'PY'
import sys

path = sys.argv[1]
text = open(path, "r", encoding="utf-8").read()
text = text.replace("release_revision_matches=True", "release_revision_matches=False")
text = text.replace("public_status_release_binding_ok=True", "public_status_release_binding_ok=False")
open(path, "w", encoding="utf-8").write(text)
PY
write_sha_manifest "$STATUS_RELEASE_DIR" "PUBLIC-ACCEPTANCE-SHA256SUMS"
(
  cd "$STATUS_RELEASE_ROOT"
  tar -czf "$STATUS_RELEASE_ARCHIVE" "$(basename "$STATUS_RELEASE_DIR")"
)
sha256_file "$STATUS_RELEASE_ARCHIVE" >"$STATUS_RELEASE_ARCHIVE.sha256"

if "$VERIFY_ACCEPTANCE_SCRIPT" "$STATUS_RELEASE_ARCHIVE" "$STATUS_RELEASE_ARCHIVE.sha256" >"$OUTPUT_DIR/status-release-binding-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/status-release-binding-verify.log" >&2
  fail "public acceptance verifier accepted mismatched public status release binding"
fi

if grep -Fq "public status release revision matches receipt missing" "$OUTPUT_DIR/status-release-binding-verify.log"; then
  printf 'ok: mismatched public acceptance status release binding rejected\n'
else
  cat "$OUTPUT_DIR/status-release-binding-verify.log" >&2
  fail "status release binding package failed for an unexpected reason"
fi

CANARY_MIN_ROOT="$OUTPUT_DIR/canary-minimum-binding"
mkdir -p "$CANARY_MIN_ROOT"
CANARY_MIN_ARCHIVE="$OUTPUT_DIR/public-acceptance-canary-minimum-mismatch.tar.gz"
make_acceptance_package "$CANARY_MIN_ROOT" "https://1.1.1.1" "8.8.8.8:3333" "$CANARY_MIN_ARCHIVE" 1
CANARY_MIN_DIR="$CANARY_MIN_ROOT/public-acceptance-evidence"
python3 - "$CANARY_MIN_DIR/public-acceptance-summary.json" "$CANARY_MIN_DIR/http-public-canary-miner.json" "$CANARY_MIN_DIR/public-canary-miner.json" <<'PY'
import json
import sys

summary_path, miner_path, canary_path = sys.argv[1:4]
with open(summary_path, "r", encoding="utf-8") as handle:
    summary = json.load(handle)
summary["accepted_share_required"] = "1"
summary["accepted_share_minimum"] = 2
with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(summary, handle, indent=2, sort_keys=True)
    handle.write("\n")
with open(miner_path, "r", encoding="utf-8") as handle:
    miner = json.load(handle)
miner["shares_accepted"] = 2
with open(miner_path, "w", encoding="utf-8") as handle:
    json.dump(miner, handle, indent=2, sort_keys=True)
    handle.write("\n")
with open(canary_path, "r", encoding="utf-8") as handle:
    canary = json.load(handle)
canary["accepted_share_required"] = True
canary["accepted_share_minimum"] = 1
canary["canary_source"] = "configured"
canary.setdefault("checks", {})["accepted_share_minimum_met"] = True
with open(canary_path, "w", encoding="utf-8") as handle:
    json.dump(canary, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
write_sha_manifest "$CANARY_MIN_DIR" "PUBLIC-ACCEPTANCE-SHA256SUMS"
(
  cd "$CANARY_MIN_ROOT"
  tar -czf "$CANARY_MIN_ARCHIVE" "$(basename "$CANARY_MIN_DIR")"
)
sha256_file "$CANARY_MIN_ARCHIVE" >"$CANARY_MIN_ARCHIVE.sha256"

if "$VERIFY_ACCEPTANCE_SCRIPT" "$CANARY_MIN_ARCHIVE" "$CANARY_MIN_ARCHIVE.sha256" >"$OUTPUT_DIR/canary-minimum-binding-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/canary-minimum-binding-verify.log" >&2
  fail "public acceptance verifier accepted mismatched canary accepted-share minimum"
fi

if grep -Fq "accepted_share_minimum_declared_consistently=False" "$OUTPUT_DIR/canary-minimum-binding-verify.log"; then
  printf 'ok: mismatched public acceptance canary minimum rejected\n'
else
  cat "$OUTPUT_DIR/canary-minimum-binding-verify.log" >&2
  fail "canary minimum binding package failed for an unexpected reason"
fi

REQUIRED_SOURCE_ROOT="$OUTPUT_DIR/required-canary-source"
mkdir -p "$REQUIRED_SOURCE_ROOT"
REQUIRED_SOURCE_ARCHIVE="$OUTPUT_DIR/public-acceptance-required-canary-source.tar.gz"
make_acceptance_package "$REQUIRED_SOURCE_ROOT" "https://1.1.1.1" "8.8.8.8:3333" "$REQUIRED_SOURCE_ARCHIVE" 1
REQUIRED_SOURCE_DIR="$REQUIRED_SOURCE_ROOT/public-acceptance-evidence"
python3 - "$REQUIRED_SOURCE_DIR/public-acceptance-summary.json" "$REQUIRED_SOURCE_DIR/http-public-canary-miner.json" "$REQUIRED_SOURCE_DIR/public-canary-miner.json" <<'PY'
import json
import sys

summary_path, miner_path, canary_path = sys.argv[1:4]
with open(summary_path, "r", encoding="utf-8") as handle:
    summary = json.load(handle)
summary["accepted_share_required"] = "1"
summary["accepted_share_minimum"] = 1
with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(summary, handle, indent=2, sort_keys=True)
    handle.write("\n")
with open(miner_path, "r", encoding="utf-8") as handle:
    miner = json.load(handle)
miner["shares_accepted"] = 1
with open(miner_path, "w", encoding="utf-8") as handle:
    json.dump(miner, handle, indent=2, sort_keys=True)
    handle.write("\n")
with open(canary_path, "r", encoding="utf-8") as handle:
    canary = json.load(handle)
canary["accepted_share_required"] = True
canary["accepted_share_minimum"] = 1
canary["canary_source"] = "smoke-success-worker"
canary.setdefault("checks", {})["accepted_share_minimum_met"] = True
with open(canary_path, "w", encoding="utf-8") as handle:
    json.dump(canary, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
write_sha_manifest "$REQUIRED_SOURCE_DIR" "PUBLIC-ACCEPTANCE-SHA256SUMS"
(
  cd "$REQUIRED_SOURCE_ROOT"
  tar -czf "$REQUIRED_SOURCE_ARCHIVE" "$(basename "$REQUIRED_SOURCE_DIR")"
)
sha256_file "$REQUIRED_SOURCE_ARCHIVE" >"$REQUIRED_SOURCE_ARCHIVE.sha256"

if "$VERIFY_ACCEPTANCE_SCRIPT" "$REQUIRED_SOURCE_ARCHIVE" "$REQUIRED_SOURCE_ARCHIVE.sha256" >"$OUTPUT_DIR/required-canary-source-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/required-canary-source-verify.log" >&2
  fail "public acceptance verifier accepted required accepted-share evidence without configured canary source"
fi

if grep -Fq "accepted_share_canary_source_configured=False" "$OUTPUT_DIR/required-canary-source-verify.log"; then
  printf 'ok: required public acceptance configured canary source enforced\n'
else
  cat "$OUTPUT_DIR/required-canary-source-verify.log" >&2
  fail "required canary source package failed for an unexpected reason"
fi

INSUFFICIENT_SHARE_ROOT="$OUTPUT_DIR/insufficient-accepted-share"
mkdir -p "$INSUFFICIENT_SHARE_ROOT"
INSUFFICIENT_SHARE_ARCHIVE="$OUTPUT_DIR/public-acceptance-insufficient-accepted-share.tar.gz"
make_acceptance_package "$INSUFFICIENT_SHARE_ROOT" "https://1.1.1.1" "8.8.8.8:3333" "$INSUFFICIENT_SHARE_ARCHIVE" 1
INSUFFICIENT_SHARE_DIR="$INSUFFICIENT_SHARE_ROOT/public-acceptance-evidence"
python3 - "$INSUFFICIENT_SHARE_DIR/public-acceptance-summary.json" "$INSUFFICIENT_SHARE_DIR/http-public-canary-miner.json" "$INSUFFICIENT_SHARE_DIR/public-canary-miner.json" <<'PY'
import json
import sys

summary_path, miner_path, canary_path = sys.argv[1:4]
with open(summary_path, "r", encoding="utf-8") as handle:
    summary = json.load(handle)
summary["accepted_share_required"] = "1"
summary["accepted_share_minimum"] = 2
with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(summary, handle, indent=2, sort_keys=True)
    handle.write("\n")
with open(miner_path, "r", encoding="utf-8") as handle:
    miner = json.load(handle)
miner["shares_accepted"] = 1
with open(miner_path, "w", encoding="utf-8") as handle:
    json.dump(miner, handle, indent=2, sort_keys=True)
    handle.write("\n")
with open(canary_path, "r", encoding="utf-8") as handle:
    canary = json.load(handle)
canary["accepted_share_required"] = True
canary["accepted_share_minimum"] = 2
canary["canary_source"] = "configured"
canary.setdefault("checks", {})["accepted_share_minimum_met"] = True
with open(canary_path, "w", encoding="utf-8") as handle:
    json.dump(canary, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
write_sha_manifest "$INSUFFICIENT_SHARE_DIR" "PUBLIC-ACCEPTANCE-SHA256SUMS"
(
  cd "$INSUFFICIENT_SHARE_ROOT"
  tar -czf "$INSUFFICIENT_SHARE_ARCHIVE" "$(basename "$INSUFFICIENT_SHARE_DIR")"
)
sha256_file "$INSUFFICIENT_SHARE_ARCHIVE" >"$INSUFFICIENT_SHARE_ARCHIVE.sha256"

if "$VERIFY_ACCEPTANCE_SCRIPT" "$INSUFFICIENT_SHARE_ARCHIVE" "$INSUFFICIENT_SHARE_ARCHIVE.sha256" >"$OUTPUT_DIR/insufficient-accepted-share-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/insufficient-accepted-share-verify.log" >&2
  fail "public acceptance verifier accepted insufficient canary accepted shares"
fi

if grep -Fq "accepted_share_minimum_met=False" "$OUTPUT_DIR/insufficient-accepted-share-verify.log"; then
  printf 'ok: insufficient public acceptance canary accepted shares rejected\n'
else
  cat "$OUTPUT_DIR/insufficient-accepted-share-verify.log" >&2
  fail "insufficient accepted-share package failed for an unexpected reason"
fi

STALE_CANARY_ROOT="$OUTPUT_DIR/stale-canary"
mkdir -p "$STALE_CANARY_ROOT"
STALE_CANARY_ARCHIVE="$OUTPUT_DIR/public-acceptance-stale-canary.tar.gz"
make_acceptance_package "$STALE_CANARY_ROOT" "https://1.1.1.1" "8.8.8.8:3333" "$STALE_CANARY_ARCHIVE" 1
STALE_CANARY_DIR="$STALE_CANARY_ROOT/public-acceptance-evidence"
python3 - "$STALE_CANARY_DIR/public-acceptance-summary.json" "$STALE_CANARY_DIR/http-public-canary-miner.json" "$STALE_CANARY_DIR/public-canary-miner.json" <<'PY'
import json
import sys
import time

summary_path, miner_path, canary_path = sys.argv[1:4]
stale_ts = int(time.time()) - 3600
with open(summary_path, "r", encoding="utf-8") as handle:
    summary = json.load(handle)
summary["canary_max_age_seconds"] = 600
with open(summary_path, "w", encoding="utf-8") as handle:
    json.dump(summary, handle, indent=2, sort_keys=True)
    handle.write("\n")
with open(miner_path, "r", encoding="utf-8") as handle:
    miner = json.load(handle)
miner["last_seen_ts"] = stale_ts
with open(miner_path, "w", encoding="utf-8") as handle:
    json.dump(miner, handle, indent=2, sort_keys=True)
    handle.write("\n")
with open(canary_path, "r", encoding="utf-8") as handle:
    canary = json.load(handle)
canary["canary_max_age_seconds"] = 600
canary.setdefault("checks", {})["last_seen_within_max_age"] = False
canary.setdefault("miner", {})["last_seen_ts"] = stale_ts
canary["miner"]["last_seen_age_seconds"] = 3600
with open(canary_path, "w", encoding="utf-8") as handle:
    json.dump(canary, handle, indent=2, sort_keys=True)
    handle.write("\n")
PY
write_sha_manifest "$STALE_CANARY_DIR" "PUBLIC-ACCEPTANCE-SHA256SUMS"
(
  cd "$STALE_CANARY_ROOT"
  tar -czf "$STALE_CANARY_ARCHIVE" "$(basename "$STALE_CANARY_DIR")"
)
sha256_file "$STALE_CANARY_ARCHIVE" >"$STALE_CANARY_ARCHIVE.sha256"

if "$VERIFY_ACCEPTANCE_SCRIPT" "$STALE_CANARY_ARCHIVE" "$STALE_CANARY_ARCHIVE.sha256" >"$OUTPUT_DIR/stale-canary-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/stale-canary-verify.log" >&2
  fail "public acceptance verifier accepted stale canary miner last_seen_ts"
fi

if grep -Fq "last_seen_within_max_age=False" "$OUTPUT_DIR/stale-canary-verify.log"; then
  printf 'ok: stale public acceptance canary last_seen rejected\n'
else
  cat "$OUTPUT_DIR/stale-canary-verify.log" >&2
  fail "stale canary package failed for an unexpected reason"
fi

BAD_ROOT="$OUTPUT_DIR/bad"
mkdir -p "$BAD_ROOT"
BAD_ARCHIVE="$OUTPUT_DIR/public-acceptance-fixture-endpoints.tar.gz"
make_acceptance_package "$BAD_ROOT" "https://pool.example.com" "pool.example.com:3333" "$BAD_ARCHIVE" 1

if "$VERIFY_ACCEPTANCE_SCRIPT" "$BAD_ARCHIVE" "$BAD_ARCHIVE.sha256" >"$OUTPUT_DIR/bad-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/bad-verify.log" >&2
  fail "public acceptance verifier accepted fixture public endpoints"
fi

if grep -Fq "public acceptance endpoints contain fixture markers" "$OUTPUT_DIR/bad-verify.log"; then
  printf 'ok: fixture public acceptance endpoints rejected\n'
else
  cat "$OUTPUT_DIR/bad-verify.log" >&2
  fail "fixture public acceptance package failed for an unexpected reason"
fi

PRIVATE_ROOT="$OUTPUT_DIR/private"
mkdir -p "$PRIVATE_ROOT"
PRIVATE_ARCHIVE="$OUTPUT_DIR/public-acceptance-private-endpoints.tar.gz"
make_acceptance_package "$PRIVATE_ROOT" "https://10.0.0.10" "10.0.0.20:3333" "$PRIVATE_ARCHIVE" 0

if "$VERIFY_ACCEPTANCE_SCRIPT" "$PRIVATE_ARCHIVE" "$PRIVATE_ARCHIVE.sha256" >"$OUTPUT_DIR/private-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/private-verify.log" >&2
  fail "public acceptance verifier accepted non-global public endpoints"
fi

if grep -Fq "public endpoint routability passed missing" "$OUTPUT_DIR/private-verify.log" || \
   grep -Fq "public API endpoint resolves globally missing" "$OUTPUT_DIR/private-verify.log"; then
  printf 'ok: non-global public acceptance endpoints rejected\n'
else
  cat "$OUTPUT_DIR/private-verify.log" >&2
  fail "non-global public acceptance package failed for an unexpected reason"
fi

printf 'good_archive=%s\n' "$GOOD_ARCHIVE"
printf 'summary_binding_archive=%s\n' "$SUMMARY_BINDING_ARCHIVE"
printf 'receipt_sha_archive=%s\n' "$RECEIPT_SHA_ARCHIVE"
printf 'acceptance_toolchain_archive=%s\n' "$ACCEPTANCE_TOOLCHAIN_ARCHIVE"
printf 'report_context_archive=%s\n' "$REPORT_CONTEXT_ARCHIVE"
printf 'summary_release_archive=%s\n' "$SUMMARY_RELEASE_ARCHIVE"
printf 'status_release_archive=%s\n' "$STATUS_RELEASE_ARCHIVE"
printf 'canary_minimum_archive=%s\n' "$CANARY_MIN_ARCHIVE"
printf 'required_canary_source_archive=%s\n' "$REQUIRED_SOURCE_ARCHIVE"
printf 'insufficient_accepted_share_archive=%s\n' "$INSUFFICIENT_SHARE_ARCHIVE"
printf 'stale_canary_archive=%s\n' "$STALE_CANARY_ARCHIVE"
printf 'bad_archive=%s\n' "$BAD_ARCHIVE"
printf 'private_archive=%s\n' "$PRIVATE_ARCHIVE"
printf 'summary: public acceptance self-test passed\n'

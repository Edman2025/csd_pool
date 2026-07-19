#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
OUTPUT_DIR="${CSD_POOL_EVIDENCE_REDACTION_SELF_TEST_DIR:-}"
KEEP_TMP="${CSD_POOL_EVIDENCE_REDACTION_SELF_TEST_KEEP_DIR:-0}"
VERIFY_RECEIPT_SCRIPT="${CSD_POOL_VERIFY_REAL_GO_LIVE_RECEIPT_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-real-go-live-receipt.sh}"
VERIFY_ACCEPTANCE_SCRIPT="${CSD_POOL_VERIFY_PUBLIC_ACCEPTANCE_EVIDENCE_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-public-acceptance-evidence.sh}"
VERIFY_SUMMARY_SCRIPT="${CSD_POOL_VERIFY_REAL_GO_LIVE_SUMMARY_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-real-go-live-summary.sh}"
VERIFY_EVIDENCE_SCRIPT="${CSD_POOL_VERIFY_GO_LIVE_EVIDENCE_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-go-live-evidence.sh}"
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

write_launch_toolchain_manifest_fixture() {
  local dir="$1"
  local target="${2:-production}"
  cat >"$dir/launch-toolchain-manifest.json" <<JSON
{
  "target": "$target",
  "root": "/opt/csd-pool",
  "entries": [
    {"path": "/opt/csd-pool/bin/csd-pool-workers", "basename": "csd-pool-workers", "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", "executable": true},
    {"path": "/opt/csd-pool/ops/bin/csd-pool-go-live-check.sh", "basename": "csd-pool-go-live-check.sh", "sha256": "1123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", "executable": true},
    {"path": "/opt/csd-pool/ops/bin/csd-pool-verify-go-live-evidence.sh", "basename": "csd-pool-verify-go-live-evidence.sh", "sha256": "2123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", "executable": true},
    {"path": "/opt/csd-pool/ops/bin/csd-pool-generate-signoff.sh", "basename": "csd-pool-generate-signoff.sh", "sha256": "3123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", "executable": true},
    {"path": "/opt/csd-pool/ops/bin/csd-pool-export-real-go-live-receipt.sh", "basename": "csd-pool-export-real-go-live-receipt.sh", "sha256": "4123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", "executable": true},
    {"path": "/opt/csd-pool/ops/bin/csd-pool-real-env-doctor.sh", "basename": "csd-pool-real-env-doctor.sh", "sha256": "5123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef", "executable": true}
  ],
  "required_basenames": [
    "csd-pool-workers",
    "csd-pool-go-live-check.sh",
    "csd-pool-verify-go-live-evidence.sh",
    "csd-pool-generate-signoff.sh",
    "csd-pool-export-real-go-live-receipt.sh",
    "csd-pool-real-env-doctor.sh"
  ]
}
JSON
}

write_signoff_fixture() {
  local dir="$1"
  local evidence_archive="$2"
  local evidence_sha
  evidence_sha="$(sha256_file "$evidence_archive" | awk '{print $1}')"
  cat >"$dir/GO-LIVE-SIGNOFF.md" <<TXT
# CSD Pool Go-Live Signoff

## Evidence Archive

- archive_sha256: \`$evidence_sha\`

## Critical Evidence Files

- pool-endpoint-binding.log: \`present\`
- http-api-pool.json: \`present\`
- external-public-pool-binding.log: \`present\`
- http-public-api-pool.json: \`present\`
TXT
}

cleanup() {
  if [[ "$KEEP_TMP" != "1" && "$OWN_TMP_DIR" == "1" && -n "${OUTPUT_DIR:-}" && -d "$OUTPUT_DIR" ]]; then
    rm -rf "$OUTPUT_DIR"
  fi
}
trap cleanup EXIT

[[ -x "$VERIFY_RECEIPT_SCRIPT" ]] || fail "receipt verifier not executable: $VERIFY_RECEIPT_SCRIPT"
[[ -x "$VERIFY_ACCEPTANCE_SCRIPT" ]] || fail "public acceptance verifier not executable: $VERIFY_ACCEPTANCE_SCRIPT"
[[ -x "$VERIFY_SUMMARY_SCRIPT" ]] || fail "real go-live summary verifier not executable: $VERIFY_SUMMARY_SCRIPT"
[[ -x "$VERIFY_EVIDENCE_SCRIPT" ]] || fail "go-live evidence verifier not executable: $VERIFY_EVIDENCE_SCRIPT"

if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-evidence-redaction-self-test.XXXXXX")"
  OWN_TMP_DIR=1
else
  mkdir -p "$OUTPUT_DIR"
  OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
fi

printf 'CSD Pool evidence redaction self-test\n'
printf 'output_dir=%s\n' "$OUTPUT_DIR"

STUB_VERIFY="$OUTPUT_DIR/ok-evidence-verifier.sh"
cat >"$STUB_VERIFY" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'stub evidence verifier passed\n'
SH
chmod 0755 "$STUB_VERIFY"

RECEIPT_ROOT="$OUTPUT_DIR/receipt"
RECEIPT_DIR="$RECEIPT_ROOT/csd-pool-self-test-real-go-live-receipt-20260623T000000Z"
mkdir -p "$RECEIPT_DIR"
cat >"$RECEIPT_DIR/RECEIPT-MANIFEST.txt" <<'TXT'
included_files:
  REAL-GO-LIVE-SUMMARY.txt
  launch-toolchain-manifest.json
  go-live-summary.json
TXT
write_launch_toolchain_manifest_fixture "$RECEIPT_DIR" "public-beta"
cat >"$RECEIPT_DIR/REAL-GO-LIVE-SUMMARY.txt" <<'TXT'
status=passed
target=public-beta
launch_toolchain_manifest=/tmp/launch-toolchain-manifest.json
launch_toolchain_manifest_sha256=self-test-toolchain-sha
real_environment_doctor_report=/tmp/REAL-ENVIRONMENT-DOCTOR.txt
real_environment_doctor_report_sha256=self-test-doctor-report-sha
real_environment_doctor_summary=/tmp/real-environment-doctor-summary.json
real_environment_doctor_summary_sha256=self-test-doctor-summary-sha
evidence_archive=/tmp/self-test-go-live-evidence.tar.gz
evidence_sha256=/tmp/self-test-go-live-evidence.tar.gz.sha256
TXT
cat >"$RECEIPT_DIR/REAL-ENVIRONMENT-DOCTOR.txt" <<'TXT'
CSD Pool Real Environment Doctor
status=ready_for_real_go_live
TXT
printf '{"status":"ready_for_real_go_live","go_live_target":"public-beta","hard_failures":0}\n' >"$RECEIPT_DIR/real-environment-doctor-summary.json"
cat >"$RECEIPT_DIR/real-go-live-inputs.log" <<'TXT'
real_go_live_inputs_ok=True
dry_run_env_false=True
env_example_path=False
config_example_path=False
TXT
cat >"$RECEIPT_DIR/real-go-live-postcheck.log" <<'TXT'
real_go_live_postcheck_ok=True
summary_dry_run_false=True
sha256_line_matches_archive=True
TXT
cat >"$RECEIPT_DIR/GO-LIVE-REPORT.txt" <<'TXT'
CSD Pool Go Live
leaked_database_url=postgres://pool:secret-password@example.net/csd_pool
TXT
printf '{"summary":{"status":"passed"}}\n' >"$RECEIPT_DIR/go-live-summary.json"
printf 'fake go-live evidence archive placeholder\n' >"$RECEIPT_DIR/self-test-go-live-evidence.tar.gz"
sha256_file "$RECEIPT_DIR/self-test-go-live-evidence.tar.gz" >"$RECEIPT_DIR/self-test-go-live-evidence.tar.gz.sha256"
write_signoff_fixture "$RECEIPT_DIR" "$RECEIPT_DIR/self-test-go-live-evidence.tar.gz"
write_sha_manifest "$RECEIPT_DIR" "RECEIPT-SHA256SUMS"
(
  cd "$RECEIPT_ROOT"
  tar -czf "$OUTPUT_DIR/receipt-redaction-leak.tar.gz" "$(basename "$RECEIPT_DIR")"
)
sha256_file "$OUTPUT_DIR/receipt-redaction-leak.tar.gz" >"$OUTPUT_DIR/receipt-redaction-leak.tar.gz.sha256"

if CSD_POOL_GO_LIVE_VERIFY_SCRIPT="$STUB_VERIFY" "$VERIFY_RECEIPT_SCRIPT" \
  "$OUTPUT_DIR/receipt-redaction-leak.tar.gz" \
  "$OUTPUT_DIR/receipt-redaction-leak.tar.gz.sha256" \
  >"$OUTPUT_DIR/receipt-redaction-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/receipt-redaction-verify.log" >&2
  fail "receipt verifier accepted a package with a leaked PostgreSQL password URL"
fi
if grep -Fq "receipt redaction scan failed" "$OUTPUT_DIR/receipt-redaction-verify.log" && \
   grep -Fq "postgres_password_url" "$OUTPUT_DIR/receipt-redaction-verify.log"; then
  printf 'ok: receipt redaction leak rejected\n'
else
  cat "$OUTPUT_DIR/receipt-redaction-verify.log" >&2
  fail "receipt verifier failed for an unexpected reason"
fi

SUMMARY_TARGET_DIR="$OUTPUT_DIR/summary-target-binding"
mkdir -p "$SUMMARY_TARGET_DIR"
cat >"$SUMMARY_TARGET_DIR/real-go-live-inputs.log" <<'TXT'
target=public-beta
real_go_live_inputs_ok=True
dry_run_env_false=True
env_example_path=False
config_example_path=False
workers_bin_executable=True
public_required=True
public_api_https=True
public_stratum_addr=8.8.8.8:3333
TXT
cat >"$SUMMARY_TARGET_DIR/real-go-live-postcheck.log" <<'TXT'
real_go_live_postcheck_ok=True
summary_dry_run_false=True
summary_target_matches=True
sha256_line_matches_archive=True
TXT
cat >"$SUMMARY_TARGET_DIR/GO-LIVE-REPORT.txt" <<'TXT'
CSD Pool Go Live
status=passed
TXT
printf '{"target":"production","dry_run":false,"summary":{"status":"passed","fail":0}}\n' >"$SUMMARY_TARGET_DIR/go-live-summary.json"
cat >"$SUMMARY_TARGET_DIR/REAL-ENVIRONMENT-DOCTOR.txt" <<'TXT'
CSD Pool Real Environment Doctor
status=ready_for_real_go_live
TXT
printf '{"status":"ready_for_real_go_live","go_live_target":"production","hard_failures":0}\n' >"$SUMMARY_TARGET_DIR/real-environment-doctor-summary.json"
write_launch_toolchain_manifest_fixture "$SUMMARY_TARGET_DIR" "production"
printf 'fake go-live evidence archive placeholder\n' >"$SUMMARY_TARGET_DIR/self-test-go-live-evidence.tar.gz"
sha256_file "$SUMMARY_TARGET_DIR/self-test-go-live-evidence.tar.gz" >"$SUMMARY_TARGET_DIR/self-test-go-live-evidence.tar.gz.sha256"
write_signoff_fixture "$SUMMARY_TARGET_DIR" "$SUMMARY_TARGET_DIR/self-test-go-live-evidence.tar.gz"
cat >"$SUMMARY_TARGET_DIR/REAL-GO-LIVE-SUMMARY.txt" <<TXT
status=passed
target=production
real_go_live_inputs=$SUMMARY_TARGET_DIR/real-go-live-inputs.log
real_go_live_inputs_sha256=$(sha256_file "$SUMMARY_TARGET_DIR/real-go-live-inputs.log" | awk '{print $1}')
launch_toolchain_manifest=$SUMMARY_TARGET_DIR/launch-toolchain-manifest.json
launch_toolchain_manifest_sha256=$(sha256_file "$SUMMARY_TARGET_DIR/launch-toolchain-manifest.json" | awk '{print $1}')
real_environment_doctor_report=$SUMMARY_TARGET_DIR/REAL-ENVIRONMENT-DOCTOR.txt
real_environment_doctor_report_sha256=$(sha256_file "$SUMMARY_TARGET_DIR/REAL-ENVIRONMENT-DOCTOR.txt" | awk '{print $1}')
real_environment_doctor_summary=$SUMMARY_TARGET_DIR/real-environment-doctor-summary.json
real_environment_doctor_summary_sha256=$(sha256_file "$SUMMARY_TARGET_DIR/real-environment-doctor-summary.json" | awk '{print $1}')
real_go_live_postcheck=$SUMMARY_TARGET_DIR/real-go-live-postcheck.log
go_live_report=$SUMMARY_TARGET_DIR/GO-LIVE-REPORT.txt
go_live_report_sha256=$(sha256_file "$SUMMARY_TARGET_DIR/GO-LIVE-REPORT.txt" | awk '{print $1}')
go_live_summary=$SUMMARY_TARGET_DIR/go-live-summary.json
go_live_summary_sha256=$(sha256_file "$SUMMARY_TARGET_DIR/go-live-summary.json" | awk '{print $1}')
go_live_signoff=$SUMMARY_TARGET_DIR/GO-LIVE-SIGNOFF.md
go_live_signoff_sha256=$(sha256_file "$SUMMARY_TARGET_DIR/GO-LIVE-SIGNOFF.md" | awk '{print $1}')
evidence_archive=$SUMMARY_TARGET_DIR/self-test-go-live-evidence.tar.gz
evidence_archive_sha256=$(sha256_file "$SUMMARY_TARGET_DIR/self-test-go-live-evidence.tar.gz" | awk '{print $1}')
evidence_sha256=$SUMMARY_TARGET_DIR/self-test-go-live-evidence.tar.gz.sha256
TXT

if CSD_POOL_GO_LIVE_VERIFY_SCRIPT="$STUB_VERIFY" "$VERIFY_SUMMARY_SCRIPT" \
  "$SUMMARY_TARGET_DIR/REAL-GO-LIVE-SUMMARY.txt" \
  >"$OUTPUT_DIR/summary-target-binding-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/summary-target-binding-verify.log" >&2
  fail "real go-live summary verifier accepted mismatched input target"
fi
if grep -Fq "inputs_target_matches_summary=False" "$OUTPUT_DIR/summary-target-binding-verify.log"; then
  printf 'ok: real go-live summary target mismatch rejected\n'
else
  cat "$OUTPUT_DIR/summary-target-binding-verify.log" >&2
  fail "summary target mismatch package failed for an unexpected reason"
fi

SUMMARY_DOCTOR_DIR="$OUTPUT_DIR/summary-doctor-binding"
mkdir -p "$SUMMARY_DOCTOR_DIR"
cat >"$SUMMARY_DOCTOR_DIR/real-go-live-inputs.log" <<'TXT'
target=production
real_go_live_inputs_ok=True
dry_run_env_false=True
env_example_path=False
config_example_path=False
workers_bin_executable=True
public_required=True
public_api_https=True
public_stratum_addr=8.8.8.8:3333
TXT
cat >"$SUMMARY_DOCTOR_DIR/real-go-live-postcheck.log" <<'TXT'
real_go_live_postcheck_ok=True
summary_dry_run_false=True
summary_target_matches=True
sha256_line_matches_archive=True
TXT
cat >"$SUMMARY_DOCTOR_DIR/GO-LIVE-REPORT.txt" <<'TXT'
CSD Pool Go Live
status=passed
TXT
printf '{"target":"production","dry_run":false,"summary":{"status":"passed","fail":0}}\n' >"$SUMMARY_DOCTOR_DIR/go-live-summary.json"
cat >"$SUMMARY_DOCTOR_DIR/REAL-ENVIRONMENT-DOCTOR.txt" <<'TXT'
CSD Pool Real Environment Doctor
status=needs_real_inputs
TXT
printf '{"status":"needs_real_inputs","go_live_target":"production","hard_failures":1}\n' >"$SUMMARY_DOCTOR_DIR/real-environment-doctor-summary.json"
write_launch_toolchain_manifest_fixture "$SUMMARY_DOCTOR_DIR" "production"
printf 'fake go-live evidence archive placeholder\n' >"$SUMMARY_DOCTOR_DIR/self-test-go-live-evidence.tar.gz"
sha256_file "$SUMMARY_DOCTOR_DIR/self-test-go-live-evidence.tar.gz" >"$SUMMARY_DOCTOR_DIR/self-test-go-live-evidence.tar.gz.sha256"
write_signoff_fixture "$SUMMARY_DOCTOR_DIR" "$SUMMARY_DOCTOR_DIR/self-test-go-live-evidence.tar.gz"
cat >"$SUMMARY_DOCTOR_DIR/REAL-GO-LIVE-SUMMARY.txt" <<TXT
status=passed
target=production
real_go_live_inputs=$SUMMARY_DOCTOR_DIR/real-go-live-inputs.log
real_go_live_inputs_sha256=$(sha256_file "$SUMMARY_DOCTOR_DIR/real-go-live-inputs.log" | awk '{print $1}')
launch_toolchain_manifest=$SUMMARY_DOCTOR_DIR/launch-toolchain-manifest.json
launch_toolchain_manifest_sha256=$(sha256_file "$SUMMARY_DOCTOR_DIR/launch-toolchain-manifest.json" | awk '{print $1}')
real_environment_doctor_report=$SUMMARY_DOCTOR_DIR/REAL-ENVIRONMENT-DOCTOR.txt
real_environment_doctor_report_sha256=$(sha256_file "$SUMMARY_DOCTOR_DIR/REAL-ENVIRONMENT-DOCTOR.txt" | awk '{print $1}')
real_environment_doctor_summary=$SUMMARY_DOCTOR_DIR/real-environment-doctor-summary.json
real_environment_doctor_summary_sha256=$(sha256_file "$SUMMARY_DOCTOR_DIR/real-environment-doctor-summary.json" | awk '{print $1}')
real_go_live_postcheck=$SUMMARY_DOCTOR_DIR/real-go-live-postcheck.log
go_live_report=$SUMMARY_DOCTOR_DIR/GO-LIVE-REPORT.txt
go_live_report_sha256=$(sha256_file "$SUMMARY_DOCTOR_DIR/GO-LIVE-REPORT.txt" | awk '{print $1}')
go_live_summary=$SUMMARY_DOCTOR_DIR/go-live-summary.json
go_live_summary_sha256=$(sha256_file "$SUMMARY_DOCTOR_DIR/go-live-summary.json" | awk '{print $1}')
go_live_signoff=$SUMMARY_DOCTOR_DIR/GO-LIVE-SIGNOFF.md
go_live_signoff_sha256=$(sha256_file "$SUMMARY_DOCTOR_DIR/GO-LIVE-SIGNOFF.md" | awk '{print $1}')
evidence_archive=$SUMMARY_DOCTOR_DIR/self-test-go-live-evidence.tar.gz
evidence_archive_sha256=$(sha256_file "$SUMMARY_DOCTOR_DIR/self-test-go-live-evidence.tar.gz" | awk '{print $1}')
evidence_sha256=$SUMMARY_DOCTOR_DIR/self-test-go-live-evidence.tar.gz.sha256
TXT

if CSD_POOL_GO_LIVE_VERIFY_SCRIPT="$STUB_VERIFY" "$VERIFY_SUMMARY_SCRIPT" \
  "$SUMMARY_DOCTOR_DIR/REAL-GO-LIVE-SUMMARY.txt" \
  >"$OUTPUT_DIR/summary-doctor-binding-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/summary-doctor-binding-verify.log" >&2
  fail "real go-live summary verifier accepted non-ready doctor evidence"
fi
if grep -Fq "doctor_summary_status_ready=False" "$OUTPUT_DIR/summary-doctor-binding-verify.log" && \
   grep -Fq "doctor_summary_hard_failures_zero=False" "$OUTPUT_DIR/summary-doctor-binding-verify.log"; then
  printf 'ok: real go-live summary doctor readiness mismatch rejected\n'
else
  cat "$OUTPUT_DIR/summary-doctor-binding-verify.log" >&2
  fail "summary doctor readiness package failed for an unexpected reason"
fi

EVIDENCE_ROOT="$OUTPUT_DIR/go-live-evidence-endpoint-binding"
EVIDENCE_DIR="$EVIDENCE_ROOT/go-live-evidence"
mkdir -p "$EVIDENCE_DIR"
cat >"$EVIDENCE_DIR/go-live-summary.json" <<'JSON'
{
  "target": "private-beta",
  "dry_run": true,
  "summary": {"status": "passed", "pass": 10, "fail": 0, "skip": 5},
  "endpoints": {
    "api_url": "http://127.0.0.1:8080",
    "stratum_addr": "127.0.0.1:3333",
    "public_api_url": "https://pool.example.net",
    "public_stratum_probe_addr": "pool.example.net:3333",
    "public_stratum_addr": "pool.example.net:3333",
    "public_port_tiers": "3333=standard"
  },
  "evidence": {
    "archive": "/tmp/go-live-evidence.tar.gz",
    "sha256": "/tmp/go-live-evidence.tar.gz.sha256"
  }
}
JSON
cat >"$EVIDENCE_DIR/GO-LIVE-REPORT.txt" <<'TXT'
target=private-beta
dry_run=true
status=passed
pass=10
fail=0
skip=5
api_url=http://127.0.0.1:8080
stratum_addr=127.0.0.1:3333
public_api_url=https://wrong.example.net
public_stratum_probe_addr=pool.example.net:3333
public_stratum_addr=pool.example.net:3333
public_port_tiers=3333=standard
evidence_archive=/tmp/go-live-evidence.tar.gz
evidence_sha256=/tmp/go-live-evidence.tar.gz.sha256
TXT
cat >"$EVIDENCE_DIR/EVIDENCE-MANIFEST.txt" <<'TXT'
created_at_utc=2026-06-23T00:00:00Z
target=private-beta
dry_run=true
status=passed
pass=10
fail=0
skip=5
report=GO-LIVE-REPORT.txt
summary=go-live-summary.json
TXT
cat >"$EVIDENCE_DIR/evidence-redaction-safety.log" <<'TXT'
evidence_redaction_checked_files=80
evidence_redaction_findings=0
evidence_redaction_ok=True
TXT
printf '{"passed":true}\n' >"$EVIDENCE_DIR/config-snapshot.json"
printf 'world_readable=false\nCSD_POOL_DATABASE_URL=present\nCSD_POOL_OPERATOR_TOKEN=present\nCSD_POOL_SIGNER_TOKEN=present\n' >"$EVIDENCE_DIR/env-snapshot.txt"
for file in \
  secrets-permissions-safety.log real-env-readiness.log clock-safety.log disk-safety.log bind-safety.log \
  database-migration-safety.log preflight.log release-integrity.log verify.log systemd-runtime-safety.log \
  runtime-hardening-safety.log resource-limit-safety.log service-provenance-safety.log backup-artifact-safety.log \
  restore-drill.log restore-api-safety.log edge-proxy-safety.log node-endpoint-safety.log signer-safety.log \
  payout-limit-safety.log payout-safety.log payout-controls-safety.log runtime-config-binding.log \
  runtime-status-binding.log status-release-binding.log pool-endpoint-binding.log external-public-status-binding.log \
  external-public-pool-binding.log external-public-config-binding.log \
  getting-started-binding.log external-public-getting-started-binding.log public-dns-safety.log public-api-tls-safety.log \
  public-api-headers-safety.log public-api-surface-safety.log public-operator-auth-boundary.log metrics-surface-safety.log \
  operator-readiness-safety.log stratum-tcp.log public-stratum-tcp.log public-port-tiers-safety.log; do
  printf 'dry_run_fixture_ok=True\n' >"$EVIDENCE_DIR/$file"
done
for file in \
  database-migration.json database-runtime.json restore-http-health.json restore-http-pool.json restore-http-blocks.json \
  restore-http-payments.json restore-http-operator-payout-status.json check-node-template.json node-runtime.json \
  check-signer.json sample-health.json payout-preview.json http-operator-health.json http-operator-alerts.json \
  http-operator-payout-batches.json http-operator-payout-audit.json http-operator-payout-preview.json \
  http-operator-payout-status.json http-api-status.json http-api-pool.json http-api-metrics.json http-api-blocks.json \
  http-api-payments.json http-api-getting-started.json http-public-api-status.json http-public-api-pool.json \
  http-public-api-getting-started.json \
  public-port-tiers-smoke.json public-stratum-smoke.json public-stratum-load.json; do
  printf '{"dry_run_fixture":true}\n' >"$EVIDENCE_DIR/$file"
done
printf 'batch_id,status,txid,recipient,amount_base_units,amount_csd,total_base_units,total_csd\n' >"$EVIDENCE_DIR/http-operator-payout-batches.csv"
printf 'created_at,batch_id,actor,action,details_json\n' >"$EVIDENCE_DIR/http-operator-payout-audit.csv"
printf 'csd_pool_fixture_metric 1\n' >"$EVIDENCE_DIR/http-prometheus-metrics.txt"
printf 'dry-run getting started fixture\n' >"$EVIDENCE_DIR/http-public-getting-started.txt"
write_sha_manifest "$EVIDENCE_DIR" "EVIDENCE-SHA256SUMS"
(
  cd "$EVIDENCE_DIR"
  tar -czf "$OUTPUT_DIR/go-live-endpoint-mismatch.tar.gz" .
)
sha256_file "$OUTPUT_DIR/go-live-endpoint-mismatch.tar.gz" >"$OUTPUT_DIR/go-live-endpoint-mismatch.tar.gz.sha256"

EVIDENCE_VERIFY_TMP="$OUTPUT_DIR/go-live-endpoint-mismatch-tmp"
if CSD_POOL_EVIDENCE_ALLOW_DRY_RUN=1 \
  CSD_POOL_EVIDENCE_KEEP_DIR=1 \
  CSD_POOL_EVIDENCE_TMP_DIR="$EVIDENCE_VERIFY_TMP" \
  "$VERIFY_EVIDENCE_SCRIPT" \
  "$OUTPUT_DIR/go-live-endpoint-mismatch.tar.gz" \
  >"$OUTPUT_DIR/go-live-endpoint-mismatch-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/go-live-endpoint-mismatch-verify.log" >&2
  fail "go-live evidence verifier accepted mismatched public API endpoint metadata"
fi
if grep -Fq "summary_report_endpoint_public_api_url_match: failed" "$EVIDENCE_VERIFY_TMP/csd-pool-evidence-metadata-consistency.log"; then
  printf 'ok: go-live evidence endpoint mismatch rejected\n'
else
  cat "$OUTPUT_DIR/go-live-endpoint-mismatch-verify.log" >&2
  [[ -f "$EVIDENCE_VERIFY_TMP/csd-pool-evidence-metadata-consistency.log" ]] && \
    cat "$EVIDENCE_VERIFY_TMP/csd-pool-evidence-metadata-consistency.log" >&2
  fail "go-live evidence endpoint mismatch package failed for an unexpected reason"
fi

POOL_MISSING_ROOT="$OUTPUT_DIR/go-live-evidence-missing-public-pool"
POOL_MISSING_DIR="$POOL_MISSING_ROOT/go-live-evidence"
rm -rf "$POOL_MISSING_ROOT"
mkdir -p "$POOL_MISSING_ROOT"
cp -R "$EVIDENCE_DIR" "$POOL_MISSING_DIR"
python3 - "$POOL_MISSING_DIR/GO-LIVE-REPORT.txt" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
text = text.replace("public_api_url=https://wrong.example.net", "public_api_url=https://pool.example.net")
path.write_text(text, encoding="utf-8")
PY
rm -f "$POOL_MISSING_DIR/http-public-api-pool.json"
write_sha_manifest "$POOL_MISSING_DIR" "EVIDENCE-SHA256SUMS"
(
  cd "$POOL_MISSING_DIR"
  tar -czf "$OUTPUT_DIR/go-live-missing-public-pool.tar.gz" .
)
sha256_file "$OUTPUT_DIR/go-live-missing-public-pool.tar.gz" >"$OUTPUT_DIR/go-live-missing-public-pool.tar.gz.sha256"
POOL_MISSING_VERIFY_TMP="$OUTPUT_DIR/go-live-missing-public-pool-tmp"
if CSD_POOL_EVIDENCE_ALLOW_DRY_RUN=1 \
  CSD_POOL_EVIDENCE_KEEP_DIR=1 \
  CSD_POOL_EVIDENCE_TMP_DIR="$POOL_MISSING_VERIFY_TMP" \
  "$VERIFY_EVIDENCE_SCRIPT" \
  "$OUTPUT_DIR/go-live-missing-public-pool.tar.gz" \
  >"$OUTPUT_DIR/go-live-missing-public-pool-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/go-live-missing-public-pool-verify.log" >&2
  fail "go-live evidence verifier accepted evidence missing the external public pool endpoint report"
fi
if grep -Fq "external public pool endpoint report missing" "$OUTPUT_DIR/go-live-missing-public-pool-verify.log"; then
  printf 'ok: go-live evidence missing public pool endpoint rejected\n'
else
  cat "$OUTPUT_DIR/go-live-missing-public-pool-verify.log" >&2
  fail "missing public pool endpoint package failed for an unexpected reason"
fi

MANIFEST_ROOT="$OUTPUT_DIR/receipt-manifest-binding"
MANIFEST_DIR="$MANIFEST_ROOT/csd-pool-self-test-real-go-live-receipt-20260623T000001Z"
mkdir -p "$MANIFEST_DIR"
cat >"$MANIFEST_DIR/real-go-live-inputs.log" <<'TXT'
real_go_live_inputs_ok=True
dry_run_env_false=True
env_example_path=False
config_example_path=False
TXT
cat >"$MANIFEST_DIR/real-go-live-postcheck.log" <<'TXT'
real_go_live_postcheck_ok=True
summary_dry_run_false=True
sha256_line_matches_archive=True
TXT
cat >"$MANIFEST_DIR/GO-LIVE-REPORT.txt" <<'TXT'
CSD Pool Go Live
status=passed
TXT
printf '{"summary":{"status":"passed"}}\n' >"$MANIFEST_DIR/go-live-summary.json"
cat >"$MANIFEST_DIR/REAL-ENVIRONMENT-DOCTOR.txt" <<'TXT'
CSD Pool Real Environment Doctor
status=ready_for_real_go_live
TXT
printf '{"status":"ready_for_real_go_live","go_live_target":"production","hard_failures":0}\n' >"$MANIFEST_DIR/real-environment-doctor-summary.json"
write_launch_toolchain_manifest_fixture "$MANIFEST_DIR" "production"
printf 'fake go-live evidence archive placeholder\n' >"$MANIFEST_DIR/self-test-go-live-evidence.tar.gz"
sha256_file "$MANIFEST_DIR/self-test-go-live-evidence.tar.gz" >"$MANIFEST_DIR/self-test-go-live-evidence.tar.gz.sha256"
write_signoff_fixture "$MANIFEST_DIR" "$MANIFEST_DIR/self-test-go-live-evidence.tar.gz"
cat >"$MANIFEST_DIR/REAL-GO-LIVE-SUMMARY.txt" <<TXT
status=passed
target=production
real_go_live_inputs=/tmp/real-go-live-inputs.log
real_go_live_inputs_sha256=$(sha256_file "$MANIFEST_DIR/real-go-live-inputs.log" | awk '{print $1}')
launch_toolchain_manifest=/tmp/launch-toolchain-manifest.json
launch_toolchain_manifest_sha256=$(sha256_file "$MANIFEST_DIR/launch-toolchain-manifest.json" | awk '{print $1}')
real_environment_doctor_report=/tmp/REAL-ENVIRONMENT-DOCTOR.txt
real_environment_doctor_report_sha256=$(sha256_file "$MANIFEST_DIR/REAL-ENVIRONMENT-DOCTOR.txt" | awk '{print $1}')
real_environment_doctor_summary=/tmp/real-environment-doctor-summary.json
real_environment_doctor_summary_sha256=$(sha256_file "$MANIFEST_DIR/real-environment-doctor-summary.json" | awk '{print $1}')
real_go_live_postcheck=/tmp/real-go-live-postcheck.log
go_live_report=/tmp/GO-LIVE-REPORT.txt
go_live_report_sha256=$(sha256_file "$MANIFEST_DIR/GO-LIVE-REPORT.txt" | awk '{print $1}')
go_live_summary=/tmp/go-live-summary.json
go_live_summary_sha256=$(sha256_file "$MANIFEST_DIR/go-live-summary.json" | awk '{print $1}')
go_live_signoff=/tmp/GO-LIVE-SIGNOFF.md
go_live_signoff_sha256=$(sha256_file "$MANIFEST_DIR/GO-LIVE-SIGNOFF.md" | awk '{print $1}')
evidence_archive=/tmp/self-test-go-live-evidence.tar.gz
evidence_archive_sha256=$(sha256_file "$MANIFEST_DIR/self-test-go-live-evidence.tar.gz" | awk '{print $1}')
evidence_sha256=/tmp/self-test-go-live-evidence.tar.gz.sha256
TXT
cat >"$MANIFEST_DIR/RECEIPT-MANIFEST.txt" <<'TXT'
name=csd-pool-self-test-real-go-live-receipt-20260623T000001Z
created_at_utc=20260623T000001Z
target=public-beta
source_summary=/tmp/REAL-GO-LIVE-SUMMARY.txt
verify_real_go_live_summary=/opt/csd-pool/ops/bin/csd-pool-verify-real-go-live-summary.sh
included_files:
  REAL-GO-LIVE-SUMMARY.txt
  launch-toolchain-manifest.json
  REAL-ENVIRONMENT-DOCTOR.txt
  real-environment-doctor-summary.json
  go-live-summary.json
TXT
write_sha_manifest "$MANIFEST_DIR" "RECEIPT-SHA256SUMS"
(
  cd "$MANIFEST_ROOT"
  tar -czf "$OUTPUT_DIR/receipt-manifest-target-mismatch.tar.gz" "$(basename "$MANIFEST_DIR")"
)
sha256_file "$OUTPUT_DIR/receipt-manifest-target-mismatch.tar.gz" >"$OUTPUT_DIR/receipt-manifest-target-mismatch.tar.gz.sha256"

if CSD_POOL_GO_LIVE_VERIFY_SCRIPT="$STUB_VERIFY" "$VERIFY_RECEIPT_SCRIPT" \
  "$OUTPUT_DIR/receipt-manifest-target-mismatch.tar.gz" \
  "$OUTPUT_DIR/receipt-manifest-target-mismatch.tar.gz.sha256" \
  >"$OUTPUT_DIR/receipt-manifest-target-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/receipt-manifest-target-verify.log" >&2
  fail "receipt verifier accepted a package with mismatched manifest target"
fi
if grep -Fq "receipt manifest target mismatch" "$OUTPUT_DIR/receipt-manifest-target-verify.log"; then
  printf 'ok: receipt manifest target mismatch rejected\n'
else
  cat "$OUTPUT_DIR/receipt-manifest-target-verify.log" >&2
  fail "receipt manifest target mismatch package failed for an unexpected reason"
fi

DOCTOR_RECEIPT_ROOT="$OUTPUT_DIR/receipt-doctor-binding"
DOCTOR_RECEIPT_DIR="$DOCTOR_RECEIPT_ROOT/csd-pool-self-test-real-go-live-receipt-20260623T000002Z"
mkdir -p "$DOCTOR_RECEIPT_DIR"
cat >"$DOCTOR_RECEIPT_DIR/real-go-live-inputs.log" <<'TXT'
real_go_live_inputs_ok=True
dry_run_env_false=True
env_example_path=False
config_example_path=False
TXT
cat >"$DOCTOR_RECEIPT_DIR/real-go-live-postcheck.log" <<'TXT'
real_go_live_postcheck_ok=True
summary_dry_run_false=True
sha256_line_matches_archive=True
TXT
cat >"$DOCTOR_RECEIPT_DIR/GO-LIVE-REPORT.txt" <<'TXT'
CSD Pool Go Live
status=passed
TXT
printf '{"summary":{"status":"passed"}}\n' >"$DOCTOR_RECEIPT_DIR/go-live-summary.json"
cat >"$DOCTOR_RECEIPT_DIR/REAL-ENVIRONMENT-DOCTOR.txt" <<'TXT'
CSD Pool Real Environment Doctor
status=needs_real_inputs
TXT
printf '{"status":"needs_real_inputs","go_live_target":"production","hard_failures":1}\n' >"$DOCTOR_RECEIPT_DIR/real-environment-doctor-summary.json"
write_launch_toolchain_manifest_fixture "$DOCTOR_RECEIPT_DIR" "production"
printf 'fake go-live evidence archive placeholder\n' >"$DOCTOR_RECEIPT_DIR/self-test-go-live-evidence.tar.gz"
sha256_file "$DOCTOR_RECEIPT_DIR/self-test-go-live-evidence.tar.gz" >"$DOCTOR_RECEIPT_DIR/self-test-go-live-evidence.tar.gz.sha256"
write_signoff_fixture "$DOCTOR_RECEIPT_DIR" "$DOCTOR_RECEIPT_DIR/self-test-go-live-evidence.tar.gz"
cat >"$DOCTOR_RECEIPT_DIR/REAL-GO-LIVE-SUMMARY.txt" <<TXT
status=passed
target=production
real_go_live_inputs=/tmp/real-go-live-inputs.log
real_go_live_inputs_sha256=$(sha256_file "$DOCTOR_RECEIPT_DIR/real-go-live-inputs.log" | awk '{print $1}')
launch_toolchain_manifest=/tmp/launch-toolchain-manifest.json
launch_toolchain_manifest_sha256=$(sha256_file "$DOCTOR_RECEIPT_DIR/launch-toolchain-manifest.json" | awk '{print $1}')
real_environment_doctor_report=/tmp/REAL-ENVIRONMENT-DOCTOR.txt
real_environment_doctor_report_sha256=$(sha256_file "$DOCTOR_RECEIPT_DIR/REAL-ENVIRONMENT-DOCTOR.txt" | awk '{print $1}')
real_environment_doctor_summary=/tmp/real-environment-doctor-summary.json
real_environment_doctor_summary_sha256=$(sha256_file "$DOCTOR_RECEIPT_DIR/real-environment-doctor-summary.json" | awk '{print $1}')
real_go_live_postcheck=/tmp/real-go-live-postcheck.log
go_live_report=/tmp/GO-LIVE-REPORT.txt
go_live_report_sha256=$(sha256_file "$DOCTOR_RECEIPT_DIR/GO-LIVE-REPORT.txt" | awk '{print $1}')
go_live_summary=/tmp/go-live-summary.json
go_live_summary_sha256=$(sha256_file "$DOCTOR_RECEIPT_DIR/go-live-summary.json" | awk '{print $1}')
go_live_signoff=/tmp/GO-LIVE-SIGNOFF.md
go_live_signoff_sha256=$(sha256_file "$DOCTOR_RECEIPT_DIR/GO-LIVE-SIGNOFF.md" | awk '{print $1}')
evidence_archive=/tmp/self-test-go-live-evidence.tar.gz
evidence_archive_sha256=$(sha256_file "$DOCTOR_RECEIPT_DIR/self-test-go-live-evidence.tar.gz" | awk '{print $1}')
evidence_sha256=/tmp/self-test-go-live-evidence.tar.gz.sha256
TXT
cat >"$DOCTOR_RECEIPT_DIR/RECEIPT-MANIFEST.txt" <<'TXT'
name=csd-pool-self-test-real-go-live-receipt-20260623T000002Z
created_at_utc=20260623T000002Z
target=production
source_summary=/tmp/REAL-GO-LIVE-SUMMARY.txt
verify_real_go_live_summary=/opt/csd-pool/ops/bin/csd-pool-verify-real-go-live-summary.sh
included_files:
  REAL-GO-LIVE-SUMMARY.txt
  launch-toolchain-manifest.json
  REAL-ENVIRONMENT-DOCTOR.txt
  real-environment-doctor-summary.json
  go-live-summary.json
TXT
write_sha_manifest "$DOCTOR_RECEIPT_DIR" "RECEIPT-SHA256SUMS"
(
  cd "$DOCTOR_RECEIPT_ROOT"
  tar -czf "$OUTPUT_DIR/receipt-doctor-not-ready.tar.gz" "$(basename "$DOCTOR_RECEIPT_DIR")"
)
sha256_file "$OUTPUT_DIR/receipt-doctor-not-ready.tar.gz" >"$OUTPUT_DIR/receipt-doctor-not-ready.tar.gz.sha256"

if CSD_POOL_GO_LIVE_VERIFY_SCRIPT="$STUB_VERIFY" "$VERIFY_RECEIPT_SCRIPT" \
  "$OUTPUT_DIR/receipt-doctor-not-ready.tar.gz" \
  "$OUTPUT_DIR/receipt-doctor-not-ready.tar.gz.sha256" \
  >"$OUTPUT_DIR/receipt-doctor-not-ready-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/receipt-doctor-not-ready-verify.log" >&2
  fail "receipt verifier accepted non-ready doctor evidence"
fi
if grep -Fq "real environment doctor report missing ready proof" "$OUTPUT_DIR/receipt-doctor-not-ready-verify.log"; then
  printf 'ok: receipt doctor readiness mismatch rejected\n'
else
  cat "$OUTPUT_DIR/receipt-doctor-not-ready-verify.log" >&2
  fail "receipt doctor readiness package failed for an unexpected reason"
fi

ACCEPTANCE_ROOT="$OUTPUT_DIR/acceptance"
ACCEPTANCE_DIR="$ACCEPTANCE_ROOT/public-acceptance-evidence"
mkdir -p "$ACCEPTANCE_DIR"
cat >"$ACCEPTANCE_DIR/PUBLIC-ACCEPTANCE-REPORT.txt" <<'TXT'
CSD Pool Public Acceptance
status=passed
public_api_url=https://1.1.1.1
public_stratum_addr=8.8.8.8:3333
receipt_archive=/tmp/csd-pool-self-test-real-go-live-receipt.tar.gz
receipt_archive_sha256=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef
pass=12
fail=0
skip=1
TXT
cat >"$ACCEPTANCE_DIR/public-acceptance-summary.json" <<'JSON'
{
  "status":"passed",
  "pass":12,
  "fail":0,
  "skip":1,
  "public_api_url":"https://1.1.1.1",
  "public_stratum_addr":"8.8.8.8:3333",
  "receipt_archive":"/tmp/csd-pool-self-test-real-go-live-receipt.tar.gz",
  "receipt_archive_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "acceptance_toolchain_manifest":"acceptance-toolchain-manifest.json",
  "accepted_share_required":"0",
  "accepted_share_minimum":1,
  "reports":{
    "receipt_verify":"receipt-verify.log",
    "receipt_binding":"receipt-binding.log",
    "api_health":"http-public-health.json",
    "api_status":"http-public-status.json",
    "status_release_binding":"public-status-release-binding.log",
    "api_pool":"http-public-pool.json",
    "api_getting_started":"http-public-getting-started.json",
    "getting_started_binding":"getting-started-binding.log",
    "public_endpoint_routability":"public-endpoint-routability.log",
    "stratum_smoke":"public-stratum-smoke.json",
    "stratum_submit_probe":"public-stratum-submit-probe.json",
    "stratum_load":"public-stratum-load.json",
    "canary_miner":"public-canary-miner.json",
    "canary_miner_api":"http-public-canary-miner.json",
    "canary_miner_workers_api":"http-public-canary-miner-workers.json"
  }
}
JSON
cat >"$ACCEPTANCE_DIR/acceptance-toolchain-manifest.json" <<'JSON'
{
  "target": "public-acceptance",
  "public_api_url": "https://1.1.1.1",
  "public_stratum_addr": "8.8.8.8:3333",
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
cat >"$ACCEPTANCE_DIR/receipt-verify.log" <<'TXT'
ok: launch toolchain manifest proves real launch scripts
summary: pass=1 fail=0
TXT
cat >"$ACCEPTANCE_DIR/receipt-binding.log" <<'TXT'
receipt_public_api_matches=True
receipt_public_stratum_matches=True
leaked_mirror=https://pool:secret-password@example.net/api
TXT
printf '{"ok":true}\n' >"$ACCEPTANCE_DIR/http-public-health.json"
printf '{"ok":true}\n' >"$ACCEPTANCE_DIR/http-public-status.json"
cat >"$ACCEPTANCE_DIR/public-status-release-binding.log" <<'TXT'
public_status_release_binding_ok=True
public_status_revision_matches=True
public_status_commit_matches=True
public_status_build_time_matches=True
public_status_database_runtime_matches=True
TXT
printf '{"ok":true}\n' >"$ACCEPTANCE_DIR/http-public-pool.json"
printf '{"stratum_endpoint":"8.8.8.8:3333","commands":[{"command":"miner --pool 8.8.8.8:3333"}]}\n' >"$ACCEPTANCE_DIR/http-public-getting-started.json"
cat >"$ACCEPTANCE_DIR/getting-started-binding.log" <<'TXT'
stratum_endpoint_matches=True
commands_include_stratum_endpoint=True
TXT
cat >"$ACCEPTANCE_DIR/public-endpoint-routability.log" <<'TXT'
public_api_dns_all_global=True
public_stratum_dns_all_global=True
public_endpoint_routability_ok=True
TXT
printf '{"requested_clients":1,"succeeded_clients":1,"failed_clients":0,"successes":[{"worker":"abc"}]}\n' >"$ACCEPTANCE_DIR/public-stratum-smoke.json"
printf '{"passed":true,"difficulty_seen":true,"notify_seen":true,"submit_response_received":true,"submit_response_standard":true,"submit_result":true}\n' >"$ACCEPTANCE_DIR/public-stratum-submit-probe.json"
printf '{"skipped":true}\n' >"$ACCEPTANCE_DIR/public-stratum-load.json"
printf '{"address":"abc","online":true,"workers_online":1,"shares_accepted":0}\n' >"$ACCEPTANCE_DIR/http-public-canary-miner.json"
printf '{"workers":[{"worker":"abc"}]}\n' >"$ACCEPTANCE_DIR/http-public-canary-miner-workers.json"
printf '{"status":"passed","canary_address":"abc","canary_source":"smoke-success-worker","accepted_share_required":false,"accepted_share_minimum":1,"checks":{"miner_address_matches":true,"miner_online":true,"workers_online_positive":true,"worker_rows_present":true}}\n' >"$ACCEPTANCE_DIR/public-canary-miner.json"
write_sha_manifest "$ACCEPTANCE_DIR" "PUBLIC-ACCEPTANCE-SHA256SUMS"
(
  cd "$ACCEPTANCE_ROOT"
  tar -czf "$OUTPUT_DIR/public-acceptance-redaction-leak.tar.gz" "$(basename "$ACCEPTANCE_DIR")"
)
sha256_file "$OUTPUT_DIR/public-acceptance-redaction-leak.tar.gz" >"$OUTPUT_DIR/public-acceptance-redaction-leak.tar.gz.sha256"

if "$VERIFY_ACCEPTANCE_SCRIPT" \
  "$OUTPUT_DIR/public-acceptance-redaction-leak.tar.gz" \
  "$OUTPUT_DIR/public-acceptance-redaction-leak.tar.gz.sha256" \
  >"$OUTPUT_DIR/public-acceptance-redaction-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/public-acceptance-redaction-verify.log" >&2
  fail "public acceptance verifier accepted a package with a leaked URL password"
fi
if grep -Fq "public acceptance redaction scan failed" "$OUTPUT_DIR/public-acceptance-redaction-verify.log" && \
   grep -Fq "url_basic_auth_password" "$OUTPUT_DIR/public-acceptance-redaction-verify.log"; then
  printf 'ok: public acceptance redaction leak rejected\n'
else
  cat "$OUTPUT_DIR/public-acceptance-redaction-verify.log" >&2
  fail "public acceptance verifier failed for an unexpected reason"
fi

printf 'receipt_redaction_log=%s\n' "$OUTPUT_DIR/receipt-redaction-verify.log"
printf 'summary_target_binding_log=%s\n' "$OUTPUT_DIR/summary-target-binding-verify.log"
printf 'summary_doctor_binding_log=%s\n' "$OUTPUT_DIR/summary-doctor-binding-verify.log"
printf 'receipt_manifest_target_log=%s\n' "$OUTPUT_DIR/receipt-manifest-target-verify.log"
printf 'receipt_doctor_binding_log=%s\n' "$OUTPUT_DIR/receipt-doctor-not-ready-verify.log"
printf 'public_acceptance_redaction_log=%s\n' "$OUTPUT_DIR/public-acceptance-redaction-verify.log"
printf 'summary: evidence redaction self-test passed\n'

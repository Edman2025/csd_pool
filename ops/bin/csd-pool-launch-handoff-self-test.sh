#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
VERIFY_HANDOFF_SCRIPT="${CSD_POOL_VERIFY_LAUNCH_HANDOFF_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-launch-handoff.sh}"
OUTPUT_DIR="${CSD_POOL_LAUNCH_HANDOFF_SELF_TEST_DIR:-}"
KEEP_TMP="${CSD_POOL_LAUNCH_HANDOFF_SELF_TEST_KEEP_DIR:-0}"
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
  (
    cd "$dir"
    find . -type f ! -name SHA256SUMS | sort | while read -r file; do
      sha256_file "$file"
    done >SHA256SUMS
  )
}

write_passing_stub() {
  local path="$1"
  mkdir -p "$(dirname "$path")"
  cat >"$path" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'summary: pass=1 fail=0 skip=0\n'
SH
  chmod 0755 "$path"
}

cleanup() {
  if [[ "$KEEP_TMP" != "1" && "$OWN_TMP_DIR" == "1" && -n "${OUTPUT_DIR:-}" && -d "$OUTPUT_DIR" ]]; then
    rm -rf "$OUTPUT_DIR"
  fi
}
trap cleanup EXIT

[[ -x "$VERIFY_HANDOFF_SCRIPT" ]] || fail "launch handoff verifier not executable: $VERIFY_HANDOFF_SCRIPT"

if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-launch-handoff-self-test.XXXXXX")"
  OWN_TMP_DIR=1
else
  mkdir -p "$OUTPUT_DIR"
  OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
fi

printf 'CSD Pool launch handoff self-test\n'
printf 'output_dir=%s\n' "$OUTPUT_DIR"

release_name="csd-pool-self-test-release-20260623T000000Z"
release_revision="self-test-revision"
release_timestamp="2026-06-23T00:00:00Z"
release_root="$OUTPUT_DIR/release"
release_dir="$release_root/$release_name"
mkdir -p "$release_dir/bin" "$release_dir/ops/bin"
cat >"$release_dir/RELEASE-MANIFEST.txt" <<TXT
name=$release_name
revision=$release_revision
timestamp_utc=$release_timestamp
verify=ops/bin/csd-pool-verify.sh
verify_real_go_live_receipt=ops/bin/csd-pool-verify-real-go-live-receipt.sh
verify_public_acceptance_evidence=ops/bin/csd-pool-verify-public-acceptance-evidence.sh
evidence_redaction_self_test=ops/bin/csd-pool-evidence-redaction-self-test.sh
release_archive_self_test=ops/bin/csd-pool-release-archive-self-test.sh
public_acceptance=ops/bin/csd-pool-public-acceptance.sh
TXT
write_passing_stub "$release_dir/ops/bin/csd-pool-verify.sh"
write_passing_stub "$release_dir/ops/bin/csd-pool-verify-real-go-live-receipt.sh"
write_passing_stub "$release_dir/ops/bin/csd-pool-verify-public-acceptance-evidence.sh"
write_passing_stub "$release_dir/ops/bin/csd-pool-evidence-redaction-self-test.sh"
write_passing_stub "$release_dir/ops/bin/csd-pool-release-archive-self-test.sh"
write_passing_stub "$release_dir/ops/bin/csd-pool-go-live-check.sh"
write_passing_stub "$release_dir/bin/csd-pool-workers"
write_sha_manifest "$release_dir"
(
  cd "$release_root"
  tar -czf "$OUTPUT_DIR/release.tar.gz" "$release_name"
)

receipt_root="$OUTPUT_DIR/receipt"
receipt_dir="$receipt_root/csd-pool-self-test-real-go-live-receipt-20260623T000000Z"
mkdir -p "$receipt_dir"
cat >"$receipt_dir/go-live-summary.json" <<JSON
{
  "target": "public-beta",
  "dry_run": false,
  "summary": {"status": "passed", "fail": 0},
  "release": {
    "name": "$release_name",
    "revision": "$release_revision",
    "timestamp_utc": "$release_timestamp"
  }
}
JSON
(
  cd "$receipt_root"
  tar -czf "$OUTPUT_DIR/receipt.tar.gz" "$(basename "$receipt_dir")"
)
receipt_sha="$(sha256_file "$OUTPUT_DIR/receipt.tar.gz" | awk '{print $1}')"

acceptance_root="$OUTPUT_DIR/acceptance"
acceptance_dir="$acceptance_root/public-acceptance-evidence"
mkdir -p "$acceptance_dir"
cat >"$acceptance_dir/public-acceptance-summary.json" <<JSON
{
  "status": "passed",
  "pass": 1,
  "fail": 0,
  "skip": 0,
  "receipt_archive": "$OUTPUT_DIR/receipt.tar.gz",
  "receipt_archive_sha256": "$receipt_sha"
}
JSON
printf 'summary: pass=1 fail=0\n' >"$acceptance_dir/receipt-verify.log"
cat >"$acceptance_dir/http-public-status.json" <<JSON
{
  "status": "operational",
  "service": "csd-pool",
  "release": {
    "version": "0.1.0",
    "name": "$release_name",
    "revision": "tampered-revision",
    "timestamp_utc": "$release_timestamp"
  }
}
JSON
(
  cd "$acceptance_root"
  tar -czf "$OUTPUT_DIR/acceptance-mismatch.tar.gz" "$(basename "$acceptance_dir")"
)

if "$VERIFY_HANDOFF_SCRIPT" \
  "$OUTPUT_DIR/release.tar.gz" \
  "$OUTPUT_DIR/receipt.tar.gz" \
  "$OUTPUT_DIR/acceptance-mismatch.tar.gz" \
  >"$OUTPUT_DIR/handoff-mismatch.log" 2>&1; then
  cat "$OUTPUT_DIR/handoff-mismatch.log" >&2
  fail "launch handoff verifier accepted mismatched public status release"
fi
if grep -Fq "public acceptance status release mismatch with receipt" "$OUTPUT_DIR/handoff-mismatch.log"; then
  printf 'ok: mismatched public status release rejected\n'
else
  cat "$OUTPUT_DIR/handoff-mismatch.log" >&2
  fail "launch handoff mismatch failed for an unexpected reason"
fi

cat >"$acceptance_dir/http-public-status.json" <<JSON
{
  "status": "operational",
  "service": "csd-pool",
  "release": {
    "version": "0.1.0",
    "name": "$release_name",
    "revision": "$release_revision",
    "timestamp_utc": "$release_timestamp"
  }
}
JSON
(
  cd "$acceptance_root"
  tar -czf "$OUTPUT_DIR/acceptance-matched.tar.gz" "$(basename "$acceptance_dir")"
)

"$VERIFY_HANDOFF_SCRIPT" \
  "$OUTPUT_DIR/release.tar.gz" \
  "$OUTPUT_DIR/receipt.tar.gz" \
  "$OUTPUT_DIR/acceptance-matched.tar.gz" \
  >"$OUTPUT_DIR/handoff-matched.log" 2>&1 \
  || { cat "$OUTPUT_DIR/handoff-matched.log" >&2; fail "launch handoff verifier rejected matched public status release"; }
grep -Fq "public acceptance status release matches receipt" "$OUTPUT_DIR/handoff-matched.log" \
  || { cat "$OUTPUT_DIR/handoff-matched.log" >&2; fail "launch handoff verifier did not report public status release binding"; }
printf 'ok: matched public status release accepted\n'

printf 'handoff_mismatch_log=%s\n' "$OUTPUT_DIR/handoff-mismatch.log"
printf 'handoff_matched_log=%s\n' "$OUTPUT_DIR/handoff-matched.log"
printf 'summary: launch handoff self-test passed\n'

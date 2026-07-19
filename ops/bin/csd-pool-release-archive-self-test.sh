#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
RELEASE_ARCHIVE="${1:-${CSD_POOL_RELEASE_ARCHIVE_SELF_TEST_ARCHIVE:-}}"
VERIFY_SCRIPT="${CSD_POOL_RELEASE_ARCHIVE_SELF_TEST_VERIFY_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify.sh}"
OUTPUT_DIR="${CSD_POOL_RELEASE_ARCHIVE_SELF_TEST_DIR:-}"
KEEP_TMP="${CSD_POOL_RELEASE_ARCHIVE_SELF_TEST_KEEP_DIR:-0}"
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

cleanup() {
  if [[ "$KEEP_TMP" != "1" && "$OWN_TMP_DIR" == "1" && -n "${OUTPUT_DIR:-}" && -d "$OUTPUT_DIR" ]]; then
    rm -rf "$OUTPUT_DIR"
  fi
}
trap cleanup EXIT

if [[ -z "$RELEASE_ARCHIVE" ]]; then
  printf 'usage: %s /path/to/csd-pool-release.tar.gz\n' "$(basename "$0")" >&2
  exit 2
fi

[[ -f "$RELEASE_ARCHIVE" ]] || fail "release archive not found: $RELEASE_ARCHIVE"
[[ -x "$VERIFY_SCRIPT" ]] || fail "release verifier not executable: $VERIFY_SCRIPT"

if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-release-archive-self-test.XXXXXX")"
  OWN_TMP_DIR=1
else
  mkdir -p "$OUTPUT_DIR"
  OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
fi

printf 'CSD Pool release archive self-test\n'
printf 'release_archive=%s\n' "$RELEASE_ARCHIVE"
printf 'output_dir=%s\n' "$OUTPUT_DIR"

# Verify the sidecar in a directory that does not contain the build machine's
# absolute path. This catches non-portable release checksums before upload.
ARCHIVE_BASENAME="$(basename "$RELEASE_ARCHIVE")"
ARCHIVE_SHA256_PATH="${RELEASE_ARCHIVE}.sha256"
if [[ -f "$ARCHIVE_SHA256_PATH" ]]; then
  mkdir -p "$OUTPUT_DIR/portable-checksum"
  cp "$RELEASE_ARCHIVE" "$OUTPUT_DIR/portable-checksum/$ARCHIVE_BASENAME"
  cp "$ARCHIVE_SHA256_PATH" "$OUTPUT_DIR/portable-checksum/$ARCHIVE_BASENAME.sha256"
  (
    cd "$OUTPUT_DIR/portable-checksum"
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum -c "$ARCHIVE_BASENAME.sha256"
    else
      shasum -a 256 -c "$ARCHIVE_BASENAME.sha256"
    fi
  ) >"$OUTPUT_DIR/portable-checksum.log" 2>&1 \
    || { cat "$OUTPUT_DIR/portable-checksum.log" >&2; fail "release archive sidecar is not portable"; }
  printf 'ok: release archive sidecar verifies from a foreign directory\n'
else
  fail "release archive checksum sidecar missing: $ARCHIVE_SHA256_PATH"
fi

ORIGINAL_DIR="$OUTPUT_DIR/original"
mkdir -p "$ORIGINAL_DIR"
tar -xzf "$RELEASE_ARCHIVE" -C "$ORIGINAL_DIR"
ORIGINAL_ROOT="$(find "$ORIGINAL_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
[[ -n "$ORIGINAL_ROOT" && -d "$ORIGINAL_ROOT" ]] || fail "release archive did not extract to one directory"

CSD_POOL_ROOT="$ORIGINAL_ROOT" \
CSD_POOL_BIN_DIR="$ORIGINAL_ROOT/bin" \
CSD_POOL_VERIFY_HTTP=0 \
CSD_POOL_VERIFY_RELEASE=1 \
CSD_POOL_VERIFY_RELEASE_ARCHIVE="$RELEASE_ARCHIVE" \
  "$VERIFY_SCRIPT" >"$OUTPUT_DIR/original-release-verify.log" 2>&1 \
  || { cat "$OUTPUT_DIR/original-release-verify.log" >&2; fail "original release archive did not verify"; }
printf 'ok: original release archive verified\n'

CSD_POOL_ROOT="$ORIGINAL_ROOT" \
  "$ORIGINAL_ROOT/ops/bin/csd-pool-install-release-self-test.sh" "$RELEASE_ARCHIVE" >"$OUTPUT_DIR/original-install-release-self-test.log" 2>&1 \
  || { cat "$OUTPUT_DIR/original-install-release-self-test.log" >&2; fail "original release archive install self-test failed"; }
printf 'ok: original release archive install self-test passed\n'

TAMPER_DIR="$OUTPUT_DIR/tampered"
mkdir -p "$TAMPER_DIR"
tar -xzf "$RELEASE_ARCHIVE" -C "$TAMPER_DIR"
TAMPER_ROOT="$(find "$TAMPER_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
[[ -n "$TAMPER_ROOT" && -d "$TAMPER_ROOT" ]] || fail "tampered release archive did not extract to one directory"

printf '\nleaked_database_url=postgres://pool:secret-password@example.net/csd_pool\n' >>"$TAMPER_ROOT/README.md"
write_sha_manifest "$TAMPER_ROOT"

TAMPER_ARCHIVE="$OUTPUT_DIR/tampered-release-doc-leak.tar.gz"
(
  cd "$TAMPER_DIR"
  tar -czf "$TAMPER_ARCHIVE" "$(basename "$TAMPER_ROOT")"
)
sha256_file "$TAMPER_ARCHIVE" >"$TAMPER_ARCHIVE.sha256"

if CSD_POOL_ROOT="$TAMPER_ROOT" \
  CSD_POOL_BIN_DIR="$TAMPER_ROOT/bin" \
  CSD_POOL_VERIFY_HTTP=0 \
  CSD_POOL_VERIFY_RELEASE=1 \
  CSD_POOL_VERIFY_RELEASE_ARCHIVE="$TAMPER_ARCHIVE" \
    "$VERIFY_SCRIPT" >"$OUTPUT_DIR/tampered-release-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/tampered-release-verify.log" >&2
  fail "release verifier accepted a tampered package with a leaked database URL password"
fi

if grep -Fq "doc redaction scan failed" "$OUTPUT_DIR/tampered-release-verify.log"; then
  printf 'ok: tampered release archive rejected by package doc redaction scan\n'
else
  cat "$OUTPUT_DIR/tampered-release-verify.log" >&2
  fail "tampered release archive failed for an unexpected reason"
fi

MISSING_WORKFLOW_DIR="$OUTPUT_DIR/missing-workflow"
mkdir -p "$MISSING_WORKFLOW_DIR"
tar -xzf "$RELEASE_ARCHIVE" -C "$MISSING_WORKFLOW_DIR"
MISSING_WORKFLOW_ROOT="$(find "$MISSING_WORKFLOW_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
[[ -n "$MISSING_WORKFLOW_ROOT" && -d "$MISSING_WORKFLOW_ROOT" ]] || fail "missing-workflow release archive did not extract to one directory"

rm -f "$MISSING_WORKFLOW_ROOT/.github/workflows/ci.yml"
write_sha_manifest "$MISSING_WORKFLOW_ROOT"

MISSING_WORKFLOW_ARCHIVE="$OUTPUT_DIR/tampered-release-missing-workflow.tar.gz"
(
  cd "$MISSING_WORKFLOW_DIR"
  tar -czf "$MISSING_WORKFLOW_ARCHIVE" "$(basename "$MISSING_WORKFLOW_ROOT")"
)
sha256_file "$MISSING_WORKFLOW_ARCHIVE" >"$MISSING_WORKFLOW_ARCHIVE.sha256"

if CSD_POOL_ROOT="$MISSING_WORKFLOW_ROOT" \
  CSD_POOL_BIN_DIR="$MISSING_WORKFLOW_ROOT/bin" \
  CSD_POOL_VERIFY_HTTP=0 \
  CSD_POOL_VERIFY_RELEASE=1 \
  CSD_POOL_VERIFY_RELEASE_ARCHIVE="$MISSING_WORKFLOW_ARCHIVE" \
    "$VERIFY_SCRIPT" >"$OUTPUT_DIR/missing-workflow-release-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/missing-workflow-release-verify.log" >&2
  fail "release verifier accepted a package missing the CI workflow"
fi

if grep -Fq "missing file:" "$OUTPUT_DIR/missing-workflow-release-verify.log" && \
   grep -Fq ".github/workflows/ci.yml" "$OUTPUT_DIR/missing-workflow-release-verify.log"; then
  printf 'ok: tampered release archive rejected when CI workflow is missing\n'
else
  cat "$OUTPUT_DIR/missing-workflow-release-verify.log" >&2
  fail "missing-workflow release archive failed for an unexpected reason"
fi

GUTTED_WORKFLOW_DIR="$OUTPUT_DIR/gutted-workflow"
mkdir -p "$GUTTED_WORKFLOW_DIR"
tar -xzf "$RELEASE_ARCHIVE" -C "$GUTTED_WORKFLOW_DIR"
GUTTED_WORKFLOW_ROOT="$(find "$GUTTED_WORKFLOW_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
[[ -n "$GUTTED_WORKFLOW_ROOT" && -d "$GUTTED_WORKFLOW_ROOT" ]] || fail "gutted-workflow release archive did not extract to one directory"

python3 - "$GUTTED_WORKFLOW_ROOT/.github/workflows/ci.yml" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
text = text.replace("      - name: Launch gaps self-test\n        run: ops/bin/csd-pool-launch-gaps-self-test.sh\n\n", "")
path.write_text(text, encoding="utf-8")
PY
write_sha_manifest "$GUTTED_WORKFLOW_ROOT"

GUTTED_WORKFLOW_ARCHIVE="$OUTPUT_DIR/tampered-release-gutted-workflow.tar.gz"
(
  cd "$GUTTED_WORKFLOW_DIR"
  tar -czf "$GUTTED_WORKFLOW_ARCHIVE" "$(basename "$GUTTED_WORKFLOW_ROOT")"
)
sha256_file "$GUTTED_WORKFLOW_ARCHIVE" >"$GUTTED_WORKFLOW_ARCHIVE.sha256"

if CSD_POOL_ROOT="$GUTTED_WORKFLOW_ROOT" \
  CSD_POOL_BIN_DIR="$GUTTED_WORKFLOW_ROOT/bin" \
  CSD_POOL_VERIFY_HTTP=0 \
  CSD_POOL_VERIFY_RELEASE=1 \
  CSD_POOL_VERIFY_RELEASE_ARCHIVE="$GUTTED_WORKFLOW_ARCHIVE" \
    "$VERIFY_SCRIPT" >"$OUTPUT_DIR/gutted-workflow-release-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/gutted-workflow-release-verify.log" >&2
  fail "release verifier accepted a package with the CI launch gaps step removed"
fi

if grep -Fq "release archive CI workflow records launch gaps self-test" "$OUTPUT_DIR/gutted-workflow-release-verify.log" || \
   grep -Fq "release archive CI workflow runs launch gaps self-test" "$OUTPUT_DIR/gutted-workflow-release-verify.log"; then
  printf 'ok: tampered release archive rejected when CI launch gaps step is removed\n'
else
  cat "$OUTPUT_DIR/gutted-workflow-release-verify.log" >&2
  fail "gutted-workflow release archive failed for an unexpected reason"
fi

MISSING_VERIFIER_DIR="$OUTPUT_DIR/missing-public-acceptance-verifier"
mkdir -p "$MISSING_VERIFIER_DIR"
tar -xzf "$RELEASE_ARCHIVE" -C "$MISSING_VERIFIER_DIR"
MISSING_VERIFIER_ROOT="$(find "$MISSING_VERIFIER_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
[[ -n "$MISSING_VERIFIER_ROOT" && -d "$MISSING_VERIFIER_ROOT" ]] || fail "missing-verifier release archive did not extract to one directory"

rm -f "$MISSING_VERIFIER_ROOT/ops/bin/csd-pool-verify-public-acceptance-evidence.sh"
write_sha_manifest "$MISSING_VERIFIER_ROOT"

MISSING_VERIFIER_ARCHIVE="$OUTPUT_DIR/tampered-release-missing-public-acceptance-verifier.tar.gz"
(
  cd "$MISSING_VERIFIER_DIR"
  tar -czf "$MISSING_VERIFIER_ARCHIVE" "$(basename "$MISSING_VERIFIER_ROOT")"
)
sha256_file "$MISSING_VERIFIER_ARCHIVE" >"$MISSING_VERIFIER_ARCHIVE.sha256"

if CSD_POOL_ROOT="$MISSING_VERIFIER_ROOT" \
  CSD_POOL_BIN_DIR="$MISSING_VERIFIER_ROOT/bin" \
  CSD_POOL_VERIFY_HTTP=0 \
  CSD_POOL_VERIFY_RELEASE=1 \
  CSD_POOL_VERIFY_RELEASE_ARCHIVE="$MISSING_VERIFIER_ARCHIVE" \
    "$VERIFY_SCRIPT" >"$OUTPUT_DIR/missing-verifier-release-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/missing-verifier-release-verify.log" >&2
  fail "release verifier accepted a package missing the public acceptance evidence verifier"
fi

if grep -Fq "missing executable:" "$OUTPUT_DIR/missing-verifier-release-verify.log" && \
   grep -Fq "ops/bin/csd-pool-verify-public-acceptance-evidence.sh" "$OUTPUT_DIR/missing-verifier-release-verify.log"; then
  printf 'ok: tampered release archive rejected when public acceptance evidence verifier is missing\n'
else
  cat "$OUTPUT_DIR/missing-verifier-release-verify.log" >&2
  fail "missing-verifier release archive failed for an unexpected reason"
fi

MISSING_MANIFEST_ENTRY_DIR="$OUTPUT_DIR/missing-public-acceptance-verifier-manifest-entry"
mkdir -p "$MISSING_MANIFEST_ENTRY_DIR"
tar -xzf "$RELEASE_ARCHIVE" -C "$MISSING_MANIFEST_ENTRY_DIR"
MISSING_MANIFEST_ENTRY_ROOT="$(find "$MISSING_MANIFEST_ENTRY_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
[[ -n "$MISSING_MANIFEST_ENTRY_ROOT" && -d "$MISSING_MANIFEST_ENTRY_ROOT" ]] || fail "missing-manifest-entry release archive did not extract to one directory"

python3 - "$MISSING_MANIFEST_ENTRY_ROOT/RELEASE-MANIFEST.txt" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
lines = [
    line
    for line in path.read_text(encoding="utf-8").splitlines()
    if line != "verify_public_acceptance_evidence=ops/bin/csd-pool-verify-public-acceptance-evidence.sh"
]
path.write_text("\n".join(lines) + "\n", encoding="utf-8")
PY
write_sha_manifest "$MISSING_MANIFEST_ENTRY_ROOT"

MISSING_MANIFEST_ENTRY_ARCHIVE="$OUTPUT_DIR/tampered-release-missing-public-acceptance-verifier-manifest-entry.tar.gz"
(
  cd "$MISSING_MANIFEST_ENTRY_DIR"
  tar -czf "$MISSING_MANIFEST_ENTRY_ARCHIVE" "$(basename "$MISSING_MANIFEST_ENTRY_ROOT")"
)
sha256_file "$MISSING_MANIFEST_ENTRY_ARCHIVE" >"$MISSING_MANIFEST_ENTRY_ARCHIVE.sha256"

if CSD_POOL_ROOT="$MISSING_MANIFEST_ENTRY_ROOT" \
  CSD_POOL_BIN_DIR="$MISSING_MANIFEST_ENTRY_ROOT/bin" \
  CSD_POOL_VERIFY_HTTP=0 \
  CSD_POOL_VERIFY_RELEASE=1 \
  CSD_POOL_VERIFY_RELEASE_ARCHIVE="$MISSING_MANIFEST_ENTRY_ARCHIVE" \
    "$VERIFY_SCRIPT" >"$OUTPUT_DIR/missing-manifest-entry-release-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/missing-manifest-entry-release-verify.log" >&2
  fail "release verifier accepted a package missing the public acceptance evidence verifier manifest entry"
fi

if grep -Fq "release manifest records verify_public_acceptance_evidence" "$OUTPUT_DIR/missing-manifest-entry-release-verify.log"; then
  printf 'ok: tampered release archive rejected when public acceptance evidence verifier manifest entry is missing\n'
else
  cat "$OUTPUT_DIR/missing-manifest-entry-release-verify.log" >&2
  fail "missing-manifest-entry release archive failed for an unexpected reason"
fi

GUTTED_RELEASE_CHECK_DIR="$OUTPUT_DIR/gutted-release-check"
mkdir -p "$GUTTED_RELEASE_CHECK_DIR"
tar -xzf "$RELEASE_ARCHIVE" -C "$GUTTED_RELEASE_CHECK_DIR"
GUTTED_RELEASE_CHECK_ROOT="$(find "$GUTTED_RELEASE_CHECK_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
[[ -n "$GUTTED_RELEASE_CHECK_ROOT" && -d "$GUTTED_RELEASE_CHECK_ROOT" ]] || fail "gutted-release-check release archive did not extract to one directory"

python3 - "$GUTTED_RELEASE_CHECK_ROOT/ops/bin/csd-pool-release-check.sh" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
text = text.replace("check_release_manifest_coverage", "disabled_release_manifest_coverage")
path.write_text(text, encoding="utf-8")
PY
write_sha_manifest "$GUTTED_RELEASE_CHECK_ROOT"

GUTTED_RELEASE_CHECK_ARCHIVE="$OUTPUT_DIR/tampered-release-gutted-release-check.tar.gz"
(
  cd "$GUTTED_RELEASE_CHECK_DIR"
  tar -czf "$GUTTED_RELEASE_CHECK_ARCHIVE" "$(basename "$GUTTED_RELEASE_CHECK_ROOT")"
)
sha256_file "$GUTTED_RELEASE_CHECK_ARCHIVE" >"$GUTTED_RELEASE_CHECK_ARCHIVE.sha256"

if CSD_POOL_ROOT="$GUTTED_RELEASE_CHECK_ROOT" \
  CSD_POOL_BIN_DIR="$GUTTED_RELEASE_CHECK_ROOT/bin" \
  CSD_POOL_VERIFY_HTTP=0 \
  CSD_POOL_VERIFY_RELEASE=1 \
  CSD_POOL_VERIFY_RELEASE_ARCHIVE="$GUTTED_RELEASE_CHECK_ARCHIVE" \
    "$VERIFY_SCRIPT" >"$OUTPUT_DIR/gutted-release-check-release-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/gutted-release-check-release-verify.log" >&2
  fail "release verifier accepted a package with the release-check manifest coverage gate disabled"
fi

if grep -Fq "release check carries manifest coverage gate" "$OUTPUT_DIR/gutted-release-check-release-verify.log"; then
  printf 'ok: tampered release archive rejected when release-check manifest coverage gate is disabled\n'
else
  cat "$OUTPUT_DIR/gutted-release-check-release-verify.log" >&2
  fail "gutted-release-check archive failed for an unexpected reason"
fi

GUTTED_SHA_FALLBACK_DIR="$OUTPUT_DIR/gutted-sha-fallback"
mkdir -p "$GUTTED_SHA_FALLBACK_DIR"
tar -xzf "$RELEASE_ARCHIVE" -C "$GUTTED_SHA_FALLBACK_DIR"
GUTTED_SHA_FALLBACK_ROOT="$(find "$GUTTED_SHA_FALLBACK_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
[[ -n "$GUTTED_SHA_FALLBACK_ROOT" && -d "$GUTTED_SHA_FALLBACK_ROOT" ]] || fail "gutted-sha-fallback release archive did not extract to one directory"

python3 - "$GUTTED_SHA_FALLBACK_ROOT/ops/bin/csd-pool-verify.sh" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
text = text.replace("shasum -a 256 -c", "shasum -a 999 -c")
path.write_text(text, encoding="utf-8")
PY
write_sha_manifest "$GUTTED_SHA_FALLBACK_ROOT"

GUTTED_SHA_FALLBACK_ARCHIVE="$OUTPUT_DIR/tampered-release-gutted-sha-fallback.tar.gz"
(
  cd "$GUTTED_SHA_FALLBACK_DIR"
  tar -czf "$GUTTED_SHA_FALLBACK_ARCHIVE" "$(basename "$GUTTED_SHA_FALLBACK_ROOT")"
)
sha256_file "$GUTTED_SHA_FALLBACK_ARCHIVE" >"$GUTTED_SHA_FALLBACK_ARCHIVE.sha256"

if CSD_POOL_ROOT="$GUTTED_SHA_FALLBACK_ROOT" \
  CSD_POOL_BIN_DIR="$GUTTED_SHA_FALLBACK_ROOT/bin" \
  CSD_POOL_VERIFY_HTTP=0 \
  CSD_POOL_VERIFY_RELEASE=1 \
  CSD_POOL_VERIFY_RELEASE_ARCHIVE="$GUTTED_SHA_FALLBACK_ARCHIVE" \
    "$VERIFY_SCRIPT" >"$OUTPUT_DIR/gutted-sha-fallback-release-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/gutted-sha-fallback-release-verify.log" >&2
  fail "release verifier accepted a package with the shasum fallback disabled"
fi

if grep -Fq "release verifier carries shasum fallback" "$OUTPUT_DIR/gutted-sha-fallback-release-verify.log"; then
  printf 'ok: tampered release archive rejected when shasum fallback is disabled\n'
else
  cat "$OUTPUT_DIR/gutted-sha-fallback-release-verify.log" >&2
  fail "gutted-sha-fallback archive failed for an unexpected reason"
fi

MISSING_INSTALL_SELF_TEST_DIR="$OUTPUT_DIR/missing-install-release-self-test"
mkdir -p "$MISSING_INSTALL_SELF_TEST_DIR"
tar -xzf "$RELEASE_ARCHIVE" -C "$MISSING_INSTALL_SELF_TEST_DIR"
MISSING_INSTALL_SELF_TEST_ROOT="$(find "$MISSING_INSTALL_SELF_TEST_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
[[ -n "$MISSING_INSTALL_SELF_TEST_ROOT" && -d "$MISSING_INSTALL_SELF_TEST_ROOT" ]] || fail "missing-install-self-test release archive did not extract to one directory"

rm -f "$MISSING_INSTALL_SELF_TEST_ROOT/ops/bin/csd-pool-install-release-self-test.sh"
write_sha_manifest "$MISSING_INSTALL_SELF_TEST_ROOT"

MISSING_INSTALL_SELF_TEST_ARCHIVE="$OUTPUT_DIR/tampered-release-missing-install-self-test.tar.gz"
(
  cd "$MISSING_INSTALL_SELF_TEST_DIR"
  tar -czf "$MISSING_INSTALL_SELF_TEST_ARCHIVE" "$(basename "$MISSING_INSTALL_SELF_TEST_ROOT")"
)
sha256_file "$MISSING_INSTALL_SELF_TEST_ARCHIVE" >"$MISSING_INSTALL_SELF_TEST_ARCHIVE.sha256"

if CSD_POOL_ROOT="$MISSING_INSTALL_SELF_TEST_ROOT" \
  CSD_POOL_BIN_DIR="$MISSING_INSTALL_SELF_TEST_ROOT/bin" \
  CSD_POOL_VERIFY_HTTP=0 \
  CSD_POOL_VERIFY_RELEASE=1 \
  CSD_POOL_VERIFY_RELEASE_ARCHIVE="$MISSING_INSTALL_SELF_TEST_ARCHIVE" \
    "$VERIFY_SCRIPT" >"$OUTPUT_DIR/missing-install-self-test-release-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/missing-install-self-test-release-verify.log" >&2
  fail "release verifier accepted a package missing the install release self-test"
fi

if grep -Fq "missing executable:" "$OUTPUT_DIR/missing-install-self-test-release-verify.log" && \
   grep -Fq "ops/bin/csd-pool-install-release-self-test.sh" "$OUTPUT_DIR/missing-install-self-test-release-verify.log"; then
  printf 'ok: tampered release archive rejected when install release self-test is missing\n'
else
  cat "$OUTPUT_DIR/missing-install-self-test-release-verify.log" >&2
  fail "missing-install-self-test archive failed for an unexpected reason"
fi

MISSING_INSTALL_SELF_TEST_MANIFEST_DIR="$OUTPUT_DIR/missing-install-release-self-test-manifest-entry"
mkdir -p "$MISSING_INSTALL_SELF_TEST_MANIFEST_DIR"
tar -xzf "$RELEASE_ARCHIVE" -C "$MISSING_INSTALL_SELF_TEST_MANIFEST_DIR"
MISSING_INSTALL_SELF_TEST_MANIFEST_ROOT="$(find "$MISSING_INSTALL_SELF_TEST_MANIFEST_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
[[ -n "$MISSING_INSTALL_SELF_TEST_MANIFEST_ROOT" && -d "$MISSING_INSTALL_SELF_TEST_MANIFEST_ROOT" ]] || fail "missing-install-self-test-manifest release archive did not extract to one directory"

python3 - "$MISSING_INSTALL_SELF_TEST_MANIFEST_ROOT/RELEASE-MANIFEST.txt" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
lines = [
    line
    for line in path.read_text(encoding="utf-8").splitlines()
    if line != "install_release_self_test=ops/bin/csd-pool-install-release-self-test.sh"
]
path.write_text("\n".join(lines) + "\n", encoding="utf-8")
PY
write_sha_manifest "$MISSING_INSTALL_SELF_TEST_MANIFEST_ROOT"

MISSING_INSTALL_SELF_TEST_MANIFEST_ARCHIVE="$OUTPUT_DIR/tampered-release-missing-install-self-test-manifest-entry.tar.gz"
(
  cd "$MISSING_INSTALL_SELF_TEST_MANIFEST_DIR"
  tar -czf "$MISSING_INSTALL_SELF_TEST_MANIFEST_ARCHIVE" "$(basename "$MISSING_INSTALL_SELF_TEST_MANIFEST_ROOT")"
)
sha256_file "$MISSING_INSTALL_SELF_TEST_MANIFEST_ARCHIVE" >"$MISSING_INSTALL_SELF_TEST_MANIFEST_ARCHIVE.sha256"

if CSD_POOL_ROOT="$MISSING_INSTALL_SELF_TEST_MANIFEST_ROOT" \
  CSD_POOL_BIN_DIR="$MISSING_INSTALL_SELF_TEST_MANIFEST_ROOT/bin" \
  CSD_POOL_VERIFY_HTTP=0 \
  CSD_POOL_VERIFY_RELEASE=1 \
  CSD_POOL_VERIFY_RELEASE_ARCHIVE="$MISSING_INSTALL_SELF_TEST_MANIFEST_ARCHIVE" \
    "$VERIFY_SCRIPT" >"$OUTPUT_DIR/missing-install-self-test-manifest-release-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/missing-install-self-test-manifest-release-verify.log" >&2
  fail "release verifier accepted a package missing the install release self-test manifest entry"
fi

if grep -Fq "release manifest records install_release_self_test" "$OUTPUT_DIR/missing-install-self-test-manifest-release-verify.log"; then
  printf 'ok: tampered release archive rejected when install release self-test manifest entry is missing\n'
else
  cat "$OUTPUT_DIR/missing-install-self-test-manifest-release-verify.log" >&2
  fail "missing-install-self-test-manifest archive failed for an unexpected reason"
fi

MISSING_DOSSIER_SELF_TEST_DIR="$OUTPUT_DIR/missing-launch-dossier-self-test"
mkdir -p "$MISSING_DOSSIER_SELF_TEST_DIR"
tar -xzf "$RELEASE_ARCHIVE" -C "$MISSING_DOSSIER_SELF_TEST_DIR"
MISSING_DOSSIER_SELF_TEST_ROOT="$(find "$MISSING_DOSSIER_SELF_TEST_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
[[ -n "$MISSING_DOSSIER_SELF_TEST_ROOT" && -d "$MISSING_DOSSIER_SELF_TEST_ROOT" ]] || fail "missing-launch-dossier-self-test release archive did not extract to one directory"

rm -f "$MISSING_DOSSIER_SELF_TEST_ROOT/ops/bin/csd-pool-launch-dossier-self-test.sh"
write_sha_manifest "$MISSING_DOSSIER_SELF_TEST_ROOT"

MISSING_DOSSIER_SELF_TEST_ARCHIVE="$OUTPUT_DIR/tampered-release-missing-launch-dossier-self-test.tar.gz"
(
  cd "$MISSING_DOSSIER_SELF_TEST_DIR"
  tar -czf "$MISSING_DOSSIER_SELF_TEST_ARCHIVE" "$(basename "$MISSING_DOSSIER_SELF_TEST_ROOT")"
)
sha256_file "$MISSING_DOSSIER_SELF_TEST_ARCHIVE" >"$MISSING_DOSSIER_SELF_TEST_ARCHIVE.sha256"

if CSD_POOL_ROOT="$MISSING_DOSSIER_SELF_TEST_ROOT" \
  CSD_POOL_BIN_DIR="$MISSING_DOSSIER_SELF_TEST_ROOT/bin" \
  CSD_POOL_VERIFY_HTTP=0 \
  CSD_POOL_VERIFY_RELEASE=1 \
  CSD_POOL_VERIFY_RELEASE_ARCHIVE="$MISSING_DOSSIER_SELF_TEST_ARCHIVE" \
    "$VERIFY_SCRIPT" >"$OUTPUT_DIR/missing-launch-dossier-self-test-release-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/missing-launch-dossier-self-test-release-verify.log" >&2
  fail "release verifier accepted a package missing the launch dossier self-test"
fi

if grep -Fq "missing executable:" "$OUTPUT_DIR/missing-launch-dossier-self-test-release-verify.log" && \
   grep -Fq "ops/bin/csd-pool-launch-dossier-self-test.sh" "$OUTPUT_DIR/missing-launch-dossier-self-test-release-verify.log"; then
  printf 'ok: tampered release archive rejected when launch dossier self-test is missing\n'
else
  cat "$OUTPUT_DIR/missing-launch-dossier-self-test-release-verify.log" >&2
  fail "missing-launch-dossier-self-test archive failed for an unexpected reason"
fi

MISSING_DOSSIER_SELF_TEST_MANIFEST_DIR="$OUTPUT_DIR/missing-launch-dossier-self-test-manifest-entry"
mkdir -p "$MISSING_DOSSIER_SELF_TEST_MANIFEST_DIR"
tar -xzf "$RELEASE_ARCHIVE" -C "$MISSING_DOSSIER_SELF_TEST_MANIFEST_DIR"
MISSING_DOSSIER_SELF_TEST_MANIFEST_ROOT="$(find "$MISSING_DOSSIER_SELF_TEST_MANIFEST_DIR" -mindepth 1 -maxdepth 1 -type d | head -n1)"
[[ -n "$MISSING_DOSSIER_SELF_TEST_MANIFEST_ROOT" && -d "$MISSING_DOSSIER_SELF_TEST_MANIFEST_ROOT" ]] || fail "missing-launch-dossier-self-test-manifest release archive did not extract to one directory"

python3 - "$MISSING_DOSSIER_SELF_TEST_MANIFEST_ROOT/RELEASE-MANIFEST.txt" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
lines = [
    line
    for line in path.read_text(encoding="utf-8").splitlines()
    if line != "launch_dossier_self_test=ops/bin/csd-pool-launch-dossier-self-test.sh"
]
path.write_text("\n".join(lines) + "\n", encoding="utf-8")
PY
write_sha_manifest "$MISSING_DOSSIER_SELF_TEST_MANIFEST_ROOT"

MISSING_DOSSIER_SELF_TEST_MANIFEST_ARCHIVE="$OUTPUT_DIR/tampered-release-missing-launch-dossier-self-test-manifest-entry.tar.gz"
(
  cd "$MISSING_DOSSIER_SELF_TEST_MANIFEST_DIR"
  tar -czf "$MISSING_DOSSIER_SELF_TEST_MANIFEST_ARCHIVE" "$(basename "$MISSING_DOSSIER_SELF_TEST_MANIFEST_ROOT")"
)
sha256_file "$MISSING_DOSSIER_SELF_TEST_MANIFEST_ARCHIVE" >"$MISSING_DOSSIER_SELF_TEST_MANIFEST_ARCHIVE.sha256"

if CSD_POOL_ROOT="$MISSING_DOSSIER_SELF_TEST_MANIFEST_ROOT" \
  CSD_POOL_BIN_DIR="$MISSING_DOSSIER_SELF_TEST_MANIFEST_ROOT/bin" \
  CSD_POOL_VERIFY_HTTP=0 \
  CSD_POOL_VERIFY_RELEASE=1 \
  CSD_POOL_VERIFY_RELEASE_ARCHIVE="$MISSING_DOSSIER_SELF_TEST_MANIFEST_ARCHIVE" \
    "$VERIFY_SCRIPT" >"$OUTPUT_DIR/missing-launch-dossier-self-test-manifest-release-verify.log" 2>&1; then
  cat "$OUTPUT_DIR/missing-launch-dossier-self-test-manifest-release-verify.log" >&2
  fail "release verifier accepted a package missing the launch dossier self-test manifest entry"
fi

if grep -Fq "release manifest records launch_dossier_self_test" "$OUTPUT_DIR/missing-launch-dossier-self-test-manifest-release-verify.log"; then
  printf 'ok: tampered release archive rejected when launch dossier self-test manifest entry is missing\n'
else
  cat "$OUTPUT_DIR/missing-launch-dossier-self-test-manifest-release-verify.log" >&2
  fail "missing-launch-dossier-self-test-manifest archive failed for an unexpected reason"
fi

printf 'tampered_release_archive=%s\n' "$TAMPER_ARCHIVE"
printf 'tampered_release_archive_sha256=%s\n' "$TAMPER_ARCHIVE.sha256"
printf 'tampered_release_verify_log=%s\n' "$OUTPUT_DIR/tampered-release-verify.log"
printf 'missing_workflow_archive=%s\n' "$MISSING_WORKFLOW_ARCHIVE"
printf 'missing_workflow_archive_sha256=%s\n' "$MISSING_WORKFLOW_ARCHIVE.sha256"
printf 'missing_workflow_verify_log=%s\n' "$OUTPUT_DIR/missing-workflow-release-verify.log"
printf 'gutted_workflow_archive=%s\n' "$GUTTED_WORKFLOW_ARCHIVE"
printf 'gutted_workflow_archive_sha256=%s\n' "$GUTTED_WORKFLOW_ARCHIVE.sha256"
printf 'gutted_workflow_verify_log=%s\n' "$OUTPUT_DIR/gutted-workflow-release-verify.log"
printf 'missing_verifier_archive=%s\n' "$MISSING_VERIFIER_ARCHIVE"
printf 'missing_verifier_archive_sha256=%s\n' "$MISSING_VERIFIER_ARCHIVE.sha256"
printf 'missing_verifier_verify_log=%s\n' "$OUTPUT_DIR/missing-verifier-release-verify.log"
printf 'missing_manifest_entry_archive=%s\n' "$MISSING_MANIFEST_ENTRY_ARCHIVE"
printf 'missing_manifest_entry_archive_sha256=%s\n' "$MISSING_MANIFEST_ENTRY_ARCHIVE.sha256"
printf 'missing_manifest_entry_verify_log=%s\n' "$OUTPUT_DIR/missing-manifest-entry-release-verify.log"
printf 'gutted_release_check_archive=%s\n' "$GUTTED_RELEASE_CHECK_ARCHIVE"
printf 'gutted_release_check_archive_sha256=%s\n' "$GUTTED_RELEASE_CHECK_ARCHIVE.sha256"
printf 'gutted_release_check_verify_log=%s\n' "$OUTPUT_DIR/gutted-release-check-release-verify.log"
printf 'gutted_sha_fallback_archive=%s\n' "$GUTTED_SHA_FALLBACK_ARCHIVE"
printf 'gutted_sha_fallback_archive_sha256=%s\n' "$GUTTED_SHA_FALLBACK_ARCHIVE.sha256"
printf 'gutted_sha_fallback_verify_log=%s\n' "$OUTPUT_DIR/gutted-sha-fallback-release-verify.log"
printf 'missing_install_self_test_archive=%s\n' "$MISSING_INSTALL_SELF_TEST_ARCHIVE"
printf 'missing_install_self_test_archive_sha256=%s\n' "$MISSING_INSTALL_SELF_TEST_ARCHIVE.sha256"
printf 'missing_install_self_test_verify_log=%s\n' "$OUTPUT_DIR/missing-install-self-test-release-verify.log"
printf 'missing_install_self_test_manifest_archive=%s\n' "$MISSING_INSTALL_SELF_TEST_MANIFEST_ARCHIVE"
printf 'missing_install_self_test_manifest_archive_sha256=%s\n' "$MISSING_INSTALL_SELF_TEST_MANIFEST_ARCHIVE.sha256"
printf 'missing_install_self_test_manifest_verify_log=%s\n' "$OUTPUT_DIR/missing-install-self-test-manifest-release-verify.log"
printf 'missing_launch_dossier_self_test_archive=%s\n' "$MISSING_DOSSIER_SELF_TEST_ARCHIVE"
printf 'missing_launch_dossier_self_test_archive_sha256=%s\n' "$MISSING_DOSSIER_SELF_TEST_ARCHIVE.sha256"
printf 'missing_launch_dossier_self_test_verify_log=%s\n' "$OUTPUT_DIR/missing-launch-dossier-self-test-release-verify.log"
printf 'missing_launch_dossier_self_test_manifest_archive=%s\n' "$MISSING_DOSSIER_SELF_TEST_MANIFEST_ARCHIVE"
printf 'missing_launch_dossier_self_test_manifest_archive_sha256=%s\n' "$MISSING_DOSSIER_SELF_TEST_MANIFEST_ARCHIVE.sha256"
printf 'missing_launch_dossier_self_test_manifest_verify_log=%s\n' "$OUTPUT_DIR/missing-launch-dossier-self-test-manifest-release-verify.log"
printf 'install_release_self_test_log=%s\n' "$OUTPUT_DIR/original-install-release-self-test.log"
printf 'summary: release archive self-test passed\n'

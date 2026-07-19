#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
RELEASE_ARCHIVE="${1:-${CSD_POOL_INSTALL_RELEASE_SELF_TEST_ARCHIVE:-}}"
INSTALL_SCRIPT="${CSD_POOL_INSTALL_RELEASE_SELF_TEST_INSTALL_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-install-release.sh}"
ROLLBACK_SCRIPT="${CSD_POOL_INSTALL_RELEASE_SELF_TEST_ROLLBACK_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-rollback-release.sh}"
OUTPUT_DIR="${CSD_POOL_INSTALL_RELEASE_SELF_TEST_DIR:-}"
KEEP_TMP="${CSD_POOL_INSTALL_RELEASE_SELF_TEST_KEEP_DIR:-0}"
OWN_TMP_DIR=0

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
}

require_file() {
  local path="$1"
  local label="$2"
  [[ -f "$path" ]] || fail "$label missing: $path"
  printf 'ok: %s\n' "$label"
}

require_executable() {
  local path="$1"
  local label="$2"
  [[ -x "$path" ]] || fail "$label missing or not executable: $path"
  printf 'ok: %s\n' "$label"
}

require_text() {
  local path="$1"
  local text="$2"
  local label="$3"
  grep -Fq "$text" "$path" || fail "$label missing: $text"
  printf 'ok: %s\n' "$label"
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
[[ -x "$INSTALL_SCRIPT" ]] || fail "install script not executable: $INSTALL_SCRIPT"
[[ -x "$ROLLBACK_SCRIPT" ]] || fail "rollback script not executable: $ROLLBACK_SCRIPT"

if [[ -z "$OUTPUT_DIR" ]]; then
  OUTPUT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-install-release-self-test.XXXXXX")"
  OWN_TMP_DIR=1
else
  mkdir -p "$OUTPUT_DIR"
  OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"
fi

INSTALL_ROOT="$OUTPUT_DIR/install-root"

printf 'CSD Pool install release self-test\n'
printf 'release_archive=%s\n' "$RELEASE_ARCHIVE"
printf 'output_dir=%s\n' "$OUTPUT_DIR"
printf 'install_root=%s\n' "$INSTALL_ROOT"

# Production installers commonly run with a restrictive root umask. Release
# paths must still remain traversable by their dedicated service accounts.
umask 0077
CSD_POOL_INSTALL_ROOT="$INSTALL_ROOT" \
  "$INSTALL_SCRIPT" "$RELEASE_ARCHIVE" >"$OUTPUT_DIR/install-first.log" 2>&1 \
  || { cat "$OUTPUT_DIR/install-first.log" >&2; fail "first install failed"; }
printf 'ok: first install completed\n'

CURRENT_RELEASE_FILE="$INSTALL_ROOT/opt/csd-pool/CURRENT_RELEASE"
RELEASE_ENV_FILE="$INSTALL_ROOT/opt/csd-pool/release.env"
require_file "$CURRENT_RELEASE_FILE" "current release marker"
RELEASE_NAME="$(tr -d '[:space:]' <"$CURRENT_RELEASE_FILE")"
[[ -n "$RELEASE_NAME" ]] || fail "current release marker is empty"
printf 'release_name=%s\n' "$RELEASE_NAME"

RELEASE_DIR="$INSTALL_ROOT/opt/csd-pool/releases/$RELEASE_NAME"
find "$RELEASE_DIR" -maxdepth 0 -type d -perm 0755 | grep -q . \
  || fail "installed release root is not mode 0755: $RELEASE_DIR"
printf 'ok: installed release root is service-traversable under restrictive umask\n'
require_file "$RELEASE_DIR/RELEASE-MANIFEST.txt" "installed release manifest"
require_file "$RELEASE_DIR/SHA256SUMS" "installed release checksums"
require_executable "$RELEASE_DIR/ops/wallet-signer/signer.mjs" "installed wallet signer"
require_executable "$RELEASE_DIR/ops/bin/csd-pool-node-adapter-run.sh" "installed node adapter launcher"
require_file "$RELEASE_DIR/ops/wallet-signer/node_modules/@inversealtruism/csd-tx/package.json" "installed official CSD transaction SDK"
CURRENT_LINK="$INSTALL_ROOT/opt/csd-pool/current"
[[ -L "$CURRENT_LINK" ]] || fail "current release symlink missing: $CURRENT_LINK"
[[ "$(cd "$(dirname "$CURRENT_LINK")" && readlink "$CURRENT_LINK")" == "$RELEASE_DIR" ]] \
  || fail "current release symlink does not target $RELEASE_DIR"
printf 'ok: current release symlink targets installed release\n'
require_executable "$INSTALL_ROOT/opt/csd-pool/bin/csd-pool-workers" "installed workers binary"
require_executable "$INSTALL_ROOT/opt/csd-pool/bin/csd-pool-api" "installed API binary"
require_file "$INSTALL_ROOT/etc/csd-pool/config.toml" "installed config"
require_file "$INSTALL_ROOT/etc/csd-pool/csd-pool.env" "installed env file"
require_file "$INSTALL_ROOT/etc/csd-pool/node.env" "installed node env file"
require_file "$INSTALL_ROOT/etc/systemd/system/csd-pool-daemon.service" "installed daemon unit"
require_file "$INSTALL_ROOT/etc/systemd/system/csd-pool-signer.service" "installed wallet signer unit"
require_text "$INSTALL_ROOT/etc/systemd/system/csd-pool-signer.service" "/opt/csd-pool/current/ops/wallet-signer/signer.mjs" "wallet signer unit entrypoint"
require_file "$INSTALL_ROOT/etc/haproxy/haproxy.cfg" "installed HAProxy config"
require_file "$RELEASE_ENV_FILE" "release metadata env"
require_text "$RELEASE_ENV_FILE" "CSD_POOL_RELEASE_NAME=$RELEASE_NAME" "release metadata name"
require_text "$RELEASE_ENV_FILE" "CSD_POOL_RELEASE_REVISION=" "release metadata revision"
require_text "$RELEASE_ENV_FILE" "CSD_POOL_RELEASE_TIMESTAMP_UTC=" "release metadata timestamp"

CSD_POOL_INSTALL_ROOT="$INSTALL_ROOT" \
  "$INSTALL_SCRIPT" "$RELEASE_ARCHIVE" >"$OUTPUT_DIR/install-second.log" 2>&1 \
  || { cat "$OUTPUT_DIR/install-second.log" >&2; fail "second install failed"; }
printf 'ok: second install completed\n'

PREVIOUS_RELEASE_FILE="$INSTALL_ROOT/opt/csd-pool/PREVIOUS_RELEASE"
require_file "$PREVIOUS_RELEASE_FILE" "previous release marker"
PREVIOUS_RELEASE="$(tr -d '[:space:]' <"$PREVIOUS_RELEASE_FILE")"
[[ "$PREVIOUS_RELEASE" == "$RELEASE_NAME" ]] || fail "previous release marker mismatch: expected $RELEASE_NAME got $PREVIOUS_RELEASE"
printf 'ok: previous release marker matches current release after reinstall\n'

CSD_POOL_INSTALL_ROOT="$INSTALL_ROOT" \
  "$ROLLBACK_SCRIPT" "$RELEASE_NAME" >"$OUTPUT_DIR/rollback.log" 2>&1 \
  || { cat "$OUTPUT_DIR/rollback.log" >&2; fail "rollback failed"; }
printf 'ok: rollback completed\n'
require_text "$OUTPUT_DIR/rollback.log" "summary: rollback staged" "rollback summary"
[[ -L "$CURRENT_LINK" ]] || fail "current release symlink missing after rollback"
printf 'ok: rollback preserves current release symlink\n'

printf 'install_log=%s\n' "$OUTPUT_DIR/install-first.log"
printf 'reinstall_log=%s\n' "$OUTPUT_DIR/install-second.log"
printf 'rollback_log=%s\n' "$OUTPUT_DIR/rollback.log"
printf 'summary: install release self-test passed\n'

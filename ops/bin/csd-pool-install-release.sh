#!/usr/bin/env bash
set -euo pipefail

ARTIFACT="${1:-}"
INSTALL_ROOT="${CSD_POOL_INSTALL_ROOT:-}"
DRY_RUN="${CSD_POOL_INSTALL_DRY_RUN:-0}"
OPT_DIR="${CSD_POOL_INSTALL_OPT_DIR:-/opt/csd-pool}"
ETC_DIR="${CSD_POOL_INSTALL_ETC_DIR:-/etc/csd-pool}"
SYSTEMD_DIR="${CSD_POOL_INSTALL_SYSTEMD_DIR:-/etc/systemd/system}"
HAPROXY_DIR="${CSD_POOL_INSTALL_HAPROXY_DIR:-/etc/haproxy}"
LIB_DIR="${CSD_POOL_INSTALL_LIB_DIR:-/var/lib/csd-pool}"
LOG_DIR="${CSD_POOL_INSTALL_LOG_DIR:-/var/log/csd-pool}"
BACKUP_DIR="${CSD_POOL_INSTALL_BACKUP_DIR:-/var/backups/csd-pool}"

if [[ -z "$ARTIFACT" ]]; then
  printf 'usage: %s <release-dir|release.tar.gz>\n' "$0" >&2
  exit 2
fi

map_path() {
  local path="$1"
  if [[ -n "$INSTALL_ROOT" ]]; then
    printf '%s%s\n' "$INSTALL_ROOT" "$path"
  else
    printf '%s\n' "$path"
  fi
}

run() {
  if [[ "$DRY_RUN" == "1" ]]; then
    printf 'dry-run:'
    printf ' %q' "$@"
    printf '\n'
  else
    "$@"
  fi
}

sha256_check() {
  local manifest="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "$manifest"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 -c "$manifest"
  else
    printf 'sha256 tool missing\n' >&2
    return 127
  fi
}

install_file() {
  local mode="$1"
  local src="$2"
  local dst="$3"
  run install -d -m 0755 "$(dirname "$dst")"
  run install -m "$mode" "$src" "$dst"
}

install_file_if_missing() {
  local mode="$1"
  local src="$2"
  local dst="$3"
  if [[ -e "$dst" ]]; then
    printf 'keep existing: %s\n' "$dst"
    return
  fi
  install_file "$mode" "$src" "$dst"
}

TMP_DIR=""
cleanup() {
  if [[ -n "$TMP_DIR" ]]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

resolve_artifact_dir() {
  local artifact="$1"
  if [[ -d "$artifact" ]]; then
    (cd "$artifact" && pwd)
    return
  fi
  if [[ -f "$artifact" && "$artifact" == *.tar.gz ]]; then
    TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-install.XXXXXX")"
    tar -xzf "$artifact" -C "$TMP_DIR"
    find "$TMP_DIR" -maxdepth 1 -type d -name 'csd-pool-*' | sort | tail -1
    return
  fi
  printf 'artifact must be a release directory or .tar.gz: %s\n' "$artifact" >&2
  exit 2
}

RELEASE_DIR="$(resolve_artifact_dir "$ARTIFACT")"
RELEASE_NAME="$(basename "$RELEASE_DIR")"

required_paths=(
  "$RELEASE_DIR/RELEASE-MANIFEST.txt"
  "$RELEASE_DIR/SHA256SUMS"
  "$RELEASE_DIR/bin/csd-pool-daemon"
  "$RELEASE_DIR/bin/csd-pool-workers"
  "$RELEASE_DIR/bin/csd-pool-signer"
  "$RELEASE_DIR/ops/wallet-signer/signer.mjs"
  "$RELEASE_DIR/ops/wallet-signer/node_modules/@inversealtruism/csd-tx/package.json"
  "$RELEASE_DIR/ops/systemd/csd-pool-daemon.service"
  "$RELEASE_DIR/ops/config.private-beta.toml"
  "$RELEASE_DIR/ops/env/csd-pool.env.example"
)

for path in "${required_paths[@]}"; do
  if [[ ! -e "$path" ]]; then
    printf 'missing artifact path: %s\n' "$path" >&2
    exit 1
  fi
done

(
  cd "$RELEASE_DIR"
  sha256_check SHA256SUMS >/tmp/csd-pool-install-sha256.log
)
printf 'ok: artifact checksums verified\n'

OPT_PATH="$(map_path "$OPT_DIR")"
ETC_PATH="$(map_path "$ETC_DIR")"
SYSTEMD_PATH="$(map_path "$SYSTEMD_DIR")"
HAPROXY_PATH="$(map_path "$HAPROXY_DIR")"
LIB_PATH="$(map_path "$LIB_DIR")"
LOG_PATH="$(map_path "$LOG_DIR")"
BACKUP_PATH="$(map_path "$BACKUP_DIR")"
RELEASE_PATH="$OPT_PATH/releases/$RELEASE_NAME"
CURRENT_RELEASE_FILE="$OPT_PATH/CURRENT_RELEASE"
PREVIOUS_RELEASE_FILE="$OPT_PATH/PREVIOUS_RELEASE"
RELEASE_ENV_FILE="$OPT_PATH/release.env"
CURRENT_LINK="$OPT_PATH/current"

printf 'CSD Pool install release\n'
printf 'release_dir=%s\n' "$RELEASE_DIR"
printf 'install_root=%s\n' "${INSTALL_ROOT:-/}"
printf 'release_path=%s\n' "$RELEASE_PATH"

run install -d -m 0755 "$OPT_PATH/bin" "$OPT_PATH/releases" "$RELEASE_PATH"
run install -d -m 0750 "$ETC_PATH" "$LIB_PATH" "$LOG_PATH" "$BACKUP_PATH"

run rm -rf "$RELEASE_PATH"
run install -d -m 0755 "$RELEASE_PATH"
if [[ "$DRY_RUN" == "1" ]]; then
  printf 'dry-run: cp -a %q/. %q/\n' "$RELEASE_DIR" "$RELEASE_PATH"
else
  cp -a "$RELEASE_DIR/." "$RELEASE_PATH/"
fi
# The release tree is public software, so every directory must remain
# traversable by the dedicated service accounts even when the installer runs
# under a restrictive umask. File modes from the verified archive are kept.
run find "$RELEASE_PATH" -type d -exec chmod 0755 {} +

if [[ -f "$CURRENT_RELEASE_FILE" ]]; then
  if [[ "$DRY_RUN" == "1" ]]; then
    printf 'dry-run: cp %q %q\n' "$CURRENT_RELEASE_FILE" "$PREVIOUS_RELEASE_FILE"
  else
    cp "$CURRENT_RELEASE_FILE" "$PREVIOUS_RELEASE_FILE"
  fi
fi

for binary in "$RELEASE_DIR"/bin/csd-pool-*; do
  install_file 0755 "$binary" "$OPT_PATH/bin/$(basename "$binary")"
done

if [[ "$DRY_RUN" == "1" ]]; then
  printf 'dry-run: printf %%s\\\\n %q ' "$RELEASE_NAME"
  printf '> %q\n' "$CURRENT_RELEASE_FILE"
else
  printf '%s\n' "$RELEASE_NAME" >"$CURRENT_RELEASE_FILE"
fi
run ln -sfn "$RELEASE_PATH" "$CURRENT_LINK"

release_revision="$(sed -n 's/^revision=//p' "$RELEASE_DIR/RELEASE-MANIFEST.txt" | head -n 1)"
release_timestamp="$(sed -n 's/^timestamp_utc=//p' "$RELEASE_DIR/RELEASE-MANIFEST.txt" | head -n 1)"
if [[ "$DRY_RUN" == "1" ]]; then
  printf 'dry-run: write release metadata env %q\n' "$RELEASE_ENV_FILE"
else
  cat >"$RELEASE_ENV_FILE" <<ENV
CSD_POOL_RELEASE_NAME=$RELEASE_NAME
CSD_POOL_RELEASE_REVISION=${release_revision:-unknown}
CSD_POOL_RELEASE_TIMESTAMP_UTC=${release_timestamp:-unknown}
ENV
  chmod 0644 "$RELEASE_ENV_FILE"
fi

install_file_if_missing 0640 "$RELEASE_DIR/ops/config.private-beta.toml" "$ETC_PATH/config.toml"
install_file_if_missing 0640 "$RELEASE_DIR/ops/env/csd-pool.env.example" "$ETC_PATH/csd-pool.env"
install_file_if_missing 0640 "$RELEASE_DIR/ops/env/csd-pool-node.env.example" "$ETC_PATH/node.env"

for unit in "$RELEASE_DIR"/ops/systemd/*; do
  install_file 0644 "$unit" "$SYSTEMD_PATH/$(basename "$unit")"
done

if [[ -f "$RELEASE_DIR/ops/haproxy/haproxy.cfg" ]]; then
  install_file 0644 "$RELEASE_DIR/ops/haproxy/haproxy.cfg" "$HAPROXY_PATH/haproxy.cfg"
fi

printf 'summary: install staged\n'
printf 'release_env=%s\n' "$RELEASE_ENV_FILE"
printf 'next: review %s/config.toml and %s/csd-pool.env before starting services\n' "$ETC_PATH" "$ETC_PATH"

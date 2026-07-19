#!/usr/bin/env bash
set -euo pipefail

TARGET_RELEASE="${1:-}"
INSTALL_ROOT="${CSD_POOL_INSTALL_ROOT:-}"
DRY_RUN="${CSD_POOL_ROLLBACK_DRY_RUN:-${CSD_POOL_INSTALL_DRY_RUN:-0}}"
OPT_DIR="${CSD_POOL_INSTALL_OPT_DIR:-/opt/csd-pool}"

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

OPT_PATH="$(map_path "$OPT_DIR")"
CURRENT_RELEASE_FILE="$OPT_PATH/CURRENT_RELEASE"
PREVIOUS_RELEASE_FILE="$OPT_PATH/PREVIOUS_RELEASE"
CURRENT_LINK="$OPT_PATH/current"

if [[ -z "$TARGET_RELEASE" ]]; then
  if [[ ! -f "$PREVIOUS_RELEASE_FILE" ]]; then
    printf 'previous release marker missing: %s\n' "$PREVIOUS_RELEASE_FILE" >&2
    printf 'usage: %s [release-name]\n' "$0" >&2
    exit 2
  fi
  TARGET_RELEASE="$(tr -d '[:space:]' <"$PREVIOUS_RELEASE_FILE")"
fi

if [[ "$TARGET_RELEASE" == */* ]]; then
  RELEASE_PATH="$TARGET_RELEASE"
else
  RELEASE_PATH="$OPT_PATH/releases/$TARGET_RELEASE"
fi

if [[ ! -d "$RELEASE_PATH" ]]; then
  printf 'release directory missing: %s\n' "$RELEASE_PATH" >&2
  exit 1
fi
if [[ ! -f "$RELEASE_PATH/SHA256SUMS" ]]; then
  printf 'release SHA256SUMS missing: %s\n' "$RELEASE_PATH/SHA256SUMS" >&2
  exit 1
fi

(
  cd "$RELEASE_PATH"
  sha256_check SHA256SUMS >/tmp/csd-pool-rollback-sha256.log
)
printf 'ok: rollback artifact checksums verified\n'

printf 'CSD Pool rollback release\n'
printf 'install_root=%s\n' "${INSTALL_ROOT:-/}"
printf 'release_path=%s\n' "$RELEASE_PATH"

run install -d -m 0755 "$OPT_PATH/bin"
for binary in "$RELEASE_PATH"/bin/csd-pool-*; do
  run install -m 0755 "$binary" "$OPT_PATH/bin/$(basename "$binary")"
done

if [[ -f "$CURRENT_RELEASE_FILE" ]]; then
  if [[ "$DRY_RUN" == "1" ]]; then
    printf 'dry-run: cp %q %q\n' "$CURRENT_RELEASE_FILE" "$PREVIOUS_RELEASE_FILE"
  else
    cp "$CURRENT_RELEASE_FILE" "$PREVIOUS_RELEASE_FILE"
  fi
fi

release_name="$(basename "$RELEASE_PATH")"
if [[ "$DRY_RUN" == "1" ]]; then
  printf 'dry-run: printf %%s\\\\n %q ' "$release_name"
  printf '> %q\n' "$CURRENT_RELEASE_FILE"
else
  printf '%s\n' "$release_name" >"$CURRENT_RELEASE_FILE"
fi
run ln -sfn "$RELEASE_PATH" "$CURRENT_LINK"

printf 'summary: rollback staged\n'
printf 'next: restart csd-pool services and run ops/bin/csd-pool-verify.sh\n'

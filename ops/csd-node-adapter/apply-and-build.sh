#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_DIR="${1:-${CSD_SOURCE_DIR:-}}"
EXPECTED_COMMIT="d2884dd7d8dbcdb6322af66afa0f0f833a9ab98c"
EXPECTED_PATCH_SHA256="b57eababa7db5228300bc013f95dbfc3c261c31b82edfe9388e29189cf02fbc8"
PATCH_FILE="$SCRIPT_DIR/compute-substrate-pool-adapter.patch"

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
}

sha256_value() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

[[ -n "$SOURCE_DIR" ]] || fail "usage: $0 /path/to/compute-substrate"
[[ -d "$SOURCE_DIR/.git" ]] || fail "not a git checkout: $SOURCE_DIR"
[[ "$(git -C "$SOURCE_DIR" rev-parse HEAD)" == "$EXPECTED_COMMIT" ]] || \
  fail "official source must be pinned to $EXPECTED_COMMIT"
[[ "$(sha256_value "$PATCH_FILE")" == "$EXPECTED_PATCH_SHA256" ]] || \
  fail "adapter patch checksum mismatch"

for path in src/api/mod.rs src/chain/mine.rs src/cli/main.rs src/net/node.rs; do
  git -C "$SOURCE_DIR" diff --quiet -- "$path" || fail "source file already modified: $path"
done

# The pinned upstream files are not rustfmt-clean. Normalize only the three
# adapter-owned files so the compact reviewed patch applies deterministically.
rustfmt --edition 2021 \
  "$SOURCE_DIR/src/api/mod.rs" \
  "$SOURCE_DIR/src/chain/mine.rs" \
  "$SOURCE_DIR/src/cli/main.rs"
patch -d "$SOURCE_DIR" -p1 --forward <"$PATCH_FILE"

rg -q 'CSD_POOL_ADAPTER_TOKEN' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "adapter authentication code missing after patch"
rg -q '/api/rpc/mining/template' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "mining template endpoint missing after patch"
rg -q '/api/rpc/block/submit' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "block submit endpoint missing after patch"

if [[ "${CSD_NODE_ADAPTER_SKIP_BUILD:-0}" != "1" ]]; then
  cargo check --manifest-path "$SOURCE_DIR/Cargo.toml" --lib --bin csd
  if [[ "${CSD_NODE_ADAPTER_SKIP_RELEASE_BUILD:-0}" != "1" ]]; then
    cargo build --manifest-path "$SOURCE_DIR/Cargo.toml" --release --bin csd
  fi
fi

printf 'source_commit=%s\n' "$EXPECTED_COMMIT"
printf 'patch_sha256=%s\n' "$EXPECTED_PATCH_SHA256"
printf 'adapter_token_env=CSD_POOL_ADAPTER_TOKEN\n'
printf 'summary: official CSD node pool adapter applied and built\n'

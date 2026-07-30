#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SOURCE_DIR="${1:-${CSD_SOURCE_DIR:-}}"
EXPECTED_COMMIT="d2884dd7d8dbcdb6322af66afa0f0f833a9ab98c"
EXPECTED_PATCH_SHA256="06b2d72e59feacb9c30b6bea0607e4c49fc633cae8689687f51f5844b04a8c41"
EXPECTED_P2P_PATCH_SHA256="51dd08cc9cfc0a4539afa67558c1ae005c165d615223e825e75130352eec2075"
PATCH_FILE="$SCRIPT_DIR/compute-substrate-pool-adapter.patch"
P2P_PATCH_FILE="$SCRIPT_DIR/compute-substrate-p2p-backoff.patch"

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

search_source() {
  local pattern="$1"
  local path="$2"
  if command -v rg >/dev/null 2>&1; then
    rg -q "$pattern" "$path"
  else
    grep -Eq "$pattern" "$path"
  fi
}

[[ -n "$SOURCE_DIR" ]] || fail "usage: $0 /path/to/compute-substrate"
[[ -d "$SOURCE_DIR/.git" ]] || fail "not a git checkout: $SOURCE_DIR"
[[ "$(git -C "$SOURCE_DIR" rev-parse HEAD)" == "$EXPECTED_COMMIT" ]] || \
  fail "official source must be pinned to $EXPECTED_COMMIT"
[[ "$(sha256_value "$PATCH_FILE")" == "$EXPECTED_PATCH_SHA256" ]] || \
  fail "adapter patch checksum mismatch"
[[ "$(sha256_value "$P2P_PATCH_FILE")" == "$EXPECTED_P2P_PATCH_SHA256" ]] || \
  fail "P2P backoff patch checksum mismatch"

for path in src/api/mod.rs src/chain/mine.rs src/cli/main.rs src/net/mempool.rs src/net/mod.rs src/net/node.rs src/net/proto.rs; do
  git -C "$SOURCE_DIR" diff --quiet -- "$path" || fail "source file already modified: $path"
done
for path in src/net/full_block_delivery.rs src/net/receipt.rs tests/idle_connection_timeout.rs; do
  [[ ! -e "$SOURCE_DIR/$path" ]] || fail "adapter-owned source already exists: $path"
done

# The adapter patch is self-contained against the exact pinned upstream bytes.
# It folds in the separately reviewed P2P backoff change so replay cannot apply
# that overlapping node.rs patch twice.
patch -d "$SOURCE_DIR" -p1 --forward <"$PATCH_FILE"

search_source 'CSD_POOL_ADAPTER_TOKEN' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "adapter authentication code missing after patch"
search_source '/api/rpc/mining/template' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "mining template endpoint missing after patch"
search_source '/api/rpc/mining/template-material' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "stateless template material endpoint missing after patch"
search_source '/api/rpc/block/submit' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "block submit endpoint missing after patch"
search_source '/api/rpc/block/submit-full' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "stateless full candidate endpoint missing after patch"
search_source 'node_observability' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "candidate propagation observability missing after patch"
search_source 'persist_index_flush_then_apply' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "candidate durability barrier missing after patch"
search_source 'relay_ack_timeout' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "mined header relay ACK timeout missing after patch"
if search_source 'pool:relay-state' "$SOURCE_DIR/src/api/mod.rs"; then
  fail "relay success cache must not bypass a fresh gossipsub ACK"
fi
search_source 'accepted_relay_recovered' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "canonical candidate relay retry path missing after patch"
search_source 'official_full_submit_uses_primary_material_with_empty_secondary_job_cache_and_divergent_mempools' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "A-template/B-empty-cache divergent-mempool replay missing after patch"
search_source 'two_official_http_adapters_replay_primary_material_with_secondary_empty_cache' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "two-official-adapter HTTP replay missing after patch"
search_source 'malformed_full_candidate_is_rejected_before_consensus_or_relay' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "malformed stateless full candidate replay missing after patch"
search_source 'p2p_first_canonical_candidate_requires_relay_ack' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "P2P-first canonical candidate relay-ACK replay missing after patch"
search_source 'p2p_first_canonical_ancestor_remains_idempotent_after_tip_advances' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "P2P-first canonical ancestor idempotency replay missing after patch"
search_source 'local_canonical_relay_failure_is_retried_before_duplicate_success' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "local-canonical relay recovery replay missing after patch"
search_source 'MinedHeaderPublishAck' "$SOURCE_DIR/src/net/mod.rs" || \
  fail "mined header publish ACK contract missing after patch"
search_source 'duplicate_publish_is_idempotent_not_a_new_broadcast' "$SOURCE_DIR/src/net/node.rs" || \
  fail "duplicate mined-header publish idempotency replay missing after patch"
search_source 'actual_gossipsub_peer_ack_corresponds_to_delivered_header' "$SOURCE_DIR/src/net/node.rs" || \
  fail "actual gossipsub peer delivery replay missing after patch"
search_source 'local_publish_never_becomes_gossip_seen_but_remote_signal_is_signed' "$SOURCE_DIR/src/net/receipt.rs" || \
  fail "remote-signal/local-publish boundary replay missing after patch"
search_source 'simultaneous_same_header_publish_is_deduplicated_before_inbound_callbacks' "$SOURCE_DIR/src/net/node.rs" || \
  fail "simultaneous header deduplication replay missing after patch"
search_source 'actual_remote_receipt_follows_real_gossipsub_delivery' "$SOURCE_DIR/src/net/node.rs" || \
  fail "actual remote gossip delivery replay missing after patch"
search_source 'actual_sync_full_block_flush_remains_delivery_attempt_without_remote_ack' "$SOURCE_DIR/src/net/node.rs" || \
  fail "full-block delivery-attempt replay missing after patch"
search_source '/api/rpc/block/full-delivery-status' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "authenticated full-block delivery status endpoint missing after patch"
search_source 'NOT_OBSERVABLE_PROTOCOL_NO_ACK' "$SOURCE_DIR/src/net/full_block_delivery.rs" || \
  fail "protocol no-ACK evidence boundary missing after patch"
search_source 'remote_application_ack_supported: false' "$SOURCE_DIR/src/net/full_block_delivery.rs" || \
  fail "remote application ACK boundary missing after patch"
search_source 'CSD_HEADER_RECEIPT_OBSERVER_PEER' "$SOURCE_DIR/src/cli/main.rs" || \
  fail "explicit receipt observer configuration missing after patch"
search_source 'InsufficientPeers' "$SOURCE_DIR/src/net/node.rs" || \
  fail "gossipsub publish failures are not classified after patch"
search_source 'choose_pool_block_time' "$SOURCE_DIR/src/api/mod.rs" || \
  fail "non-blocking pool template time is missing after patch"
search_source 'ADDR_BACKOFF_RETENTION_SECS' "$SOURCE_DIR/src/net/node.rs" || \
  fail "persistent P2P dial backoff is missing after patch"
search_source 'dial_failed peer=' "$SOURCE_DIR/src/net/node.rs" || \
  fail "structured P2P dial failure logging is missing after patch"
search_source 'addr_dial_is_pending' "$SOURCE_DIR/src/net/node.rs" || \
  fail "duplicate in-flight P2P dial suppression is missing after patch"
search_source 'PENDING_DIAL_RETENTION_SECS' "$SOURCE_DIR/src/net/node.rs" || \
  fail "bounded pending P2P dial retention is missing after patch"
search_source 'record_dial_submission' "$SOURCE_DIR/src/net/node.rs" || \
  fail "synchronous P2P dial enqueue failures are not handled after patch"
search_source 'synchronous_dial_failure_enters_backoff_without_pending_state' "$SOURCE_DIR/src/net/node.rs" || \
  fail "synchronous P2P dial failure replay is missing after patch"
search_source 'expired_pending_dial_does_not_block_retry' "$SOURCE_DIR/src/net/node.rs" || \
  fail "expired pending P2P dial replay is missing after patch"
if search_source 'choose_block_time\(&st\.db' "$SOURCE_DIR/src/api/mod.rs"; then
  fail "pool template endpoint still calls the blocking miner clock"
fi

if [[ "${CSD_NODE_ADAPTER_SKIP_BUILD:-0}" != "1" ]]; then
  cargo test --manifest-path "$SOURCE_DIR/Cargo.toml" --lib pool_candidate_tests
  cargo test --manifest-path "$SOURCE_DIR/Cargo.toml" --lib pool_header_publish_tests
  cargo test --manifest-path "$SOURCE_DIR/Cargo.toml" --lib full_block_delivery
  cargo test --manifest-path "$SOURCE_DIR/Cargo.toml" --lib receipt
  cargo test --manifest-path "$SOURCE_DIR/Cargo.toml" --lib dial_backoff_tests
  cargo test --manifest-path "$SOURCE_DIR/Cargo.toml" --test idle_connection_timeout
  cargo check --manifest-path "$SOURCE_DIR/Cargo.toml" --lib --bin csd
  if [[ "${CSD_NODE_ADAPTER_SKIP_RELEASE_BUILD:-0}" != "1" ]]; then
    cargo build --manifest-path "$SOURCE_DIR/Cargo.toml" --release --bin csd
  fi
fi

printf 'source_commit=%s\n' "$EXPECTED_COMMIT"
printf 'patch_sha256=%s\n' "$EXPECTED_PATCH_SHA256"
printf 'p2p_patch_sha256=%s\n' "$EXPECTED_P2P_PATCH_SHA256"
printf 'p2p_patch_mode=folded_into_adapter\n'
printf 'adapter_token_env=CSD_POOL_ADAPTER_TOKEN\n'
printf 'summary: official CSD node pool adapter applied and built\n'

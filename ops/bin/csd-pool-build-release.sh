#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
DIST_DIR="${CSD_POOL_DIST_DIR:-$ROOT_DIR/dist}"
TARGET_DIR="${CSD_POOL_TARGET_DIR:-$ROOT_DIR/target}"
RELEASE_TARGET_DIR="$TARGET_DIR/release"
TIMESTAMP="${CSD_POOL_RELEASE_TIMESTAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"

git_revision() {
  if git -C "$ROOT_DIR" rev-parse --short=12 HEAD >/dev/null 2>&1; then
    git -C "$ROOT_DIR" rev-parse --short=12 HEAD
  else
    printf 'nogit'
  fi
}

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1"
  else
    printf 'sha256 tool missing: %s\n' "$1" >&2
    return 127
  fi
}

copy_file() {
  local src="$1"
  local dst="$2"
  install -d -m 0755 "$(dirname "$dst")"
  install -m 0644 "$src" "$dst"
}

copy_executable() {
  local src="$1"
  local dst="$2"
  install -d -m 0755 "$(dirname "$dst")"
  install -m 0755 "$src" "$dst"
}

REVISION="$(git_revision)"
RELEASE_NAME="${CSD_POOL_RELEASE_NAME:-csd-pool-${REVISION}-${TIMESTAMP}}"
STAGING_DIR="$DIST_DIR/$RELEASE_NAME"
ARCHIVE_PATH="$DIST_DIR/$RELEASE_NAME.tar.gz"

BINARIES=(
  csd-pool-daemon
  csd-pool-workers
  csd-pool-signer
  csd-pool-api
  csd-pool-bridge
  csd-pool-mock-node
)

printf 'CSD Pool release build\n'
printf 'root=%s\n' "$ROOT_DIR"
printf 'revision=%s\n' "$REVISION"
printf 'release=%s\n' "$RELEASE_NAME"

if [[ "${CSD_POOL_RELEASE_SKIP_BUILD:-0}" != "1" ]]; then
  cargo build --release --workspace --manifest-path "$ROOT_DIR/Cargo.toml"
else
  printf 'skip: cargo build disabled by CSD_POOL_RELEASE_SKIP_BUILD=1\n'
fi

rm -rf "$STAGING_DIR" "$ARCHIVE_PATH"
install -d -m 0755 "$STAGING_DIR/bin" "$STAGING_DIR/ops" "$STAGING_DIR/docs" "$STAGING_DIR/migrations" "$STAGING_DIR/.github"

for binary in "${BINARIES[@]}"; do
  if [[ ! -x "$RELEASE_TARGET_DIR/$binary" ]]; then
    printf 'missing release binary: %s\n' "$RELEASE_TARGET_DIR/$binary" >&2
    exit 1
  fi
  copy_executable "$RELEASE_TARGET_DIR/$binary" "$STAGING_DIR/bin/$binary"
done

copy_file "$ROOT_DIR/README.md" "$STAGING_DIR/README.md"
copy_file "$ROOT_DIR/Cargo.lock" "$STAGING_DIR/Cargo.lock"
copy_file "$ROOT_DIR/docker-compose.yml" "$STAGING_DIR/docker-compose.yml"
copy_file "$ROOT_DIR/config.example.toml" "$STAGING_DIR/config.example.toml"
copy_file "$ROOT_DIR/ops/config.private-beta.toml" "$STAGING_DIR/ops/config.private-beta.toml"
copy_file "$ROOT_DIR/ops/README.md" "$STAGING_DIR/ops/README.md"
copy_file "$ROOT_DIR/ops/RELEASE-CHECKLIST.md" "$STAGING_DIR/ops/RELEASE-CHECKLIST.md"
copy_file "$ROOT_DIR/ops/INCIDENT-RUNBOOK.md" "$STAGING_DIR/ops/INCIDENT-RUNBOOK.md"

cp -R "$ROOT_DIR/ops/bin" "$STAGING_DIR/ops/"
cp -R "$ROOT_DIR/ops/env" "$STAGING_DIR/ops/"
cp -R "$ROOT_DIR/ops/haproxy" "$STAGING_DIR/ops/"
cp -R "$ROOT_DIR/ops/systemd" "$STAGING_DIR/ops/"
cp -R "$ROOT_DIR/ops/csd-node-adapter" "$STAGING_DIR/ops/"
install -d -m 0755 "$STAGING_DIR/ops/wallet-signer"
copy_executable "$ROOT_DIR/ops/wallet-signer/signer.mjs" "$STAGING_DIR/ops/wallet-signer/signer.mjs"
copy_file "$ROOT_DIR/ops/wallet-signer/signer.test.mjs" "$STAGING_DIR/ops/wallet-signer/signer.test.mjs"
copy_file "$ROOT_DIR/ops/wallet-signer/package.json" "$STAGING_DIR/ops/wallet-signer/package.json"
copy_file "$ROOT_DIR/ops/wallet-signer/package-lock.json" "$STAGING_DIR/ops/wallet-signer/package-lock.json"
if ! command -v npm >/dev/null 2>&1; then
  printf 'npm is required to package the official CSD wallet signer\n' >&2
  exit 127
fi
npm ci --omit=dev --ignore-scripts --prefix "$STAGING_DIR/ops/wallet-signer"
cp -R "$ROOT_DIR/.github/workflows" "$STAGING_DIR/.github/"
cp -R "$ROOT_DIR/docs/." "$STAGING_DIR/docs/"
cp -R "$ROOT_DIR/migrations/." "$STAGING_DIR/migrations/"
find "$STAGING_DIR/ops/bin" -type f -name '*.sh' -exec chmod 0755 {} +

cat >"$STAGING_DIR/RELEASE-MANIFEST.txt" <<MANIFEST
name=$RELEASE_NAME
revision=$REVISION
timestamp_utc=$TIMESTAMP
root=$ROOT_DIR
binaries=${BINARIES[*]}
ci_workflow=.github/workflows/ci.yml
wallet_signer=ops/wallet-signer/signer.mjs
node_adapter_patch=ops/csd-node-adapter/compute-substrate-pool-adapter.patch
node_p2p_backoff_patch=ops/csd-node-adapter/compute-substrate-p2p-backoff.patch
node_adapter_manifest=ops/csd-node-adapter/MANIFEST.txt
node_adapter_build=ops/csd-node-adapter/apply-and-build.sh
node_adapter_run=ops/bin/csd-pool-node-adapter-run.sh
verify=ops/bin/csd-pool-verify.sh
real_env_doctor=ops/bin/csd-pool-real-env-doctor.sh
real_env_doctor_self_test=ops/bin/csd-pool-real-env-doctor-self-test.sh
live_startup_policy_self_test=ops/bin/csd-pool-live-startup-policy-self-test.sh
go_live=ops/bin/csd-pool-go-live-check.sh
real_go_live=ops/bin/csd-pool-real-go-live.sh
go_live_evidence=go-live-evidence.tar.gz
verify_go_live_evidence=ops/bin/csd-pool-verify-go-live-evidence.sh
verify_real_go_live_summary=ops/bin/csd-pool-verify-real-go-live-summary.sh
export_real_go_live_receipt=ops/bin/csd-pool-export-real-go-live-receipt.sh
verify_real_go_live_receipt=ops/bin/csd-pool-verify-real-go-live-receipt.sh
public_acceptance=ops/bin/csd-pool-public-acceptance.sh
verify_public_acceptance_evidence=ops/bin/csd-pool-verify-public-acceptance-evidence.sh
public_acceptance_self_test=ops/bin/csd-pool-public-acceptance-self-test.sh
verify_launch_handoff=ops/bin/csd-pool-verify-launch-handoff.sh
launch_handoff_self_test=ops/bin/csd-pool-launch-handoff-self-test.sh
export_launch_handoff=ops/bin/csd-pool-export-launch-handoff.sh
verify_launch_handoff_package=ops/bin/csd-pool-verify-launch-handoff-package.sh
audit_launch_readiness=ops/bin/csd-pool-audit-launch-readiness.sh
export_launch_dossier=ops/bin/csd-pool-export-launch-dossier.sh
verify_launch_dossier=ops/bin/csd-pool-verify-launch-dossier.sh
launch_dossier_self_test=ops/bin/csd-pool-launch-dossier-self-test.sh
finalize_launch=ops/bin/csd-pool-finalize-launch.sh
explain_launch_gaps=ops/bin/csd-pool-explain-launch-gaps.sh
launch_gaps_self_test=ops/bin/csd-pool-launch-gaps-self-test.sh
export_final_review=ops/bin/csd-pool-export-final-review.sh
verify_final_review=ops/bin/csd-pool-verify-final-review.sh
final_review_self_test=ops/bin/csd-pool-final-review-self-test.sh
evidence_redaction_self_test=ops/bin/csd-pool-evidence-redaction-self-test.sh
release_archive_self_test=ops/bin/csd-pool-release-archive-self-test.sh
generate_signoff=ops/bin/csd-pool-generate-signoff.sh
install_release=ops/bin/csd-pool-install-release.sh
rollback_release=ops/bin/csd-pool-rollback-release.sh
install_release_self_test=ops/bin/csd-pool-install-release-self-test.sh
payout_serialization_self_test=ops/bin/csd-pool-payout-serialization-self-test.sh
local_e2e=ops/bin/csd-pool-local-e2e.sh
dev_env=ops/bin/csd-pool-dev-env.sh
release_check=ops/bin/csd-pool-release-check.sh
MANIFEST

(
  cd "$STAGING_DIR"
  find . -type f | sort | while read -r file; do
    if [[ "$file" == "./SHA256SUMS" ]]; then
      continue
    fi
    sha256_file "$file"
  done >SHA256SUMS
)

(
  cd "$DIST_DIR"
  tar -czf "$ARCHIVE_PATH" "$RELEASE_NAME"
)
(
  cd "$DIST_DIR"
  # Keep the sidecar portable: it is copied beside the archive to another
  # host before verification, so it must contain a relative basename.
  sha256_file "$(basename "$ARCHIVE_PATH")" >"$ARCHIVE_PATH.sha256"
)

printf 'artifact_dir=%s\n' "$STAGING_DIR"
printf 'artifact_tar=%s\n' "$ARCHIVE_PATH"
printf 'artifact_sha256=%s.sha256\n' "$ARCHIVE_PATH"
printf 'summary: release artifact created\n'

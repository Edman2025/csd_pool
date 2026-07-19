#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
ENV_PATH="${CSD_POOL_CI_ENV_FILE:-/tmp/csd-pool-ci-local.env}"
CONFIG_PATH="${CSD_POOL_CI_CONFIG:-$ROOT_DIR/ops/config.private-beta.toml}"

cd "$ROOT_DIR"

require_command() {
  local command="$1"
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'missing required command: %s\n' "$command" >&2
    exit 127
  fi
}

require_cargo_subcommand() {
  local subcommand="$1"
  local install_hint="$2"
  if ! cargo "$subcommand" --version >/dev/null 2>&1; then
    printf 'missing cargo subcommand: cargo %s\n' "$subcommand" >&2
    printf '%s\n' "$install_hint" >&2
    exit 127
  fi
}

require_command cargo
require_command python3
require_command npm
require_cargo_subcommand fmt "install with: rustup component add rustfmt"
require_cargo_subcommand clippy "install with: rustup component add clippy"

printf 'CSD Pool local CI\n'
printf 'root=%s\n' "$ROOT_DIR"

cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace
cargo build --workspace
npm ci --ignore-scripts --prefix ops/wallet-signer
npm test --prefix ops/wallet-signer
ops/bin/csd-pool-release-check.sh
ops/bin/csd-pool-real-env-doctor-self-test.sh
ops/bin/csd-pool-public-acceptance-self-test.sh
ops/bin/csd-pool-evidence-redaction-self-test.sh
ops/bin/csd-pool-launch-handoff-self-test.sh
ops/bin/csd-pool-launch-dossier-self-test.sh
ops/bin/csd-pool-launch-gaps-self-test.sh

rm -rf /tmp/csd-pool-ci-go-live-evidence
CSD_POOL_GO_LIVE_DRY_RUN=1 \
CSD_POOL_GO_LIVE_REPORT_DIR=/tmp/csd-pool-ci-go-live-evidence \
CSD_POOL_GO_LIVE_EVIDENCE_NAME=csd-pool-ci-go-live-evidence \
CSD_POOL_ENV_FILE=ops/env/csd-pool.env.example \
CSD_POOL_CONFIG=ops/config.private-beta.toml \
CSD_POOL_BIN_DIR=target/release \
CSD_POOL_GO_LIVE_API_URL=http://127.0.0.1:8080 \
CSD_POOL_GO_LIVE_STRATUM_ADDR=127.0.0.1:3333 \
  ops/bin/csd-pool-go-live-check.sh
CSD_POOL_EVIDENCE_ALLOW_DRY_RUN=1 \
  ops/bin/csd-pool-verify-go-live-evidence.sh \
    /tmp/csd-pool-ci-go-live-evidence/csd-pool-ci-go-live-evidence.tar.gz

rm -f "$ENV_PATH"
CSD_POOL_DATABASE_URL="${CSD_POOL_CI_DATABASE_URL:-postgres://csd_pool:ci-secret@127.0.0.1:5432/csd_pool_ci}" \
  ops/bin/csd-pool-generate-env.sh "$ENV_PATH"

CSD_POOL_ENV_FILE="$ENV_PATH" \
CSD_POOL_PREFLIGHT_CONFIG="$CONFIG_PATH" \
  ops/bin/csd-pool-preflight.sh

CSD_POOL_VERIFY_HTTP=0 \
CSD_POOL_ENV_FILE="$ENV_PATH" \
  ops/bin/csd-pool-verify.sh

CSD_POOL_E2E_DATABASE_URL="${CSD_POOL_CI_DATABASE_URL:-postgres://csd_pool:ci-secret@127.0.0.1:5432/csd_pool_ci}" \
CSD_POOL_BIN_DIR=target/debug \
  ops/bin/csd-pool-local-e2e.sh
CSD_POOL_PAYOUT_SERIALIZATION_DATABASE_URL="${CSD_POOL_CI_DATABASE_URL:-postgres://csd_pool:ci-secret@127.0.0.1:5432/csd_pool_ci}" \
CSD_POOL_BIN_DIR=target/debug \
  ops/bin/csd-pool-payout-serialization-self-test.sh
ops/bin/csd-pool-build-release.sh

archive="$(ls -t dist/*.tar.gz | head -1)"
shasum -a 256 -c "$archive.sha256"
latest="$(ls -td dist/csd-pool-*/ | head -1)"
CSD_POOL_BIN_DIR="$latest/bin" ops/bin/csd-pool-live-startup-policy-self-test.sh
CSD_POOL_VERIFY_HTTP=0 \
CSD_POOL_VERIFY_RELEASE=1 \
CSD_POOL_BIN_DIR="$latest/bin" \
CSD_POOL_VERIFY_RELEASE_ARCHIVE="$archive" \
  ops/bin/csd-pool-verify.sh
ops/bin/csd-pool-install-release-self-test.sh "$archive"
ops/bin/csd-pool-release-archive-self-test.sh "$archive"

if [[ -n "${CSD_POOL_CI_FINAL_REVIEW_PACKAGE:-}" ]]; then
  final_review_sha="${CSD_POOL_CI_FINAL_REVIEW_PACKAGE_SHA256:-}"
  if [[ -z "$final_review_sha" && -f "$CSD_POOL_CI_FINAL_REVIEW_PACKAGE.sha256" ]]; then
    final_review_sha="$CSD_POOL_CI_FINAL_REVIEW_PACKAGE.sha256"
  fi
  if [[ -n "$final_review_sha" ]]; then
    ops/bin/csd-pool-verify-final-review.sh "$CSD_POOL_CI_FINAL_REVIEW_PACKAGE" "$final_review_sha"
    ops/bin/csd-pool-final-review-self-test.sh "$CSD_POOL_CI_FINAL_REVIEW_PACKAGE" "$final_review_sha"
  else
    ops/bin/csd-pool-verify-final-review.sh "$CSD_POOL_CI_FINAL_REVIEW_PACKAGE"
    ops/bin/csd-pool-final-review-self-test.sh "$CSD_POOL_CI_FINAL_REVIEW_PACKAGE"
  fi
else
  printf 'skip: final review package self-test disabled; set CSD_POOL_CI_FINAL_REVIEW_PACKAGE\n'
fi

printf 'summary: local CI passed\n'

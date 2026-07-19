#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TEMPLATE_PATH="${CSD_POOL_ENV_TEMPLATE:-$ROOT_DIR/ops/env/csd-pool.env.example}"
OUTPUT_PATH="${1:-${CSD_POOL_ENV_OUTPUT:-/etc/csd-pool/csd-pool.env}}"
DATABASE_URL="${CSD_POOL_DATABASE_URL:-}"
FORCE="${CSD_POOL_ENV_FORCE:-0}"

usage() {
  cat <<USAGE
usage: CSD_POOL_DATABASE_URL=postgres://user:pass@host:5432/csd_pool $0 [output-path]

Generates a production CSD pool env file with fresh operator, signer, and node tokens.
Set CSD_POOL_ENV_FORCE=1 to overwrite an existing output file.
USAGE
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ -z "$DATABASE_URL" ]]; then
  printf 'CSD_POOL_DATABASE_URL is required\n' >&2
  usage >&2
  exit 2
fi

if [[ ! -f "$TEMPLATE_PATH" ]]; then
  printf 'template missing: %s\n' "$TEMPLATE_PATH" >&2
  exit 1
fi

if [[ -e "$OUTPUT_PATH" && "$FORCE" != "1" ]]; then
  printf 'refusing to overwrite existing env file: %s\n' "$OUTPUT_PATH" >&2
  printf 'set CSD_POOL_ENV_FORCE=1 to overwrite\n' >&2
  exit 1
fi

generate_token() {
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex 32
  elif command -v python3 >/dev/null 2>&1; then
    python3 -c 'import secrets; print(secrets.token_hex(32))'
  else
    printf 'openssl or python3 is required to generate tokens\n' >&2
    return 127
  fi
}

escape_sed() {
  printf '%s' "$1" | sed -e 's/[\/&]/\\&/g'
}

OPERATOR_TOKEN="${CSD_POOL_OPERATOR_TOKEN:-$(generate_token)}"
SIGNER_TOKEN="${CSD_POOL_SIGNER_TOKEN:-$(generate_token)}"
NODE_TOKEN="${CSD_POOL_NODE_TOKEN:-$(generate_token)}"
ESC_DATABASE_URL="$(escape_sed "$DATABASE_URL")"
ESC_OPERATOR_TOKEN="$(escape_sed "$OPERATOR_TOKEN")"
ESC_SIGNER_TOKEN="$(escape_sed "$SIGNER_TOKEN")"
ESC_NODE_TOKEN="$(escape_sed "$NODE_TOKEN")"

umask 0077
mkdir -p "$(dirname "$OUTPUT_PATH")"
sed \
  -e "s/^CSD_POOL_DATABASE_URL=.*/CSD_POOL_DATABASE_URL=$ESC_DATABASE_URL/" \
  -e "s/^CSD_POOL_OPERATOR_TOKEN=.*/CSD_POOL_OPERATOR_TOKEN=$ESC_OPERATOR_TOKEN/" \
  -e "s/^CSD_POOL_SIGNER_TOKEN=.*/CSD_POOL_SIGNER_TOKEN=$ESC_SIGNER_TOKEN/" \
  -e "s/^CSD_POOL_NODE_TOKEN=.*/CSD_POOL_NODE_TOKEN=$ESC_NODE_TOKEN/" \
  "$TEMPLATE_PATH" >"$OUTPUT_PATH"
chmod 0640 "$OUTPUT_PATH" 2>/dev/null || true

printf 'env_file=%s\n' "$OUTPUT_PATH"
printf 'operator_token_len=%s\n' "${#OPERATOR_TOKEN}"
printf 'signer_token_len=%s\n' "${#SIGNER_TOKEN}"
printf 'node_token_len=%s\n' "${#NODE_TOKEN}"
printf 'summary: env file generated\n'

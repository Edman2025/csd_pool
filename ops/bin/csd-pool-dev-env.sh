#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
ENV_PATH="${CSD_POOL_DEV_ENV_FILE:-/tmp/csd-pool-dev.env}"
CONFIG_PATH="${CSD_POOL_DEV_CONFIG:-$ROOT_DIR/ops/config.private-beta.toml}"
DATABASE_URL="${CSD_POOL_DEV_DATABASE_URL:-postgres://csd_pool:csd_pool_dev@127.0.0.1:5432/csd_pool}"
COMPOSE_PROJECT="${CSD_POOL_COMPOSE_PROJECT:-csd-pool-dev}"
COMMAND="${1:-up}"

cd "$ROOT_DIR"

compose() {
  if docker compose version >/dev/null 2>&1; then
    docker compose -p "$COMPOSE_PROJECT" "$@"
  elif command -v docker-compose >/dev/null 2>&1; then
    docker-compose -p "$COMPOSE_PROJECT" "$@"
  else
    printf 'docker compose or docker-compose is required\n' >&2
    return 127
  fi
}

docker_daemon_available() {
  docker info >/dev/null 2>&1
}

run_workers_command() {
  if [[ -x "${CSD_POOL_WORKERS_BIN:-}" ]]; then
    "$CSD_POOL_WORKERS_BIN" "$@"
  else
    cargo run --quiet --manifest-path "$ROOT_DIR/Cargo.toml" -p csd-pool-workers -- "$@"
  fi
}

wait_for_postgres() {
  local attempts="${CSD_POOL_DEV_WAIT_ATTEMPTS:-60}"
  local i
  for ((i = 1; i <= attempts; i++)); do
    if compose exec -T postgres pg_isready -U csd_pool -d csd_pool >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  printf 'postgres did not become healthy after %s attempts\n' "$attempts" >&2
  return 1
}

wait_for_redis() {
  local attempts="${CSD_POOL_DEV_WAIT_ATTEMPTS:-60}"
  local i
  for ((i = 1; i <= attempts; i++)); do
    if compose exec -T redis redis-cli ping 2>/dev/null | grep -q PONG; then
      return 0
    fi
    sleep 1
  done
  printf 'redis did not become healthy after %s attempts\n' "$attempts" >&2
  return 1
}

write_env() {
  CSD_POOL_ENV_FORCE=1 \
  CSD_POOL_DATABASE_URL="$DATABASE_URL" \
    "$ROOT_DIR/ops/bin/csd-pool-generate-env.sh" "$ENV_PATH" >/tmp/csd-pool-dev-generate-env.log
  tmp_env="$(mktemp)"
  sed \
    -e "s|^CSD_POOL_CONFIG=.*|CSD_POOL_CONFIG=$CONFIG_PATH|" \
    -e "s|^CSD_POOL_REDIS_URL=.*|CSD_POOL_REDIS_URL=redis://127.0.0.1:6379/0|" \
    "$ENV_PATH" >"$tmp_env"
  mv "$tmp_env" "$ENV_PATH"
  chmod 0640 "$ENV_PATH" 2>/dev/null || true
  printf 'env_file=%s\n' "$ENV_PATH"
}

case "$COMMAND" in
  up)
    if ! docker_daemon_available; then
      printf 'docker daemon is not available; start Docker and retry\n' >&2
      exit 1
    fi
    compose up -d postgres redis
    wait_for_postgres
    wait_for_redis
    write_env
    set -a
    # shellcheck disable=SC1090
    source "$ENV_PATH"
    set +a
    CSD_POOL_CONFIG="$CONFIG_PATH" run_workers_command migrate >/tmp/csd-pool-dev-migrate.json
    CSD_POOL_CONFIG="$CONFIG_PATH" \
    CSD_POOL_CHECK_CONFIG_REQUIRE_ENV=1 \
      run_workers_command check-config "$CONFIG_PATH" >/tmp/csd-pool-dev-check-config.json
    printf 'database_url=%s\n' "$DATABASE_URL"
    printf 'redis_url=%s\n' "${CSD_POOL_REDIS_URL:-redis://127.0.0.1:6379/0}"
    printf 'reports=/tmp/csd-pool-dev-{migrate,check-config}.json\n'
    printf 'summary: dev dependencies ready\n'
    ;;
  down)
    if ! docker_daemon_available; then
      printf 'skip: docker daemon is not available; nothing stopped\n'
      exit 0
    fi
    compose down
    printf 'summary: dev dependencies stopped\n'
    ;;
  reset)
    if ! docker_daemon_available; then
      printf 'skip: docker daemon is not available; nothing reset\n'
      exit 0
    fi
    compose down -v
    printf 'summary: dev dependencies stopped and volumes removed\n'
    ;;
  status)
    if ! docker_daemon_available; then
      printf 'skip: docker daemon is not available\n'
      exit 0
    fi
    compose ps
    ;;
  *)
    printf 'usage: %s [up|down|reset|status]\n' "$0" >&2
    exit 2
    ;;
esac

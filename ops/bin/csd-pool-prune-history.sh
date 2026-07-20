#!/usr/bin/env bash
set -euo pipefail

DATABASE="${CSD_POOL_RETENTION_DATABASE:-csd_pool}"
SHARE_DAYS="${CSD_POOL_SHARE_RETENTION_DAYS:-14}"
EVENT_DAYS="${CSD_POOL_SHARE_EVENT_RETENTION_DAYS:-14}"
NODE_SAMPLE_DAYS="${CSD_POOL_NODE_SAMPLE_RETENTION_DAYS:-90}"
ALERT_DAYS="${CSD_POOL_RESOLVED_ALERT_RETENTION_DAYS:-180}"
BATCH_SIZE="${CSD_POOL_RETENTION_BATCH_SIZE:-50000}"
PAUSE_SECS="${CSD_POOL_RETENTION_PAUSE_SECS:-0.05}"
DRY_RUN="${CSD_POOL_RETENTION_DRY_RUN:-0}"
PSQL_BIN="${CSD_POOL_PSQL_BIN:-psql}"

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
}

require_positive_integer() {
  local name="$1"
  local value="$2"
  [[ "$value" =~ ^[1-9][0-9]*$ ]] || fail "$name must be a positive integer"
}

require_positive_integer CSD_POOL_SHARE_RETENTION_DAYS "$SHARE_DAYS"
require_positive_integer CSD_POOL_SHARE_EVENT_RETENTION_DAYS "$EVENT_DAYS"
require_positive_integer CSD_POOL_NODE_SAMPLE_RETENTION_DAYS "$NODE_SAMPLE_DAYS"
require_positive_integer CSD_POOL_RESOLVED_ALERT_RETENTION_DAYS "$ALERT_DAYS"
require_positive_integer CSD_POOL_RETENTION_BATCH_SIZE "$BATCH_SIZE"
[[ "$PAUSE_SECS" =~ ^[0-9]+([.][0-9]+)?$ ]] ||
  fail "CSD_POOL_RETENTION_PAUSE_SECS must be a non-negative number"
[[ "$DRY_RUN" == "0" || "$DRY_RUN" == "1" ]] ||
  fail "CSD_POOL_RETENTION_DRY_RUN must be 0 or 1"
command -v "$PSQL_BIN" >/dev/null 2>&1 || fail "psql binary not found"

psql_query() {
  "$PSQL_BIN" \
    --no-psqlrc \
    --no-align \
    --tuples-only \
    --quiet \
    --set=ON_ERROR_STOP=1 \
    --dbname "$DATABASE" \
    --command "$1"
}

prune_table() {
  local table="$1"
  local timestamp_column="$2"
  local retention_days="$3"
  local predicate="${4:-}"
  local total=0
  local deleted

  if [[ "$DRY_RUN" == "1" ]]; then
    deleted="$(
      psql_query \
        "select count(*) from ${table}
         where ${timestamp_column} < clock_timestamp() - interval '${retention_days} days'
         ${predicate};"
    )"
    printf 'dry_run table=%s eligible=%s retention_days=%s\n' \
      "$table" "${deleted//[[:space:]]/}" "$retention_days"
    return
  fi

  while true; do
    deleted="$(
      psql_query \
        "with doomed as (
           select id
           from ${table}
           where ${timestamp_column} < clock_timestamp() - interval '${retention_days} days'
           ${predicate}
           order by id
           limit ${BATCH_SIZE}
         ),
         removed as (
           delete from ${table} target
           using doomed
           where target.id = doomed.id
           returning 1
         )
         select count(*) from removed;"
    )"
    deleted="${deleted//[[:space:]]/}"
    [[ "$deleted" =~ ^[0-9]+$ ]] || fail "unexpected delete count for $table"
    total=$((total + deleted))
    (( deleted < BATCH_SIZE )) && break
    sleep "$PAUSE_SECS"
  done

  if (( total > 0 )); then
    psql_query "analyze ${table};" >/dev/null
  fi
  printf 'pruned table=%s rows=%s retention_days=%s\n' \
    "$table" "$total" "$retention_days"
}

prune_table share_events created_at "$EVENT_DAYS"
prune_table shares created_at "$SHARE_DAYS"
prune_table node_samples sampled_at "$NODE_SAMPLE_DAYS"
prune_table alert_events last_seen_at "$ALERT_DAYS" "and status = 'resolved'"

printf 'retention complete database=%s dry_run=%s\n' "$DATABASE" "$DRY_RUN"

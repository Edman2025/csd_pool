#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
DATABASE_URL="${CSD_POOL_PAYOUT_SERIALIZATION_DATABASE_URL:-${CSD_POOL_CI_DATABASE_URL:-}}"
BIN_DIR="${CSD_POOL_BIN_DIR:-$ROOT_DIR/target/debug}"
WORKERS_BIN="${CSD_POOL_WORKERS_BIN:-$BIN_DIR/csd-pool-workers}"
SIGNER_BIN="${CSD_POOL_SIGNER_BIN:-$BIN_DIR/csd-pool-signer}"
MOCK_NODE_BIN="${CSD_POOL_MOCK_NODE_BIN:-$BIN_DIR/csd-pool-mock-node}"
PSQL_BIN="${CSD_POOL_PSQL_BIN:-$(command -v psql || true)}"
SIGNER_ADDR="${CSD_POOL_PAYOUT_SERIALIZATION_SIGNER_ADDR:-127.0.0.1:19890}"
SIGNER_URL="http://$SIGNER_ADDR"
MOCK_NODE_ADDR="${CSD_POOL_PAYOUT_SERIALIZATION_NODE_ADDR:-127.0.0.1:19891}"
MOCK_NODE_URL="http://$MOCK_NODE_ADDR"
OUTPUT_DIR="${CSD_POOL_PAYOUT_SERIALIZATION_OUTPUT_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-payout-serialization.XXXXXX")}"
BATCH_A="serialization-${$}-a"
BATCH_B="serialization-${$}-b"
ADDRESS_A="aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
ADDRESS_B="bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
SIGNER_PID=""
MOCK_NODE_PID=""
OLD_PAYOUTS_ENABLED="false"

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
}

psql_run() {
  "$PSQL_BIN" -X -v ON_ERROR_STOP=1 -qAt "$DATABASE_URL" "$@"
}

cleanup() {
  if [[ -n "$SIGNER_PID" ]]; then
    kill "$SIGNER_PID" >/dev/null 2>&1 || true
    wait "$SIGNER_PID" >/dev/null 2>&1 || true
  fi
  if [[ -n "$MOCK_NODE_PID" ]]; then
    kill "$MOCK_NODE_PID" >/dev/null 2>&1 || true
    wait "$MOCK_NODE_PID" >/dev/null 2>&1 || true
  fi
  if [[ -n "$DATABASE_URL" && -n "$PSQL_BIN" ]]; then
    psql_run -c "delete from payout_audit_events where batch_id in ('$BATCH_A', '$BATCH_B'); delete from payout_recipients where batch_id in ('$BATCH_A', '$BATCH_B'); delete from payout_batches where id in ('$BATCH_A', '$BATCH_B'); update pool_settings set value = '$OLD_PAYOUTS_ENABLED', updated_at = now() where key = 'payouts_enabled';" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

[[ -n "$DATABASE_URL" ]] || fail "set CSD_POOL_PAYOUT_SERIALIZATION_DATABASE_URL"
database_name="${DATABASE_URL%%\?*}"
database_name="${database_name##*/}"
if [[ "${CSD_POOL_PAYOUT_SERIALIZATION_ALLOW_NON_TEST_DB:-0}" != "1" && ! "$database_name" =~ (test|e2e|ci) ]]; then
  fail "database name must contain test, e2e, or ci (got $database_name)"
fi
[[ -n "$PSQL_BIN" && -x "$PSQL_BIN" ]] || fail "psql is required; set CSD_POOL_PSQL_BIN"
[[ -x "$WORKERS_BIN" ]] || fail "workers binary missing: $WORKERS_BIN"
[[ -x "$SIGNER_BIN" ]] || fail "mock signer binary missing: $SIGNER_BIN"
[[ -x "$MOCK_NODE_BIN" ]] || fail "mock node binary missing: $MOCK_NODE_BIN"
mkdir -p "$OUTPUT_DIR"

printf 'CSD Pool payout serialization self-test\n'
printf 'database=%s\n' "$database_name"
printf 'output_dir=%s\n' "$OUTPUT_DIR"

CSD_POOL_DATABASE_URL="$DATABASE_URL" "$WORKERS_BIN" migrate >"$OUTPUT_DIR/migrate.json"
OLD_PAYOUTS_ENABLED="$(psql_run -c "select coalesce((select value from pool_settings where key = 'payouts_enabled'), 'false')")"

CSD_POOL_SIGNER_LISTEN="$SIGNER_ADDR" env -u CSD_POOL_SIGNER_TOKEN \
  "$SIGNER_BIN" >"$OUTPUT_DIR/signer.log" 2>&1 &
SIGNER_PID="$!"
for _ in $(seq 1 40); do
  if curl --fail --silent --max-time 1 "$SIGNER_URL/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
curl --fail --silent --max-time 2 "$SIGNER_URL/health" >"$OUTPUT_DIR/signer-health.json" \
  || fail "mock signer did not become healthy"

CSD_POOL_MOCK_NODE_LISTEN="$MOCK_NODE_ADDR" "$MOCK_NODE_BIN" >"$OUTPUT_DIR/mock-node.log" 2>&1 &
MOCK_NODE_PID="$!"
for _ in $(seq 1 40); do
  if curl --fail --silent --max-time 1 "$MOCK_NODE_URL/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
curl --fail --silent --max-time 2 "$MOCK_NODE_URL/health" >"$OUTPUT_DIR/mock-node-health.json" \
  || fail "mock node did not become healthy"

psql_run -c "
insert into miners(address) values ('$ADDRESS_A'), ('$ADDRESS_B') on conflict(address) do nothing;
insert into payout_batches(id, status, total_base_units, recipient_count)
values ('$BATCH_A', 'created', 546, 1), ('$BATCH_B', 'created', 546, 1);
insert into payout_recipients(batch_id, miner_id, address, amount_base_units)
select '$BATCH_A', id, address, 546 from miners where address = '$ADDRESS_A';
insert into payout_recipients(batch_id, miner_id, address, amount_base_units)
select '$BATCH_B', id, address, 546 from miners where address = '$ADDRESS_B';
update pool_settings set value = 'true', updated_at = now() where key = 'payouts_enabled';
" >/dev/null

set +e
env -u CSD_POOL_CONFIG -u CSD_POOL_SIGNER_TOKEN \
  CSD_POOL_DATABASE_URL="$DATABASE_URL" CSD_POOL_SIGNER_URL="$SIGNER_URL" \
  "$WORKERS_BIN" sign-payouts >"$OUTPUT_DIR/sign-1.json" 2>"$OUTPUT_DIR/sign-1.log" &
PID_ONE="$!"
env -u CSD_POOL_CONFIG -u CSD_POOL_SIGNER_TOKEN \
  CSD_POOL_DATABASE_URL="$DATABASE_URL" CSD_POOL_SIGNER_URL="$SIGNER_URL" \
  "$WORKERS_BIN" sign-payouts >"$OUTPUT_DIR/sign-2.json" 2>"$OUTPUT_DIR/sign-2.log" &
PID_TWO="$!"
wait "$PID_ONE"; STATUS_ONE="$?"
wait "$PID_TWO"; STATUS_TWO="$?"
set -e
[[ "$STATUS_ONE" == "0" && "$STATUS_TWO" == "0" ]] || fail "concurrent sign workers failed"

python3 - "$OUTPUT_DIR/sign-1.json" "$OUTPUT_DIR/sign-2.json" "$BATCH_A" <<'PY'
import json
import sys

paths = sys.argv[1:3]
expected = sys.argv[3]
runs = [json.load(open(path, encoding="utf-8")) for path in paths]
outcomes = [item for run in runs for item in run.get("outcomes", [])]
blocked = [run.get("blocked_by_inflight_batch") for run in runs if run.get("blocked_by_inflight_batch")]
if len(outcomes) != 1 or outcomes[0].get("batch_id") != expected or outcomes[0].get("status") != "signed":
    raise SystemExit(f"expected exactly one signed outcome for {expected}: {outcomes}")
if blocked != [expected]:
    raise SystemExit(f"expected exactly one blocked run for {expected}: {blocked}")
PY

statuses="$(psql_run -c "select id || '=' || status from payout_batches where id in ('$BATCH_A', '$BATCH_B') order by id")"
expected_statuses="$(printf '%s\n%s' "$BATCH_A=signed" "$BATCH_B=created")"
[[ "$statuses" == "$expected_statuses" ]] || fail "unexpected payout states: $statuses"

printf 'ok: concurrent workers produced one signed batch and one blocked run\n'
printf 'ok: second payout batch remained created\n'

node_tx='{"version":1,"inputs":[{"prevout":{"txid":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"vout":0},"script_sig":[2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2,2]}],"outputs":[{"value":546,"script_pubkey":[170,170,170,170,170,170,170,170,170,170,170,170,170,170,170,170,170,170,170,170]}],"locktime":0,"app":"None"}'
psql_run -c "update payout_batches set raw_tx_hash = 'csd-node-json-v1:$node_tx', txid = '$(printf '12%.0s' {1..32})' where id = '$BATCH_A';" >/dev/null

env -u CSD_POOL_CONFIG -u CSD_POOL_SUBMIT_NODE_URL -u CSD_POOL_NODE_URL \
  CSD_POOL_DATABASE_URL="$DATABASE_URL" CSD_POOL_PAYOUT_NODE_URL="$MOCK_NODE_URL" \
  "$WORKERS_BIN" submit-payouts >"$OUTPUT_DIR/submit-official.json" 2>"$OUTPUT_DIR/submit-official.log" \
  || fail "official payout submission failed"
grep -Fq '"status": "submitted"' "$OUTPUT_DIR/submit-official.json" \
  || fail "official payout did not enter submitted state"
submitted_status="$(psql_run -c "select status from payout_batches where id = '$BATCH_A'")"
[[ "$submitted_status" == "submitted" ]] || fail "official payout database status is $submitted_status"
printf 'ok: official /tx/submit payout broadcast entered submitted state\n'

psql_run -c "update payout_batches set status = 'signed', raw_tx_hash = 'csd-node-json-v1:$node_tx', txid = '$(printf '34%.0s' {1..32})', signed_at = now() where id = '$BATCH_B';" >/dev/null
env -u CSD_POOL_CONFIG -u CSD_POOL_SUBMIT_NODE_URL -u CSD_POOL_NODE_URL \
  CSD_POOL_DATABASE_URL="$DATABASE_URL" CSD_POOL_PAYOUT_NODE_URL="http://127.0.0.1:1" \
  "$WORKERS_BIN" submit-payouts >"$OUTPUT_DIR/submit-uncertain.json" 2>"$OUTPUT_DIR/submit-uncertain.log" \
  || fail "uncertain payout submission command failed"
python3 - "$OUTPUT_DIR/submit-uncertain.json" "$BATCH_B" <<'PY'
import json
import sys

run = json.load(open(sys.argv[1], encoding="utf-8"))
expected = sys.argv[2]
matches = [item for item in run.get("outcomes", []) if item.get("batch_id") == expected]
if len(matches) != 1 or matches[0].get("status") != "signed" or matches[0].get("updated") is not False:
    raise SystemExit(f"uncertain submit did not remain signed: {matches}")
PY
uncertain_status="$(psql_run -c "select status from payout_batches where id = '$BATCH_B'")"
[[ "$uncertain_status" == "signed" ]] || fail "uncertain payout database status is $uncertain_status"
deferred_audits="$(psql_run -c "select count(*) from payout_audit_events where batch_id = '$BATCH_B' and action = 'submit_deferred'")"
[[ "$deferred_audits" -ge 1 ]] || fail "uncertain payout did not record submit_deferred audit"
printf 'ok: uncertain broadcast remained signed and locked for investigation\n'
printf 'summary: payout serialization self-test passed\n'

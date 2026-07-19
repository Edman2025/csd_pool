#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
BIN_DIR="${CSD_POOL_BIN_DIR:-/opt/csd-pool/bin}"
WORKERS_BIN="${CSD_POOL_WORKERS_BIN:-$BIN_DIR/csd-pool-workers}"
VERIFY_RECEIPT_SCRIPT="${CSD_POOL_VERIFY_REAL_GO_LIVE_RECEIPT_SCRIPT:-$ROOT_DIR/ops/bin/csd-pool-verify-real-go-live-receipt.sh}"
RECEIPT_ARCHIVE="${1:-${CSD_POOL_ACCEPTANCE_RECEIPT:-}}"
PUBLIC_API_URL="${CSD_POOL_ACCEPTANCE_PUBLIC_API_URL:-${CSD_POOL_GO_LIVE_PUBLIC_API_URL:-${CSD_POOL_PUBLIC_API_URL:-}}}"
PUBLIC_STRATUM_ADDR="${CSD_POOL_ACCEPTANCE_PUBLIC_STRATUM_ADDR:-${CSD_POOL_GO_LIVE_PUBLIC_STRATUM_ADDR:-${CSD_POOL_PUBLIC_STRATUM_ADDR:-}}}"
REPORT_DIR="${CSD_POOL_ACCEPTANCE_REPORT_DIR:-/tmp/csd-pool-public-acceptance}"
TIMEOUT_SECS="${CSD_POOL_ACCEPTANCE_TIMEOUT_SECS:-10}"
RUN_LOAD="${CSD_POOL_ACCEPTANCE_LOAD:-0}"
CANARY_ADDRESS_OVERRIDE="${CSD_POOL_ACCEPTANCE_CANARY_ADDRESS:-}"
REQUIRE_ACCEPTED_SHARE="${CSD_POOL_ACCEPTANCE_REQUIRE_ACCEPTED_SHARE:-0}"
MIN_ACCEPTED_SHARES="${CSD_POOL_ACCEPTANCE_MIN_ACCEPTED_SHARES:-1}"
CANARY_MAX_AGE_SECONDS="${CSD_POOL_ACCEPTANCE_CANARY_MAX_AGE_SECONDS:-600}"
KEEP_TMP="${CSD_POOL_ACCEPTANCE_KEEP_DIR:-0}"
TMP_DIR=""

PASS=0
FAIL=0
SKIP=0

ok() {
  PASS=$((PASS + 1))
  printf 'ok: %s\n' "$1"
}

fail() {
  FAIL=$((FAIL + 1))
  printf 'fail: %s\n' "$1" >&2
}

skip() {
  SKIP=$((SKIP + 1))
  printf 'skip: %s\n' "$1"
}

cleanup() {
  if [[ "$KEEP_TMP" != "1" && -n "$TMP_DIR" && -d "$TMP_DIR" ]]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

json_escape() {
  sed 's/\\/\\\\/g; s/"/\\"/g'
}

json_string() {
  printf '"%s"' "$(printf '%s' "$1" | json_escape)"
}

sha256_value() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
  else
    printf 'sha256-tool-missing'
  fi
}

status_release_field() {
  local field="$1"
  local path="$REPORT_DIR/http-public-status.json"
  if [[ ! -f "$path" ]]; then
    printf 'missing'
    return
  fi
  python3 - "$path" "$field" <<'PY'
import json
import sys

path, field = sys.argv[1:3]
try:
    with open(path, "r", encoding="utf-8") as handle:
        data = json.load(handle)
except (OSError, json.JSONDecodeError):
    print("missing")
    raise SystemExit(0)
value = (data.get("release") or {}).get(field)
print(value if value else "missing")
PY
}

write_acceptance_toolchain_manifest() {
  local path="$1"
  python3 - \
    "$path" \
    "$PUBLIC_API_URL" \
    "$PUBLIC_STRATUM_ADDR" \
    "$ROOT_DIR/ops/bin/csd-pool-public-acceptance.sh" \
    "$VERIFY_RECEIPT_SCRIPT" \
    "$WORKERS_BIN" <<'PY'
import hashlib
import json
import pathlib
import sys

output, public_api_url, public_stratum_addr, *paths = sys.argv[1:]
entries = []
for raw in paths:
    path = pathlib.Path(raw)
    entry = {
        "path": str(path),
        "basename": path.name,
        "exists": path.is_file(),
        "executable": bool(path.is_file() and (path.stat().st_mode & 0o111)),
    }
    entry["sha256"] = hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else "missing"
    entries.append(entry)
payload = {
    "target": "public-acceptance",
    "public_api_url": public_api_url or "missing",
    "public_stratum_addr": public_stratum_addr or "missing",
    "entries": entries,
    "required_basenames": [
        "csd-pool-public-acceptance.sh",
        "csd-pool-verify-real-go-live-receipt.sh",
        "csd-pool-workers",
    ],
}
pathlib.Path(output).write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
}

normalize_url() {
  printf '%s' "$1" | sed 's#/*$##'
}

write_summary() {
  local status archive_path archive_sha receipt_archive_sha
  local release_name release_revision release_built_at release_version
  if [[ "$FAIL" -eq 0 ]]; then
    status="passed"
  else
    status="failed"
  fi
  archive_path="$REPORT_DIR/public-acceptance-evidence.tar.gz"
  archive_sha="$archive_path.sha256"
  if [[ -n "$RECEIPT_ARCHIVE" && -f "$RECEIPT_ARCHIVE" ]]; then
    receipt_archive_sha="$(sha256_value "$RECEIPT_ARCHIVE")"
  else
    receipt_archive_sha="missing"
  fi
  release_name="$(status_release_field name)"
  release_revision="$(status_release_field revision)"
  release_built_at="$(status_release_field built_at)"
  release_version="$(status_release_field version)"
  cat >"$REPORT_DIR/public-acceptance-summary.json" <<JSON
{
  "status": $(json_string "$status"),
  "target": "public-acceptance",
  "public_api_url": $(json_string "${PUBLIC_API_URL:-missing}"),
  "public_stratum_addr": $(json_string "${PUBLIC_STRATUM_ADDR:-missing}"),
  "receipt_archive": $(json_string "${RECEIPT_ARCHIVE:-missing}"),
  "receipt_archive_sha256": $(json_string "$receipt_archive_sha"),
  "public_status_release": {
    "name": $(json_string "$release_name"),
    "revision": $(json_string "$release_revision"),
    "built_at": $(json_string "$release_built_at"),
    "version": $(json_string "$release_version")
  },
  "acceptance_toolchain_manifest": $(json_string "$REPORT_DIR/acceptance-toolchain-manifest.json"),
  "pass": $PASS,
  "fail": $FAIL,
  "skip": $SKIP,
    "accepted_share_required": $(json_string "$REQUIRE_ACCEPTED_SHARE"),
    "accepted_share_minimum": $MIN_ACCEPTED_SHARES,
    "canary_max_age_seconds": $CANARY_MAX_AGE_SECONDS,
  "reports": {
    "receipt_verify": $(json_string "$REPORT_DIR/receipt-verify.log"),
    "receipt_binding": $(json_string "$REPORT_DIR/receipt-binding.log"),
    "api_health": $(json_string "$REPORT_DIR/http-public-health.json"),
    "api_status": $(json_string "$REPORT_DIR/http-public-status.json"),
    "status_release_binding": $(json_string "$REPORT_DIR/public-status-release-binding.log"),
    "api_pool": $(json_string "$REPORT_DIR/http-public-pool.json"),
    "api_getting_started": $(json_string "$REPORT_DIR/http-public-getting-started.json"),
    "getting_started_binding": $(json_string "$REPORT_DIR/getting-started-binding.log"),
    "public_endpoint_routability": $(json_string "$REPORT_DIR/public-endpoint-routability.log"),
    "stratum_smoke": $(json_string "$REPORT_DIR/public-stratum-smoke.json"),
    "stratum_submit_probe": $(json_string "$REPORT_DIR/public-stratum-submit-probe.json"),
    "stratum_load": $(json_string "$REPORT_DIR/public-stratum-load.json"),
    "canary_miner": $(json_string "$REPORT_DIR/public-canary-miner.json"),
    "canary_miner_api": $(json_string "$REPORT_DIR/http-public-canary-miner.json"),
    "canary_miner_workers_api": $(json_string "$REPORT_DIR/http-public-canary-miner-workers.json")
  },
  "evidence_archive": $(json_string "$archive_path"),
  "evidence_sha256": $(json_string "$archive_sha")
}
JSON
  {
    printf 'CSD Pool Public Acceptance\n'
    printf 'status=%s\n' "$status"
    printf 'public_api_url=%s\n' "${PUBLIC_API_URL:-missing}"
    printf 'public_stratum_addr=%s\n' "${PUBLIC_STRATUM_ADDR:-missing}"
    printf 'receipt_archive=%s\n' "${RECEIPT_ARCHIVE:-missing}"
    printf 'receipt_archive_sha256=%s\n' "$receipt_archive_sha"
    printf 'public_status_release_name=%s\n' "$release_name"
    printf 'public_status_release_revision=%s\n' "$release_revision"
    printf 'public_status_release_built_at=%s\n' "$release_built_at"
    printf 'public_status_release_version=%s\n' "$release_version"
    printf 'pass=%s\n' "$PASS"
    printf 'fail=%s\n' "$FAIL"
    printf 'skip=%s\n' "$SKIP"
  } >"$REPORT_DIR/PUBLIC-ACCEPTANCE-REPORT.txt"
}

package_evidence() {
  local archive_path archive_tmp archive_sha
  archive_path="$REPORT_DIR/public-acceptance-evidence.tar.gz"
  archive_tmp="$(dirname "$REPORT_DIR")/.$(basename "$REPORT_DIR").evidence.tmp.tar.gz"
  archive_sha="$archive_path.sha256"
  rm -f "$archive_path" "$archive_sha" "$archive_tmp"
  write_summary
  (
    cd "$REPORT_DIR"
    find . -type f | sort | while read -r file; do
      if [[ "$file" == "./PUBLIC-ACCEPTANCE-SHA256SUMS" ]]; then
        continue
      fi
      if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$file"
      else
        shasum -a 256 "$file"
      fi
    done >PUBLIC-ACCEPTANCE-SHA256SUMS
  )
  (
    cd "$(dirname "$REPORT_DIR")"
    tar -czf "$archive_tmp" "$(basename "$REPORT_DIR")"
  )
  mv "$archive_tmp" "$archive_path"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$archive_path" >"$archive_sha"
  else
    shasum -a 256 "$archive_path" >"$archive_sha"
  fi
}

validate_json_file() {
  local path="$1"
  local label="$2"
  if python3 -m json.tool "$path" >/dev/null 2>&1; then
    ok "$label JSON valid"
  else
    fail "$label JSON invalid; see $path"
  fi
}

curl_json() {
  local path="$1"
  local output="$2"
  local label="$3"
  local url
  url="$(normalize_url "$PUBLIC_API_URL")$path"
  if curl -fsS --max-time "$TIMEOUT_SECS" "$url" -o "$output"; then
    ok "$label fetched"
    validate_json_file "$output" "$label"
  else
    fail "$label fetch failed: $url"
  fi
}

check_receipt_binding() {
  local summary_path="$1"
  if python3 - "$summary_path" "$PUBLIC_API_URL" "$PUBLIC_STRATUM_ADDR" >"$REPORT_DIR/receipt-binding.log" 2>&1 <<'PY'
import json
import sys

summary_path, public_api_url, public_stratum_addr = sys.argv[1:4]
with open(summary_path, "r", encoding="utf-8") as handle:
    summary = json.load(handle)
endpoints = summary.get("endpoints", {})
expected_api = endpoints.get("public_api_url")
expected_stratum = endpoints.get("public_stratum_probe_addr") or endpoints.get("public_stratum_addr")
checks = {
    "receipt_public_api_matches": bool(expected_api) and expected_api.rstrip("/") == public_api_url.rstrip("/"),
    "receipt_public_stratum_matches": bool(expected_stratum) and expected_stratum == public_stratum_addr,
}
for key, value in checks.items():
    print(f"{key}={value}")
print(f"receipt_public_api_url={expected_api or 'missing'}")
print(f"acceptance_public_api_url={public_api_url or 'missing'}")
print(f"receipt_public_stratum_addr={expected_stratum or 'missing'}")
print(f"acceptance_public_stratum_addr={public_stratum_addr or 'missing'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "receipt endpoint binding matches acceptance target"
  else
    fail "receipt endpoint binding mismatch; see $REPORT_DIR/receipt-binding.log"
  fi
}

check_getting_started_binding() {
  if python3 - "$REPORT_DIR/http-public-getting-started.json" "$PUBLIC_STRATUM_ADDR" >"$REPORT_DIR/getting-started-binding.log" 2>&1 <<'PY'
import json
import sys

path, expected = sys.argv[1:3]
with open(path, "r", encoding="utf-8") as handle:
    data = json.load(handle)
commands = data.get("commands") or []
command_values = [item.get("command", "") for item in commands if isinstance(item, dict)]
checks = {
    "stratum_endpoint_matches": data.get("stratum_endpoint") == expected,
    "commands_include_stratum_endpoint": bool(command_values) and all(expected in command for command in command_values),
}
for key, value in checks.items():
    print(f"{key}={value}")
print(f"expected_endpoint={expected}")
print(f"actual_endpoint={data.get('stratum_endpoint', 'missing')}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "getting-started endpoint binds public Stratum address"
  else
    fail "getting-started endpoint binding failed; see $REPORT_DIR/getting-started-binding.log"
  fi
}

check_public_status_release_binding() {
  local receipt_summary_path="$1"
  if [[ -z "$receipt_summary_path" || ! -f "$receipt_summary_path" ]]; then
    fail "public status release binding skipped; receipt go-live summary missing"
    printf 'receipt_summary_present=False\npublic_status_release_binding_ok=False\n' >"$REPORT_DIR/public-status-release-binding.log"
    return
  fi
  if [[ ! -f "$REPORT_DIR/http-public-status.json" ]]; then
    fail "public status release binding skipped; http-public-status.json missing"
    printf 'public_status_present=False\npublic_status_release_binding_ok=False\n' >"$REPORT_DIR/public-status-release-binding.log"
    return
  fi
  if python3 - "$receipt_summary_path" "$REPORT_DIR/http-public-status.json" >"$REPORT_DIR/public-status-release-binding.log" 2>&1 <<'PY'
import json
import sys

summary_path, status_path = sys.argv[1:3]
with open(summary_path, "r", encoding="utf-8") as handle:
    summary = json.load(handle)
with open(status_path, "r", encoding="utf-8") as handle:
    status = json.load(handle)
expected = summary.get("release") or {}
actual = status.get("release") or {}
keys = ["name", "revision", "built_at"]
checks = {f"release_{key}_matches": bool(expected.get(key)) and actual.get(key) == expected.get(key) for key in keys}
checks["release_version_present"] = bool(actual.get("version"))
checks["public_status_release_binding_ok"] = all(checks.values())
for key in keys:
    print(f"{key}: expected={expected.get(key) or 'missing'} actual={actual.get(key) or 'missing'}")
print(f"version: actual={actual.get('version') or 'missing'}")
for key, value in checks.items():
    print(f"{key}={value}")
if not checks["public_status_release_binding_ok"]:
    sys.exit(1)
PY
  then
    ok "public /api/status release identity matches receipt"
  else
    fail "public /api/status release identity mismatch; see $REPORT_DIR/public-status-release-binding.log"
  fi
}

check_public_endpoint_routability() {
  if python3 - "$PUBLIC_API_URL" "$PUBLIC_STRATUM_ADDR" >"$REPORT_DIR/public-endpoint-routability.log" 2>&1 <<'PY'
import ipaddress
import socket
import sys
from urllib.parse import urlparse

public_api_url, public_stratum_addr = sys.argv[1:3]


def api_host(value):
    parsed = urlparse(value or "")
    return parsed.hostname or ""


def stratum_host(value):
    raw = value or ""
    if ":" not in raw:
        return ""
    host, _port = raw.rsplit(":", 1)
    return host.strip("[]")


def resolve(host):
    if not host:
        return [], "missing host"
    try:
        return [str(ipaddress.ip_address(host))], ""
    except ValueError:
        pass
    try:
        infos = socket.getaddrinfo(host, None)
    except OSError as exc:
        return [], str(exc)
    addresses = sorted({item[4][0] for item in infos})
    return addresses, ""


def all_global(addresses):
    if not addresses:
        return False
    parsed = []
    for value in addresses:
        try:
            parsed.append(ipaddress.ip_address(value))
        except ValueError:
            return False
    return all(item.is_global for item in parsed)


api = api_host(public_api_url)
stratum = stratum_host(public_stratum_addr)
api_addresses, api_error = resolve(api)
stratum_addresses, stratum_error = resolve(stratum)
checks = {
    "public_api_host_present": bool(api),
    "public_api_dns_resolves": bool(api_addresses),
    "public_api_dns_all_global": all_global(api_addresses),
    "public_stratum_host_present": bool(stratum),
    "public_stratum_dns_resolves": bool(stratum_addresses),
    "public_stratum_dns_all_global": all_global(stratum_addresses),
}
checks["public_endpoint_routability_ok"] = all(checks.values())
print(f"public_api_url={public_api_url or 'missing'}")
print(f"public_api_host={api or 'missing'}")
print(f"public_api_addresses={','.join(api_addresses) if api_addresses else 'missing'}")
print(f"public_api_dns_error={api_error or 'none'}")
print(f"public_stratum_addr={public_stratum_addr or 'missing'}")
print(f"public_stratum_host={stratum or 'missing'}")
print(f"public_stratum_addresses={','.join(stratum_addresses) if stratum_addresses else 'missing'}")
print(f"public_stratum_dns_error={stratum_error or 'none'}")
for key, value in checks.items():
    print(f"{key}={value}")
if not checks["public_endpoint_routability_ok"]:
    print("public API and Stratum hosts must resolve to global public addresses")
    sys.exit(1)
PY
  then
    ok "public endpoint routability passed"
  else
    fail "public endpoint routability failed; see $REPORT_DIR/public-endpoint-routability.log"
  fi
}

first_smoke_success_worker() {
  python3 - "$REPORT_DIR/public-stratum-smoke.json" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    data = json.load(handle)
for item in data.get("successes") or []:
    worker = item.get("worker")
    if isinstance(worker, str) and worker:
        print(worker)
        sys.exit(0)
sys.exit(1)
PY
}

check_public_canary_miner() {
  local canary_address status
  if [[ -z "$PUBLIC_API_URL" ]]; then
    skip "public canary miner API check skipped; set CSD_POOL_ACCEPTANCE_PUBLIC_API_URL"
    printf '{"skipped":true,"reason":"set CSD_POOL_ACCEPTANCE_PUBLIC_API_URL"}\n' >"$REPORT_DIR/public-canary-miner.json"
    return
  fi
  if [[ ! -f "$REPORT_DIR/public-stratum-smoke.json" ]]; then
    fail "public canary miner check missing public-stratum-smoke.json"
    printf '{"status":"failed","reason":"missing public-stratum-smoke.json"}\n' >"$REPORT_DIR/public-canary-miner.json"
    return
  fi
  if [[ -n "$CANARY_ADDRESS_OVERRIDE" ]]; then
    canary_address="$CANARY_ADDRESS_OVERRIDE"
    canary_source="configured"
  else
    canary_address="$(first_smoke_success_worker || true)"
    canary_source="smoke-success-worker"
  fi
  if [[ "$REQUIRE_ACCEPTED_SHARE" == "1" && "$canary_source" != "configured" ]]; then
    fail "accepted-share evidence requires CSD_POOL_ACCEPTANCE_CANARY_ADDRESS to name a real miner"
    printf '{"status":"failed","reason":"accepted-share evidence requires configured canary address"}\n' >"$REPORT_DIR/public-canary-miner.json"
    return
  fi
  if [[ -z "$canary_address" ]]; then
    fail "public canary miner check could not find a successful smoke worker"
    printf '{"status":"failed","reason":"no successful smoke worker"}\n' >"$REPORT_DIR/public-canary-miner.json"
    return
  fi

  curl_json "/api/miner/$canary_address" "$REPORT_DIR/http-public-canary-miner.json" "public canary miner profile"
  curl_json "/api/miner/$canary_address/workers" "$REPORT_DIR/http-public-canary-miner-workers.json" "public canary miner workers"

  if python3 - "$canary_address" "$canary_source" "$REQUIRE_ACCEPTED_SHARE" "$MIN_ACCEPTED_SHARES" "$CANARY_MAX_AGE_SECONDS" "$REPORT_DIR/http-public-canary-miner.json" "$REPORT_DIR/http-public-canary-miner-workers.json" >"$REPORT_DIR/public-canary-miner.json" <<'PY'
import json
import sys
import time

address, canary_source, require_accepted, min_accepted_raw, max_age_raw, miner_path, workers_path = sys.argv[1:8]
min_accepted = int(min_accepted_raw)
max_age_seconds = int(max_age_raw)
now_ts = int(time.time())
with open(miner_path, "r", encoding="utf-8") as handle:
    miner = json.load(handle)
with open(workers_path, "r", encoding="utf-8") as handle:
    workers = json.load(handle)
worker_rows = workers.get("workers") if isinstance(workers, dict) else []
shares_accepted = int(miner.get("shares_accepted") or 0)
try:
    last_seen_ts = int(miner.get("last_seen_ts") or 0)
except (TypeError, ValueError):
    last_seen_ts = 0
last_seen_age_seconds = now_ts - last_seen_ts if last_seen_ts > 0 else None
checks = {
    "miner_address_matches": miner.get("address") == address,
    "miner_online": miner.get("online") is True,
    "workers_online_positive": int(miner.get("workers_online") or 0) >= 1,
    "worker_rows_present": isinstance(worker_rows, list) and len(worker_rows) >= 1,
    "last_seen_ts_present": last_seen_ts > 0,
    "last_seen_not_from_future": last_seen_ts > 0 and last_seen_age_seconds >= -30,
    "last_seen_within_max_age": last_seen_ts > 0 and last_seen_age_seconds <= max_age_seconds,
    "accepted_share_minimum_met": shares_accepted >= min_accepted,
}
required_checks = dict(checks)
if require_accepted != "1":
    required_checks.pop("accepted_share_minimum_met", None)
status = "passed" if all(required_checks.values()) else "failed"
print(json.dumps({
    "status": status,
    "target": "public-canary-miner",
    "canary_address": address,
    "canary_source": canary_source,
    "accepted_share_required": require_accepted == "1",
    "accepted_share_minimum": min_accepted,
    "canary_max_age_seconds": max_age_seconds,
    "checks": checks,
    "miner": {
        "online": miner.get("online"),
        "workers_online": miner.get("workers_online"),
        "shares_accepted": miner.get("shares_accepted"),
        "last_seen_ts": miner.get("last_seen_ts"),
        "last_seen_age_seconds": last_seen_age_seconds,
        "observed_at_ts": now_ts,
    },
    "worker_count": len(worker_rows) if isinstance(worker_rows, list) else 0,
}, indent=2, sort_keys=True))
if status != "passed":
    sys.exit(1)
PY
  then
    ok "public canary miner is visible through public API"
    status="passed"
  else
    fail "public canary miner is not visible through public API; see $REPORT_DIR/public-canary-miner.json"
    status="failed"
  fi
  [[ "$status" == "passed" ]] || return 0
}

run_workers_command() {
  if [[ -n "${CSD_POOL_CONFIG:-}" ]]; then
    CSD_POOL_CONFIG="$CSD_POOL_CONFIG" "$WORKERS_BIN" "$@"
  else
    "$WORKERS_BIN" "$@"
  fi
}

mkdir -p "$REPORT_DIR"
REPORT_DIR="$(cd "$REPORT_DIR" && pwd)"
rm -f "$REPORT_DIR"/*.json "$REPORT_DIR"/*.log "$REPORT_DIR"/*.txt "$REPORT_DIR"/*.tar.gz "$REPORT_DIR"/*.sha256 2>/dev/null || true
write_acceptance_toolchain_manifest "$REPORT_DIR/acceptance-toolchain-manifest.json"

printf 'CSD Pool public acceptance\n'
printf 'report_dir=%s\n' "$REPORT_DIR"
printf 'public_api_url=%s\n' "${PUBLIC_API_URL:-missing}"
printf 'public_stratum_addr=%s\n' "${PUBLIC_STRATUM_ADDR:-missing}"
printf 'canary_address=%s\n' "${CANARY_ADDRESS_OVERRIDE:-smoke-success-worker}"
printf 'require_accepted_share=%s\n' "$REQUIRE_ACCEPTED_SHARE"
printf 'canary_max_age_seconds=%s\n' "$CANARY_MAX_AGE_SECONDS"

if [[ -z "$PUBLIC_API_URL" || "$PUBLIC_API_URL" != https://* ]]; then
  fail "CSD_POOL_ACCEPTANCE_PUBLIC_API_URL must be an HTTPS public URL"
else
  ok "HTTPS public API URL configured"
fi

if [[ -z "$PUBLIC_STRATUM_ADDR" || "$PUBLIC_STRATUM_ADDR" != *:* ]]; then
  fail "CSD_POOL_ACCEPTANCE_PUBLIC_STRATUM_ADDR must be host:port"
else
  ok "public Stratum address configured"
fi

if [[ "$MIN_ACCEPTED_SHARES" =~ ^[0-9]+$ && "$MIN_ACCEPTED_SHARES" -ge 1 ]]; then
  ok "accepted share minimum configured"
else
  fail "CSD_POOL_ACCEPTANCE_MIN_ACCEPTED_SHARES must be a positive integer"
fi

if [[ "$REQUIRE_ACCEPTED_SHARE" == "1" && -z "$CANARY_ADDRESS_OVERRIDE" ]]; then
  fail "CSD_POOL_ACCEPTANCE_CANARY_ADDRESS is required when CSD_POOL_ACCEPTANCE_REQUIRE_ACCEPTED_SHARE=1"
fi

if [[ "$CANARY_MAX_AGE_SECONDS" =~ ^[0-9]+$ && "$CANARY_MAX_AGE_SECONDS" -ge 60 ]]; then
  ok "canary max age configured"
else
  fail "CSD_POOL_ACCEPTANCE_CANARY_MAX_AGE_SECONDS must be an integer >= 60"
fi

if [[ -n "$RECEIPT_ARCHIVE" ]]; then
  if [[ -x "$VERIFY_RECEIPT_SCRIPT" ]]; then
    ok "receipt verifier executable"
    if "$VERIFY_RECEIPT_SCRIPT" "$RECEIPT_ARCHIVE" >"$REPORT_DIR/receipt-verify.log" 2>&1; then
      ok "real go-live receipt verified"
      TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-public-acceptance.XXXXXX")"
      tar -xzf "$RECEIPT_ARCHIVE" -C "$TMP_DIR"
      summary_path="$(find "$TMP_DIR" -type f -name go-live-summary.json | head -n1)"
      if [[ -n "$summary_path" && -f "$summary_path" ]]; then
        ok "receipt go-live summary extracted"
        check_receipt_binding "$summary_path"
      else
        fail "receipt go-live summary missing after extraction"
      fi
    else
      fail "real go-live receipt verification failed; see $REPORT_DIR/receipt-verify.log"
    fi
  else
    fail "receipt verifier not executable: $VERIFY_RECEIPT_SCRIPT"
  fi
else
  skip "receipt verification skipped; set CSD_POOL_ACCEPTANCE_RECEIPT or pass receipt archive"
fi

if [[ -n "$PUBLIC_API_URL" ]]; then
  check_public_endpoint_routability
  curl_json "/health" "$REPORT_DIR/http-public-health.json" "public /health"
  curl_json "/api/status" "$REPORT_DIR/http-public-status.json" "public /api/status"
  if [[ -n "${summary_path:-}" ]]; then
    check_public_status_release_binding "$summary_path"
  else
    fail "public /api/status release binding requires a verified receipt"
    printf 'receipt_summary_present=False\npublic_status_release_binding_ok=False\n' >"$REPORT_DIR/public-status-release-binding.log"
  fi
  curl_json "/api/pool" "$REPORT_DIR/http-public-pool.json" "public /api/pool"
  curl_json "/api/getting-started" "$REPORT_DIR/http-public-getting-started.json" "public /api/getting-started"
  if [[ -f "$REPORT_DIR/http-public-getting-started.json" ]]; then
    check_getting_started_binding
  fi
fi

if [[ -x "$WORKERS_BIN" && -n "$PUBLIC_STRATUM_ADDR" ]]; then
  if run_workers_command stratum-smoke "$PUBLIC_STRATUM_ADDR" >"$REPORT_DIR/public-stratum-smoke.json" 2>&1; then
    ok "public Stratum smoke passed"
    validate_json_file "$REPORT_DIR/public-stratum-smoke.json" "public Stratum smoke"
    check_public_canary_miner
  else
    fail "public Stratum smoke failed; see $REPORT_DIR/public-stratum-smoke.json"
    printf '{"status":"failed","reason":"public Stratum smoke failed"}\n' >"$REPORT_DIR/public-canary-miner.json"
  fi
  if run_workers_command stratum-submit-probe "$PUBLIC_STRATUM_ADDR" >"$REPORT_DIR/public-stratum-submit-probe.json" 2>&1; then
    ok "public Stratum submit probe passed"
    validate_json_file "$REPORT_DIR/public-stratum-submit-probe.json" "public Stratum submit probe"
  else
    fail "public Stratum submit probe failed; see $REPORT_DIR/public-stratum-submit-probe.json"
  fi
  if [[ "$RUN_LOAD" == "1" ]]; then
    if run_workers_command stratum-load-test "$PUBLIC_STRATUM_ADDR" >"$REPORT_DIR/public-stratum-load.json" 2>&1; then
      ok "public Stratum load passed"
      validate_json_file "$REPORT_DIR/public-stratum-load.json" "public Stratum load"
    else
      fail "public Stratum load failed; see $REPORT_DIR/public-stratum-load.json"
    fi
  else
    skip "public Stratum load skipped; set CSD_POOL_ACCEPTANCE_LOAD=1"
    printf '{"skipped":true,"reason":"set CSD_POOL_ACCEPTANCE_LOAD=1 to run public Stratum load"}\n' >"$REPORT_DIR/public-stratum-load.json"
  fi
else
  fail "workers binary missing or public Stratum address missing"
fi

package_evidence
printf 'acceptance_report=%s\n' "$REPORT_DIR/PUBLIC-ACCEPTANCE-REPORT.txt"
printf 'acceptance_summary=%s\n' "$REPORT_DIR/public-acceptance-summary.json"
printf 'acceptance_evidence=%s\n' "$REPORT_DIR/public-acceptance-evidence.tar.gz"
printf 'summary: pass=%s fail=%s skip=%s\n' "$PASS" "$FAIL" "$SKIP"

if [[ "$FAIL" -ne 0 ]]; then
  exit 1
fi

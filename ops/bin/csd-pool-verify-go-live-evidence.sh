#!/usr/bin/env bash
set -euo pipefail

ARCHIVE_PATH="${1:-${CSD_POOL_GO_LIVE_EVIDENCE_ARCHIVE:-}}"
ALLOW_DRY_RUN="${CSD_POOL_EVIDENCE_ALLOW_DRY_RUN:-0}"
KEEP_DIR="${CSD_POOL_EVIDENCE_KEEP_DIR:-0}"
TMP_DIR="${CSD_POOL_EVIDENCE_TMP_DIR:-}"
MAX_AGE_HOURS="${CSD_POOL_EVIDENCE_MAX_AGE_HOURS:-48}"
MAX_CLOCK_SKEW_SECONDS="${CSD_POOL_EVIDENCE_MAX_CLOCK_SKEW_SECONDS:-300}"
MAX_STATUS_SAMPLE_AGE_MINUTES="${CSD_POOL_STATUS_SAMPLE_MAX_AGE_MINUTES:-15}"
OWN_TMP_DIR=0

PASS=0
FAIL=0
SKIP=0
WORK_DIR=""

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
  if [[ -n "$WORK_DIR" && "$KEEP_DIR" != "1" ]]; then
    rm -rf "$WORK_DIR"
  fi
  if [[ -n "$TMP_DIR" && "$OWN_TMP_DIR" == "1" && "$KEEP_DIR" != "1" ]]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT

if [[ -z "$TMP_DIR" ]]; then
  TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-evidence-verify.XXXXXX")"
  OWN_TMP_DIR=1
else
  mkdir -p "$TMP_DIR"
fi

usage() {
  printf 'usage: %s /path/to/go-live-evidence.tar.gz\n' "$(basename "$0")" >&2
}

sha256_check() {
  local checksum_path="$1"
  local checksum_dir
  checksum_dir="$(cd "$(dirname "$checksum_path")" && pwd)"
  if command -v sha256sum >/dev/null 2>&1; then
    (cd "$checksum_dir" && sha256sum -c "$(basename "$checksum_path")")
  elif command -v shasum >/dev/null 2>&1; then
    (cd "$checksum_dir" && shasum -a 256 -c "$(basename "$checksum_path")")
  else
    printf 'sha256 tool missing\n' >&2
    return 127
  fi
}

require_file() {
  local path="$1"
  local label="$2"
  if [[ -f "$path" ]]; then
    ok "$label exists"
  else
    fail "$label missing: $path"
  fi
}

require_text() {
  local path="$1"
  local pattern="$2"
  local label="$3"
  if grep -Fq "$pattern" "$path"; then
    ok "$label"
  else
    fail "$label"
  fi
}

json_query() {
  local path="$1"
  local expr="$2"
  python3 - "$path" "$expr" <<'PY'
import json
import sys

path, expr = sys.argv[1], sys.argv[2]
with open(path, "r", encoding="utf-8") as f:
    value = json.load(f)
for part in expr.split("."):
    if isinstance(value, dict) and part in value:
        value = value[part]
    else:
        sys.exit(2)
if isinstance(value, bool):
    print("true" if value else "false")
else:
    print(value)
PY
}

check_json_value() {
  local path="$1"
  local expr="$2"
  local expected="$3"
  local label="$4"
  local actual
  if actual="$(json_query "$path" "$expr")" && [[ "$actual" == "$expected" ]]; then
    ok "$label"
  else
    fail "$label"
  fi
}

validate_json_file() {
  local path="$1"
  local label="$2"
  if python3 -m json.tool "$path" >"$TMP_DIR/csd-pool-evidence-json.pretty.json"; then
    ok "$label JSON validates"
  else
    fail "$label JSON validates"
  fi
}

check_json_key_present() {
  local path="$1"
  local key="$2"
  local label="$3"
  if python3 - "$path" "$key" <<'PY'
import json
import sys

path, key = sys.argv[1:3]
with open(path, "r", encoding="utf-8") as f:
    data = json.load(f)
if not isinstance(data, dict) or key not in data:
    sys.exit(1)
PY
  then
    ok "$label"
  else
    fail "$label"
  fi
}

require_csv_header() {
  local path="$1"
  local expected="$2"
  local label="$3"
  local first_line
  first_line="$(sed -n '1p' "$path" | tr -d '\r')"
  if [[ "$first_line" == "$expected" ]]; then
    ok "$label CSV header"
  else
    fail "$label CSV header"
  fi
}

check_status_release_binding() {
  local summary_path="$1"
  local status_path="$2"
  local label="$3"
  if python3 - "$summary_path" "$status_path" > $TMP_DIR/csd-pool-evidence-release-binding.log 2>&1 <<'PY'
import json
import sys

summary_path, status_path = sys.argv[1:3]
with open(summary_path, "r", encoding="utf-8") as f:
    summary = json.load(f)
with open(status_path, "r", encoding="utf-8") as f:
    status = json.load(f)
expected = summary.get("release") or {}
actual = status.get("release") or {}
failed = False
for field in ("name", "revision", "timestamp_utc"):
    expected_value = expected.get(field)
    actual_value = actual.get(field)
    print(f"{field}: expected={expected_value} actual={actual_value}")
    if not expected_value or expected_value == "unknown" or actual_value != expected_value:
        failed = True
if not actual.get("version"):
    print("version: missing")
    failed = True
sys.exit(1 if failed else 0)
PY
  then
    ok "$label"
  else
    fail "$label; see $TMP_DIR/csd-pool-evidence-release-binding.log"
  fi
}

check_evidence_freshness() {
  local summary_path="$1"
  local manifest_path="$2"
  if python3 - "$summary_path" "$manifest_path" "$MAX_AGE_HOURS" "$MAX_CLOCK_SKEW_SECONDS" > "$TMP_DIR/csd-pool-evidence-freshness.log" 2>&1 <<'PY'
import datetime as dt
import json
import sys

summary_path, manifest_path, max_age_hours_raw, max_clock_skew_raw = sys.argv[1:5]

def parse_positive_int(raw, name):
    try:
        value = int(raw)
    except ValueError:
        raise SystemExit(f"{name} must be an integer")
    if value <= 0:
        raise SystemExit(f"{name} must be positive")
    return value

def parse_rfc3339(value, name):
    if not value or value == "unknown":
        raise ValueError(f"{name} missing")
    normalized = value[:-1] + "+00:00" if value.endswith("Z") else value
    parsed = dt.datetime.fromisoformat(normalized)
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=dt.timezone.utc)
    return parsed.astimezone(dt.timezone.utc)

def read_key_values(path):
    values = {}
    with open(path, "r", encoding="utf-8") as f:
        for raw in f:
            line = raw.rstrip("\n")
            if "=" in line and not line.startswith("  "):
                key, value = line.split("=", 1)
                values[key] = value
    return values

max_age_hours = parse_positive_int(max_age_hours_raw, "CSD_POOL_EVIDENCE_MAX_AGE_HOURS")
max_clock_skew_seconds = parse_positive_int(max_clock_skew_raw, "CSD_POOL_EVIDENCE_MAX_CLOCK_SKEW_SECONDS")
max_age_seconds = max_age_hours * 60 * 60

with open(summary_path, "r", encoding="utf-8") as f:
    summary = json.load(f)
manifest = read_key_values(manifest_path)

now = dt.datetime.now(dt.timezone.utc)
finished_at_raw = summary.get("finished_at_utc", "")
created_at_raw = manifest.get("created_at_utc", "")
try:
    finished_at = parse_rfc3339(finished_at_raw, "finished_at_utc")
    created_at = parse_rfc3339(created_at_raw, "created_at_utc")
    parse_ok = True
except Exception as exc:
    finished_at = created_at = None
    parse_ok = False
    print(f"freshness_parse_error={exc}")

if parse_ok:
    evidence_age_seconds = int((now - finished_at).total_seconds())
    manifest_to_summary_seconds = int((finished_at - created_at).total_seconds())
else:
    evidence_age_seconds = -1
    manifest_to_summary_seconds = -1

checks = {
    "evidence_finished_at_present": bool(finished_at_raw),
    "evidence_manifest_created_at_present": bool(created_at_raw),
    "evidence_timestamps_parse": parse_ok,
    "evidence_not_from_future": parse_ok and evidence_age_seconds >= -max_clock_skew_seconds,
    "evidence_within_max_age": parse_ok and evidence_age_seconds <= max_age_seconds,
    "evidence_manifest_not_after_summary": parse_ok and manifest_to_summary_seconds >= -max_clock_skew_seconds,
    "evidence_manifest_to_summary_within_max_age": parse_ok and manifest_to_summary_seconds <= max_age_seconds,
}

print(f"finished_at_utc={finished_at_raw or 'missing'}")
print(f"manifest_created_at_utc={created_at_raw or 'missing'}")
print(f"verified_at_utc={now.strftime('%Y-%m-%dT%H:%M:%SZ')}")
print(f"max_age_hours={max_age_hours}")
print(f"max_age_seconds={max_age_seconds}")
print(f"max_clock_skew_seconds={max_clock_skew_seconds}")
print(f"evidence_age_seconds={evidence_age_seconds}")
print(f"manifest_to_summary_seconds={manifest_to_summary_seconds}")
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "evidence freshness checks passed"
  else
    fail "evidence freshness failed; see $TMP_DIR/csd-pool-evidence-freshness.log"
  fi
}

check_real_env_readiness() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-real-env-readiness.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

checks = [
    ("database_url_scheme", values.get("database_url_scheme") in {"postgres", "postgresql"}),
    ("watch_node_url_present", values.get("watch_node_url_present") == "present"),
    ("submit_node_url_present", values.get("submit_node_url_present") == "present"),
    ("payout_node_url_present", values.get("payout_node_url_present") == "present"),
    ("signer_url_present", values.get("signer_url_present") == "present"),
    ("signer_wallet_address_present", values.get("signer_wallet_address_present") == "present"),
    ("signer_wallet_address_valid", values.get("signer_wallet_address_valid") == "true"),
    ("signer_wallet_address_not_example", values.get("signer_wallet_address_not_example") == "true"),
    ("restore_database_separate", values.get("restore_database_separate") == "true"),
]
checks.extend([
    ("operator_token_length", int(values.get("operator_token_length", "0")) >= 32),
    ("signer_token_length", int(values.get("signer_token_length", "0")) >= 32),
])
failed = False
for name, passed in checks:
    print(f"{name}: {'ok' if passed else 'failed'}")
    if not passed:
        failed = True
sys.exit(1 if failed else 0)
PY
  then
    ok "real environment readiness checks passed"
  else
    fail "real environment readiness checks failed; see $TMP_DIR/csd-pool-evidence-real-env-readiness.log"
  fi
}

check_secrets_permissions_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-secrets-permissions-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

checks = {
    "env_file_exists": values.get("env_file_exists") == "True",
    "env_owner_readable": values.get("env_owner_readable") == "True",
    "env_group_other_permissions_zero": values.get("env_group_other_permissions_zero") == "True",
    "env_restricted": values.get("env_restricted") == "True",
    "config_file_exists": values.get("config_file_exists") == "True",
    "config_owner_readable": values.get("config_owner_readable") == "True",
    "config_group_other_permissions_zero": values.get("config_group_other_permissions_zero") == "True",
    "config_restricted": values.get("config_restricted") == "True",
    "secrets_permissions_ok": values.get("secrets_permissions_ok") == "True",
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "secrets permissions safety checks passed"
  else
    fail "secrets permissions safety checks failed; see $TMP_DIR/csd-pool-evidence-secrets-permissions-safety.log"
  fi
}

check_evidence_redaction_safety() {
  local work_dir="$1"
  local report_path="$2"
  if python3 - "$work_dir" "$report_path" > $TMP_DIR/csd-pool-evidence-redaction-safety.log 2>&1 <<'PY'
import re
import sys
from pathlib import Path

work_dir = Path(sys.argv[1])
report_path = Path(sys.argv[2]).resolve()

rules = [
    ("authorization_bearer", re.compile(r"Authorization:\s*Bearer\s+(?!<redacted>|redacted\b)[A-Za-z0-9._~+/=-]{8,}", re.I)),
    ("sensitive_env_value", re.compile(r"\b[A-Z0-9_]*(?:TOKEN|SECRET|PASSWORD|PRIVATE_KEY)=[^\s`'\"]+", re.I)),
    ("postgres_password_url", re.compile(r"postgres(?:ql)?://[^:/@\s]+:(?!<redacted>@|redacted@)[^@\s]+@", re.I)),
]
allowed_values = {"present", "missing", "<redacted>", "redacted", "true", "false"}

def is_binary(path):
    try:
        return b"\0" in path.read_bytes()[:4096]
    except OSError:
        return True

def safe_sensitive_assignment(match_text):
    value = match_text.split("=", 1)[1].strip().strip("'\"`").replace("\\", "")
    return value.lower() in allowed_values

report_values = {}
if report_path.exists():
    for line in report_path.read_text(encoding="utf-8", errors="replace").splitlines():
        if "=" in line:
            key, value = line.split("=", 1)
            report_values[key] = value

findings = []
checked = 0
for path in sorted(work_dir.rglob("*")):
    if not path.is_file():
        continue
    if path.suffix in {".gz"} or path.name.endswith(".sha256"):
        continue
    if is_binary(path):
        continue
    checked += 1
    text = path.read_text(encoding="utf-8", errors="replace")
    for name, pattern in rules:
        for match in pattern.finditer(text):
            matched = match.group(0)
            if name == "sensitive_env_value" and safe_sensitive_assignment(matched):
                continue
            line_no = text.count("\n", 0, match.start()) + 1
            findings.append((path.name, name, line_no))

report_ok = report_values.get("evidence_redaction_ok") == "True"
report_zero = report_values.get("evidence_redaction_findings") == "0"
print(f"report_evidence_redaction_ok={report_ok}")
print(f"report_evidence_redaction_findings_zero={report_zero}")
print(f"archive_redaction_checked_files={checked}")
print(f"archive_redaction_findings={len(findings)}")
for name, rule, line_no in findings[:100]:
    print(f"finding={name}:{rule}:line={line_no}")
if not (report_ok and report_zero and len(findings) == 0):
    sys.exit(1)
PY
  then
    ok "evidence redaction safety checks passed"
  else
    fail "evidence redaction safety checks failed; see $TMP_DIR/csd-pool-evidence-redaction-safety.log"
  fi
}

check_clock_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-clock-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

checks = {
    "timedatectl_available": values.get("timedatectl_available") == "True",
    "clock_synchronized": values.get("clock_synchronized") == "True",
    "clock_reported_utc_present": values.get("clock_reported_utc_present") == "True" and bool(values.get("checked_at_utc")),
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "clock safety checks passed"
  else
    fail "clock safety checks failed; see $TMP_DIR/csd-pool-evidence-clock-safety.log"
  fi
}

check_disk_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-disk-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

def int_at_least(name, minimum):
    try:
        return int(values.get(name, "0")) >= minimum
    except ValueError:
        return False

checks = {
    "disk_all_paths_ok": values.get("disk_all_paths_ok") == "True",
    "disk_path_count_positive": int_at_least("disk_path_count", 1),
    "disk_min_free_bytes_present": int_at_least("disk_min_free_bytes", 1),
    "disk_min_free_inodes_present": int_at_least("disk_min_free_inodes", 1),
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "disk safety checks passed"
  else
    fail "disk safety checks failed; see $TMP_DIR/csd-pool-evidence-disk-safety.log"
  fi
}

check_runtime_status_binding() {
  local status_path="$1"
  local summary_path="$2"
  if python3 - "$status_path" "$summary_path" "$MAX_STATUS_SAMPLE_AGE_MINUTES" "$MAX_CLOCK_SKEW_SECONDS" > $TMP_DIR/csd-pool-evidence-runtime-status-binding.log 2>&1 <<'PY'
import datetime as dt
import json
import sys

path, summary_path, max_age_minutes_raw, max_clock_skew_raw = sys.argv[1:5]

def parse_positive_int(raw, name):
    try:
        value = int(raw)
    except ValueError:
        raise SystemExit(f"{name} must be an integer")
    if value <= 0:
        raise SystemExit(f"{name} must be positive")
    return value

def parse_time(value, name):
    if not value:
        raise ValueError(f"{name} missing")
    normalized = value.strip()
    if normalized.endswith("Z"):
        normalized = normalized[:-1] + "+00:00"
    parsed = dt.datetime.fromisoformat(normalized)
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=dt.timezone.utc)
    return parsed.astimezone(dt.timezone.utc)

max_age_minutes = parse_positive_int(max_age_minutes_raw, "CSD_POOL_STATUS_SAMPLE_MAX_AGE_MINUTES")
max_age_seconds = max_age_minutes * 60
max_clock_skew_seconds = parse_positive_int(max_clock_skew_raw, "CSD_POOL_EVIDENCE_MAX_CLOCK_SKEW_SECONDS")
with open(path, "r", encoding="utf-8") as f:
    status = json.load(f)
with open(summary_path, "r", encoding="utf-8") as f:
    summary = json.load(f)
latest_sample_at_raw = status.get("latest_sample_at") or ""
finished_at_raw = summary.get("finished_at_utc") or ""
try:
    latest_sample_at = parse_time(latest_sample_at_raw, "latest_sample_at")
    finished_at = parse_time(finished_at_raw, "finished_at_utc")
    timestamps_parse = True
    sample_age_seconds = int((finished_at - latest_sample_at).total_seconds())
except Exception as exc:
    print(f"runtime_sample_recency_parse_error={exc}")
    timestamps_parse = False
    sample_age_seconds = -1
checks = {
    "service": status.get("service") == "csd-pool",
    "status_operational": status.get("status") == "operational",
    "api_ok": status.get("api_ok") is True,
    "data_source_postgres": status.get("data_source") == "postgres",
    "node_count_positive": isinstance(status.get("node_count"), int) and status["node_count"] >= 1,
    "latest_sample_at_present": bool(latest_sample_at_raw),
    "runtime_sample_timestamps_parse": timestamps_parse,
    "runtime_sample_not_from_future": timestamps_parse and sample_age_seconds >= -max_clock_skew_seconds,
    "runtime_sample_within_max_age": timestamps_parse and sample_age_seconds <= max_age_seconds,
}
print(f"latest_sample_at={latest_sample_at_raw or 'missing'}")
print(f"finished_at_utc={finished_at_raw or 'missing'}")
print(f"runtime_sample_age_seconds={sample_age_seconds}")
print(f"runtime_sample_max_age_minutes={max_age_minutes}")
print(f"runtime_sample_max_age_seconds={max_age_seconds}")
failed = False
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
    if not passed:
        failed = True
sys.exit(1 if failed else 0)
PY
  then
    ok "runtime status proves PostgreSQL-backed operational service"
  else
    fail "runtime status binding failed; see $TMP_DIR/csd-pool-evidence-runtime-status-binding.log"
  fi
}

check_bind_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-bind-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

checks = {
    "bind_safety_ok": values.get("bind_safety_ok") == "True",
    "stratum_listen_loopback": values.get("stratum_listen_loopback") == "True",
    "api_listen_loopback": values.get("api_listen_loopback") == "True",
    "signer_listen_loopback": values.get("signer_listen_loopback") == "True",
    "public_api_configured_when_required": values.get("public_api_configured_when_required") == "True",
    "public_stratum_configured_when_required": values.get("public_stratum_configured_when_required") == "True",
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "bind safety checks passed"
  else
    fail "bind safety checks failed; see $TMP_DIR/csd-pool-evidence-bind-safety.log"
  fi
}

check_runtime_config_binding() {
  local config_path="$1"
  local status_path="$2"
  if python3 - "$config_path" "$status_path" > $TMP_DIR/csd-pool-evidence-runtime-config-binding.log 2>&1 <<'PY'
import json
import sys

config_path, status_path = sys.argv[1:3]
with open(config_path, "r", encoding="utf-8") as f:
    config = json.load(f)
with open(status_path, "r", encoding="utf-8") as f:
    status = json.load(f)
runtime = status.get("config") or {}

def int_or_none(value):
    if value is None:
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None

checks = {
    "runtime_config_present": bool(runtime),
    "pool_id_matches": runtime.get("pool_id") == config.get("pool_id"),
    "mining_address_matches": runtime.get("mining_address") == config.get("mining_address"),
    "fee_percent_matches": runtime.get("fee_percent") == config.get("fee_percent"),
    "confirm_depth_matches": runtime.get("confirm_depth") == config.get("confirm_depth"),
    "stratum_listen_matches": runtime.get("stratum_listen") == config.get("stratum_listen"),
    "api_listen_matches": runtime.get("api_listen") == config.get("api_listen"),
    "signer_listen_matches": runtime.get("signer_listen") == config.get("signer_listen"),
    "minimum_payout_matches": int_or_none(runtime.get("minimum_payout_base_units")) == config.get("minimum_payout_base_units"),
    "manual_payout_approval_matches": int_or_none(runtime.get("manual_payout_approval_base_units")) == config.get("manual_payout_approval_base_units"),
    "max_payout_batch_matches": int_or_none(runtime.get("max_payout_batch_base_units")) == config.get("max_payout_batch_base_units"),
    "max_daily_payout_matches": int_or_none(runtime.get("max_daily_payout_base_units")) == config.get("max_daily_payout_base_units"),
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "runtime config binding checks passed"
  else
    fail "runtime config binding failed; see $TMP_DIR/csd-pool-evidence-runtime-config-binding.log"
  fi
}

check_pool_endpoint_binding() {
  local pool_path="$1"
  local status_path="$2"
  local label="$3"
  if python3 - "$pool_path" "$status_path" "$label" > $TMP_DIR/csd-pool-evidence-pool-endpoint-binding.log 2>&1 <<'PY'
import json
import sys

pool_path, status_path, label = sys.argv[1:4]
with open(pool_path, "r", encoding="utf-8") as f:
    pool = json.load(f)
with open(status_path, "r", encoding="utf-8") as f:
    status = json.load(f)
config = status.get("config") or {}

def number_equal(left, right):
    try:
        return float(left) == float(right)
    except (TypeError, ValueError):
        return left == right

checks = {
    "pool_endpoint_json_object": isinstance(pool, dict),
    "pool_fee_matches_status_config": number_equal(pool.get("pool_fee_pct"), config.get("fee_percent")),
    "confirm_depth_matches_status_config": pool.get("confirm_depth") == config.get("confirm_depth"),
    "workers_online_matches_status": pool.get("workers_online") == status.get("workers_online"),
    "shares_accepted_matches_status": pool.get("shares_accepted") == status.get("shares_accepted"),
    "shares_rejected_matches_status": pool.get("shares_rejected") == status.get("shares_rejected"),
    "shares_stale_matches_status": pool.get("shares_stale") == status.get("shares_stale"),
    "updated_ts_present": isinstance(pool.get("updated_ts"), int) and pool.get("updated_ts", 0) > 0,
}
print(f"pool_binding_label={label}")
for name, passed in checks.items():
    print(f"{name}={'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "$label pool endpoint binding checks passed"
  else
    fail "$label pool endpoint binding failed; see $TMP_DIR/csd-pool-evidence-pool-endpoint-binding.log"
  fi
}

check_external_public_config_binding() {
  local config_path="$1"
  local status_path="$2"
  if python3 - "$config_path" "$status_path" > $TMP_DIR/csd-pool-evidence-external-public-config-binding.log 2>&1 <<'PY'
import json
import sys

config_path, status_path = sys.argv[1:3]
with open(config_path, "r", encoding="utf-8") as f:
    config = json.load(f)
with open(status_path, "r", encoding="utf-8") as f:
    status = json.load(f)
runtime = status.get("config") or {}

def int_or_none(value):
    if value is None:
        return None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None

checks = {
    "external_public_config_present": bool(runtime),
    "pool_id_matches": runtime.get("pool_id") == config.get("pool_id"),
    "mining_address_matches": runtime.get("mining_address") == config.get("mining_address"),
    "fee_percent_matches": runtime.get("fee_percent") == config.get("fee_percent"),
    "confirm_depth_matches": runtime.get("confirm_depth") == config.get("confirm_depth"),
    "stratum_listen_matches": runtime.get("stratum_listen") == config.get("stratum_listen"),
    "api_listen_matches": runtime.get("api_listen") == config.get("api_listen"),
    "signer_listen_matches": runtime.get("signer_listen") == config.get("signer_listen"),
    "minimum_payout_matches": int_or_none(runtime.get("minimum_payout_base_units")) == config.get("minimum_payout_base_units"),
    "manual_payout_approval_matches": int_or_none(runtime.get("manual_payout_approval_base_units")) == config.get("manual_payout_approval_base_units"),
    "max_payout_batch_matches": int_or_none(runtime.get("max_payout_batch_base_units")) == config.get("max_payout_batch_base_units"),
    "max_daily_payout_matches": int_or_none(runtime.get("max_daily_payout_base_units")) == config.get("max_daily_payout_base_units"),
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "external public config binding checks passed"
  else
    fail "external public config binding failed; see $TMP_DIR/csd-pool-evidence-external-public-config-binding.log"
  fi
}

check_getting_started_binding() {
  local json_path="$1"
  local expected_endpoint="$2"
  local tiers_raw="$3"
  local label="$4"
  local log_path="$TMP_DIR/csd-pool-evidence-${label// /-}-getting-started-binding.log"
  if python3 - "$json_path" "$expected_endpoint" "$tiers_raw" "$label" >"$log_path" 2>&1 <<'PY'
import json
import sys

json_path, expected_endpoint, tiers_raw, label = sys.argv[1:5]
with open(json_path, "r", encoding="utf-8") as f:
    data = json.load(f)

def default_port(endpoint):
    try:
        return int(endpoint.rsplit(":", 1)[1])
    except (IndexError, ValueError):
        return 3333

def parse_tiers(raw, endpoint):
    raw = (raw or "").strip()
    if not raw or raw == "missing":
        return [{"port": default_port(endpoint), "label": "standard", "enabled": True}]
    tiers = []
    for item in raw.split(","):
        parts = [part.strip() for part in item.split(":")]
        if not parts or not parts[0]:
            continue
        try:
            port = int(parts[0])
        except ValueError:
            continue
        label_value = parts[1] if len(parts) > 1 and parts[1] else "custom"
        flag = parts[3].lower() if len(parts) > 3 else ""
        tiers.append({"port": port, "label": label_value, "enabled": flag not in {"disabled", "off", "0", "false"}})
    return tiers

def normalize_tiers(tiers):
    normalized = []
    for tier in tiers or []:
        normalized.append({
            "port": int(tier.get("port", 0)),
            "label": str(tier.get("label", "")),
            "enabled": bool(tier.get("enabled", False)),
        })
    return normalized

expected_tiers = parse_tiers(tiers_raw, expected_endpoint)
actual_tiers = normalize_tiers(data.get("port_tiers"))
commands = data.get("commands") or []
command_values = [str(command.get("command", "")) for command in commands if isinstance(command, dict)]
checks = {
    "getting_started_json_present": bool(data),
    "stratum_endpoint_matches": bool(expected_endpoint) and expected_endpoint != "missing" and data.get("stratum_endpoint") == expected_endpoint,
    "commands_present": bool(command_values),
    "commands_include_stratum_endpoint": bool(command_values) and all(expected_endpoint in command for command in command_values),
    "port_tiers_match": actual_tiers == expected_tiers,
    "enabled_port_tier_present": any(tier.get("enabled") for tier in actual_tiers),
    "payout_rules_present": isinstance(data.get("payout"), dict) and bool(data.get("payout")),
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    print(f"{label} /api/getting-started must match go-live public Stratum settings")
    sys.exit(1)
PY
  then
    ok "$label getting-started binding checks passed"
  else
    fail "$label getting-started binding failed; see $log_path"
  fi
}

check_public_api_tls_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-public-api-tls-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

def int_at_least(name, minimum):
    try:
        return int(values.get(name, "-1")) >= minimum
    except ValueError:
        return False

checks = {
    "public_api_scheme_https": values.get("public_api_scheme_https") == "True",
    "public_api_host_present": values.get("public_api_host_present") == "True",
    "public_api_tls_handshake": values.get("public_api_tls_handshake") == "True",
    "public_api_tls_hostname_valid": values.get("public_api_tls_hostname_valid") == "True",
    "public_api_tls_not_after_present": values.get("public_api_tls_not_after_present") == "True",
    "public_api_tls_min_valid_days_present": int_at_least("public_api_tls_min_valid_days", 1),
    "cert_seconds_remaining_positive": int_at_least("cert_seconds_remaining", 1),
    "public_api_tls_not_expiring_soon": values.get("public_api_tls_not_expiring_soon") == "True",
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "public API TLS safety checks passed"
  else
    fail "public API TLS safety checks failed; see $TMP_DIR/csd-pool-evidence-public-api-tls-safety.log"
  fi
}

check_public_api_headers_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-public-api-headers-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

checks = {
    "content_security_policy_present": values.get("content_security_policy_present") == "True",
    "content_security_policy_frame_ancestors_none": values.get("content_security_policy_frame_ancestors_none") == "True",
    "x_content_type_options_nosniff": values.get("x_content_type_options_nosniff") == "True",
    "x_frame_options_deny": values.get("x_frame_options_deny") == "True",
    "referrer_policy_no_referrer": values.get("referrer_policy_no_referrer") == "True",
    "permissions_policy_present": values.get("permissions_policy_present") == "True",
    "public_api_headers_ok": values.get("public_api_headers_ok") == "True",
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "public API security headers checks passed"
  else
    fail "public API security headers checks failed; see $TMP_DIR/csd-pool-evidence-public-api-headers-safety.log"
  fi
}

check_public_dns_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-public-dns-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

checks = {
    "public_api_dns_host_present": values.get("public_api_dns_host_present") == "True",
    "public_api_dns_resolves": values.get("public_api_dns_resolves") == "True",
    "public_api_dns_all_global": values.get("public_api_dns_all_global") == "True",
    "public_stratum_dns_host_present": values.get("public_stratum_dns_host_present") == "True",
    "public_stratum_dns_resolves": values.get("public_stratum_dns_resolves") == "True",
    "public_stratum_dns_all_global": values.get("public_stratum_dns_all_global") == "True",
    "public_dns_all_global": values.get("public_dns_all_global") == "True",
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "public DNS safety checks passed"
  else
    fail "public DNS safety checks failed; see $TMP_DIR/csd-pool-evidence-public-dns-safety.log"
  fi
}

check_database_runtime_report() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-database-runtime.log 2>&1 <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as f:
    report = json.load(f)
table_counts = report.get("table_counts")
checks = {
    "passed": report.get("passed") is True,
    "failed_checks_zero": report.get("failed_checks") == 0,
    "database_url_present": report.get("database_url_present") is True,
    "database_name_present": bool(report.get("database_name")),
    "database_user_present": bool(report.get("database_user")),
    "server_version_present": bool(report.get("server_version")),
    "ping_ok": report.get("ping_ok") is True,
    "migrations_complete": report.get("migrations_complete") is True,
    "latest_database_matches_known": report.get("latest_database_matches_known") is True,
    "table_counts_present": isinstance(table_counts, list) and len(table_counts) >= 16,
    "transaction_write_ok": report.get("transaction_write_ok") is True,
    "transaction_rollback_ok": report.get("transaction_rollback_ok") is True,
    "query_latency_ok": report.get("query_latency_ok") is True,
    "transaction_latency_ok": report.get("transaction_latency_ok") is True,
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "database runtime checks passed"
  else
    fail "database runtime checks failed; see $TMP_DIR/csd-pool-evidence-database-runtime.log"
  fi
}

check_edge_proxy_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-edge-proxy-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

checks = {
    "edge_proxy_config_exists": values.get("edge_proxy_config_exists") == "True",
    "edge_proxy_haproxy_installed": values.get("edge_proxy_haproxy_installed") == "True",
    "edge_proxy_haproxy_config_valid": values.get("edge_proxy_haproxy_config_valid") == "True",
    "edge_proxy_stratum_public_bind_matches": values.get("edge_proxy_stratum_public_bind_matches") == "True",
    "edge_proxy_stratum_connection_cap_present": values.get("edge_proxy_stratum_connection_cap_present") == "True",
    "edge_proxy_stratum_backend_matches_config": values.get("edge_proxy_stratum_backend_matches_config") == "True",
    "edge_proxy_api_backend_matches_config": values.get("edge_proxy_api_backend_matches_config") == "True",
    "edge_proxy_api_health_check_present": values.get("edge_proxy_api_health_check_present") == "True",
    "edge_proxy_safety_ok": values.get("edge_proxy_safety_ok") == "True",
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "edge proxy safety checks passed"
  else
    fail "edge proxy safety checks failed; see $TMP_DIR/csd-pool-evidence-edge-proxy-safety.log"
  fi
}

check_signer_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-signer-safety.log 2>&1 <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as f:
    report = json.load(f)
health_mode = (report.get("health_mode") or "").strip().lower()
health_service = report.get("health_service") or ""
health_wallet_address = (report.get("health_wallet_address") or "").strip().lower()
expected_wallet_address = (report.get("expected_wallet_address") or "").strip().lower()
raw_tx_mock_prefix_present = report.get("raw_tx_mock_prefix_present") is True
node_tx = report.get("node_tx")
node_tx_present = report.get("node_tx_present") is True
node_tx_valid = report.get("node_tx_valid") is True
node_tx_outputs_match_request = report.get("node_tx_outputs_match_request") is True
blocked_modes = {"mock", "dev", "development", "test", "testing"}
def is_addr20(value):
    return len(value) == 40 and all(ch in "0123456789abcdef" for ch in value)
example_wallets = {"0123456789abcdef0123456789abcdef01234567"}
checks = {
    "passed": report.get("passed") is True,
    "health_ok": report.get("health_ok") is True,
    "health_service_present": bool(health_service),
    "health_mode_present": bool(health_mode),
    "health_wallet_address_present": bool(health_wallet_address),
    "health_wallet_address_valid": is_addr20(health_wallet_address),
    "expected_wallet_address_present": bool(expected_wallet_address),
    "expected_wallet_address_valid": is_addr20(expected_wallet_address),
    "expected_wallet_address_not_example": expected_wallet_address not in example_wallets,
    "signer_wallet_matches_expected": bool(health_wallet_address) and health_wallet_address == expected_wallet_address,
    "sign_ok": report.get("sign_ok") is True,
    "txid_present": bool(report.get("txid")),
    "signer_mode_allowed": bool(health_mode) and health_mode not in blocked_modes,
    "raw_tx_not_mock_prefix": not raw_tx_mock_prefix_present,
    "official_node_tx_present": node_tx_present and isinstance(node_tx, dict),
    "official_node_tx_valid": node_tx_valid,
    "official_node_tx_outputs_match_request": node_tx_outputs_match_request,
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "signer safety checks passed"
  else
    fail "signer safety checks failed; see $TMP_DIR/csd-pool-evidence-signer-safety.log"
  fi
}

check_node_endpoint_safety() {
  local config_path="$1"
  local node_path="$2"
  if python3 - "$config_path" "$node_path" > $TMP_DIR/csd-pool-evidence-node-endpoint-safety.log 2>&1 <<'PY'
import json
import sys
from urllib.parse import urlparse

config_path, node_path = sys.argv[1:3]
with open(config_path, "r", encoding="utf-8") as f:
    config = json.load(f)
with open(node_path, "r", encoding="utf-8") as f:
    node = json.load(f)

def host_from_url(url):
    return (urlparse(url).hostname or "").strip().lower()

def host_allowed(host):
    if not host:
        return False
    if host in {"localhost", "0.0.0.0", "::1"}:
        return False
    if host.startswith("127."):
        return False
    if "mock" in host or "example" in host:
        return False
    return True

nodes = config.get("nodes") or []
role_coverage = set()
config_urls_allowed = True
for item in nodes:
    if not host_allowed(host_from_url(item.get("rpc_url") or "")):
        config_urls_allowed = False
    for role in item.get("roles") or []:
        role_coverage.add(str(role))

submit_health = node.get("submit_health") or {}
checks = {
    "config_nodes_present": bool(nodes),
    "config_node_urls_allowed": config_urls_allowed,
    "config_roles_cover_template_submit_watch": {"template", "submit", "watch"}.issubset(role_coverage),
    "template_node_url_allowed": host_allowed(host_from_url(node.get("template_node_url") or "")),
    "submit_node_url_allowed": host_allowed(host_from_url(node.get("submit_node_url") or "")),
    "template_contract_passed": node.get("passed") is True and node.get("template_ok") is True,
    "adapter_auth_required": node.get("adapter_auth_required") is True,
    "adapter_auth_boundary_ok": node.get("adapter_auth_boundary_ok") is True,
    "unauthenticated_template_rejected": node.get("unauthenticated_template_status") == 401,
    "template_health_ok": node.get("health_ok") is True,
    "network_ok": node.get("network_ok") is True,
    "submit_health_ok": submit_health.get("ok") is True,
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "CSD node endpoint safety checks passed"
  else
    fail "CSD node endpoint safety checks failed; see $TMP_DIR/csd-pool-evidence-node-endpoint-safety.log"
  fi
}

check_node_runtime_report() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-node-runtime.log 2>&1 <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as f:
    report = json.load(f)

nodes = report.get("nodes") or []
checks = {
    "passed": report.get("passed") is True,
    "failed_checks_empty": not report.get("failed_checks"),
    "config_node_count_positive": int(report.get("config_node_count") or 0) > 0,
    "role_quorum_ok": report.get("role_quorum_ok") is True,
    "health_quorum_ok": report.get("health_quorum_ok") is True,
    "network_ok": report.get("network_ok") is True,
    "template_contract_ok": report.get("template_contract_ok") is True,
    "latency_ok": report.get("latency_ok") is True,
    "template_quorum_met": int(report.get("healthy_template_nodes") or 0) >= int(report.get("min_template_nodes") or 1),
    "submit_quorum_met": int(report.get("healthy_submit_nodes") or 0) >= int(report.get("min_submit_nodes") or 1),
    "watch_quorum_met": int(report.get("healthy_watch_nodes") or 0) >= int(report.get("min_watch_nodes") or 1),
    "all_nodes_health_ok": bool(nodes) and all(node.get("health_ok") is True for node in nodes),
    "all_nodes_network_ok": bool(nodes) and all(node.get("network_ok") is True for node in nodes),
    "template_nodes_have_jobs": any(
        "template" in str(node.get("role") or "").lower()
        and node.get("template_ok") is True
        and bool(node.get("job_id"))
        for node in nodes
    ),
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "CSD node runtime quorum checks passed"
  else
    fail "CSD node runtime quorum checks failed; see $TMP_DIR/csd-pool-evidence-node-runtime.log"
  fi
}

check_restore_api_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-restore-api-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as f:
    log = f.read()
checks = {
    "restore_complete": "restore drill complete" in log,
    "restore_health_ok": "ok: restore API /health" in log,
    "restore_pool_ok": "ok: restore API /api/pool" in log,
    "restore_blocks_ok": "ok: restore API /api/blocks" in log,
    "restore_payments_ok": "ok: restore API /api/payments" in log,
    "restore_operator_payout_status_ok": "ok: restore API operator payout status" in log,
    "restore_api_not_skipped": "skip: restore API checks disabled" not in log,
    "restore_operator_not_skipped": "skip: restore operator API check disabled" not in log,
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "restore API safety checks passed"
  else
    fail "restore API safety checks failed; see $TMP_DIR/csd-pool-evidence-restore-api-safety.log"
  fi
}

check_payout_limit_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-payout-limit-safety.log 2>&1 <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as f:
    config = json.load(f)

def int_field(name):
    value = config.get(name)
    if value is None:
        return None
    return int(value)

minimum = int_field("minimum_payout_base_units")
manual = int_field("manual_payout_approval_base_units")
batch = int_field("max_payout_batch_base_units")
daily = int_field("max_daily_payout_base_units")
checks = {
    "minimum_positive": minimum is not None and minimum > 0,
    "manual_approval_present": manual is not None and manual > 0,
    "max_payout_batch_positive": batch is not None and batch > 0,
    "max_daily_payout_positive": daily is not None and daily > 0,
    "manual_at_or_above_minimum": None not in (manual, minimum) and manual >= minimum,
    "manual_below_max_batch": None not in (manual, batch) and manual < batch,
    "daily_at_or_above_max_batch": None not in (daily, batch) and daily >= batch,
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "payout limit safety checks passed"
  else
    fail "payout limit safety checks failed; see $TMP_DIR/csd-pool-evidence-payout-limit-safety.log"
  fi
}

check_payout_controls_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-payout-controls-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

checks = {
    "payouts_paused": values.get("payouts_paused") == "True",
    "preview_amount_fields_present": values.get("preview_amount_fields_present") == "True",
    "preview_display_fields_present": values.get("preview_display_fields_present") == "True",
    "preview_count_present": values.get("preview_count_present") == "True",
    "preview_recipients_array": values.get("preview_recipients_array") == "True",
    "preview_control_booleans_present": values.get("preview_control_booleans_present") == "True",
    "manual_approval_threshold_positive": values.get("manual_approval_threshold_positive") == "True",
    "manual_below_max_batch": values.get("manual_below_max_batch") == "True",
    "batches_array_present": values.get("batches_array_present") == "True",
    "batch_statuses_allowed": values.get("batch_statuses_allowed") == "True",
    "batch_actions_have_targets": values.get("batch_actions_have_targets") == "True",
    "audit_events_array_present": values.get("audit_events_array_present") == "True",
    "audit_actions_allowed": values.get("audit_actions_allowed") == "True",
    "audit_events_have_actor_and_batch": values.get("audit_events_have_actor_and_batch") == "True",
    "payout_batches_csv_header": values.get("payout_batches_csv_header") == "True",
    "payout_audit_csv_header": values.get("payout_audit_csv_header") == "True",
    "payout_controls_ok": values.get("payout_controls_ok") == "True",
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "payout controls safety checks passed"
  else
    fail "payout controls safety checks failed; see $TMP_DIR/csd-pool-evidence-payout-controls-safety.log"
  fi
}

check_database_migration_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-database-migration-safety.log 2>&1 <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as f:
    report = json.load(f)

known_versions = report.get("known_versions") or []
database_versions = report.get("database_versions") or []
known_set = {int(version) for version in known_versions}
database_set = {int(version) for version in database_versions}
latest_known = int(report.get("latest_known_version") or 0)
latest_database = int(report.get("latest_database_version") or 0)
known_count = int(report.get("known_migration_count") or 0)
checks = {
    "complete": report.get("complete") is True,
    "known_migration_count_positive": known_count > 0,
    "known_versions_match_count": len(known_set) == known_count,
    "database_versions_present": len(database_set) > 0,
    "latest_database_matches_known": latest_database == latest_known and latest_known > 0,
    "database_contains_all_known": known_set.issubset(database_set),
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "database migration safety checks passed"
  else
    fail "database migration safety checks failed; see $TMP_DIR/csd-pool-evidence-database-migration-safety.log"
  fi
}

check_public_api_surface_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-public-api-surface-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

def int_at_least(name, minimum):
    try:
        return int(values.get(name, "0")) >= minimum
    except ValueError:
        return False

checks = {
    "public_api_surface_endpoint_count": int_at_least("public_api_surface_endpoint_count", 5),
    "public_api_surface_curl_ok": values.get("public_api_surface_curl_ok") == "True",
    "public_api_surface_all_json_valid": values.get("public_api_surface_all_json_valid") == "True",
    "public_api_surface_content_types_ok": values.get("public_api_surface_content_types_ok") == "True",
    "public_api_surface_cache_control_ok": values.get("public_api_surface_cache_control_ok") == "True",
    "public_api_surface_operator_leak_free": values.get("public_api_surface_operator_leak_free") == "True",
    "public_api_surface_ok": values.get("public_api_surface_ok") == "True",
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "public API surface safety checks passed"
  else
    fail "public API surface safety checks failed; see $TMP_DIR/csd-pool-evidence-public-api-surface-safety.log"
  fi
}

check_operator_readiness_safety() {
  local health_path="$1"
  local alerts_path="$2"
  if python3 - "$health_path" "$alerts_path" > $TMP_DIR/csd-pool-evidence-operator-readiness-safety.log 2>&1 <<'PY'
import json
import sys

health_path, alerts_path = sys.argv[1:3]
with open(health_path, "r", encoding="utf-8") as f:
    health = json.load(f)
with open(alerts_path, "r", encoding="utf-8") as f:
    alerts_response = json.load(f)
samples = health.get("samples") or []
alerts = alerts_response.get("alerts") or []
node_samples = [sample for sample in samples if str(sample.get("node_name", "")).startswith("node:")]
signer_samples = [sample for sample in samples if sample.get("node_name") == "signer"]
checks = {
    "operator_health_ok": health.get("ok") is True,
    "health_samples_present": len(samples) > 0,
    "node_sample_present": len(node_samples) > 0,
    "signer_sample_present": len(signer_samples) > 0,
    "all_samples_ok": bool(samples) and all(sample.get("ok") is True for sample in samples),
    "active_alerts_empty": isinstance(alerts, list) and len(alerts) == 0,
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "operator readiness safety checks passed"
  else
    fail "operator readiness safety checks failed; see $TMP_DIR/csd-pool-evidence-operator-readiness-safety.log"
  fi
}

check_systemd_runtime_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-systemd-runtime-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

units = [
    "csd_pool_daemon_service",
    "csd_pool_signer_service",
    "csd_pool_reconcile_blocks_timer",
    "csd_pool_rewards_timer",
    "csd_pool_payout_create_timer",
    "csd_pool_payout_sign_timer",
    "csd_pool_payout_submit_timer",
    "csd_pool_payout_reconcile_timer",
    "csd_pool_monitoring_timer",
    "csd_pool_backup_timer",
]
checks = {"systemd_runtime_ok": values.get("systemd_runtime_ok") == "True"}
for unit in units:
    checks[f"{unit}_enabled"] = values.get(f"{unit}_enabled") == "True"
    checks[f"{unit}_active"] = values.get(f"{unit}_active") == "True"
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "systemd runtime safety checks passed"
  else
    fail "systemd runtime safety checks failed; see $TMP_DIR/csd-pool-evidence-systemd-runtime-safety.log"
  fi
}

check_runtime_hardening_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-runtime-hardening-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

checks = {
    "runtime_hardening_ok": values.get("runtime_hardening_ok") == "True",
    "daemon_user_ok": values.get("daemon_user_ok") == "True",
    "daemon_group_ok": values.get("daemon_group_ok") == "True",
    "daemon_no_new_privileges_ok": values.get("daemon_no_new_privileges_ok") == "True",
    "daemon_private_tmp_ok": values.get("daemon_private_tmp_ok") == "True",
    "daemon_protect_home_ok": values.get("daemon_protect_home_ok") == "True",
    "daemon_protect_system_strict": values.get("daemon_protect_system_strict") == "True",
    "signer_user_ok": values.get("signer_user_ok") == "True",
    "signer_group_ok": values.get("signer_group_ok") == "True",
    "signer_no_new_privileges_ok": values.get("signer_no_new_privileges_ok") == "True",
    "signer_private_tmp_ok": values.get("signer_private_tmp_ok") == "True",
    "signer_protect_home_ok": values.get("signer_protect_home_ok") == "True",
    "signer_protect_system_strict": values.get("signer_protect_system_strict") == "True",
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "runtime hardening safety checks passed"
  else
    fail "runtime hardening safety checks failed; see $TMP_DIR/csd-pool-evidence-runtime-hardening-safety.log"
  fi
}

check_resource_limit_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-resource-limit-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

checks = {
    "resource_limits_ok": values.get("resource_limits_ok") == "True",
    "daemon_systemd_limit_nofile_ok": values.get("daemon_systemd_limit_nofile_ok") == "True",
    "daemon_proc_limit_nofile_ok": values.get("daemon_proc_limit_nofile_ok") == "True",
    "daemon_main_pid_positive": values.get("daemon_main_pid_positive") == "True",
    "signer_systemd_limit_nofile_ok": values.get("signer_systemd_limit_nofile_ok") == "True",
    "signer_proc_limit_nofile_ok": values.get("signer_proc_limit_nofile_ok") == "True",
    "signer_main_pid_positive": values.get("signer_main_pid_positive") == "True",
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "resource limit safety checks passed"
  else
    fail "resource limit safety checks failed; see $TMP_DIR/csd-pool-evidence-resource-limit-safety.log"
  fi
}

check_service_provenance_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-service-provenance-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

checks = {
    "service_provenance_ok": values.get("service_provenance_ok") == "True",
    "current_release_matches_manifest": values.get("current_release_matches_manifest") == "True",
    "daemon_main_pid_positive": values.get("daemon_main_pid_positive") == "True",
    "signer_main_pid_positive": values.get("signer_main_pid_positive") == "True",
    "daemon_binary_matches_release": values.get("daemon_binary_matches_release") == "True",
    "signer_binary_matches_release": values.get("signer_binary_matches_release") == "True",
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "service provenance safety checks passed"
  else
    fail "service provenance safety checks failed; see $TMP_DIR/csd-pool-evidence-service-provenance-safety.log"
  fi
}

check_public_operator_auth_boundary() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-public-operator-auth-boundary.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

checks = {
    "operator_token_redacted": values.get("operator_token_redacted") == "True",
    "unauth_status_rejected": values.get("unauth_status") in {"401", "403"},
    "wrong_token_status_rejected": values.get("wrong_token_status") in {"401", "403"},
    "authorized_status_ok": values.get("authorized_status") == "200",
    "public_api_url_present": values.get("public_api_url_present") == "True",
    "operator_token_present": values.get("operator_token_present") == "True",
    "unauth_rejected": values.get("unauth_rejected") == "True",
    "wrong_token_rejected": values.get("wrong_token_rejected") == "True",
    "authorized_ok": values.get("authorized_ok") == "True",
    "authorized_json_valid": values.get("authorized_json_valid") == "True",
    "authorized_health_ok_field_present": values.get("authorized_health_ok_field_present") == "True",
    "public_operator_auth_boundary_ok": values.get("public_operator_auth_boundary_ok") == "True",
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "public operator auth boundary checks passed"
  else
    fail "public operator auth boundary checks failed; see $TMP_DIR/csd-pool-evidence-public-operator-auth-boundary.log"
  fi
}

check_metrics_surface_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-metrics-surface-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

required = [
    "csd_pool_workers_online",
    "csd_pool_hashrate_hs",
    "csd_pool_round_share_difficulty",
    "csd_pool_stratum_connections",
    "csd_pool_shares_total",
    "csd_pool_share_validation_seconds_sum",
    "csd_pool_share_validation_seconds_count",
    "csd_pool_blocks_found_total",
    "csd_pool_jobs_created_total",
    "csd_pool_job_age_seconds",
    "csd_pool_payout_batches_total",
    "csd_pool_fee_revenue_base_units",
    "csd_pool_service_up",
    "csd_node_rpc_latency_seconds",
    "csd_pool_next_payout_seconds",
    "csd_pool_updated_timestamp_seconds",
]
checks = {
    "prometheus_metrics_not_empty": values.get("prometheus_metrics_not_empty") == "True",
    "prometheus_metrics_help_present": values.get("prometheus_metrics_help_present") == "True",
    "prometheus_metrics_type_present": values.get("prometheus_metrics_type_present") == "True",
    "prometheus_metrics_samples_present": values.get("prometheus_metrics_samples_present") == "True",
    "prometheus_metrics_accepted_share_counter": values.get("prometheus_metrics_accepted_share_counter") == "True",
    "prometheus_metrics_rejected_share_counter": values.get("prometheus_metrics_rejected_share_counter") == "True",
    "prometheus_metrics_stale_share_counter": values.get("prometheus_metrics_stale_share_counter") == "True",
    "prometheus_metrics_health_has_node_sample": values.get("prometheus_metrics_health_has_node_sample") == "True",
    "prometheus_metrics_health_has_signer_sample": values.get("prometheus_metrics_health_has_signer_sample") == "True",
    "metrics_surface_ok": values.get("metrics_surface_ok") == "True",
}
for metric in required:
    checks[f"metric_{metric}_sample_present"] = values.get(f"metric_{metric}_sample_present") == "True"
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "metrics surface safety checks passed"
  else
    fail "metrics surface safety checks failed; see $TMP_DIR/csd-pool-evidence-metrics-surface-safety.log"
  fi
}

check_backup_artifact_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-backup-artifact-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

checks = {
    "backup_path_present": values.get("backup_path_present") == "True",
    "backup_file_exists": values.get("backup_file_exists") == "True",
    "backup_regular_file": values.get("backup_regular_file") == "True",
    "backup_size_at_or_above_min": values.get("backup_size_at_or_above_min") == "True",
    "backup_fresh": values.get("backup_fresh") == "True",
    "backup_sha256_present": bool(values.get("backup_sha256")),
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "backup artifact safety checks passed"
  else
    fail "backup artifact safety checks failed; see $TMP_DIR/csd-pool-evidence-backup-artifact-safety.log"
  fi
}

check_stratum_smoke_report() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-stratum-smoke.log 2>&1 <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as f:
    report = json.load(f)
requested = report.get("requested_clients")
succeeded = report.get("succeeded_clients")
failed = report.get("failed_clients")
checks = {
    "requested_clients_positive": isinstance(requested, int) and requested > 0,
    "failed_clients_zero": failed == 0,
    "succeeded_all_requested": isinstance(succeeded, int) and succeeded == requested,
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "external public Stratum smoke passed"
  else
    fail "external public Stratum smoke failed; see $TMP_DIR/csd-pool-evidence-stratum-smoke.log"
  fi
}

check_stratum_load_report() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-stratum-load.log 2>&1 <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as f:
    report = json.load(f)
requested = report.get("requested_clients")
min_success = report.get("min_success_clients")
succeeded = report.get("succeeded_clients")
failed = report.get("failed_clients")
checks = {
    "passed_true": report.get("passed") is True,
    "requested_clients_positive": isinstance(requested, int) and requested > 0,
    "min_success_clients_positive": isinstance(min_success, int) and min_success > 0,
    "min_success_not_above_requested": isinstance(requested, int) and isinstance(min_success, int) and min_success <= requested,
    "failed_clients_zero": failed == 0,
    "succeeded_at_or_above_min_success": isinstance(succeeded, int) and isinstance(min_success, int) and succeeded >= min_success,
    "succeeded_not_above_requested": isinstance(succeeded, int) and isinstance(requested, int) and succeeded <= requested,
    "failures_empty": report.get("failures") == [],
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "external public Stratum load passed"
  else
    fail "external public Stratum load failed; see $TMP_DIR/csd-pool-evidence-stratum-load.log"
  fi
}

check_public_port_tiers_smoke_report() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-public-port-tiers-smoke.log 2>&1 <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as f:
    report = json.load(f)
tiers = report.get("tiers")
checks = {
    "overall_ok": report.get("ok") is True,
    "enabled_tier_count_positive": isinstance(report.get("enabled_tier_count"), int) and report["enabled_tier_count"] > 0,
    "failed_tier_count_zero": report.get("failed_tier_count") == 0,
    "tiers_array_present": isinstance(tiers, list) and len(tiers) > 0,
}
if isinstance(tiers, list):
    checks["all_tiers_passed"] = all(tier.get("passed") is True for tier in tiers)
    checks["all_tiers_requested_clients"] = all(isinstance(tier.get("requested_clients"), int) and tier["requested_clients"] > 0 for tier in tiers)
    checks["all_tiers_failed_clients_zero"] = all(tier.get("failed_clients") == 0 for tier in tiers)
    checks["all_tiers_succeeded_requested"] = all(tier.get("succeeded_clients") == tier.get("requested_clients") for tier in tiers)
else:
    checks["all_tiers_passed"] = False
    checks["all_tiers_requested_clients"] = False
    checks["all_tiers_failed_clients_zero"] = False
    checks["all_tiers_succeeded_requested"] = False
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "public Stratum port tier smoke checks passed"
  else
    fail "public Stratum port tier smoke checks failed; see $TMP_DIR/csd-pool-evidence-public-port-tiers-smoke.log"
  fi
}

check_public_port_tiers_safety() {
  local path="$1"
  if python3 - "$path" > $TMP_DIR/csd-pool-evidence-public-port-tiers-safety.log 2>&1 <<'PY'
import sys

path = sys.argv[1]
values = {}
with open(path, "r", encoding="utf-8") as f:
    for line in f:
        line = line.rstrip("\n")
        if "=" in line:
            key, value = line.split("=", 1)
            values[key] = value

def int_at_least(name, minimum):
    try:
        return int(values.get(name, "0")) >= minimum
    except ValueError:
        return False

checks = {
    "public_stratum_probe_addr_valid": values.get("public_stratum_probe_addr_valid") == "True",
    "public_port_tiers_parse_ok": values.get("public_port_tiers_parse_ok") == "True",
    "public_port_tier_count_positive": int_at_least("public_port_tier_count", 1),
    "public_port_tier_enabled_count_positive": int_at_least("public_port_tier_enabled_count", 1),
    "probe_port_in_enabled_tiers": values.get("probe_port_in_enabled_tiers") == "True",
    "enabled_tiers_tcp_connected": values.get("enabled_tiers_tcp_connected") == "True",
    "public_port_tiers_ok": values.get("public_port_tiers_ok") == "True",
}
for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "public Stratum port tiers safety checks passed"
  else
    fail "public Stratum port tiers safety checks failed; see $TMP_DIR/csd-pool-evidence-public-port-tiers-safety.log"
  fi
}

check_evidence_metadata_consistency() {
  local summary_path="$1"
  local report_path="$2"
  local manifest_path="$3"
  if python3 - "$summary_path" "$report_path" "$manifest_path" > $TMP_DIR/csd-pool-evidence-metadata-consistency.log 2>&1 <<'PY'
import json
import os
import sys

summary_path, report_path, manifest_path = sys.argv[1:4]

def read_key_values(path):
    values = {}
    with open(path, "r", encoding="utf-8") as f:
        for raw in f:
            line = raw.rstrip("\n")
            if "=" in line and not line.startswith("  "):
                key, value = line.split("=", 1)
                values[key] = value
    return values

with open(summary_path, "r", encoding="utf-8") as f:
    summary = json.load(f)
report = read_key_values(report_path)
manifest = read_key_values(manifest_path)

summary_counts = summary.get("summary") or {}
summary_evidence = summary.get("evidence") or {}
summary_endpoints = summary.get("endpoints") or {}

def as_str(value):
    if value is None:
        return ""
    return str(value)

def norm_bool(value):
    if isinstance(value, bool):
        return "true" if value else "false"
    normalized = as_str(value).lower()
    if normalized in {"1", "true", "yes"}:
        return "true"
    if normalized in {"0", "false", "no"}:
        return "false"
    return normalized

checks = {
    "summary_report_target_match": as_str(summary.get("target")) == report.get("target"),
    "summary_manifest_target_match": as_str(summary.get("target")) == manifest.get("target"),
    "summary_report_dry_run_match": norm_bool(summary.get("dry_run")) == norm_bool(report.get("dry_run")),
    "summary_manifest_dry_run_match": norm_bool(summary.get("dry_run")) == norm_bool(manifest.get("dry_run")),
    "summary_status_passed": summary_counts.get("status") == "passed",
    "report_status_passed": report.get("status") == "passed",
    "manifest_status_passed": manifest.get("status") == "passed",
    "summary_report_status_match": summary_counts.get("status") == report.get("status"),
    "summary_manifest_status_match": summary_counts.get("status") == manifest.get("status"),
    "summary_report_fail_match": as_str(summary_counts.get("fail")) == report.get("fail"),
    "summary_manifest_fail_match": as_str(summary_counts.get("fail")) == manifest.get("fail"),
    "summary_report_pass_match": "pass" in report and as_str(summary_counts.get("pass")) == report.get("pass"),
    "summary_manifest_pass_match": "pass" in manifest and as_str(summary_counts.get("pass")) == manifest.get("pass"),
    "summary_report_skip_match": "skip" in report and as_str(summary_counts.get("skip")) == report.get("skip"),
    "summary_manifest_skip_match": "skip" in manifest and as_str(summary_counts.get("skip")) == manifest.get("skip"),
    "manifest_report_name": manifest.get("report") == "GO-LIVE-REPORT.txt",
    "manifest_summary_name": manifest.get("summary") == "go-live-summary.json",
    "summary_evidence_archive_present": bool(summary_evidence.get("archive")),
    "summary_evidence_sha256_present": bool(summary_evidence.get("sha256")),
    "report_evidence_archive_present": bool(report.get("evidence_archive")),
    "report_evidence_sha256_present": bool(report.get("evidence_sha256")),
}

checks["summary_report_endpoint_api_url_match"] = as_str(summary_endpoints.get("api_url")) == report.get("api_url")
checks["summary_report_endpoint_stratum_addr_match"] = as_str(summary_endpoints.get("stratum_addr")) == report.get("stratum_addr")
checks["summary_report_endpoint_public_api_url_match"] = as_str(summary_endpoints.get("public_api_url")) == report.get("public_api_url")
checks["summary_report_endpoint_public_stratum_probe_addr_match"] = as_str(summary_endpoints.get("public_stratum_probe_addr")) == report.get("public_stratum_probe_addr")
checks["summary_report_endpoint_public_stratum_addr_match"] = as_str(summary_endpoints.get("public_stratum_addr")) == report.get("public_stratum_addr")
checks["summary_report_endpoint_public_port_tiers_match"] = as_str(summary_endpoints.get("public_port_tiers")) == report.get("public_port_tiers")

if summary_evidence.get("archive") and report.get("evidence_archive"):
    checks["summary_report_archive_basename_match"] = (
        os.path.basename(summary_evidence["archive"]) == os.path.basename(report["evidence_archive"])
    )
else:
    checks["summary_report_archive_basename_match"] = False

if summary_evidence.get("sha256") and report.get("evidence_sha256"):
    checks["summary_report_sha256_basename_match"] = (
        os.path.basename(summary_evidence["sha256"]) == os.path.basename(report["evidence_sha256"])
    )
else:
    checks["summary_report_sha256_basename_match"] = False

for name, passed in checks.items():
    print(f"{name}: {'ok' if passed else 'failed'}")

if not all(checks.values()):
    sys.exit(1)
PY
  then
    ok "evidence metadata consistency checks passed"
  else
    fail "evidence metadata consistency failed; see $TMP_DIR/csd-pool-evidence-metadata-consistency.log"
  fi
}

if [[ -z "$ARCHIVE_PATH" ]]; then
  usage
  exit 2
fi

if [[ ! -f "$ARCHIVE_PATH" ]]; then
  fail "archive missing: $ARCHIVE_PATH"
  printf 'summary: pass=%s fail=%s skip=%s\n' "$PASS" "$FAIL" "$SKIP"
  exit 1
fi
ok "archive exists: $ARCHIVE_PATH"

SHA256_PATH="${CSD_POOL_GO_LIVE_EVIDENCE_SHA256:-$ARCHIVE_PATH.sha256}"
if [[ -f "$SHA256_PATH" ]]; then
  if sha256_check "$SHA256_PATH" >"$TMP_DIR/csd-pool-evidence-sha256.log" 2>&1; then
    ok "evidence archive sha256 verified"
  else
    fail "evidence archive sha256 failed; see $TMP_DIR/csd-pool-evidence-sha256.log"
  fi
else
  fail "evidence sha256 file missing: $SHA256_PATH"
fi

if tar -tzf "$ARCHIVE_PATH" >"$TMP_DIR/csd-pool-evidence-tar-list.txt"; then
  ok "evidence archive can be listed"
else
  fail "evidence archive cannot be listed"
fi

if grep -Eq '(^/|(^|/)\.\.(/|$))' "$TMP_DIR/csd-pool-evidence-tar-list.txt"; then
  fail "evidence archive contains unsafe paths"
else
  ok "evidence archive paths are relative and safe"
fi

WORK_DIR="$(mktemp -d "$TMP_DIR/csd-pool-evidence.XXXXXX")"
if tar -xzf "$ARCHIVE_PATH" -C "$WORK_DIR"; then
  ok "evidence archive extracted"
else
  fail "evidence archive extraction failed"
fi

SUMMARY_JSON="$WORK_DIR/go-live-summary.json"
REPORT_TXT="$WORK_DIR/GO-LIVE-REPORT.txt"
MANIFEST_TXT="$WORK_DIR/EVIDENCE-MANIFEST.txt"
INTERNAL_SUMS="$WORK_DIR/EVIDENCE-SHA256SUMS"

require_file "$SUMMARY_JSON" "go-live summary"
require_file "$REPORT_TXT" "go-live report"
require_file "$MANIFEST_TXT" "evidence manifest"
require_file "$INTERNAL_SUMS" "evidence internal sha256 manifest"
require_file "$WORK_DIR/config-snapshot.json" "config snapshot"
require_file "$WORK_DIR/env-snapshot.txt" "environment snapshot"
require_file "$WORK_DIR/secrets-permissions-safety.log" "secrets permissions safety report"
require_file "$WORK_DIR/evidence-redaction-safety.log" "evidence redaction safety report"
require_file "$WORK_DIR/real-env-readiness.log" "real environment readiness report"
require_file "$WORK_DIR/clock-safety.log" "clock safety report"
require_file "$WORK_DIR/disk-safety.log" "disk safety report"
require_file "$WORK_DIR/bind-safety.log" "bind safety report"
require_file "$WORK_DIR/database-migration.json" "database migration report"
require_file "$WORK_DIR/database-migration-safety.log" "database migration safety report"
require_file "$WORK_DIR/database-runtime.json" "database runtime report"
require_file "$WORK_DIR/preflight.log" "preflight log"
require_file "$WORK_DIR/release-integrity.log" "release integrity report"
require_file "$WORK_DIR/verify.log" "verify log"
require_file "$WORK_DIR/systemd-runtime-safety.log" "systemd runtime safety report"
require_file "$WORK_DIR/runtime-hardening-safety.log" "runtime hardening safety report"
require_file "$WORK_DIR/resource-limit-safety.log" "resource limit safety report"
require_file "$WORK_DIR/service-provenance-safety.log" "service provenance safety report"
require_file "$WORK_DIR/backup-artifact-safety.log" "backup artifact safety report"
require_file "$WORK_DIR/restore-drill.log" "restore drill report"
require_file "$WORK_DIR/restore-api-safety.log" "restore API safety report"
require_file "$WORK_DIR/restore-http-health.json" "restore API health response"
require_file "$WORK_DIR/restore-http-pool.json" "restore API pool response"
require_file "$WORK_DIR/restore-http-blocks.json" "restore API blocks response"
require_file "$WORK_DIR/restore-http-payments.json" "restore API payments response"
require_file "$WORK_DIR/restore-http-operator-payout-status.json" "restore API operator payout status response"
require_file "$WORK_DIR/edge-proxy-safety.log" "edge proxy safety report"
require_file "$WORK_DIR/check-node-template.json" "node template report"
require_file "$WORK_DIR/node-runtime.json" "CSD node runtime report"
require_file "$WORK_DIR/node-endpoint-safety.log" "CSD node endpoint safety report"
require_file "$WORK_DIR/check-signer.json" "signer report"
require_file "$WORK_DIR/signer-safety.log" "signer safety report"
require_file "$WORK_DIR/sample-health.json" "runtime health sample report"
require_file "$WORK_DIR/payout-preview.json" "payout preview report"
require_file "$WORK_DIR/payout-limit-safety.log" "payout limit safety report"
require_file "$WORK_DIR/payout-safety.log" "payout launch safety report"
require_file "$WORK_DIR/payout-controls-safety.log" "payout controls safety report"
require_file "$WORK_DIR/runtime-config-binding.log" "runtime config binding report"
require_file "$WORK_DIR/runtime-status-binding.log" "runtime status binding report"
require_file "$WORK_DIR/status-release-binding.log" "status release binding report"
require_file "$WORK_DIR/pool-endpoint-binding.log" "pool endpoint binding report"
require_file "$WORK_DIR/external-public-status-binding.log" "external public status binding report"
require_file "$WORK_DIR/external-public-pool-binding.log" "external public pool binding report"
require_file "$WORK_DIR/external-public-config-binding.log" "external public config binding report"
require_file "$WORK_DIR/getting-started-binding.log" "getting-started binding report"
require_file "$WORK_DIR/external-public-getting-started-binding.log" "external public getting-started binding report"
require_file "$WORK_DIR/public-dns-safety.log" "public DNS safety report"
require_file "$WORK_DIR/public-api-tls-safety.log" "public API TLS safety report"
require_file "$WORK_DIR/public-api-headers-safety.log" "public API security headers safety report"
require_file "$WORK_DIR/public-api-surface-safety.log" "public API surface safety report"
require_file "$WORK_DIR/public-operator-auth-boundary.log" "public operator auth boundary report"
require_file "$WORK_DIR/metrics-surface-safety.log" "metrics surface safety report"
require_file "$WORK_DIR/http-operator-health.json" "operator health report"
require_file "$WORK_DIR/http-operator-alerts.json" "operator alerts report"
require_file "$WORK_DIR/operator-readiness-safety.log" "operator readiness safety report"
require_file "$WORK_DIR/http-operator-payout-batches.json" "operator payout batches report"
require_file "$WORK_DIR/http-operator-payout-batches.csv" "operator payout batches CSV report"
require_file "$WORK_DIR/http-operator-payout-audit.json" "operator payout audit report"
require_file "$WORK_DIR/http-operator-payout-audit.csv" "operator payout audit CSV report"
require_file "$WORK_DIR/http-operator-payout-preview.json" "operator payout preview report"
require_file "$WORK_DIR/http-operator-payout-status.json" "operator payout status report"
require_file "$WORK_DIR/http-api-status.json" "status endpoint report"
require_file "$WORK_DIR/http-api-pool.json" "pool endpoint report"
require_file "$WORK_DIR/http-api-metrics.json" "metrics endpoint report"
require_file "$WORK_DIR/http-prometheus-metrics.txt" "Prometheus metrics endpoint report"
require_file "$WORK_DIR/http-api-blocks.json" "blocks endpoint report"
require_file "$WORK_DIR/http-api-payments.json" "payments endpoint report"
require_file "$WORK_DIR/http-api-getting-started.json" "getting-started endpoint report"
require_file "$WORK_DIR/stratum-tcp.log" "stratum TCP probe report"
require_file "$WORK_DIR/http-public-api-status.json" "external public status endpoint report"
require_file "$WORK_DIR/http-public-api-pool.json" "external public pool endpoint report"
require_file "$WORK_DIR/http-public-api-getting-started.json" "external public getting-started endpoint report"
require_file "$WORK_DIR/http-public-getting-started.txt" "external getting-started page report"
require_file "$WORK_DIR/public-stratum-tcp.log" "external public Stratum TCP probe report"
require_file "$WORK_DIR/public-port-tiers-safety.log" "public Stratum port tiers safety report"
require_file "$WORK_DIR/public-port-tiers-smoke.json" "public Stratum port tier smoke report"
require_file "$WORK_DIR/public-stratum-smoke.json" "external public Stratum smoke report"
require_file "$WORK_DIR/public-stratum-load.json" "external public Stratum load report"

if [[ -f "$INTERNAL_SUMS" ]]; then
  if sha256_check "$INTERNAL_SUMS" >"$TMP_DIR/csd-pool-evidence-internal-sha256.log" 2>&1; then
    ok "evidence internal file checksums verified"
  else
    fail "evidence internal file checksums failed; see $TMP_DIR/csd-pool-evidence-internal-sha256.log"
  fi
fi

if python3 -m json.tool "$SUMMARY_JSON" >"$TMP_DIR/csd-pool-evidence-summary.pretty.json"; then
  ok "go-live summary JSON validates"
else
  fail "go-live summary JSON validates"
fi

check_evidence_metadata_consistency "$SUMMARY_JSON" "$REPORT_TXT" "$MANIFEST_TXT"
check_json_value "$SUMMARY_JSON" "summary.status" "passed" "summary status passed"
check_json_value "$SUMMARY_JSON" "summary.fail" "0" "summary fail count is zero"

dry_run_value="$(json_query "$SUMMARY_JSON" "dry_run" 2>/dev/null || printf 'missing')"
target_value="$(json_query "$SUMMARY_JSON" "target" 2>/dev/null || printf 'missing')"
if [[ "$dry_run_value" == "false" ]]; then
  ok "summary is not dry-run"
  check_evidence_freshness "$SUMMARY_JSON" "$MANIFEST_TXT"
  require_text "$TMP_DIR/csd-pool-evidence-freshness.log" "evidence_within_max_age: ok" "evidence freshness report shows recent launch evidence"
  require_text "$TMP_DIR/csd-pool-evidence-freshness.log" "evidence_not_from_future: ok" "evidence freshness report shows timestamp is not in the future"
  release_manifest_value="$(json_query "$SUMMARY_JSON" "release.manifest" 2>/dev/null || printf 'missing')"
  if [[ "$release_manifest_value" != "unknown" && "$release_manifest_value" != "missing" && -n "$release_manifest_value" ]]; then
    ok "summary release manifest recorded"
  else
    fail "summary release manifest must be recorded for launch evidence"
  fi
  require_text "$WORK_DIR/env-snapshot.txt" "world_readable=false" "environment snapshot is not world-readable"
  require_text "$WORK_DIR/env-snapshot.txt" "CSD_POOL_DATABASE_URL=present" "environment snapshot has database URL"
  require_text "$WORK_DIR/env-snapshot.txt" "CSD_POOL_OPERATOR_TOKEN=present" "environment snapshot has operator token"
  require_text "$WORK_DIR/env-snapshot.txt" "CSD_POOL_SIGNER_TOKEN=present" "environment snapshot has signer token"
  require_text "$WORK_DIR/env-snapshot.txt" "CSD_POOL_NODE_TOKEN=present" "environment snapshot has node adapter token"
  require_text "$WORK_DIR/env-snapshot.txt" "CSD_POOL_SIGNER_URL=present" "environment snapshot has signer URL"
  require_text "$WORK_DIR/env-snapshot.txt" "CSD_POOL_SIGNER_WALLET_ADDRESS=present" "environment snapshot has signer wallet address"
  require_text "$WORK_DIR/env-snapshot.txt" "CSD_POOL_WATCH_NODE_URL=present" "environment snapshot has watch node URL"
  require_text "$WORK_DIR/env-snapshot.txt" "CSD_POOL_SUBMIT_NODE_URL=present" "environment snapshot has submit node URL"
  require_text "$WORK_DIR/env-snapshot.txt" "CSD_POOL_PAYOUT_NODE_URL=present" "environment snapshot has payout node URL"
  require_text "$WORK_DIR/env-snapshot.txt" "CSD_POOL_PUBLIC_STRATUM_ADDR=present" "environment snapshot has public Stratum address"
  require_text "$WORK_DIR/env-snapshot.txt" "CSD_POOL_RESTORE_DATABASE_URL=present" "environment snapshot has restore database URL"
  check_real_env_readiness "$WORK_DIR/real-env-readiness.log"
  check_secrets_permissions_safety "$WORK_DIR/secrets-permissions-safety.log"
  require_text "$WORK_DIR/secrets-permissions-safety.log" "secrets_permissions_ok=True" "secrets permissions report shows owner-only secret files"
  check_evidence_redaction_safety "$WORK_DIR" "$WORK_DIR/evidence-redaction-safety.log"
  require_text "$WORK_DIR/evidence-redaction-safety.log" "evidence_redaction_ok=True" "evidence redaction report shows no leaked secrets"
  check_clock_safety "$WORK_DIR/clock-safety.log"
  require_text "$WORK_DIR/clock-safety.log" "clock_synchronized=True" "clock safety report shows synchronized system clock"
  check_disk_safety "$WORK_DIR/disk-safety.log"
  require_text "$WORK_DIR/disk-safety.log" "disk_all_paths_ok=True" "disk safety report shows enough free space and inodes"
  check_bind_safety "$WORK_DIR/bind-safety.log"
  require_text "$WORK_DIR/bind-safety.log" "bind_safety_ok=True" "bind safety report shows internal listeners are loopback-only"
  check_edge_proxy_safety "$WORK_DIR/edge-proxy-safety.log"
  require_text "$WORK_DIR/edge-proxy-safety.log" "edge_proxy_safety_ok=True" "edge proxy safety report shows validated public ingress mapping"
  validate_json_file "$WORK_DIR/database-migration.json" "database migration report"
  check_database_migration_safety "$WORK_DIR/database-migration.json"
  require_text "$WORK_DIR/database-migration-safety.log" "complete=True" "database migration safety report shows complete migrations"
  require_text "$WORK_DIR/database-migration-safety.log" "latest_database_matches_known=True" "database migration safety report shows latest version match"
  require_text "$WORK_DIR/database-migration-safety.log" "database_contains_all_known=True" "database migration safety report shows all migrations applied"
  validate_json_file "$WORK_DIR/database-runtime.json" "database runtime report"
  check_database_runtime_report "$WORK_DIR/database-runtime.json"
  check_systemd_runtime_safety "$WORK_DIR/systemd-runtime-safety.log"
  require_text "$WORK_DIR/systemd-runtime-safety.log" "systemd_runtime_ok=True" "systemd runtime report shows all units ready"
  require_text "$WORK_DIR/systemd-runtime-safety.log" "csd_pool_daemon_service_active=True" "systemd runtime report shows daemon active"
  require_text "$WORK_DIR/systemd-runtime-safety.log" "csd_pool_signer_service_active=True" "systemd runtime report shows signer active"
  require_text "$WORK_DIR/systemd-runtime-safety.log" "csd_pool_backup_timer_active=True" "systemd runtime report shows backup timer active"
  check_runtime_hardening_safety "$WORK_DIR/runtime-hardening-safety.log"
  require_text "$WORK_DIR/runtime-hardening-safety.log" "runtime_hardening_ok=True" "runtime hardening report shows loaded systemd protections"
  check_resource_limit_safety "$WORK_DIR/resource-limit-safety.log"
  require_text "$WORK_DIR/resource-limit-safety.log" "resource_limits_ok=True" "resource limit report shows enough open-file capacity"
  check_service_provenance_safety "$WORK_DIR/service-provenance-safety.log"
  require_text "$WORK_DIR/service-provenance-safety.log" "current_release_matches_manifest=True" "service provenance report shows current release marker match"
  require_text "$WORK_DIR/service-provenance-safety.log" "daemon_binary_matches_release=True" "service provenance report shows daemon binary matches release"
  require_text "$WORK_DIR/service-provenance-safety.log" "signer_binary_matches_release=True" "service provenance report shows signer binary matches release"
  check_backup_artifact_safety "$WORK_DIR/backup-artifact-safety.log"
  require_text "$WORK_DIR/backup-artifact-safety.log" "backup_fresh=True" "backup artifact safety report shows fresh backup"
  require_text "$WORK_DIR/backup-artifact-safety.log" "backup_size_at_or_above_min=True" "backup artifact safety report shows backup size"
  check_restore_api_safety "$WORK_DIR/restore-drill.log"
  require_text "$WORK_DIR/restore-api-safety.log" "restore_api_url_present=True" "restore API safety report shows restore API URL"
  require_text "$WORK_DIR/restore-api-safety.log" "restore_operator_payout_status_ok=True" "restore API safety report shows operator payout status"
  validate_json_file "$WORK_DIR/restore-http-health.json" "restore API health response"
  validate_json_file "$WORK_DIR/restore-http-pool.json" "restore API pool response"
  validate_json_file "$WORK_DIR/restore-http-blocks.json" "restore API blocks response"
  validate_json_file "$WORK_DIR/restore-http-payments.json" "restore API payments response"
  validate_json_file "$WORK_DIR/restore-http-operator-payout-status.json" "restore API operator payout status response"
  check_json_key_present "$WORK_DIR/restore-http-operator-payout-status.json" "payouts_enabled" "restore API operator payout status exposes payouts_enabled"
  validate_json_file "$WORK_DIR/config-snapshot.json" "config snapshot"
  validate_json_file "$WORK_DIR/check-node-template.json" "node template report"
  validate_json_file "$WORK_DIR/node-runtime.json" "CSD node runtime report"
  validate_json_file "$WORK_DIR/sample-health.json" "runtime health sample"
  validate_json_file "$WORK_DIR/check-signer.json" "signer report"
  check_json_value "$WORK_DIR/config-snapshot.json" "passed" "true" "config snapshot passed"
  check_node_endpoint_safety "$WORK_DIR/config-snapshot.json" "$WORK_DIR/check-node-template.json"
  check_node_runtime_report "$WORK_DIR/node-runtime.json"
  require_text "$WORK_DIR/node-endpoint-safety.log" "config_node_urls_allowed=True" "CSD node endpoint safety report shows non-loopback config nodes"
  require_text "$WORK_DIR/node-endpoint-safety.log" "template_contract_passed=True" "CSD node endpoint safety report shows template contract passed"
  require_text "$WORK_DIR/node-endpoint-safety.log" "submit_health_ok=True" "CSD node endpoint safety report shows submit health passed"
  require_text "$TMP_DIR/csd-pool-evidence-node-runtime.log" "health_quorum_ok: ok" "CSD node runtime report shows healthy role quorum"
  require_text "$TMP_DIR/csd-pool-evidence-node-runtime.log" "latency_ok: ok" "CSD node runtime report shows latency within thresholds"
  check_signer_safety "$WORK_DIR/check-signer.json"
  require_text "$WORK_DIR/signer-safety.log" "signer_mode_allowed=True" "signer safety report shows non-mock mode"
  require_text "$WORK_DIR/signer-safety.log" "signer_wallet_matches_expected=True" "signer safety report shows expected wallet match"
  check_payout_limit_safety "$WORK_DIR/config-snapshot.json"
  require_text "$WORK_DIR/payout-limit-safety.log" "manual_below_max_batch=True" "payout limit safety report shows manual approval below max batch"
  require_text "$WORK_DIR/payout-limit-safety.log" "daily_at_or_above_max_batch=True" "payout limit safety report shows daily cap covers max batch"
  for field in pool_id stratum_listen api_listen signer_listen minimum_payout_base_units max_payout_batch_base_units; do
    if json_query "$WORK_DIR/config-snapshot.json" "$field" >/dev/null 2>&1; then
      ok "config snapshot has $field"
    else
      fail "config snapshot missing $field"
    fi
  done
  for json_report in \
    "$WORK_DIR/http-api-status.json:public status endpoint" \
    "$WORK_DIR/http-api-pool.json:public pool endpoint" \
    "$WORK_DIR/http-api-metrics.json:public metrics endpoint" \
    "$WORK_DIR/http-api-blocks.json:public blocks endpoint" \
    "$WORK_DIR/http-api-payments.json:public payments endpoint" \
    "$WORK_DIR/http-api-getting-started.json:getting-started endpoint" \
    "$WORK_DIR/http-operator-health.json:operator health" \
    "$WORK_DIR/http-operator-alerts.json:operator alerts" \
    "$WORK_DIR/http-operator-payout-batches.json:operator payout batches" \
    "$WORK_DIR/http-operator-payout-audit.json:operator payout audit" \
    "$WORK_DIR/http-operator-payout-preview.json:operator payout preview" \
    "$WORK_DIR/http-operator-payout-status.json:operator payout status"; do
    validate_json_file "${json_report%%:*}" "${json_report#*:}"
  done
  check_operator_readiness_safety "$WORK_DIR/http-operator-health.json" "$WORK_DIR/http-operator-alerts.json"
  require_text "$WORK_DIR/operator-readiness-safety.log" "operator_health_ok=True" "operator readiness report shows healthy operator status"
  require_text "$WORK_DIR/operator-readiness-safety.log" "node_sample_present=True" "operator readiness report shows node sample"
  require_text "$WORK_DIR/operator-readiness-safety.log" "signer_sample_present=True" "operator readiness report shows signer sample"
  require_text "$WORK_DIR/operator-readiness-safety.log" "active_alerts_empty=True" "operator readiness report shows zero active alerts"
  check_status_release_binding "$SUMMARY_JSON" "$WORK_DIR/http-api-status.json" "status release matches summary release"
  check_runtime_status_binding "$WORK_DIR/http-api-status.json" "$SUMMARY_JSON"
  check_runtime_config_binding "$WORK_DIR/config-snapshot.json" "$WORK_DIR/http-api-status.json"
  check_pool_endpoint_binding "$WORK_DIR/http-api-pool.json" "$WORK_DIR/http-api-status.json" "local"
  require_text "$WORK_DIR/pool-endpoint-binding.log" "pool_endpoint_binding_ok=True" "pool endpoint binding report shows status match"
  require_text "$WORK_DIR/runtime-config-binding.log" "pool_id_matches=True" "runtime config binding report shows pool id match"
  require_text "$WORK_DIR/runtime-config-binding.log" "mining_address_matches=True" "runtime config binding report shows mining address match"
  require_text "$WORK_DIR/runtime-status-binding.log" "data_source_postgres=True" "runtime status binding report shows PostgreSQL data source"
  require_text "$WORK_DIR/runtime-status-binding.log" "node_count_positive=True" "runtime status binding report shows node samples"
  check_metrics_surface_safety "$WORK_DIR/metrics-surface-safety.log"
  require_text "$WORK_DIR/metrics-surface-safety.log" "metrics_surface_ok=True" "metrics surface report shows core Prometheus metrics"
  public_stratum_addr="$(json_query "$SUMMARY_JSON" "endpoints.public_stratum_addr" 2>/dev/null || printf 'missing')"
  public_port_tiers_raw="$(json_query "$SUMMARY_JSON" "endpoints.public_port_tiers" 2>/dev/null || printf 'missing')"
  check_getting_started_binding "$WORK_DIR/http-api-getting-started.json" "$public_stratum_addr" "$public_port_tiers_raw" "local"
  require_text "$WORK_DIR/getting-started-binding.log" "stratum_endpoint_matches=True" "getting-started binding report shows Stratum endpoint match"
  require_text "$WORK_DIR/getting-started-binding.log" "port_tiers_match=True" "getting-started binding report shows port tiers match"
  if [[ "$target_value" == "public-beta" || "$target_value" == "production" ]]; then
    check_public_dns_safety "$WORK_DIR/public-dns-safety.log"
    require_text "$WORK_DIR/public-dns-safety.log" "public_api_dns_resolves=True" "public DNS report shows API DNS resolution"
    require_text "$WORK_DIR/public-dns-safety.log" "public_stratum_dns_resolves=True" "public DNS report shows Stratum DNS resolution"
    require_text "$WORK_DIR/public-dns-safety.log" "public_dns_all_global=True" "public DNS report shows only global public addresses"
    check_public_api_tls_safety "$WORK_DIR/public-api-tls-safety.log"
    require_text "$WORK_DIR/public-api-tls-safety.log" "public_api_scheme_https=True" "public API TLS report shows HTTPS"
    require_text "$WORK_DIR/public-api-tls-safety.log" "public_api_tls_handshake=True" "public API TLS report shows handshake"
    require_text "$WORK_DIR/public-api-tls-safety.log" "public_api_tls_hostname_valid=True" "public API TLS report shows hostname-valid certificate"
    check_public_api_headers_safety "$WORK_DIR/public-api-headers-safety.log"
    require_text "$WORK_DIR/public-api-headers-safety.log" "public_api_headers_ok=True" "public API security headers report shows baseline headers"
    check_public_api_surface_safety "$WORK_DIR/public-api-surface-safety.log"
    require_text "$WORK_DIR/public-api-surface-safety.log" "public_api_surface_ok=True" "public API surface report shows reviewed public JSON endpoints"
    check_public_operator_auth_boundary "$WORK_DIR/public-operator-auth-boundary.log"
    require_text "$WORK_DIR/public-operator-auth-boundary.log" "public_operator_auth_boundary_ok=True" "public operator auth boundary report shows protected operator API"
    validate_json_file "$WORK_DIR/http-public-api-status.json" "external public status endpoint"
    validate_json_file "$WORK_DIR/http-public-api-pool.json" "external public pool endpoint"
    validate_json_file "$WORK_DIR/http-public-api-getting-started.json" "external public getting-started endpoint"
    check_status_release_binding "$SUMMARY_JSON" "$WORK_DIR/http-public-api-status.json" "external public status release matches summary release"
    check_runtime_status_binding "$WORK_DIR/http-public-api-status.json" "$SUMMARY_JSON"
    check_external_public_config_binding "$WORK_DIR/config-snapshot.json" "$WORK_DIR/http-public-api-status.json"
    check_pool_endpoint_binding "$WORK_DIR/http-public-api-pool.json" "$WORK_DIR/http-public-api-status.json" "external public"
    check_getting_started_binding "$WORK_DIR/http-public-api-getting-started.json" "$public_stratum_addr" "$public_port_tiers_raw" "external public"
    require_text "$WORK_DIR/external-public-status-binding.log" "release_matches=True" "external public status binding report shows release match"
    require_text "$WORK_DIR/external-public-status-binding.log" "data_source_postgres=True" "external public status binding report shows PostgreSQL data source"
    require_text "$WORK_DIR/external-public-status-binding.log" "node_count_positive=True" "external public status binding report shows node samples"
    require_text "$WORK_DIR/external-public-config-binding.log" "pool_id_matches=True" "external public config binding report shows pool id match"
    require_text "$WORK_DIR/external-public-config-binding.log" "mining_address_matches=True" "external public config binding report shows mining address match"
    require_text "$WORK_DIR/external-public-pool-binding.log" "pool_endpoint_binding_ok=True" "external public pool binding report shows status match"
    require_text "$WORK_DIR/external-public-getting-started-binding.log" "stratum_endpoint_matches=True" "external public getting-started binding report shows Stratum endpoint match"
    require_text "$WORK_DIR/external-public-getting-started-binding.log" "port_tiers_match=True" "external public getting-started binding report shows port tiers match"
    require_text "$WORK_DIR/public-stratum-tcp.log" "connected" "external public Stratum TCP probe connected"
    check_public_port_tiers_safety "$WORK_DIR/public-port-tiers-safety.log"
    require_text "$WORK_DIR/public-port-tiers-safety.log" "public_port_tiers_ok=True" "public Stratum port tiers report shows enabled tiers reachable"
    validate_json_file "$WORK_DIR/public-port-tiers-smoke.json" "public Stratum port tier smoke"
    check_public_port_tiers_smoke_report "$WORK_DIR/public-port-tiers-smoke.json"
    validate_json_file "$WORK_DIR/public-stratum-smoke.json" "external public Stratum smoke"
    check_stratum_smoke_report "$WORK_DIR/public-stratum-smoke.json"
    validate_json_file "$WORK_DIR/public-stratum-load.json" "external public Stratum load"
    check_stratum_load_report "$WORK_DIR/public-stratum-load.json"
  else
    skip "external public endpoint content checks not required for $target_value"
  fi
  require_csv_header "$WORK_DIR/http-operator-payout-batches.csv" \
    "batch_id,status,txid,recipient,amount_base_units,amount_csd,total_base_units,total_csd" \
    "operator payout batches export"
  require_csv_header "$WORK_DIR/http-operator-payout-audit.csv" \
    "created_at,batch_id,actor,action,details_json" \
    "operator payout audit export"
  payout_status_value="$(json_query "$WORK_DIR/http-operator-payout-status.json" "payouts_enabled" 2>/dev/null || printf 'missing')"
  if [[ "$payout_status_value" == "true" || "$payout_status_value" == "false" ]]; then
    ok "operator payout status exposes payouts_enabled"
  else
    fail "operator payout status missing payouts_enabled"
  fi
  if [[ "$payout_status_value" == "false" ]]; then
    ok "operator payouts paused for launch"
  else
    fail "operator payouts must be paused for launch"
  fi
  require_text "$WORK_DIR/payout-safety.log" "payouts_enabled=False" "payout safety report shows payouts paused"
  check_payout_controls_safety "$WORK_DIR/payout-controls-safety.log"
  require_text "$WORK_DIR/payout-controls-safety.log" "payout_controls_ok=True" "payout controls safety report shows operator payout controls ready"
  require_text "$WORK_DIR/release-integrity.log" "OK" "release artifact checksums verified"
  require_text "$WORK_DIR/restore-drill.log" "restore drill complete" "restore drill completed"
  require_text "$WORK_DIR/stratum-tcp.log" "connected" "stratum TCP probe connected"
elif [[ "$dry_run_value" == "true" && "$ALLOW_DRY_RUN" == "1" ]]; then
  skip "summary is dry-run but allowed by CSD_POOL_EVIDENCE_ALLOW_DRY_RUN=1"
  skip "evidence freshness check skipped for dry-run evidence"
  skip "config snapshot content check skipped for dry-run evidence"
  skip "environment snapshot content check skipped for dry-run evidence"
  skip "secrets permissions safety check skipped for dry-run evidence"
  check_evidence_redaction_safety "$WORK_DIR" "$WORK_DIR/evidence-redaction-safety.log"
  skip "real environment readiness check skipped for dry-run evidence"
  skip "clock safety check skipped for dry-run evidence"
  skip "disk safety check skipped for dry-run evidence"
  skip "bind safety check skipped for dry-run evidence"
  skip "edge proxy safety check skipped for dry-run evidence"
  skip "database migration safety check skipped for dry-run evidence"
  skip "database runtime check skipped for dry-run evidence"
  skip "systemd runtime safety check skipped for dry-run evidence"
  skip "runtime hardening safety check skipped for dry-run evidence"
  skip "resource limit safety check skipped for dry-run evidence"
  skip "service provenance safety check skipped for dry-run evidence"
  skip "backup artifact safety check skipped for dry-run evidence"
  skip "restore API safety check skipped for dry-run evidence"
  skip "CSD node endpoint safety check skipped for dry-run evidence"
  skip "CSD node runtime quorum check skipped for dry-run evidence"
  skip "signer safety check skipped for dry-run evidence"
  skip "runtime config binding check skipped for dry-run evidence"
  skip "runtime status binding check skipped for dry-run evidence"
  skip "pool endpoint binding check skipped for dry-run evidence"
  skip "metrics surface safety check skipped for dry-run evidence"
  skip "status release binding check skipped for dry-run evidence"
  skip "getting-started binding check skipped for dry-run evidence"
  skip "public DNS safety check skipped for dry-run evidence"
  skip "public API TLS safety check skipped for dry-run evidence"
  skip "public API security headers safety check skipped for dry-run evidence"
  skip "public API surface safety check skipped for dry-run evidence"
  skip "public operator auth boundary check skipped for dry-run evidence"
  skip "external public endpoint content checks skipped for dry-run evidence"
  skip "external public config binding check skipped for dry-run evidence"
  skip "external public pool binding check skipped for dry-run evidence"
  skip "external public getting-started binding check skipped for dry-run evidence"
  skip "public Stratum port tiers safety check skipped for dry-run evidence"
  skip "public Stratum port tier smoke check skipped for dry-run evidence"
  skip "external public Stratum smoke check skipped for dry-run evidence"
  skip "external public Stratum load check skipped for dry-run evidence"
  skip "HTTP JSON content checks skipped for dry-run evidence"
  skip "operator CSV content checks skipped for dry-run evidence"
  skip "operator readiness safety check skipped for dry-run evidence"
  skip "operator payout status content check skipped for dry-run evidence"
  skip "payout limit safety check skipped for dry-run evidence"
  skip "payout launch safety check skipped for dry-run evidence"
  skip "payout controls safety check skipped for dry-run evidence"
  skip "release artifact checksum content check skipped for dry-run evidence"
  skip "restore drill completion check skipped for dry-run evidence"
  skip "stratum TCP probe success check skipped for dry-run evidence"
else
  fail "summary must not be dry-run for launch evidence"
fi

require_text "$REPORT_TXT" "status=passed" "report status passed"
require_text "$MANIFEST_TXT" "status=passed" "manifest status passed"
require_text "$MANIFEST_TXT" "fail=0" "manifest fail count is zero"

printf 'extracted_dir=%s\n' "$WORK_DIR"
printf 'summary: pass=%s fail=%s skip=%s\n' "$PASS" "$FAIL" "$SKIP"
if [[ "$FAIL" -gt 0 ]]; then
  exit 1
fi

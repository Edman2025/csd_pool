#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
BIN_DIR="${CSD_POOL_BIN_DIR:-/opt/csd-pool/bin}"
WORKERS_BIN="${CSD_POOL_WORKERS_BIN:-$BIN_DIR/csd-pool-workers}"
ENV_PATH="${CSD_POOL_ENV_FILE:-/etc/csd-pool/csd-pool.env}"
CONFIG_PATH="${CSD_POOL_GO_LIVE_CONFIG:-${CSD_POOL_CONFIG:-/etc/csd-pool/config.toml}}"
HAPROXY_CONFIG="${CSD_POOL_HAPROXY_CONFIG:-/etc/haproxy/haproxy.cfg}"
API_URL="${CSD_POOL_GO_LIVE_API_URL:-${CSD_POOL_VERIFY_API_URL:-http://127.0.0.1:8080}}"
STRATUM_ADDR="${CSD_POOL_GO_LIVE_STRATUM_ADDR:-${CSD_POOL_VERIFY_STRATUM_ADDR:-127.0.0.1:3333}}"
PUBLIC_API_URL="${CSD_POOL_GO_LIVE_PUBLIC_API_URL:-${CSD_POOL_PUBLIC_API_URL:-}}"
PUBLIC_STRATUM_ADDR="${CSD_POOL_GO_LIVE_PUBLIC_STRATUM_ADDR:-${CSD_POOL_PUBLIC_STRATUM_ADDR:-}}"
REPORT_DIR="${CSD_POOL_GO_LIVE_REPORT_DIR:-/tmp/csd-pool-go-live}"
TARGET="${CSD_POOL_GO_LIVE_TARGET:-private-beta}"
DRY_RUN="${CSD_POOL_GO_LIVE_DRY_RUN:-0}"
STATUS_SAMPLE_MAX_AGE_MINUTES="${CSD_POOL_STATUS_SAMPLE_MAX_AGE_MINUTES:-15}"
STATUS_SAMPLE_MAX_CLOCK_SKEW_SECONDS="${CSD_POOL_STATUS_SAMPLE_MAX_CLOCK_SKEW_SECONDS:-300}"
SUMMARY_JSON="$REPORT_DIR/go-live-summary.json"
REPORT_TXT="$REPORT_DIR/GO-LIVE-REPORT.txt"
EVIDENCE_NAME="${CSD_POOL_GO_LIVE_EVIDENCE_NAME:-go-live-evidence}"
EVIDENCE_ARCHIVE="$REPORT_DIR/${EVIDENCE_NAME}.tar.gz"
EVIDENCE_SHA256="$EVIDENCE_ARCHIVE.sha256"

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

json_escape() {
  sed 's/\\/\\\\/g; s/"/\\"/g'
}

json_string() {
  printf '"%s"' "$(printf '%s' "$1" | json_escape)"
}

sha256_value() {
  local path="$1"
  if [[ -f "$path" ]]; then
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum "$path" | awk '{print $1}'
    elif command -v shasum >/dev/null 2>&1; then
      shasum -a 256 "$path" | awk '{print $1}'
    else
      printf 'sha256-tool-missing'
    fi
  else
    printf 'missing'
  fi
}

command_value() {
  if "$@" >/dev/null 2>&1; then
    "$@"
  else
    printf 'unavailable'
  fi
}

normalize_report_paths() {
  mkdir -p "$REPORT_DIR"
  REPORT_DIR="$(cd "$REPORT_DIR" && pwd)"
  SUMMARY_JSON="$REPORT_DIR/go-live-summary.json"
  REPORT_TXT="$REPORT_DIR/GO-LIVE-REPORT.txt"
  EVIDENCE_ARCHIVE="$REPORT_DIR/${EVIDENCE_NAME}.tar.gz"
  EVIDENCE_SHA256="$EVIDENCE_ARCHIVE.sha256"
}

sha256_file_line() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path"
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path"
  else
    printf 'sha256 tool missing: %s\n' "$path" >&2
    return 127
  fi
}

redact_url_value() {
  printf '%s' "$1" | sed -E 's#(postgres(ql)?://[^:/@]+):[^@]*@#\1:<redacted>@#'
}

redact_command_arg() {
  local arg="$1"
  case "$arg" in
    *TOKEN=*|*SECRET=*|*PASSWORD=*|*PRIVATE_KEY=*)
      printf '%s=<redacted>' "${arg%%=*}"
      ;;
    *DATABASE_URL=*|*POSTGRES_URL=*)
      printf '%s=%s' "${arg%%=*}" "$(redact_url_value "${arg#*=}")"
      ;;
    Authorization:*)
      printf 'Authorization: <redacted>'
      ;;
    *)
      printf '%s' "$arg"
      ;;
  esac
}

write_redacted_command() {
  local arg redacted
  for arg in "$@"; do
    redacted="$(redact_command_arg "$arg")"
    printf ' %q' "$redacted"
  done
}

file_mode() {
  stat -f '%Lp' "$1" 2>/dev/null || stat -c '%a' "$1" 2>/dev/null || true
}

env_key_present() {
  local key="$1"
  local value="${!key:-}"
  if [[ -n "$value" ]]; then
    printf 'present'
  else
    printf 'missing'
  fi
}

write_env_snapshot() {
  local log_path="$1"
  local mode world_digit world_readable
  mkdir -p "$REPORT_DIR"
  mode="$(file_mode "$ENV_PATH")"
  world_readable="unknown"
  if [[ -n "$mode" ]]; then
    world_digit="${mode: -1}"
    if [[ "$world_digit" =~ ^[0-7]$ && "$world_digit" -eq 0 ]]; then
      world_readable="false"
    else
      world_readable="true"
    fi
  fi
  {
    printf 'env_path=%s\n' "$ENV_PATH"
    printf 'env_sha256=%s\n' "$(sha256_value "$ENV_PATH")"
    printf 'env_mode=%s\n' "${mode:-unknown}"
    printf 'world_readable=%s\n' "$world_readable"
    printf 'required_keys:\n'
    for key in \
      CSD_POOL_DATABASE_URL \
      CSD_POOL_OPERATOR_TOKEN \
      CSD_POOL_SIGNER_TOKEN \
      CSD_POOL_NODE_TOKEN \
      CSD_POOL_SIGNER_URL \
      CSD_POOL_SIGNER_WALLET_ADDRESS \
      CSD_POOL_WATCH_NODE_URL \
      CSD_POOL_SUBMIT_NODE_URL \
      CSD_POOL_PAYOUT_NODE_URL \
      CSD_POOL_PUBLIC_STRATUM_ADDR \
      CSD_POOL_RESTORE_DATABASE_URL; do
      printf '  %s=%s\n' "$key" "$(env_key_present "$key")"
    done
    printf 'public_keys:\n'
    printf '  CSD_POOL_PUBLIC_STRATUM_ADDR=%s\n' "${CSD_POOL_PUBLIC_STRATUM_ADDR:-missing}"
    printf '  CSD_POOL_PUBLIC_PORT_TIERS=%s\n' "${CSD_POOL_PUBLIC_PORT_TIERS:-missing}"
  } >"$log_path"

  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: environment snapshot review"
  elif [[ "$world_readable" == "false" ]]; then
    ok "environment snapshot captured with restricted mode"
  else
    fail "environment snapshot captured world-readable env mode; see $log_path"
  fi
}

check_secrets_permissions_safety() {
  local log_path="$1"
  mkdir -p "$REPORT_DIR"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: secrets permissions safety"
    {
      printf 'dry-run secrets permissions safety check\n'
      printf 'expected=env/config and optional secret files exist and have no group/other permissions\n'
      printf 'env_path=%s\n' "$ENV_PATH"
      printf 'config_path=%s\n' "$CONFIG_PATH"
      printf 'extra_secret_files=%s\n' "${CSD_POOL_SECRET_FILES:-missing}"
    } >"$log_path"
    return
  fi
  if python3 - "$ENV_PATH" "$CONFIG_PATH" "${CSD_POOL_SECRET_FILES:-}" >"$log_path" 2>&1 <<'PY'
import os
import stat
import sys
from pathlib import Path

env_path, config_path, extra = sys.argv[1:4]
raw_paths = [("env", env_path), ("config", config_path)]
for index, item in enumerate([part.strip() for part in extra.split(",") if part.strip()], start=1):
    raw_paths.append((f"extra_{index}", item))

checks = {}
for label, raw in raw_paths:
    path = Path(raw)
    exists = path.is_file()
    checks[f"{label}_file_exists"] = exists
    print(f"{label}_path={raw or 'missing'}")
    print(f"{label}_file_exists={exists}")
    if not exists:
        print(f"{label}_mode=missing")
        print(f"{label}_group_other_permissions_zero=False")
        checks[f"{label}_group_other_permissions_zero"] = False
        continue
    st = path.stat()
    mode = stat.S_IMODE(st.st_mode)
    group_other = mode & 0o077
    owner_readable = bool(mode & stat.S_IRUSR)
    restricted = group_other == 0 and owner_readable
    print(f"{label}_mode={mode:03o}")
    print(f"{label}_uid={st.st_uid}")
    print(f"{label}_gid={st.st_gid}")
    print(f"{label}_owner_readable={owner_readable}")
    print(f"{label}_group_other_permissions={group_other:03o}")
    print(f"{label}_group_other_permissions_zero={group_other == 0}")
    print(f"{label}_restricted={restricted}")
    checks[f"{label}_owner_readable"] = owner_readable
    checks[f"{label}_group_other_permissions_zero"] = group_other == 0
    checks[f"{label}_restricted"] = restricted

overall = bool(checks) and all(checks.values())
print(f"secret_file_count={len(raw_paths)}")
print(f"secrets_permissions_ok={overall}")
if not overall:
    print("env/config/signing secret files must be regular files readable by owner and inaccessible to group/other before go-live")
    sys.exit(1)
PY
  then
    ok "secrets permissions safety"
  else
    fail "secrets permissions safety; see $log_path"
  fi
}

check_evidence_redaction_safety() {
  local log_path="$1"
  mkdir -p "$REPORT_DIR"
  if python3 - "$REPORT_DIR" "$log_path" >"$log_path.tmp" 2>&1 <<'PY'
import os
import re
import sys
from pathlib import Path

report_dir = Path(sys.argv[1])
log_path = Path(sys.argv[2]).resolve()

rules = [
    ("authorization_bearer", re.compile(r"Authorization:\s*Bearer\s+(?!<redacted>|redacted\b)[A-Za-z0-9._~+/=-]{8,}", re.I)),
    ("sensitive_env_value", re.compile(r"\b[A-Z0-9_]*(?:TOKEN|SECRET|PASSWORD|PRIVATE_KEY)=[^\s`'\"]+", re.I)),
    ("postgres_password_url", re.compile(r"postgres(?:ql)?://[^:/@\s]+:(?!<redacted>@|redacted@)[^@\s]+@", re.I)),
]
allowed_values = {
    "present",
    "missing",
    "<redacted>",
    "redacted",
    "true",
    "false",
}

def is_binary(path):
    try:
        chunk = path.read_bytes()[:4096]
    except OSError:
        return True
    return b"\0" in chunk

def safe_sensitive_assignment(match_text):
    if "=" not in match_text:
        return False
    value = match_text.split("=", 1)[1].strip().strip("'\"`").replace("\\", "")
    return value.lower() in allowed_values

findings = []
checked = 0
for path in sorted(report_dir.rglob("*")):
    if not path.is_file():
        continue
    resolved = path.resolve()
    if resolved == log_path or path.suffix in {".gz"} or path.name.endswith(".sha256"):
        continue
    if is_binary(path):
        continue
    checked += 1
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError as exc:
        findings.append((path, "read_error", str(exc)))
        continue
    for name, pattern in rules:
        for match in pattern.finditer(text):
            matched = match.group(0)
            if name == "sensitive_env_value" and safe_sensitive_assignment(matched):
                continue
            line_no = text.count("\n", 0, match.start()) + 1
            findings.append((path, name, f"line={line_no}"))

print(f"evidence_redaction_checked_files={checked}")
print(f"evidence_redaction_findings={len(findings)}")
for path, name, detail in findings[:100]:
    print(f"finding={path.relative_to(report_dir)}:{name}:{detail}")
ok = len(findings) == 0
print(f"evidence_redaction_ok={ok}")
if not ok:
    print("go-live evidence must not contain bearer tokens, plaintext secret env values, or database URLs with passwords")
    sys.exit(1)
PY
  then
    mv "$log_path.tmp" "$log_path"
    ok "evidence redaction safety"
  else
    mv "$log_path.tmp" "$log_path"
    fail "evidence redaction safety; see $log_path"
  fi
}

resolve_release_manifest() {
  local opt_dir current_release release_name candidate
  if [[ -n "${CSD_POOL_RELEASE_MANIFEST:-}" ]]; then
    printf '%s\n' "$CSD_POOL_RELEASE_MANIFEST"
    return
  fi
  if [[ -f "$BIN_DIR/../RELEASE-MANIFEST.txt" ]]; then
    printf '%s/RELEASE-MANIFEST.txt\n' "$(cd "$BIN_DIR/.." && pwd)"
    return
  fi
  opt_dir="$(cd "$BIN_DIR/.." 2>/dev/null && pwd || true)"
  if [[ -n "$opt_dir" ]]; then
    current_release="$opt_dir/CURRENT_RELEASE"
    if [[ -f "$current_release" ]]; then
      release_name="$(sed -n '1p' "$current_release")"
      candidate="$opt_dir/releases/$release_name/RELEASE-MANIFEST.txt"
      if [[ -f "$candidate" ]]; then
        printf '%s\n' "$candidate"
        return
      fi
    fi
  fi
  printf '\n'
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

check_release_integrity() {
  local log_path="$1"
  local release_manifest release_dir sums
  mkdir -p "$REPORT_DIR"
  release_manifest="$(resolve_release_manifest)"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: release artifact checksum verification"
    {
      printf 'dry-run release integrity check\n'
      printf 'release_manifest=%s\n' "${release_manifest:-unresolved}"
      printf 'expected_command=cd <release-dir> && sha256sum -c SHA256SUMS\n'
    } >"$log_path"
    return
  fi
  if [[ -z "$release_manifest" ]]; then
    fail "release artifact checksum verification: release manifest not found"
    printf 'release manifest not found for BIN_DIR=%s\n' "$BIN_DIR" >"$log_path"
    return
  fi
  if [[ ! -f "$release_manifest" ]]; then
    fail "release artifact checksum verification: release manifest missing: $release_manifest"
    printf 'release manifest missing: %s\n' "$release_manifest" >"$log_path"
    return
  fi
  release_dir="$(cd "$(dirname "$release_manifest")" && pwd)"
  sums="$release_dir/SHA256SUMS"
  if [[ ! -f "$sums" ]]; then
    fail "release artifact checksum verification: SHA256SUMS missing: $sums"
    printf 'SHA256SUMS missing: %s\n' "$sums" >"$log_path"
    return
  fi
  if sha256_check "$sums" >"$log_path" 2>&1; then
    ok "release artifact checksum verification"
  else
    fail "release artifact checksum verification; see $log_path"
  fi
}

write_evidence_archive() {
  local manifest="$REPORT_DIR/EVIDENCE-MANIFEST.txt"
  local sums="$REPORT_DIR/EVIDENCE-SHA256SUMS"
  {
    printf 'name=%s\n' "$EVIDENCE_NAME"
    printf 'created_at_utc=%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    printf 'report=%s\n' "$(basename "$REPORT_TXT")"
    printf 'summary=%s\n' "$(basename "$SUMMARY_JSON")"
    printf 'target=%s\n' "$TARGET"
    printf 'dry_run=%s\n' "$DRY_RUN"
    printf 'status=%s\n' "$([[ "$FAIL" -eq 0 ]] && printf passed || printf failed)"
    printf 'pass=%s\n' "$PASS"
    printf 'fail=%s\n' "$FAIL"
    printf 'skip=%s\n' "$SKIP"
    printf 'files:\n'
    find "$REPORT_DIR" -maxdepth 2 -type f \
      ! -name "$(basename "$EVIDENCE_ARCHIVE")" \
      ! -name "$(basename "$EVIDENCE_SHA256")" \
      -print | sort | while read -r file; do
        printf '  %s\n' "${file#$REPORT_DIR/}"
      done
  } >"$manifest"

  (
    cd "$REPORT_DIR"
    find . -maxdepth 2 -type f \
      ! -name "$(basename "$EVIDENCE_ARCHIVE")" \
      ! -name "$(basename "$EVIDENCE_SHA256")" \
      ! -name "$(basename "$sums")" \
      -print | sort | while read -r file; do
        sha256_file_line "$file"
      done >"$(basename "$sums")"
  )

  (
    cd "$REPORT_DIR"
    tar -czf "$EVIDENCE_ARCHIVE" \
      --exclude "$(basename "$EVIDENCE_ARCHIVE")" \
      --exclude "$(basename "$EVIDENCE_SHA256")" \
      .
  )
  sha256_file_line "$EVIDENCE_ARCHIVE" >"$EVIDENCE_SHA256"
}

write_final_reports() {
  local finished_at host kernel env_sha config_sha workers_sha release_manifest release_name release_revision release_timestamp
  finished_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  host="$(hostname 2>/dev/null || printf 'unknown')"
  kernel="$(uname -a 2>/dev/null || printf 'unknown')"
  env_sha="$(sha256_value "$ENV_PATH")"
  config_sha="$(sha256_value "$CONFIG_PATH")"
  workers_sha="$(sha256_value "$WORKERS_BIN")"
  release_manifest="$(resolve_release_manifest)"
  release_name="unknown"
  release_revision="unknown"
  release_timestamp="unknown"
  if [[ -f "$release_manifest" ]]; then
    release_name="$(sed -n 's/^name=//p' "$release_manifest" | head -n 1)"
    release_revision="$(sed -n 's/^revision=//p' "$release_manifest" | head -n 1)"
    release_timestamp="$(sed -n 's/^timestamp_utc=//p' "$release_manifest" | head -n 1)"
  fi

  {
    printf 'CSD Pool Go-Live Report\n'
    printf 'finished_at_utc=%s\n' "$finished_at"
    printf 'target=%s\n' "$TARGET"
    printf 'dry_run=%s\n' "$DRY_RUN"
    printf 'host=%s\n' "$host"
    printf 'kernel=%s\n' "$kernel"
    printf 'user=%s\n' "${USER:-unknown}"
    printf 'root=%s\n' "$ROOT_DIR"
    printf 'bin_dir=%s\n' "$BIN_DIR"
    printf 'env=%s\n' "$ENV_PATH"
    printf 'env_sha256=%s\n' "$env_sha"
    printf 'config=%s\n' "$CONFIG_PATH"
    printf 'config_sha256=%s\n' "$config_sha"
    printf 'workers_bin=%s\n' "$WORKERS_BIN"
    printf 'workers_sha256=%s\n' "$workers_sha"
    printf 'release_manifest=%s\n' "${release_manifest:-unknown}"
    printf 'release_name=%s\n' "$release_name"
    printf 'release_revision=%s\n' "$release_revision"
    printf 'release_timestamp_utc=%s\n' "$release_timestamp"
    printf 'api_url=%s\n' "$API_URL"
    printf 'stratum_addr=%s\n' "$STRATUM_ADDR"
    printf 'public_api_url=%s\n' "${PUBLIC_API_URL:-missing}"
    printf 'public_stratum_probe_addr=%s\n' "${PUBLIC_STRATUM_ADDR:-missing}"
    printf 'public_stratum_addr=%s\n' "${CSD_POOL_PUBLIC_STRATUM_ADDR:-missing}"
    printf 'public_port_tiers=%s\n' "${CSD_POOL_PUBLIC_PORT_TIERS:-missing}"
    printf 'pass=%s\n' "$PASS"
    printf 'fail=%s\n' "$FAIL"
    printf 'skip=%s\n' "$SKIP"
    printf 'status=%s\n' "$([[ "$FAIL" -eq 0 ]] && printf passed || printf failed)"
    printf 'logs:\n'
    printf '  config_snapshot=%s\n' "$REPORT_DIR/config-snapshot.json"
    printf '  preflight=%s\n' "$REPORT_DIR/preflight.log"
    printf '  env_snapshot=%s\n' "$REPORT_DIR/env-snapshot.txt"
    printf '  secrets_permissions_safety=%s\n' "$REPORT_DIR/secrets-permissions-safety.log"
    printf '  evidence_redaction_safety=%s\n' "$REPORT_DIR/evidence-redaction-safety.log"
    printf '  real_env_readiness=%s\n' "$REPORT_DIR/real-env-readiness.log"
    printf '  clock_safety=%s\n' "$REPORT_DIR/clock-safety.log"
    printf '  disk_safety=%s\n' "$REPORT_DIR/disk-safety.log"
    printf '  bind_safety=%s\n' "$REPORT_DIR/bind-safety.log"
    printf '  edge_proxy_safety=%s\n' "$REPORT_DIR/edge-proxy-safety.log"
    printf '  database_migration=%s\n' "$REPORT_DIR/database-migration.json"
    printf '  database_migration_safety=%s\n' "$REPORT_DIR/database-migration-safety.log"
    printf '  database_runtime=%s\n' "$REPORT_DIR/database-runtime.json"
    printf '  release_integrity=%s\n' "$REPORT_DIR/release-integrity.log"
    printf '  verify=%s\n' "$REPORT_DIR/verify.log"
    printf '  systemd_runtime_safety=%s\n' "$REPORT_DIR/systemd-runtime-safety.log"
    printf '  runtime_hardening_safety=%s\n' "$REPORT_DIR/runtime-hardening-safety.log"
    printf '  resource_limit_safety=%s\n' "$REPORT_DIR/resource-limit-safety.log"
    printf '  service_provenance_safety=%s\n' "$REPORT_DIR/service-provenance-safety.log"
    printf '  backup_artifact_safety=%s\n' "$REPORT_DIR/backup-artifact-safety.log"
    printf '  restore_drill=%s\n' "$REPORT_DIR/restore-drill.log"
    printf '  restore_api_safety=%s\n' "$REPORT_DIR/restore-api-safety.log"
    printf '  restore_http_health=%s\n' "$REPORT_DIR/restore-http-health.json"
    printf '  restore_http_pool=%s\n' "$REPORT_DIR/restore-http-pool.json"
    printf '  restore_http_blocks=%s\n' "$REPORT_DIR/restore-http-blocks.json"
    printf '  restore_http_payments=%s\n' "$REPORT_DIR/restore-http-payments.json"
    printf '  restore_http_operator_payout_status=%s\n' "$REPORT_DIR/restore-http-operator-payout-status.json"
    printf '  check_node_template=%s\n' "$REPORT_DIR/check-node-template.json"
    printf '  node_runtime=%s\n' "$REPORT_DIR/node-runtime.json"
    printf '  node_endpoint_safety=%s\n' "$REPORT_DIR/node-endpoint-safety.log"
    printf '  check_signer=%s\n' "$REPORT_DIR/check-signer.json"
    printf '  signer_safety=%s\n' "$REPORT_DIR/signer-safety.log"
    printf '  sample_health=%s\n' "$REPORT_DIR/sample-health.json"
    printf '  payout_preview=%s\n' "$REPORT_DIR/payout-preview.json"
    printf '  payout_limit_safety=%s\n' "$REPORT_DIR/payout-limit-safety.log"
    printf '  payout_safety=%s\n' "$REPORT_DIR/payout-safety.log"
    printf '  payout_controls_safety=%s\n' "$REPORT_DIR/payout-controls-safety.log"
    printf '  runtime_config_binding=%s\n' "$REPORT_DIR/runtime-config-binding.log"
    printf '  runtime_status_binding=%s\n' "$REPORT_DIR/runtime-status-binding.log"
    printf '  status_release_binding=%s\n' "$REPORT_DIR/status-release-binding.log"
    printf '  pool_endpoint_binding=%s\n' "$REPORT_DIR/pool-endpoint-binding.log"
    printf '  external_public_status_binding=%s\n' "$REPORT_DIR/external-public-status-binding.log"
    printf '  external_public_pool_binding=%s\n' "$REPORT_DIR/external-public-pool-binding.log"
    printf '  external_public_config_binding=%s\n' "$REPORT_DIR/external-public-config-binding.log"
    printf '  getting_started_binding=%s\n' "$REPORT_DIR/getting-started-binding.log"
    printf '  external_public_getting_started_binding=%s\n' "$REPORT_DIR/external-public-getting-started-binding.log"
    printf '  public_dns_safety=%s\n' "$REPORT_DIR/public-dns-safety.log"
    printf '  public_api_tls_safety=%s\n' "$REPORT_DIR/public-api-tls-safety.log"
    printf '  public_api_headers_safety=%s\n' "$REPORT_DIR/public-api-headers-safety.log"
    printf '  public_api_surface_safety=%s\n' "$REPORT_DIR/public-api-surface-safety.log"
    printf '  public_operator_auth_boundary=%s\n' "$REPORT_DIR/public-operator-auth-boundary.log"
    printf '  metrics_surface_safety=%s\n' "$REPORT_DIR/metrics-surface-safety.log"
    printf '  http_api_status=%s\n' "$REPORT_DIR/http-api-status.json"
    printf '  http_api_pool=%s\n' "$REPORT_DIR/http-api-pool.json"
    printf '  http_api_metrics=%s\n' "$REPORT_DIR/http-api-metrics.json"
    printf '  http_prometheus_metrics=%s\n' "$REPORT_DIR/http-prometheus-metrics.txt"
    printf '  http_api_blocks=%s\n' "$REPORT_DIR/http-api-blocks.json"
    printf '  http_api_payments=%s\n' "$REPORT_DIR/http-api-payments.json"
    printf '  http_api_getting_started=%s\n' "$REPORT_DIR/http-api-getting-started.json"
    printf '  http_public_api_status=%s\n' "$REPORT_DIR/http-public-api-status.json"
    printf '  http_public_api_pool=%s\n' "$REPORT_DIR/http-public-api-pool.json"
    printf '  http_public_api_getting_started=%s\n' "$REPORT_DIR/http-public-api-getting-started.json"
    printf '  http_public_getting_started=%s\n' "$REPORT_DIR/http-public-getting-started.txt"
    printf '  public_stratum_tcp=%s\n' "$REPORT_DIR/public-stratum-tcp.log"
    printf '  public_port_tiers_safety=%s\n' "$REPORT_DIR/public-port-tiers-safety.log"
    printf '  public_port_tiers_smoke=%s\n' "$REPORT_DIR/public-port-tiers-smoke.json"
    printf '  public_stratum_smoke=%s\n' "$REPORT_DIR/public-stratum-smoke.json"
    printf '  public_stratum_load=%s\n' "$REPORT_DIR/public-stratum-load.json"
    printf '  stratum_tcp=%s\n' "$REPORT_DIR/stratum-tcp.log"
    printf '  operator_health=%s\n' "$REPORT_DIR/http-operator-health.json"
    printf '  operator_alerts=%s\n' "$REPORT_DIR/http-operator-alerts.json"
    printf '  operator_readiness_safety=%s\n' "$REPORT_DIR/operator-readiness-safety.log"
    printf '  operator_payout_batches_json=%s\n' "$REPORT_DIR/http-operator-payout-batches.json"
    printf '  operator_payout_batches_csv=%s\n' "$REPORT_DIR/http-operator-payout-batches.csv"
    printf '  operator_payout_audit=%s\n' "$REPORT_DIR/http-operator-payout-audit.json"
    printf '  operator_payout_audit_csv=%s\n' "$REPORT_DIR/http-operator-payout-audit.csv"
    printf '  operator_payout_preview=%s\n' "$REPORT_DIR/http-operator-payout-preview.json"
    printf '  operator_payout_status=%s\n' "$REPORT_DIR/http-operator-payout-status.json"
    printf 'evidence_archive=%s\n' "$EVIDENCE_ARCHIVE"
    printf 'evidence_sha256=%s\n' "$EVIDENCE_SHA256"
  } >"$REPORT_TXT"

  {
    printf '{\n'
    printf '  "finished_at_utc": '; json_string "$finished_at"; printf ',\n'
    printf '  "target": '; json_string "$TARGET"; printf ',\n'
    printf '  "dry_run": %s,\n' "$([[ "$DRY_RUN" == "1" ]] && printf true || printf false)"
    printf '  "host": '; json_string "$host"; printf ',\n'
    printf '  "kernel": '; json_string "$kernel"; printf ',\n'
    printf '  "user": '; json_string "${USER:-unknown}"; printf ',\n'
    printf '  "root": '; json_string "$ROOT_DIR"; printf ',\n'
    printf '  "bin_dir": '; json_string "$BIN_DIR"; printf ',\n'
    printf '  "env": {"path": '; json_string "$ENV_PATH"; printf ', "sha256": '; json_string "$env_sha"; printf '},\n'
    printf '  "config": {"path": '; json_string "$CONFIG_PATH"; printf ', "sha256": '; json_string "$config_sha"; printf '},\n'
    printf '  "workers": {"path": '; json_string "$WORKERS_BIN"; printf ', "sha256": '; json_string "$workers_sha"; printf '},\n'
    printf '  "release": {"manifest": '; json_string "${release_manifest:-unknown}"; printf ', "name": '; json_string "$release_name"; printf ', "revision": '; json_string "$release_revision"; printf ', "timestamp_utc": '; json_string "$release_timestamp"; printf '},\n'
    printf '  "endpoints": {"api_url": '; json_string "$API_URL"; printf ', "stratum_addr": '; json_string "$STRATUM_ADDR"; printf ', "public_api_url": '; json_string "${PUBLIC_API_URL:-missing}"; printf ', "public_stratum_probe_addr": '; json_string "${PUBLIC_STRATUM_ADDR:-missing}"; printf ', "public_stratum_addr": '; json_string "${CSD_POOL_PUBLIC_STRATUM_ADDR:-missing}"; printf ', "public_port_tiers": '; json_string "${CSD_POOL_PUBLIC_PORT_TIERS:-missing}"; printf '},\n'
    printf '  "summary": {"pass": %s, "fail": %s, "skip": %s, "status": ' "$PASS" "$FAIL" "$SKIP"
    json_string "$([[ "$FAIL" -eq 0 ]] && printf passed || printf failed)"
    printf '},\n'
    printf '  "logs": {\n'
    printf '    "config_snapshot": '; json_string "$REPORT_DIR/config-snapshot.json"; printf ',\n'
    printf '    "preflight": '; json_string "$REPORT_DIR/preflight.log"; printf ',\n'
    printf '    "env_snapshot": '; json_string "$REPORT_DIR/env-snapshot.txt"; printf ',\n'
    printf '    "secrets_permissions_safety": '; json_string "$REPORT_DIR/secrets-permissions-safety.log"; printf ',\n'
    printf '    "evidence_redaction_safety": '; json_string "$REPORT_DIR/evidence-redaction-safety.log"; printf ',\n'
    printf '    "real_env_readiness": '; json_string "$REPORT_DIR/real-env-readiness.log"; printf ',\n'
    printf '    "clock_safety": '; json_string "$REPORT_DIR/clock-safety.log"; printf ',\n'
    printf '    "disk_safety": '; json_string "$REPORT_DIR/disk-safety.log"; printf ',\n'
    printf '    "bind_safety": '; json_string "$REPORT_DIR/bind-safety.log"; printf ',\n'
    printf '    "edge_proxy_safety": '; json_string "$REPORT_DIR/edge-proxy-safety.log"; printf ',\n'
    printf '    "database_migration": '; json_string "$REPORT_DIR/database-migration.json"; printf ',\n'
    printf '    "database_migration_safety": '; json_string "$REPORT_DIR/database-migration-safety.log"; printf ',\n'
    printf '    "database_runtime": '; json_string "$REPORT_DIR/database-runtime.json"; printf ',\n'
    printf '    "release_integrity": '; json_string "$REPORT_DIR/release-integrity.log"; printf ',\n'
    printf '    "verify": '; json_string "$REPORT_DIR/verify.log"; printf ',\n'
    printf '    "systemd_runtime_safety": '; json_string "$REPORT_DIR/systemd-runtime-safety.log"; printf ',\n'
    printf '    "runtime_hardening_safety": '; json_string "$REPORT_DIR/runtime-hardening-safety.log"; printf ',\n'
    printf '    "resource_limit_safety": '; json_string "$REPORT_DIR/resource-limit-safety.log"; printf ',\n'
    printf '    "service_provenance_safety": '; json_string "$REPORT_DIR/service-provenance-safety.log"; printf ',\n'
    printf '    "backup_artifact_safety": '; json_string "$REPORT_DIR/backup-artifact-safety.log"; printf ',\n'
    printf '    "restore_drill": '; json_string "$REPORT_DIR/restore-drill.log"; printf ',\n'
    printf '    "restore_api_safety": '; json_string "$REPORT_DIR/restore-api-safety.log"; printf ',\n'
    printf '    "restore_http_health": '; json_string "$REPORT_DIR/restore-http-health.json"; printf ',\n'
    printf '    "restore_http_pool": '; json_string "$REPORT_DIR/restore-http-pool.json"; printf ',\n'
    printf '    "restore_http_blocks": '; json_string "$REPORT_DIR/restore-http-blocks.json"; printf ',\n'
    printf '    "restore_http_payments": '; json_string "$REPORT_DIR/restore-http-payments.json"; printf ',\n'
    printf '    "restore_http_operator_payout_status": '; json_string "$REPORT_DIR/restore-http-operator-payout-status.json"; printf ',\n'
    printf '    "check_node_template": '; json_string "$REPORT_DIR/check-node-template.json"; printf ',\n'
    printf '    "node_runtime": '; json_string "$REPORT_DIR/node-runtime.json"; printf ',\n'
    printf '    "node_endpoint_safety": '; json_string "$REPORT_DIR/node-endpoint-safety.log"; printf ',\n'
    printf '    "check_signer": '; json_string "$REPORT_DIR/check-signer.json"; printf ',\n'
    printf '    "signer_safety": '; json_string "$REPORT_DIR/signer-safety.log"; printf ',\n'
    printf '    "sample_health": '; json_string "$REPORT_DIR/sample-health.json"; printf ',\n'
    printf '    "payout_preview": '; json_string "$REPORT_DIR/payout-preview.json"; printf ',\n'
    printf '    "payout_limit_safety": '; json_string "$REPORT_DIR/payout-limit-safety.log"; printf ',\n'
    printf '    "payout_safety": '; json_string "$REPORT_DIR/payout-safety.log"; printf ',\n'
    printf '    "payout_controls_safety": '; json_string "$REPORT_DIR/payout-controls-safety.log"; printf ',\n'
    printf '    "runtime_config_binding": '; json_string "$REPORT_DIR/runtime-config-binding.log"; printf ',\n'
    printf '    "runtime_status_binding": '; json_string "$REPORT_DIR/runtime-status-binding.log"; printf ',\n'
    printf '    "status_release_binding": '; json_string "$REPORT_DIR/status-release-binding.log"; printf ',\n'
    printf '    "pool_endpoint_binding": '; json_string "$REPORT_DIR/pool-endpoint-binding.log"; printf ',\n'
    printf '    "external_public_status_binding": '; json_string "$REPORT_DIR/external-public-status-binding.log"; printf ',\n'
    printf '    "external_public_pool_binding": '; json_string "$REPORT_DIR/external-public-pool-binding.log"; printf ',\n'
    printf '    "external_public_config_binding": '; json_string "$REPORT_DIR/external-public-config-binding.log"; printf ',\n'
    printf '    "getting_started_binding": '; json_string "$REPORT_DIR/getting-started-binding.log"; printf ',\n'
    printf '    "external_public_getting_started_binding": '; json_string "$REPORT_DIR/external-public-getting-started-binding.log"; printf ',\n'
    printf '    "public_dns_safety": '; json_string "$REPORT_DIR/public-dns-safety.log"; printf ',\n'
    printf '    "public_api_tls_safety": '; json_string "$REPORT_DIR/public-api-tls-safety.log"; printf ',\n'
    printf '    "public_api_headers_safety": '; json_string "$REPORT_DIR/public-api-headers-safety.log"; printf ',\n'
    printf '    "public_api_surface_safety": '; json_string "$REPORT_DIR/public-api-surface-safety.log"; printf ',\n'
    printf '    "public_operator_auth_boundary": '; json_string "$REPORT_DIR/public-operator-auth-boundary.log"; printf ',\n'
    printf '    "metrics_surface_safety": '; json_string "$REPORT_DIR/metrics-surface-safety.log"; printf ',\n'
    printf '    "http_api_status": '; json_string "$REPORT_DIR/http-api-status.json"; printf ',\n'
    printf '    "http_api_pool": '; json_string "$REPORT_DIR/http-api-pool.json"; printf ',\n'
    printf '    "http_api_metrics": '; json_string "$REPORT_DIR/http-api-metrics.json"; printf ',\n'
    printf '    "http_prometheus_metrics": '; json_string "$REPORT_DIR/http-prometheus-metrics.txt"; printf ',\n'
    printf '    "http_api_blocks": '; json_string "$REPORT_DIR/http-api-blocks.json"; printf ',\n'
    printf '    "http_api_payments": '; json_string "$REPORT_DIR/http-api-payments.json"; printf ',\n'
    printf '    "http_api_getting_started": '; json_string "$REPORT_DIR/http-api-getting-started.json"; printf ',\n'
    printf '    "http_public_api_status": '; json_string "$REPORT_DIR/http-public-api-status.json"; printf ',\n'
    printf '    "http_public_api_pool": '; json_string "$REPORT_DIR/http-public-api-pool.json"; printf ',\n'
    printf '    "http_public_api_getting_started": '; json_string "$REPORT_DIR/http-public-api-getting-started.json"; printf ',\n'
    printf '    "http_public_getting_started": '; json_string "$REPORT_DIR/http-public-getting-started.txt"; printf ',\n'
    printf '    "public_stratum_tcp": '; json_string "$REPORT_DIR/public-stratum-tcp.log"; printf ',\n'
    printf '    "public_port_tiers_safety": '; json_string "$REPORT_DIR/public-port-tiers-safety.log"; printf ',\n'
    printf '    "public_port_tiers_smoke": '; json_string "$REPORT_DIR/public-port-tiers-smoke.json"; printf ',\n'
    printf '    "public_stratum_smoke": '; json_string "$REPORT_DIR/public-stratum-smoke.json"; printf ',\n'
    printf '    "public_stratum_load": '; json_string "$REPORT_DIR/public-stratum-load.json"; printf ',\n'
    printf '    "stratum_tcp": '; json_string "$REPORT_DIR/stratum-tcp.log"; printf ',\n'
    printf '    "operator_health": '; json_string "$REPORT_DIR/http-operator-health.json"; printf ',\n'
    printf '    "operator_alerts": '; json_string "$REPORT_DIR/http-operator-alerts.json"; printf ',\n'
    printf '    "operator_readiness_safety": '; json_string "$REPORT_DIR/operator-readiness-safety.log"; printf ',\n'
    printf '    "operator_payout_batches_json": '; json_string "$REPORT_DIR/http-operator-payout-batches.json"; printf ',\n'
    printf '    "operator_payout_batches_csv": '; json_string "$REPORT_DIR/http-operator-payout-batches.csv"; printf ',\n'
    printf '    "operator_payout_audit": '; json_string "$REPORT_DIR/http-operator-payout-audit.json"; printf ',\n'
    printf '    "operator_payout_audit_csv": '; json_string "$REPORT_DIR/http-operator-payout-audit.csv"; printf ',\n'
    printf '    "operator_payout_preview": '; json_string "$REPORT_DIR/http-operator-payout-preview.json"; printf ',\n'
    printf '    "operator_payout_status": '; json_string "$REPORT_DIR/http-operator-payout-status.json"; printf '\n'
    printf '  },\n'
    printf '  "evidence": {"archive": '; json_string "$EVIDENCE_ARCHIVE"; printf ', "sha256": '; json_string "$EVIDENCE_SHA256"; printf '}\n'
    printf '}\n'
  } >"$SUMMARY_JSON"
}

is_example_path() {
  case "$1" in
    *.example|*.sample|*.template|*/ops/env/*|*/ops/config.private-beta.toml)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

require_file() {
  local path="$1"
  local label="$2"
  if [[ -f "$path" ]]; then
    ok "$label exists: $path"
  else
    fail "$label missing: $path"
  fi
}

require_real_file() {
  local path="$1"
  local label="$2"
  require_file "$path" "$label"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "$label real-file check disabled in dry-run"
    return
  fi
  if is_example_path "$path"; then
    fail "$label must not be an example/template path: $path"
  else
    ok "$label is not an example/template"
  fi
}

require_executable_or_cargo() {
  local path="$1"
  local label="$2"
  if [[ -x "$path" ]]; then
    ok "$label executable: $path"
  elif command -v cargo >/dev/null 2>&1; then
    skip "$label binary missing; cargo fallback is available"
  else
    fail "$label binary missing and cargo unavailable: $path"
  fi
}

load_env_file() {
  if [[ ! -f "$ENV_PATH" ]]; then
    return
  fi
  set -a
  # shellcheck disable=SC1090
  source "$ENV_PATH"
  set +a
  ok "environment file loaded"
}

require_env() {
  local name="$1"
  local value="${!name:-}"
  if [[ -n "$value" ]]; then
    ok "env present: $name"
  elif [[ "$DRY_RUN" == "1" ]]; then
    skip "env missing in dry-run: $name"
  else
    fail "env missing: $name"
  fi
}

require_no_placeholder_env() {
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "placeholder scan disabled in dry-run"
    return
  fi
  if grep -Eiq '(change-me|dev-secret|example-secret|placeholder|replace-me)' "$ENV_PATH"; then
    fail "environment file contains placeholder values"
  else
    ok "environment file has no common placeholders"
  fi
}

is_loopback_host() {
  local host="$1"
  case "$host" in
    localhost|127.*|0.0.0.0|::1|\[::1\])
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

url_scheme() {
  local url="$1"
  python3 - "$url" <<'PY'
import sys
from urllib.parse import urlparse

parsed = urlparse(sys.argv[1])
print(parsed.scheme or "")
PY
}

write_real_env_readiness() {
  local log_path="$1"
  local database_scheme watch_scheme submit_scheme payout_scheme signer_scheme
  local operator_token signer_token operator_len signer_token_len restore_database_url
  local signer_wallet_address signer_wallet_normalized signer_wallet_valid
  mkdir -p "$REPORT_DIR"
  operator_token="${CSD_POOL_OPERATOR_TOKEN:-}"
  signer_token="${CSD_POOL_SIGNER_TOKEN:-}"
  operator_len="${#operator_token}"
  signer_token_len="${#signer_token}"
  restore_database_url="${CSD_POOL_RESTORE_DATABASE_URL:-}"
  signer_wallet_address="${CSD_POOL_SIGNER_WALLET_ADDRESS:-}"
  signer_wallet_normalized="$(
    python3 - "$signer_wallet_address" <<'PY'
import sys
value = sys.argv[1].strip().lower()
if value.startswith("0x"):
    value = value[2:]
print(value)
PY
  )"
  if [[ "$signer_wallet_normalized" =~ ^[0-9a-f]{40}$ ]]; then
    signer_wallet_valid="true"
  else
    signer_wallet_valid="false"
  fi
  database_scheme="$(url_scheme "${CSD_POOL_DATABASE_URL:-}" 2>/dev/null || true)"
  watch_scheme="$(url_scheme "${CSD_POOL_WATCH_NODE_URL:-}" 2>/dev/null || true)"
  submit_scheme="$(url_scheme "${CSD_POOL_SUBMIT_NODE_URL:-}" 2>/dev/null || true)"
  payout_scheme="$(url_scheme "${CSD_POOL_PAYOUT_NODE_URL:-}" 2>/dev/null || true)"
  signer_scheme="$(url_scheme "${CSD_POOL_SIGNER_URL:-}" 2>/dev/null || true)"
  {
    printf 'target=%s\n' "$TARGET"
    printf 'dry_run=%s\n' "$DRY_RUN"
    printf 'env_path=%s\n' "$ENV_PATH"
    printf 'config_path=%s\n' "$CONFIG_PATH"
    printf 'database_url_present=%s\n' "$(env_key_present CSD_POOL_DATABASE_URL)"
    printf 'database_url_scheme=%s\n' "${database_scheme:-missing}"
    printf 'watch_node_url_present=%s\n' "$(env_key_present CSD_POOL_WATCH_NODE_URL)"
    printf 'watch_node_url_scheme=%s\n' "${watch_scheme:-missing}"
    printf 'submit_node_url_present=%s\n' "$(env_key_present CSD_POOL_SUBMIT_NODE_URL)"
    printf 'submit_node_url_scheme=%s\n' "${submit_scheme:-missing}"
    printf 'payout_node_url_present=%s\n' "$(env_key_present CSD_POOL_PAYOUT_NODE_URL)"
    printf 'payout_node_url_scheme=%s\n' "${payout_scheme:-missing}"
    printf 'signer_url_present=%s\n' "$(env_key_present CSD_POOL_SIGNER_URL)"
    printf 'signer_url_scheme=%s\n' "${signer_scheme:-missing}"
    printf 'signer_wallet_address_present=%s\n' "$(env_key_present CSD_POOL_SIGNER_WALLET_ADDRESS)"
    printf 'signer_wallet_address_valid=%s\n' "$signer_wallet_valid"
    if [[ "$signer_wallet_normalized" == "0123456789abcdef0123456789abcdef01234567" ]]; then
      printf 'signer_wallet_address_not_example=false\n'
    else
      printf 'signer_wallet_address_not_example=true\n'
    fi
    printf 'operator_token_length=%s\n' "$operator_len"
    printf 'signer_token_length=%s\n' "$signer_token_len"
    printf 'restore_database_url_present=%s\n' "$(env_key_present CSD_POOL_RESTORE_DATABASE_URL)"
    if [[ -n "$restore_database_url" && -n "${CSD_POOL_DATABASE_URL:-}" && "$restore_database_url" == "$CSD_POOL_DATABASE_URL" ]]; then
      printf 'restore_database_separate=false\n'
    elif [[ -n "$restore_database_url" ]]; then
      printf 'restore_database_separate=true\n'
    else
      printf 'restore_database_separate=missing\n'
    fi
  } >"$log_path"

  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: real environment readiness"
    return
  fi

  case "$database_scheme" in
    postgres|postgresql)
      ok "real environment uses PostgreSQL database URL"
      ;;
    *)
      fail "real environment database URL must use postgres/postgresql"
      ;;
  esac
  if [[ -n "${CSD_POOL_WATCH_NODE_URL:-}" && "$watch_scheme" =~ ^https?$ ]]; then
    ok "real environment watch node URL configured"
  else
    fail "real environment watch node URL missing or invalid"
  fi
  if [[ -n "${CSD_POOL_SUBMIT_NODE_URL:-}" && "$submit_scheme" =~ ^https?$ ]]; then
    ok "real environment submit node URL configured"
  else
    fail "real environment submit node URL missing or invalid"
  fi
  if [[ -n "${CSD_POOL_PAYOUT_NODE_URL:-}" && "$payout_scheme" =~ ^https?$ ]]; then
    ok "real environment payout node URL configured"
  else
    fail "real environment payout node URL missing or invalid"
  fi
  if [[ -n "${CSD_POOL_SIGNER_URL:-}" && "$signer_scheme" =~ ^https?$ ]]; then
    ok "real environment signer URL configured"
  else
    fail "real environment signer URL missing or invalid"
  fi
  if [[ -n "$signer_wallet_address" && "$signer_wallet_valid" == "true" && "$signer_wallet_normalized" != "0123456789abcdef0123456789abcdef01234567" ]]; then
    ok "real environment signer wallet address configured"
  else
    fail "real environment signer wallet address missing, invalid, or example"
  fi
  if [[ "$operator_len" -ge 32 ]]; then
    ok "operator token length is production-grade"
  else
    fail "operator token length must be at least 32 characters"
  fi
  if [[ "$signer_token_len" -ge 32 ]]; then
    ok "signer token length is production-grade"
  else
    fail "signer token length must be at least 32 characters"
  fi
  if [[ -n "$restore_database_url" && "$restore_database_url" != "${CSD_POOL_DATABASE_URL:-}" ]]; then
    ok "restore drill database URL is separate from primary"
  else
    fail "restore drill database URL must be set and separate from primary"
  fi
}

url_host() {
  local url="$1"
  python3 - "$url" <<'PY'
import sys
from urllib.parse import urlparse

parsed = urlparse(sys.argv[1])
print(parsed.hostname or "")
PY
}

require_public_endpoint_config() {
  local api_host stratum_host parsed
  if [[ "$TARGET" != "public-beta" && "$TARGET" != "production" ]]; then
    if [[ -n "$PUBLIC_API_URL" ]]; then
      ok "optional public API URL configured"
    else
      skip "public API URL not required for $TARGET"
    fi
    if [[ -n "$PUBLIC_STRATUM_ADDR" ]]; then
      ok "optional public Stratum probe address configured"
    else
      skip "public Stratum probe not required for $TARGET"
    fi
    return
  fi

  if [[ -z "$PUBLIC_API_URL" ]]; then
    fail "public API URL required for $TARGET"
  else
    api_host="$(url_host "$PUBLIC_API_URL" 2>/dev/null || true)"
    if [[ -z "$api_host" ]]; then
      fail "public API URL must include a host"
    elif is_loopback_host "$api_host"; then
      fail "public API URL must not be loopback for $TARGET"
    else
      ok "public API URL is externally scoped"
    fi
    if [[ "$PUBLIC_API_URL" != https://* ]]; then
      fail "public API URL must use https for $TARGET"
    elif [[ "$PUBLIC_API_URL" == https://* ]]; then
      ok "public API URL uses https"
    fi
  fi

  if [[ -z "$PUBLIC_STRATUM_ADDR" ]]; then
    fail "public Stratum probe address required for $TARGET"
  else
    if parsed="$(split_host_port "$PUBLIC_STRATUM_ADDR")"; then
      stratum_host="$(printf '%s\n' "$parsed" | sed -n '1p')"
      if is_loopback_host "$stratum_host"; then
        fail "public Stratum probe address must not be loopback for $TARGET"
      else
        ok "public Stratum probe address is externally scoped"
      fi
    else
      fail "public Stratum probe address must be host:port or [ipv6]:port"
    fi
  fi
}

run_or_plan() {
  local label="$1"
  local log_path="$2"
  shift 2
  mkdir -p "$REPORT_DIR"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: $label"
    printf 'dry-run command for %s:\n' "$label" >"$log_path"
    write_redacted_command "$@" >>"$log_path"
    printf '\n' >>"$log_path"
    return
  fi
  if "$@" >"$log_path" 2>&1; then
    ok "$label"
  else
    fail "$label; see $log_path"
  fi
}

run_workers_command() {
  if [[ -x "$WORKERS_BIN" ]]; then
    "$WORKERS_BIN" "$@"
  elif command -v cargo >/dev/null 2>&1; then
    cargo run --quiet --manifest-path "$ROOT_DIR/Cargo.toml" -p csd-pool-workers -- "$@"
  else
    return 127
  fi
}

run_workers_or_plan() {
  local label="$1"
  local log_path="$2"
  shift 2
  mkdir -p "$REPORT_DIR"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: $label"
    printf 'dry-run workers command for %s:\n' "$label" >"$log_path"
    printf '  CSD_POOL_CONFIG=%q ' "$CONFIG_PATH" >>"$log_path"
    printf '%q ' "$WORKERS_BIN" "$@" >>"$log_path"
    printf '\n' >>"$log_path"
    return
  fi
  if CSD_POOL_CONFIG="$CONFIG_PATH" run_workers_command "$@" >"$log_path" 2>&1; then
    ok "$label"
  else
    fail "$label; see $log_path"
  fi
}

write_config_snapshot() {
  local log_path="$1"
  mkdir -p "$REPORT_DIR"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: redacted config snapshot"
    printf 'dry-run workers command for redacted config snapshot:\n' >"$log_path"
    printf '  CSD_POOL_CHECK_CONFIG_REQUIRE_ENV=1 CSD_POOL_CONFIG=%q ' "$CONFIG_PATH" >>"$log_path"
    printf '%q check-config %q\n' "$WORKERS_BIN" "$CONFIG_PATH" >>"$log_path"
    return
  fi
  if CSD_POOL_CHECK_CONFIG_REQUIRE_ENV=1 \
    CSD_POOL_CONFIG="$CONFIG_PATH" \
    run_workers_command check-config "$CONFIG_PATH" >"$log_path" 2>&1; then
    ok "redacted config snapshot"
  else
    fail "redacted config snapshot; see $log_path"
  fi
}

check_clock_safety() {
  local log_path="$1"
  local now synced ntp synchronized_bool ntp_bool
  mkdir -p "$REPORT_DIR"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: clock safety"
    {
      printf 'dry-run clock safety check\n'
      printf 'expected=timedatectl reports NTPSynchronized=true on the target host\n'
    } >"$log_path"
    return
  fi
  now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if ! command -v timedatectl >/dev/null 2>&1; then
    fail "clock safety: timedatectl not available"
    {
      printf 'checked_at_utc=%s\n' "$now"
      printf 'clock_source=timedatectl\n'
      printf 'timedatectl_available=False\n'
      printf 'clock_synchronized=False\n'
      printf 'clock_reported_utc_present=True\n'
    } >"$log_path"
    return
  fi
  synced="$(timedatectl show -p NTPSynchronized --value 2>/dev/null || true)"
  ntp="$(timedatectl show -p NTP --value 2>/dev/null || true)"
  case "$synced" in
    yes|true|1) synchronized_bool=True ;;
    *) synchronized_bool=False ;;
  esac
  case "$ntp" in
    yes|true|1) ntp_bool=True ;;
    *) ntp_bool=False ;;
  esac
  {
    printf 'checked_at_utc=%s\n' "$now"
    printf 'clock_source=timedatectl\n'
    printf 'timedatectl_available=True\n'
    printf 'timedatectl_ntp_synchronized=%s\n' "${synced:-missing}"
    printf 'timedatectl_ntp=%s\n' "${ntp:-missing}"
    printf 'clock_synchronized=%s\n' "$synchronized_bool"
    printf 'ntp_enabled=%s\n' "$ntp_bool"
    printf 'clock_reported_utc_present=True\n'
  } >"$log_path"
  if [[ "$synchronized_bool" == "True" ]]; then
    ok "clock safety"
  else
    fail "clock safety: system clock is not NTP synchronized; see $log_path"
  fi
}

check_disk_safety() {
  local log_path="$1"
  local min_free_bytes="${CSD_POOL_DISK_MIN_FREE_BYTES:-5368709120}"
  local min_free_inodes="${CSD_POOL_DISK_MIN_FREE_INODES:-10000}"
  local install_root="${CSD_POOL_INSTALL_ROOT:-$(dirname "$BIN_DIR")}"
  local backup_path="${CSD_POOL_BACKUP_PATH:-}"
  local backup_dir=""
  mkdir -p "$REPORT_DIR"
  if [[ -n "$backup_path" ]]; then
    if [[ -d "$backup_path" ]]; then
      backup_dir="$backup_path"
    else
      backup_dir="$(dirname "$backup_path")"
    fi
  fi
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: disk safety"
    {
      printf 'dry-run disk safety check\n'
      printf 'expected=target host filesystems have enough free bytes and inodes for runtime data, reports, and backups\n'
      printf 'disk_min_free_bytes=%s\n' "$min_free_bytes"
      printf 'disk_min_free_inodes=%s\n' "$min_free_inodes"
      printf 'install_root=%s\n' "$install_root"
      printf 'report_dir=%s\n' "$REPORT_DIR"
      printf 'backup_dir=%s\n' "${backup_dir:-missing}"
      printf 'postgres_data_dir=%s\n' "${CSD_POOL_POSTGRES_DATA_DIR:-missing}"
    } >"$log_path"
    return
  fi
  if python3 - "$min_free_bytes" "$min_free_inodes" "$install_root" "$REPORT_DIR" "$backup_dir" "${CSD_POOL_POSTGRES_DATA_DIR:-}" >"$log_path" 2>&1 <<'PY'
import os
import sys
from pathlib import Path

min_free_bytes = int(sys.argv[1])
min_free_inodes = int(sys.argv[2])
raw_paths = [
    ("install_root", sys.argv[3]),
    ("report_dir", sys.argv[4]),
    ("backup_dir", sys.argv[5]),
    ("postgres_data_dir", sys.argv[6]),
]

def existing_stat_path(raw):
    if not raw:
        return None
    path = Path(raw)
    if path.exists():
        return path
    parent = path.parent
    while str(parent) and not parent.exists() and parent != parent.parent:
        parent = parent.parent
    return parent if parent.exists() else None

print(f"disk_min_free_bytes={min_free_bytes}")
print(f"disk_min_free_inodes={min_free_inodes}")
checks = []
seen_devices = set()
for label, raw in raw_paths:
    present = bool(raw)
    stat_path = existing_stat_path(raw)
    print(f"{label}_path={raw or 'missing'}")
    print(f"{label}_present={present}")
    print(f"{label}_stat_path={str(stat_path) if stat_path else 'missing'}")
    if not present:
        if label == "backup_dir":
            checks.append((f"{label}_present", False))
        continue
    if stat_path is None:
        checks.append((f"{label}_stat_path_exists", False))
        continue
    st = os.stat(stat_path)
    vfs = os.statvfs(stat_path)
    device = st.st_dev
    if device in seen_devices:
        print(f"{label}_duplicate_filesystem=True")
        continue
    seen_devices.add(device)
    free_bytes = vfs.f_bavail * vfs.f_frsize
    free_inodes = vfs.f_favail
    bytes_ok = free_bytes >= min_free_bytes
    inodes_ok = free_inodes >= min_free_inodes
    print(f"{label}_filesystem_device={device}")
    print(f"{label}_free_bytes={free_bytes}")
    print(f"{label}_free_inodes={free_inodes}")
    print(f"{label}_free_bytes_ok={bytes_ok}")
    print(f"{label}_free_inodes_ok={inodes_ok}")
    checks.append((f"{label}_free_bytes_ok", bytes_ok))
    checks.append((f"{label}_free_inodes_ok", inodes_ok))

all_ok = bool(checks) and all(passed for _, passed in checks)
for name, passed in checks:
    print(f"check_{name}={'ok' if passed else 'failed'}")
print(f"disk_path_count={len(seen_devices)}")
print(f"disk_all_paths_ok={all_ok}")
if not all_ok:
    print("target filesystems must have enough free bytes and inodes before go-live")
    sys.exit(1)
PY
  then
    ok "disk safety"
  else
    fail "disk safety; see $log_path"
  fi
}

check_bind_safety() {
  local log_path="$1"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: bind safety"
    {
      printf 'dry-run bind safety check\n'
      printf 'expected=api, stratum, and signer internal listeners stay on loopback; public ingress goes through configured public endpoints\n'
    } >"$log_path"
    return
  fi
  if python3 - "$REPORT_DIR/config-snapshot.json" "$TARGET" "$PUBLIC_API_URL" "$PUBLIC_STRATUM_ADDR" >"$log_path" 2>&1 <<'PY'
import ipaddress
import json
import sys
from urllib.parse import urlparse

config_path, target, public_api_url, public_stratum_addr = sys.argv[1:5]
with open(config_path, "r", encoding="utf-8") as f:
    config = json.load(f)

def host_port(value):
    raw = str(value or "")
    if raw.startswith("[") and "]:" in raw:
        host, port = raw[1:].split("]:", 1)
        return host, port
    if ":" in raw:
        host, port = raw.rsplit(":", 1)
        return host, port
    return raw, ""

def is_loopback_host(host):
    host = (host or "").strip().lower()
    if host == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False

def public_host(value):
    if not value:
        return ""
    if "://" in value:
        return (urlparse(value).hostname or "").strip().lower()
    host, _ = host_port(value)
    return host.strip().lower()

checks = {}
for label, field in [
    ("stratum", "stratum_listen"),
    ("api", "api_listen"),
    ("signer", "signer_listen"),
]:
    value = config.get(field) or ""
    host, port = host_port(value)
    loopback = is_loopback_host(host)
    port_present = bool(port)
    print(f"{label}_listen={value}")
    print(f"{label}_listen_host={host or 'missing'}")
    print(f"{label}_listen_port={port or 'missing'}")
    print(f"{label}_listen_loopback={loopback}")
    print(f"{label}_listen_port_present={port_present}")
    checks[f"{label}_listen_loopback"] = loopback
    checks[f"{label}_listen_port_present"] = port_present

public_required = target in {"public-beta", "production"}
api_public_host = public_host(public_api_url)
stratum_public_host = public_host(public_stratum_addr)
checks["public_api_configured_when_required"] = (not public_required) or bool(api_public_host)
checks["public_stratum_configured_when_required"] = (not public_required) or bool(stratum_public_host)
print(f"public_target={public_required}")
print(f"public_api_host={api_public_host or 'missing'}")
print(f"public_stratum_host={stratum_public_host or 'missing'}")
print(f"public_api_configured_when_required={checks['public_api_configured_when_required']}")
print(f"public_stratum_configured_when_required={checks['public_stratum_configured_when_required']}")

overall = all(checks.values())
print(f"bind_safety_ok={overall}")
if not overall:
    print("internal pool listeners must remain loopback-only and public ingress must use the configured edge endpoints")
    sys.exit(1)
PY
  then
    ok "bind safety"
  else
    fail "bind safety; see $log_path"
  fi
}

check_edge_proxy_safety() {
  local log_path="$1"
  if [[ "$TARGET" != "public-beta" && "$TARGET" != "production" ]]; then
    skip "edge proxy safety not required for $TARGET"
    printf 'edge proxy safety not required for %s\n' "$TARGET" >"$log_path"
    return
  fi
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: edge proxy safety"
    {
      printf 'dry-run edge proxy safety check\n'
      printf 'haproxy_config=%s\n' "$HAPROXY_CONFIG"
      printf 'public_stratum_addr=%s\n' "${PUBLIC_STRATUM_ADDR:-missing}"
      printf 'expected=target HAProxy config validates and maps public Stratum/API edge to loopback pool backends\n'
    } >"$log_path"
    return
  fi
  if [[ ! -f "$HAPROXY_CONFIG" ]]; then
    fail "edge proxy safety: HAProxy config missing"
    printf 'haproxy_config=%s\nedge_proxy_config_exists=False\nedge_proxy_safety_ok=False\n' "$HAPROXY_CONFIG" >"$log_path"
    return
  fi

  local haproxy_check_log="$log_path.haproxy"
  local haproxy_validate_ok="False"
  local haproxy_installed="False"
  if command -v haproxy >/dev/null 2>&1; then
    haproxy_installed="True"
    if haproxy -c -f "$HAPROXY_CONFIG" >"$haproxy_check_log" 2>&1; then
      haproxy_validate_ok="True"
    fi
  else
    printf 'haproxy command not installed\n' >"$haproxy_check_log"
  fi

  if python3 - "$HAPROXY_CONFIG" "$REPORT_DIR/config-snapshot.json" "${PUBLIC_STRATUM_ADDR:-}" "$haproxy_installed" "$haproxy_validate_ok" "$haproxy_check_log" >"$log_path" 2>&1 <<'PY'
import ipaddress
import json
import re
import sys

haproxy_path, config_path, public_stratum_addr, haproxy_installed, haproxy_validate_ok, haproxy_log = sys.argv[1:7]
text = open(haproxy_path, "r", encoding="utf-8", errors="replace").read()
with open(config_path, "r", encoding="utf-8") as f:
    pool_config = json.load(f)

def host_port(value):
    raw = str(value or "").strip()
    if raw.startswith("[") and "]:" in raw:
        host, port = raw[1:].split("]:", 1)
        return host, int(port)
    if ":" in raw:
        host, port = raw.rsplit(":", 1)
        return host, int(port)
    raise ValueError(f"expected host:port: {value}")

def is_loopback(host):
    if host.lower() == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False

def section(name):
    pattern = re.compile(rf"(?ms)^(frontend|backend)\s+{re.escape(name)}\s*$([\\s\\S]*?)(?=^(?:frontend|backend|listen|global|defaults)\\s+|\\Z)")
    match = pattern.search(text)
    return match.group(2) if match else ""

def contains_bind_port(section_text, port):
    return re.search(rf"(?m)^\\s*bind\\s+(?:[^\\s:]+:|\\[::\\]:|:)?{port}(?:\\s|$)", section_text) is not None

def contains_server(section_text, name, host, port):
    return re.search(rf"(?m)^\\s*server\\s+{re.escape(name)}\\s+{re.escape(host)}:{port}(?:\\s|$)", section_text) is not None

try:
    _, public_stratum_port = host_port(public_stratum_addr)
except Exception:
    public_stratum_port = 0

stratum_host, stratum_port = host_port(pool_config.get("stratum_listen", ""))
api_host, api_port = host_port(pool_config.get("api_listen", ""))

stratum_frontend = section("csd_stratum_3333")
stratum_backend = section("csd_stratum_backend")
http_frontend = section("csd_http")
api_backend = section("csd_api_backend")

checks = {
    "edge_proxy_config_exists": True,
    "edge_proxy_haproxy_installed": haproxy_installed == "True",
    "edge_proxy_haproxy_config_valid": haproxy_validate_ok == "True",
    "edge_proxy_stratum_public_port_known": public_stratum_port > 0,
    "edge_proxy_stratum_frontend_present": bool(stratum_frontend),
    "edge_proxy_stratum_public_bind_matches": bool(stratum_frontend) and contains_bind_port(stratum_frontend, public_stratum_port),
    "edge_proxy_stratum_stick_table_present": "stick-table type ip" in stratum_frontend,
    "edge_proxy_stratum_connection_cap_present": "tcp-request connection reject" in stratum_frontend and "sc0_conn_cur" in stratum_frontend,
    "edge_proxy_stratum_backend_present": bool(stratum_backend),
    "edge_proxy_stratum_backend_loopback": is_loopback(stratum_host),
    "edge_proxy_stratum_backend_matches_config": bool(stratum_backend) and contains_server(stratum_backend, "local_pool", stratum_host, stratum_port),
    "edge_proxy_http_frontend_present": bool(http_frontend),
    "edge_proxy_http_public_bind_present": bool(http_frontend) and contains_bind_port(http_frontend, 80),
    "edge_proxy_api_backend_present": bool(api_backend),
    "edge_proxy_api_backend_loopback": is_loopback(api_host),
    "edge_proxy_api_backend_matches_config": bool(api_backend) and contains_server(api_backend, "local_api", api_host, api_port),
    "edge_proxy_api_health_check_present": "option httpchk GET /health" in api_backend,
}

print(f"haproxy_config={haproxy_path}")
print(f"haproxy_check_log={haproxy_log}")
print(f"public_stratum_addr={public_stratum_addr or 'missing'}")
print(f"public_stratum_port={public_stratum_port or 'missing'}")
print(f"config_stratum_listen={pool_config.get('stratum_listen', 'missing')}")
print(f"config_api_listen={pool_config.get('api_listen', 'missing')}")
for name, passed in checks.items():
    print(f"{name}={passed}")
overall = all(checks.values())
print(f"edge_proxy_safety_ok={overall}")
if not overall:
    print("edge proxy config must validate and map public Stratum/API ingress to loopback pool backends with expected limits and health checks")
    sys.exit(1)
PY
  then
    ok "edge proxy safety"
  else
    fail "edge proxy safety; see $log_path"
  fi
  rm -f "$haproxy_check_log"
}

check_public_dns_safety() {
  local log_path="$1"
  if [[ "$TARGET" != "public-beta" && "$TARGET" != "production" ]]; then
    skip "public DNS safety not required for $TARGET"
    printf 'public DNS safety not required for %s\n' "$TARGET" >"$log_path"
    return
  fi
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: public DNS safety"
    {
      printf 'dry-run public DNS safety check\n'
      printf 'public_api_url=%s\n' "${PUBLIC_API_URL:-missing}"
      printf 'public_stratum_addr=%s\n' "${PUBLIC_STRATUM_ADDR:-missing}"
      printf 'expected=public API and Stratum hosts resolve only to global addresses\n'
    } >"$log_path"
    return
  fi
  if python3 - "$PUBLIC_API_URL" "$PUBLIC_STRATUM_ADDR" >"$log_path" 2>&1 <<'PY'
import ipaddress
import socket
import sys
from urllib.parse import urlparse

public_api_url, public_stratum_addr = sys.argv[1:3]

def host_from_url(value):
    parsed = urlparse(value)
    return parsed.hostname or ""

def host_from_hostport(value):
    value = value.strip()
    if value.startswith("["):
        end = value.find("]")
        return value[1:end] if end != -1 else ""
    return value.rsplit(":", 1)[0] if ":" in value else value

def resolve(host):
    addresses = []
    if not host:
        return addresses, "missing host"
    try:
        for item in socket.getaddrinfo(host, None, type=socket.SOCK_STREAM):
            address = item[4][0]
            if address not in addresses:
                addresses.append(address)
        return addresses, ""
    except Exception as exc:
        return addresses, str(exc)

def all_global(addresses):
    if not addresses:
        return False
    parsed = []
    for address in addresses:
        try:
            parsed.append(ipaddress.ip_address(address))
        except ValueError:
            return False
    return all(address.is_global for address in parsed)

api_host = host_from_url(public_api_url)
stratum_host = host_from_hostport(public_stratum_addr)
api_addresses, api_error = resolve(api_host)
stratum_addresses, stratum_error = resolve(stratum_host)

checks = {
    "public_api_dns_host_present": bool(api_host),
    "public_api_dns_resolves": bool(api_addresses),
    "public_api_dns_all_global": all_global(api_addresses),
    "public_stratum_dns_host_present": bool(stratum_host),
    "public_stratum_dns_resolves": bool(stratum_addresses),
    "public_stratum_dns_all_global": all_global(stratum_addresses),
}
checks["public_dns_all_global"] = checks["public_api_dns_all_global"] and checks["public_stratum_dns_all_global"]

print(f"public_api_url={public_api_url or 'missing'}")
print(f"public_api_host={api_host or 'missing'}")
print(f"public_api_addresses={','.join(api_addresses) if api_addresses else 'missing'}")
print(f"public_api_dns_error={api_error or 'none'}")
print(f"public_stratum_addr={public_stratum_addr or 'missing'}")
print(f"public_stratum_host={stratum_host or 'missing'}")
print(f"public_stratum_addresses={','.join(stratum_addresses) if stratum_addresses else 'missing'}")
print(f"public_stratum_dns_error={stratum_error or 'none'}")
for name, passed in checks.items():
    print(f"{name}={passed}")
if not all(checks.values()):
    print("public API and Stratum hosts must resolve to global public addresses")
    sys.exit(1)
PY
  then
    ok "public DNS safety"
  else
    fail "public DNS safety; see $log_path"
  fi
}

check_public_api_tls_safety() {
  local log_path="$1"
  local min_valid_days="${CSD_POOL_PUBLIC_API_TLS_MIN_VALID_DAYS:-14}"
  if [[ "$TARGET" != "public-beta" && "$TARGET" != "production" ]]; then
    skip "public API TLS safety not required for $TARGET"
    printf 'public API TLS safety not required for %s\n' "$TARGET" >"$log_path"
    return
  fi
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: public API TLS safety"
    {
      printf 'dry-run public API TLS safety check\n'
      printf 'public_api_url=%s\n' "${PUBLIC_API_URL:-missing}"
      printf 'public_api_tls_min_valid_days=%s\n' "$min_valid_days"
      printf 'expected=https URL with valid certificate, hostname match, and enough remaining validity\n'
    } >"$log_path"
    return
  fi
  if python3 - "$PUBLIC_API_URL" "$min_valid_days" >"$log_path" 2>&1 <<'PY'
import socket
import ssl
import sys
import time
from urllib.parse import urlparse

url = sys.argv[1]
min_valid_days = int(sys.argv[2])
min_valid_seconds = min_valid_days * 24 * 60 * 60
parsed = urlparse(url)
host = parsed.hostname or ""
port = parsed.port or 443
scheme = parsed.scheme
checks = {
    "public_api_url_present": bool(url),
    "public_api_scheme_https": scheme == "https",
    "public_api_host_present": bool(host),
}
subject = issuer = not_after = ""
tls_version = cipher = ""
seconds_remaining = -1
days_remaining = -1
if all(checks.values()):
    try:
        context = ssl.create_default_context()
        with socket.create_connection((host, port), timeout=8) as sock:
            with context.wrap_socket(sock, server_hostname=host) as tls:
                cert = tls.getpeercert()
                tls_version = tls.version() or ""
                cipher_info = tls.cipher()
                cipher = cipher_info[0] if cipher_info else ""
        subject = ",".join(
            "=".join(attribute)
            for rdn in cert.get("subject", [])
            for attribute in rdn
        )
        issuer = ",".join(
            "=".join(attribute)
            for rdn in cert.get("issuer", [])
            for attribute in rdn
        )
        not_after = cert.get("notAfter", "")
        if not_after:
            not_after_epoch = ssl.cert_time_to_seconds(not_after)
            seconds_remaining = int(not_after_epoch - time.time())
            days_remaining = seconds_remaining // (24 * 60 * 60)
        checks.update({
            "public_api_tls_handshake": True,
            "public_api_tls_hostname_valid": True,
            "public_api_tls_not_after_present": bool(not_after),
            "public_api_tls_not_expiring_soon": seconds_remaining >= min_valid_seconds,
        })
    except Exception as exc:
        print(f"tls_error={exc}")
        checks.update({
            "public_api_tls_handshake": False,
            "public_api_tls_hostname_valid": False,
            "public_api_tls_not_after_present": False,
            "public_api_tls_not_expiring_soon": False,
        })
print(f"public_api_url={url or 'missing'}")
print(f"public_api_host={host or 'missing'}")
print(f"public_api_port={port}")
print(f"public_api_tls_min_valid_days={min_valid_days}")
print(f"public_api_tls_min_valid_seconds={min_valid_seconds}")
print(f"tls_version={tls_version or 'missing'}")
print(f"tls_cipher={cipher or 'missing'}")
print(f"cert_subject={subject or 'missing'}")
print(f"cert_issuer={issuer or 'missing'}")
print(f"cert_not_after={not_after or 'missing'}")
print(f"cert_seconds_remaining={seconds_remaining}")
print(f"cert_days_remaining={days_remaining}")
for name, passed in checks.items():
    print(f"{name}={passed}")
if not all(checks.values()):
    print("public API must use HTTPS with a valid certificate for the configured hostname and enough remaining validity")
    sys.exit(1)
PY
  then
    ok "public API TLS safety"
  else
    fail "public API TLS safety; see $log_path"
  fi
}

check_public_api_headers_safety() {
  local log_path="$1"
  if [[ "$TARGET" != "public-beta" && "$TARGET" != "production" ]]; then
    skip "public API security headers not required for $TARGET"
    printf 'public API security headers not required for %s\n' "$TARGET" >"$log_path"
    return
  fi
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: public API security headers"
    {
      printf 'dry-run public API security headers check\n'
      printf 'public_api_url=%s\n' "${PUBLIC_API_URL:-missing}"
      printf 'expected=public /api/status returns CSP, nosniff, DENY frame policy, no-referrer, and Permissions-Policy\n'
    } >"$log_path"
    return
  fi
  if [[ -z "$PUBLIC_API_URL" ]]; then
    fail "public API security headers: public API URL missing"
    printf 'public_api_url=missing\npublic_api_headers_ok=False\n' >"$log_path"
    return
  fi
  if ! command -v curl >/dev/null 2>&1; then
    fail "public API security headers: curl not installed"
    printf 'curl not installed\npublic_api_headers_ok=False\n' >"$log_path"
    return
  fi
  if curl --fail --silent --show-error --max-time 8 -D "$log_path.headers" -o /dev/null "$PUBLIC_API_URL/api/status" 2>"$log_path.curl"; then
    if python3 - "$PUBLIC_API_URL/api/status" "$log_path.headers" >"$log_path" 2>&1 <<'PY'
import sys
from pathlib import Path

url, headers_path = sys.argv[1:3]
raw = Path(headers_path).read_text(encoding="utf-8", errors="replace").splitlines()
headers = {}
status_lines = []
for line in raw:
    line = line.rstrip("\r")
    if not line:
        continue
    if line.lower().startswith("http/"):
        status_lines.append(line)
        headers = {}
        continue
    if ":" not in line:
        continue
    key, value = line.split(":", 1)
    headers[key.strip().lower()] = value.strip()

checks = {
    "content_security_policy_present": bool(headers.get("content-security-policy")),
    "content_security_policy_frame_ancestors_none": "frame-ancestors 'none'" in headers.get("content-security-policy", ""),
    "x_content_type_options_nosniff": headers.get("x-content-type-options", "").lower() == "nosniff",
    "x_frame_options_deny": headers.get("x-frame-options", "").lower() == "deny",
    "referrer_policy_no_referrer": headers.get("referrer-policy", "").lower() == "no-referrer",
    "permissions_policy_present": bool(headers.get("permissions-policy")),
}
print(f"public_api_headers_url={url}")
print(f"public_api_headers_status={status_lines[-1] if status_lines else 'missing'}")
for key in [
    "content-security-policy",
    "x-content-type-options",
    "x-frame-options",
    "referrer-policy",
    "permissions-policy",
]:
    print(f"header_{key}={headers.get(key, 'missing')}")
for name, passed in checks.items():
    print(f"{name}={passed}")
ok = all(checks.values())
print(f"public_api_headers_ok={ok}")
if not ok:
    print("public API edge must preserve baseline browser security headers")
    sys.exit(1)
PY
    then
      ok "public API security headers"
    else
      fail "public API security headers; see $log_path"
    fi
  else
    fail "public API security headers probe failed; see $log_path.curl"
    {
      printf 'public_api_headers_url=%s\n' "$PUBLIC_API_URL/api/status"
      printf 'curl_failed=True\n'
      printf 'public_api_headers_ok=False\n'
    } >"$log_path"
  fi
  rm -f "$log_path.headers" "$log_path.curl"
}

check_public_api_surface_safety() {
  local log_path="$1"
  if [[ "$TARGET" != "public-beta" && "$TARGET" != "production" ]]; then
    skip "public API surface safety not required for $TARGET"
    printf 'public API surface safety not required for %s\n' "$TARGET" >"$log_path"
    return
  fi
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: public API surface safety"
    {
      printf 'dry-run public API surface safety check\n'
      printf 'public_api_url=%s\n' "${PUBLIC_API_URL:-missing}"
      printf 'expected=public JSON endpoints return JSON, no-store cache policy, and no operator-only fields\n'
    } >"$log_path"
    return
  fi
  if [[ -z "$PUBLIC_API_URL" ]]; then
    fail "public API surface safety: public API URL missing"
    printf 'public_api_url=missing\npublic_api_surface_ok=False\n' >"$log_path"
    return
  fi
  if ! command -v curl >/dev/null 2>&1; then
    fail "public API surface safety: curl not installed"
    printf 'curl not installed\npublic_api_surface_ok=False\n' >"$log_path"
    return
  fi

  local temp_dir
  temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-public-api-surface.XXXXXX")"
  python3 - "$PUBLIC_API_URL" "$temp_dir" >"$temp_dir/plan.tsv" <<'PY'
import pathlib
import sys

base = sys.argv[1].rstrip("/")
temp = pathlib.Path(sys.argv[2])
paths = [
    "/api/status",
    "/api/metrics",
    "/api/blocks",
    "/api/payments",
    "/api/getting-started",
]
for index, path in enumerate(paths, start=1):
    print(f"{index}\t{path}\t{base}{path}\t{temp / f'headers-{index}.txt'}\t{temp / f'body-{index}.json'}")
PY
  local curl_failed=0
  local index path url headers body
  while IFS=$'\t' read -r index path url headers body; do
    if ! curl --fail --silent --show-error --max-time 8 -D "$headers" -o "$body" "$url" 2>"$temp_dir/curl-${index}.log"; then
      curl_failed=1
      printf 'curl_failed_%s=%s\n' "$index" "$path" >>"$temp_dir/curl-failures.log"
      cat "$temp_dir/curl-${index}.log" >>"$temp_dir/curl-failures.log"
    fi
  done <"$temp_dir/plan.tsv"

  if python3 - "$PUBLIC_API_URL" "$temp_dir" "$curl_failed" >"$log_path" 2>&1 <<'PY'
import json
import pathlib
import sys

base_url, temp_dir, curl_failed = sys.argv[1], pathlib.Path(sys.argv[2]), sys.argv[3] == "1"
forbidden_keys = {
    "operator_token",
    "signer_token",
    "database_url",
    "signer_url",
    "payouts_enabled",
    "cap_exceeded",
    "daily_cap_exceeded",
    "manual_approval_required",
    "would_create_batch",
    "audit_events",
    "batches",
    "samples",
    "alerts",
}
forbidden_value_fragments = [
    "/api/operator/",
    "bearer ",
    "postgres://",
    "postgresql://",
]

def parse_headers(path):
    headers = {}
    status_lines = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.rstrip("\r")
        if not line:
            continue
        if line.lower().startswith("http/"):
            status_lines.append(line)
            headers = {}
            continue
        if ":" not in line:
            continue
        key, value = line.split(":", 1)
        headers[key.strip().lower()] = value.strip()
    return status_lines, headers

def walk_json(value, prefix="$"):
    if isinstance(value, dict):
        for key, child in value.items():
            yield prefix + "." + str(key), key, child
            yield from walk_json(child, prefix + "." + str(key))
    elif isinstance(value, list):
        for index, child in enumerate(value):
            yield from walk_json(child, f"{prefix}[{index}]")

plan = []
for line in (temp_dir / "plan.tsv").read_text(encoding="utf-8").splitlines():
    index, path, url, headers_path, body_path = line.split("\t", 4)
    plan.append((int(index), path, url, pathlib.Path(headers_path), pathlib.Path(body_path)))

endpoint_results = []
leaks = []
for index, path, url, headers_path, body_path in plan:
    status_lines, headers = parse_headers(headers_path)
    body_text = body_path.read_text(encoding="utf-8", errors="replace") if body_path.exists() else ""
    try:
        parsed = json.loads(body_text)
        json_valid = True
    except Exception as exc:
        parsed = None
        json_valid = False
        leaks.append(f"{path}: invalid json: {exc}")
    content_type = headers.get("content-type", "")
    cache_control = headers.get("cache-control", "")
    cache_lower = cache_control.lower()
    cache_safe = any(marker in cache_lower for marker in ["no-store", "no-cache", "private", "max-age=0"])
    content_type_json = "application/json" in content_type.lower()
    if json_valid:
        for json_path, key, child in walk_json(parsed):
            key_lower = str(key).lower()
            if key_lower in forbidden_keys:
                leaks.append(f"{path}: forbidden key {json_path}")
            if isinstance(child, str):
                lower = child.lower()
                for fragment in forbidden_value_fragments:
                    if fragment in lower:
                        leaks.append(f"{path}: forbidden value fragment {fragment} at {json_path}")
    endpoint_results.append({
        "path": path,
        "status": status_lines[-1] if status_lines else "missing",
        "content_type_json": content_type_json,
        "cache_control_safe": cache_safe,
        "json_valid": json_valid,
        "content_type": content_type or "missing",
        "cache_control": cache_control or "missing",
    })

print(f"public_api_surface_url={base_url}")
print(f"public_api_surface_endpoint_count={len(endpoint_results)}")
for result in endpoint_results:
    safe_name = result["path"].strip("/").replace("/", "_").replace("-", "_")
    print(f"{safe_name}_status={result['status']}")
    print(f"{safe_name}_content_type={result['content_type']}")
    print(f"{safe_name}_cache_control={result['cache_control']}")
    print(f"{safe_name}_json_valid={result['json_valid']}")
    print(f"{safe_name}_content_type_json={result['content_type_json']}")
    print(f"{safe_name}_cache_control_safe={result['cache_control_safe']}")
print(f"public_api_surface_curl_ok={not curl_failed}")
print(f"public_api_surface_all_json_valid={all(item['json_valid'] for item in endpoint_results)}")
print(f"public_api_surface_content_types_ok={all(item['content_type_json'] for item in endpoint_results)}")
print(f"public_api_surface_cache_control_ok={all(item['cache_control_safe'] for item in endpoint_results)}")
print(f"public_api_surface_operator_leak_free={not leaks}")
if leaks:
    for leak in leaks:
        print(f"operator_leak={leak}")
ok = (
    not curl_failed
    and len(endpoint_results) == 5
    and all(item["json_valid"] for item in endpoint_results)
    and all(item["content_type_json"] for item in endpoint_results)
    and all(item["cache_control_safe"] for item in endpoint_results)
    and not leaks
)
print(f"public_api_surface_ok={ok}")
if not ok:
    print("public API edge must serve only reviewed JSON endpoints with safe cache headers and no operator-only fields")
    sys.exit(1)
PY
  then
    ok "public API surface safety"
  else
    fail "public API surface safety; see $log_path"
  fi
  rm -rf "$temp_dir"
}

check_public_operator_auth_boundary() {
  local log_path="$1"
  local path="/api/operator/health"
  if [[ "$TARGET" != "public-beta" && "$TARGET" != "production" ]]; then
    skip "public operator auth boundary not required for $TARGET"
    printf 'public operator auth boundary not required for %s\n' "$TARGET" >"$log_path"
    return
  fi
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: public operator auth boundary"
    {
      printf 'dry-run public operator auth boundary check\n'
      printf 'expected=public operator endpoint rejects missing and invalid bearer tokens, accepts configured operator token\n'
    } >"$log_path"
    return
  fi
  if [[ -z "$PUBLIC_API_URL" ]]; then
    fail "public operator auth boundary: public API URL missing"
    printf 'public_operator_auth_boundary_ok=False\npublic_api_url_present=False\n' >"$log_path"
    return
  fi
  if [[ -z "${CSD_POOL_OPERATOR_TOKEN:-}" ]]; then
    fail "public operator auth boundary: CSD_POOL_OPERATOR_TOKEN missing"
    printf 'public_operator_auth_boundary_ok=False\noperator_token_present=False\n' >"$log_path"
    return
  fi
  if ! command -v curl >/dev/null 2>&1; then
    fail "public operator auth boundary: curl not installed"
    printf 'public_operator_auth_boundary_ok=False\ncurl_present=False\n' >"$log_path"
    return
  fi

  local temp_dir unauth_body wrong_body auth_body
  local unauth_status wrong_status auth_status result
  temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-public-operator-auth.XXXXXX")"
  unauth_body="$temp_dir/unauth.body"
  wrong_body="$temp_dir/wrong.body"
  auth_body="$temp_dir/auth.body"
  unauth_status="$(curl --silent --show-error --max-time 5 -o "$unauth_body" -w '%{http_code}' "$PUBLIC_API_URL$path" 2>/dev/null || printf '000')"
  wrong_status="$(curl --silent --show-error --max-time 5 -o "$wrong_body" -w '%{http_code}' -H 'Authorization: Bearer invalid-public-operator-token' "$PUBLIC_API_URL$path" 2>/dev/null || printf '000')"
  auth_status="$(curl --silent --show-error --max-time 5 -o "$auth_body" -w '%{http_code}' -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" "$PUBLIC_API_URL$path" 2>/dev/null || printf '000')"

  if python3 - "$log_path" "$unauth_status" "$wrong_status" "$auth_status" "$auth_body" <<'PY'
import json
import sys

log_path, unauth_status, wrong_status, auth_status, auth_body_path = sys.argv[1:6]
checks = {
    "public_api_url_present": True,
    "operator_token_present": True,
    "unauth_rejected": unauth_status in {"401", "403"},
    "wrong_token_rejected": wrong_status in {"401", "403"},
    "authorized_ok": auth_status == "200",
}
try:
    with open(auth_body_path, "r", encoding="utf-8") as f:
        body = json.load(f)
    checks["authorized_json_valid"] = isinstance(body, dict)
    checks["authorized_health_ok_field_present"] = "ok" in body
except Exception:
    checks["authorized_json_valid"] = False
    checks["authorized_health_ok_field_present"] = False

overall = all(checks.values())
with open(log_path, "w", encoding="utf-8") as f:
    f.write(f"unauth_status={unauth_status}\n")
    f.write(f"wrong_token_status={wrong_status}\n")
    f.write(f"authorized_status={auth_status}\n")
    f.write("operator_token_redacted=True\n")
    for name, passed in checks.items():
        f.write(f"{name}={passed}\n")
    f.write(f"public_operator_auth_boundary_ok={overall}\n")
if not overall:
    sys.exit(1)
PY
  then
    result=0
  else
    result=1
  fi
  rm -rf "$temp_dir"
  if [[ "$result" -eq 0 ]]; then
    ok "public operator auth boundary"
  else
    fail "public operator auth boundary; see $log_path"
  fi
}

check_metrics_surface_safety() {
  local log_path="$1"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: metrics surface safety"
    printf 'dry-run metrics surface safety check: require Prometheus /metrics core pool, Stratum, payout, and health metrics\n' >"$log_path"
    return
  fi
  if python3 - "$REPORT_DIR/http-prometheus-metrics.txt" >"$log_path" 2>&1 <<'PY'
import re
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as f:
    text = f.read()

required_metrics = [
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

def has_metric_sample(name):
    pattern = re.compile(rf"^{re.escape(name)}(?:\{{[^\n]*\}})?\s+[-+0-9.eE]+$", re.MULTILINE)
    return pattern.search(text) is not None

def metric_family(name):
    if name.endswith("_sum") or name.endswith("_count"):
        return name.rsplit("_", 1)[0]
    return name

def has_help(name):
    return f"# HELP {metric_family(name)} " in text

def has_type(name):
    return f"# TYPE {metric_family(name)} " in text

checks = {
    "prometheus_metrics_not_empty": bool(text.strip()),
    "prometheus_metrics_help_present": all(has_help(name) for name in required_metrics),
    "prometheus_metrics_type_present": all(has_type(name) for name in required_metrics),
    "prometheus_metrics_samples_present": all(has_metric_sample(name) for name in required_metrics),
    "prometheus_metrics_accepted_share_counter": 'csd_pool_shares_total{result="accepted"}' in text,
    "prometheus_metrics_rejected_share_counter": 'csd_pool_shares_total{result="rejected"}' in text,
    "prometheus_metrics_stale_share_counter": 'csd_pool_shares_total{result="stale"}' in text,
    "prometheus_metrics_health_has_node_sample": re.search(r'^csd_pool_service_up\{service="node:[^"]+"\}\s+[01]$', text, re.MULTILINE) is not None,
    "prometheus_metrics_health_has_signer_sample": re.search(r'^csd_pool_service_up\{service="signer"\}\s+[01]$', text, re.MULTILINE) is not None,
}
for name in required_metrics:
    print(f"metric_{name}_sample_present={has_metric_sample(name)}")
for name, passed in checks.items():
    print(f"{name}={passed}")
overall = all(checks.values())
print(f"metrics_surface_ok={overall}")
if not overall:
    print("Prometheus /metrics must expose core pool, Stratum, share, payout, and health metrics before launch")
    sys.exit(1)
PY
  then
    ok "metrics surface safety"
  else
    fail "metrics surface safety; see $log_path"
  fi
}

check_http() {
  local url="$1"
  local label="$2"
  local log_path="$3"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: $label"
    printf 'dry-run HTTP probe: %s\n' "$url" >"$log_path"
    return
  fi
  if command -v curl >/dev/null 2>&1; then
    if curl --fail --silent --show-error --max-time 5 "$url" >"$log_path" 2>&1; then
      ok "$label"
    else
      fail "$label; see $log_path"
    fi
  else
    fail "$label: curl not installed"
  fi
}

check_public_http() {
  local path="$1"
  local label="$2"
  local log_path="$3"
  if [[ -z "$PUBLIC_API_URL" ]]; then
    if [[ "$TARGET" == "public-beta" || "$TARGET" == "production" ]]; then
      fail "$label: public API URL missing"
      printf 'public API URL missing for %s\n' "$path" >"$log_path"
    else
      skip "$label disabled; set CSD_POOL_GO_LIVE_PUBLIC_API_URL"
      printf 'public API probe disabled for %s\n' "$path" >"$log_path"
    fi
    return
  fi
  check_http "$PUBLIC_API_URL$path" "$label" "$log_path"
}

check_status_release_binding() {
  local log_path="$1"
  local release_manifest release_name release_revision release_timestamp
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: status release binding"
    printf 'dry-run status release binding check\n' >"$log_path"
    return
  fi
  release_manifest="$(resolve_release_manifest)"
  if [[ ! -f "$release_manifest" ]]; then
    fail "status release binding: release manifest missing"
    printf 'release manifest missing: %s\n' "${release_manifest:-unresolved}" >"$log_path"
    return
  fi
  release_name="$(sed -n 's/^name=//p' "$release_manifest" | head -n 1)"
  release_revision="$(sed -n 's/^revision=//p' "$release_manifest" | head -n 1)"
  release_timestamp="$(sed -n 's/^timestamp_utc=//p' "$release_manifest" | head -n 1)"
  if python3 - "$REPORT_DIR/http-api-status.json" "$release_name" "$release_revision" "$release_timestamp" >"$log_path" 2>&1 <<'PY'
import json
import sys

path, expected_name, expected_revision, expected_timestamp = sys.argv[1:5]
with open(path, "r", encoding="utf-8") as f:
    status = json.load(f)
release = status.get("release") or {}
checks = {
    "name": expected_name,
    "revision": expected_revision,
    "timestamp_utc": expected_timestamp,
}
failed = False
for field, expected in checks.items():
    actual = release.get(field)
    print(f"{field}: expected={expected} actual={actual}")
    if not expected or actual != expected:
        failed = True
if not release.get("version"):
    print("version: missing")
    failed = True
sys.exit(1 if failed else 0)
PY
  then
    ok "status release binding"
  else
    fail "status release binding; see $log_path"
  fi
}

check_runtime_status_binding() {
  local log_path="$1"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: runtime status binding"
    printf 'dry-run runtime status binding check\n' >"$log_path"
    return
  fi
  if python3 - "$REPORT_DIR/http-api-status.json" "$STATUS_SAMPLE_MAX_AGE_MINUTES" "$STATUS_SAMPLE_MAX_CLOCK_SKEW_SECONDS" >"$log_path" 2>&1 <<'PY'
import datetime as dt
import json
import sys

path, max_age_minutes_raw, max_clock_skew_raw = sys.argv[1:4]

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
max_clock_skew_seconds = parse_positive_int(max_clock_skew_raw, "CSD_POOL_STATUS_SAMPLE_MAX_CLOCK_SKEW_SECONDS")
now = dt.datetime.now(dt.timezone.utc)
with open(path, "r", encoding="utf-8") as f:
    status = json.load(f)
latest_sample_at_raw = status.get("latest_sample_at") or ""
try:
    latest_sample_at = parse_time(latest_sample_at_raw, "latest_sample_at")
    timestamps_parse = True
    sample_age_seconds = int((now - latest_sample_at).total_seconds())
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
print(f"checked_at_utc={now.strftime('%Y-%m-%dT%H:%M:%SZ')}")
print(f"runtime_sample_age_seconds={sample_age_seconds}")
print(f"runtime_sample_max_age_minutes={max_age_minutes}")
print(f"runtime_sample_max_age_seconds={max_age_seconds}")
for name, passed in checks.items():
    print(f"{name}={passed}")
if not all(checks.values()):
    print("runtime status must prove live PostgreSQL-backed operational service with node health samples")
    sys.exit(1)
PY
  then
    ok "runtime status binding"
  else
    fail "runtime status binding; see $log_path"
  fi
}

check_runtime_config_binding() {
  local log_path="$1"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: runtime config binding"
    printf 'dry-run runtime config binding check\n' >"$log_path"
    return
  fi
  if python3 - "$REPORT_DIR/config-snapshot.json" "$REPORT_DIR/http-api-status.json" >"$log_path" 2>&1 <<'PY'
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
    print(f"{name}={passed}")
if not all(checks.values()):
    print("runtime /api/status config must match go-live config-snapshot.json")
    sys.exit(1)
PY
  then
    ok "runtime config binding"
  else
    fail "runtime config binding; see $log_path"
  fi
}

check_pool_endpoint_binding() {
  local pool_path="$1"
  local status_path="$2"
  local log_path="$3"
  local label="$4"
  if [[ "$label" == "external public" && -z "$PUBLIC_API_URL" && "$TARGET" != "public-beta" && "$TARGET" != "production" ]]; then
    skip "external public pool endpoint binding not required for $TARGET"
    printf 'external public pool endpoint binding not required for %s\n' "$TARGET" >"$log_path"
    return
  fi
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: $label pool endpoint binding"
    printf 'dry-run %s pool endpoint binding check\n' "$label" >"$log_path"
    return
  fi
  if python3 - "$pool_path" "$status_path" "$label" >"$log_path" 2>&1 <<'PY'
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
print(f"pool_path={pool_path}")
print(f"status_path={status_path}")
for name, passed in checks.items():
    print(f"{name}={passed}")
ok = all(checks.values())
print(f"pool_endpoint_binding_ok={ok}")
if not ok:
    print("/api/pool must describe the same runtime counters and public payout settings as /api/status")
    sys.exit(1)
PY
  then
    ok "$label pool endpoint binding"
  else
    fail "$label pool endpoint binding; see $log_path"
  fi
}

check_external_public_config_binding() {
  local log_path="$1"
  if [[ "$TARGET" != "public-beta" && "$TARGET" != "production" ]]; then
    skip "external public config binding not required for $TARGET"
    printf 'external public config binding not required for %s\n' "$TARGET" >"$log_path"
    return
  fi
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: external public config binding"
    printf 'dry-run external public config binding check\n' >"$log_path"
    return
  fi
  if python3 - "$REPORT_DIR/config-snapshot.json" "$REPORT_DIR/http-public-api-status.json" >"$log_path" 2>&1 <<'PY'
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
    print(f"{name}={passed}")
if not all(checks.values()):
    print("external public /api/status config must match go-live config-snapshot.json")
    sys.exit(1)
PY
  then
    ok "external public config binding"
  else
    fail "external public config binding; see $log_path"
  fi
}

check_getting_started_binding() {
  local json_path="$1"
  local log_path="$2"
  local label="$3"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: $label getting-started binding"
    printf 'dry-run %s getting-started binding check\n' "$label" >"$log_path"
    return
  fi
  if python3 - "$json_path" "${CSD_POOL_PUBLIC_STRATUM_ADDR:-}" "${CSD_POOL_PUBLIC_PORT_TIERS:-}" "$label" >"$log_path" 2>&1 <<'PY'
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
    if not raw:
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
        label = parts[1] if len(parts) > 1 and parts[1] else "custom"
        flag = parts[3].lower() if len(parts) > 3 else ""
        tiers.append({"port": port, "label": label, "enabled": flag not in {"disabled", "off", "0", "false"}})
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
    "stratum_endpoint_matches": bool(expected_endpoint) and data.get("stratum_endpoint") == expected_endpoint,
    "commands_present": bool(command_values),
    "commands_include_stratum_endpoint": bool(command_values) and all(expected_endpoint in command for command in command_values),
    "port_tiers_match": actual_tiers == expected_tiers,
    "enabled_port_tier_present": any(tier.get("enabled") for tier in actual_tiers),
    "payout_rules_present": isinstance(data.get("payout"), dict) and bool(data.get("payout")),
}
for name, passed in checks.items():
    print(f"{name}={passed}")
if not all(checks.values()):
    print(f"{label} /api/getting-started must match CSD_POOL_PUBLIC_STRATUM_ADDR and CSD_POOL_PUBLIC_PORT_TIERS")
    print(f"expected_endpoint={expected_endpoint or 'missing'}")
    print(f"expected_tiers={expected_tiers}")
    print(f"actual_endpoint={data.get('stratum_endpoint', 'missing')}")
    print(f"actual_tiers={actual_tiers}")
    sys.exit(1)
PY
  then
    ok "$label getting-started binding"
  else
    fail "$label getting-started binding; see $log_path"
  fi
}

check_external_public_status_binding() {
  local log_path="$1"
  local release_manifest release_name release_revision release_timestamp
  if [[ "$TARGET" != "public-beta" && "$TARGET" != "production" ]]; then
    skip "external public status binding not required for $TARGET"
    printf 'external public status binding not required for %s\n' "$TARGET" >"$log_path"
    return
  fi
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: external public status binding"
    printf 'dry-run external public status binding check\n' >"$log_path"
    return
  fi
  release_manifest="$(resolve_release_manifest)"
  if [[ ! -f "$release_manifest" ]]; then
    fail "external public status binding: release manifest missing"
    printf 'release manifest missing: %s\n' "${release_manifest:-unresolved}" >"$log_path"
    return
  fi
  release_name="$(sed -n 's/^name=//p' "$release_manifest" | head -n 1)"
  release_revision="$(sed -n 's/^revision=//p' "$release_manifest" | head -n 1)"
  release_timestamp="$(sed -n 's/^timestamp_utc=//p' "$release_manifest" | head -n 1)"
  if python3 - "$REPORT_DIR/http-public-api-status.json" "$release_name" "$release_revision" "$release_timestamp" "$STATUS_SAMPLE_MAX_AGE_MINUTES" "$STATUS_SAMPLE_MAX_CLOCK_SKEW_SECONDS" >"$log_path" 2>&1 <<'PY'
import datetime as dt
import json
import sys

path, expected_name, expected_revision, expected_timestamp, max_age_minutes_raw, max_clock_skew_raw = sys.argv[1:7]

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
max_clock_skew_seconds = parse_positive_int(max_clock_skew_raw, "CSD_POOL_STATUS_SAMPLE_MAX_CLOCK_SKEW_SECONDS")
now = dt.datetime.now(dt.timezone.utc)
with open(path, "r", encoding="utf-8") as f:
    status = json.load(f)
release = status.get("release") or {}
latest_sample_at_raw = status.get("latest_sample_at") or ""
try:
    latest_sample_at = parse_time(latest_sample_at_raw, "latest_sample_at")
    timestamps_parse = True
    sample_age_seconds = int((now - latest_sample_at).total_seconds())
except Exception as exc:
    print(f"runtime_sample_recency_parse_error={exc}")
    timestamps_parse = False
    sample_age_seconds = -1
checks = {
    "release_matches": (
        release.get("name") == expected_name
        and release.get("revision") == expected_revision
        and release.get("timestamp_utc") == expected_timestamp
        and bool(release.get("version"))
    ),
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
print(f"checked_at_utc={now.strftime('%Y-%m-%dT%H:%M:%SZ')}")
print(f"runtime_sample_age_seconds={sample_age_seconds}")
print(f"runtime_sample_max_age_minutes={max_age_minutes}")
print(f"runtime_sample_max_age_seconds={max_age_seconds}")
for name, passed in checks.items():
    print(f"{name}={passed}")
if not all(checks.values()):
    print("external public /api/status must prove current release and PostgreSQL-backed operational runtime")
    sys.exit(1)
PY
  then
    ok "external public status binding"
  else
    fail "external public status binding; see $log_path"
  fi
}

check_signer_safety() {
  local log_path="$1"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: signer launch safety"
    printf 'dry-run signer safety check: require non-mock signer health mode before launch\n' >"$log_path"
    return
  fi
  if python3 - "$REPORT_DIR/check-signer.json" >"$log_path" 2>&1 <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as f:
    report = json.load(f)
health_mode = (report.get("health_mode") or "").strip().lower()
health_service = report.get("health_service") or ""
health_wallet_address = (report.get("health_wallet_address") or "").strip().lower()
expected_wallet_address = (report.get("expected_wallet_address") or "").strip().lower()
sign_ok = report.get("sign_ok") is True
raw_tx_hex = report.get("raw_tx_hex") or ""
txid = report.get("txid") or ""
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
    "health_service_present": bool(health_service),
    "health_mode_present": bool(health_mode),
    "health_wallet_address_present": bool(health_wallet_address),
    "health_wallet_address_valid": is_addr20(health_wallet_address),
    "expected_wallet_address_present": bool(expected_wallet_address),
    "expected_wallet_address_valid": is_addr20(expected_wallet_address),
    "expected_wallet_address_not_example": expected_wallet_address not in example_wallets,
    "signer_wallet_matches_expected": bool(health_wallet_address) and health_wallet_address == expected_wallet_address,
    "sign_ok": sign_ok,
    "txid_present": bool(txid),
    "signer_mode_allowed": bool(health_mode) and health_mode not in blocked_modes,
    "raw_tx_not_mock_prefix": not raw_tx_mock_prefix_present,
    "official_node_tx_present": node_tx_present and isinstance(node_tx, dict),
    "official_node_tx_valid": node_tx_valid,
    "official_node_tx_outputs_match_request": node_tx_outputs_match_request,
}
print(f"health_service={health_service or 'missing'}")
print(f"health_mode={health_mode or 'missing'}")
print(f"health_wallet_address={health_wallet_address or 'missing'}")
print(f"expected_wallet_address={expected_wallet_address or 'missing'}")
for name, passed in checks.items():
    print(f"{name}={passed}")
if not all(checks.values()):
    print("signer must return an official signed CSD node_tx with exact payout outputs; mock/dev/test modes and legacy raw-only payloads are forbidden")
    sys.exit(1)
PY
  then
    ok "signer launch safety"
  else
    fail "signer launch safety; see $log_path"
  fi
}

check_node_endpoint_safety() {
  local log_path="$1"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: CSD node endpoint safety"
    printf 'dry-run CSD node endpoint safety check: require non-loopback non-mock CSD node URLs and passing template/submit health\n' >"$log_path"
    return
  fi
  if python3 - "$REPORT_DIR/config-snapshot.json" "$REPORT_DIR/check-node-template.json" >"$log_path" 2>&1 <<'PY'
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
node_hosts = []
node_urls_allowed = True
role_coverage = set()
for item in nodes:
    url = item.get("rpc_url") or ""
    host = host_from_url(url)
    node_hosts.append(host or "missing")
    if not host_allowed(host):
        node_urls_allowed = False
    for role in item.get("roles") or []:
        role_coverage.add(str(role))

template_host = host_from_url(node.get("template_node_url") or "")
submit_url = node.get("submit_node_url") or ""
submit_host = host_from_url(submit_url) if submit_url else ""
submit_health = node.get("submit_health") or {}
checks = {
    "config_nodes_present": bool(nodes),
    "config_node_urls_allowed": node_urls_allowed,
    "config_roles_cover_template_submit_watch": {"template", "submit", "watch"}.issubset(role_coverage),
    "template_node_url_allowed": host_allowed(template_host),
    "submit_node_url_allowed": bool(submit_host) and host_allowed(submit_host),
    "template_contract_passed": node.get("passed") is True and node.get("template_ok") is True,
    "adapter_auth_required": node.get("adapter_auth_required") is True,
    "adapter_auth_boundary_ok": node.get("adapter_auth_boundary_ok") is True,
    "unauthenticated_template_rejected": node.get("unauthenticated_template_status") == 401,
    "template_health_ok": node.get("health_ok") is True,
    "network_ok": node.get("network_ok") is True,
    "submit_health_ok": submit_health.get("ok") is True,
}
print(f"config_node_hosts={','.join(node_hosts) if node_hosts else 'missing'}")
print(f"template_node_host={template_host or 'missing'}")
print(f"submit_node_host={submit_host or 'missing'}")
for name, passed in checks.items():
    print(f"{name}={passed}")
if not all(checks.values()):
    print("CSD node endpoints must be real non-loopback/non-mock URLs with passing template, network, and submit health")
    sys.exit(1)
PY
  then
    ok "CSD node endpoint safety"
  else
    fail "CSD node endpoint safety; see $log_path"
  fi
}

check_payout_safety() {
  local log_path="$1"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: payout launch safety"
    printf 'dry-run payout safety check: require payouts_enabled=false before launch\n' >"$log_path"
    return
  fi
  if python3 - "$REPORT_DIR/http-operator-payout-status.json" >"$log_path" 2>&1 <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as f:
    status = json.load(f)
payouts_enabled = status.get("payouts_enabled")
print(f"payouts_enabled={payouts_enabled}")
if payouts_enabled is not False:
    print("payouts must be paused before go-live signoff")
    sys.exit(1)
PY
  then
    ok "payout launch safety"
  else
    fail "payout launch safety; see $log_path"
  fi
}

check_payout_controls_safety() {
  local log_path="$1"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: payout controls safety"
    printf 'dry-run payout controls safety check: require operator payout status, preview, batch list, audit JSON, and CSV exports\n' >"$log_path"
    return
  fi
  if python3 - \
    "$REPORT_DIR/http-operator-payout-status.json" \
    "$REPORT_DIR/http-operator-payout-preview.json" \
    "$REPORT_DIR/http-operator-payout-batches.json" \
    "$REPORT_DIR/http-operator-payout-audit.json" \
    "$REPORT_DIR/http-operator-payout-batches.csv" \
    "$REPORT_DIR/http-operator-payout-audit.csv" \
    >"$log_path" 2>&1 <<'PY'
import csv
import json
import sys

status_path, preview_path, batches_path, audit_path, batches_csv_path, audit_csv_path = sys.argv[1:7]
with open(status_path, "r", encoding="utf-8") as f:
    status = json.load(f)
with open(preview_path, "r", encoding="utf-8") as f:
    preview = json.load(f)
with open(batches_path, "r", encoding="utf-8") as f:
    batches_response = json.load(f)
with open(audit_path, "r", encoding="utf-8") as f:
    audit_response = json.load(f)

def int_string(value):
    return isinstance(value, str) and value.isdigit()

def bool_field(obj, name):
    return isinstance(obj.get(name), bool)

def csv_header(path):
    with open(path, "r", encoding="utf-8", newline="") as f:
        return next(csv.reader(f), [])

batches = batches_response.get("batches")
events = audit_response.get("events")
allowed_statuses = {"needs_approval", "created", "signed", "submitted", "confirmed", "failed", "cancelled"}
allowed_actions = {"create", "approve", "cancel", "retry", "sign", "submit", "confirm", "fail", "pause_payouts", "resume_payouts"}
preview_amount_fields = [
    "minimum_payout_base_units",
    "max_payout_batch_base_units",
    "max_daily_payout_base_units",
    "manual_payout_approval_base_units",
    "daily_payout_used_base_units",
    "daily_remaining_base_units",
    "total_base_units",
]
preview_display_fields = [
    "minimum_payout_csd",
    "max_payout_batch_csd",
    "max_daily_payout_csd",
    "manual_payout_approval_csd",
    "daily_payout_used_csd",
    "daily_remaining_csd",
    "total_csd",
]
checks = {
    "payouts_paused": status.get("payouts_enabled") is False,
    "preview_amount_fields_present": all(int_string(preview.get(field)) for field in preview_amount_fields),
    "preview_display_fields_present": all(isinstance(preview.get(field), str) and preview.get(field) for field in preview_display_fields),
    "preview_count_present": isinstance(preview.get("recipient_count"), int),
    "preview_recipients_array": isinstance(preview.get("recipients"), list),
    "preview_control_booleans_present": all(bool_field(preview, field) for field in ["would_create_batch", "cap_exceeded", "daily_cap_exceeded", "manual_approval_required"]),
    "manual_approval_threshold_positive": int_string(preview.get("manual_payout_approval_base_units")) and int(preview["manual_payout_approval_base_units"]) > 0,
    "manual_below_max_batch": int_string(preview.get("manual_payout_approval_base_units")) and int_string(preview.get("max_payout_batch_base_units")) and int(preview["manual_payout_approval_base_units"]) < int(preview["max_payout_batch_base_units"]),
    "batches_array_present": isinstance(batches, list),
    "batch_statuses_allowed": isinstance(batches, list) and all(batch.get("status") in allowed_statuses for batch in batches if isinstance(batch, dict)),
    "batch_actions_have_targets": isinstance(batches, list) and all(bool(batch.get("batch_id")) and isinstance(batch.get("recipients"), list) for batch in batches if isinstance(batch, dict)),
    "audit_events_array_present": isinstance(events, list),
    "audit_actions_allowed": isinstance(events, list) and all(event.get("action") in allowed_actions for event in events if isinstance(event, dict)),
    "audit_events_have_actor_and_batch": isinstance(events, list) and all(bool(event.get("actor")) and bool(event.get("batch_id")) for event in events if isinstance(event, dict)),
    "payout_batches_csv_header": csv_header(batches_csv_path) == ["batch_id", "status", "txid", "recipient", "amount_base_units", "amount_csd", "total_base_units", "total_csd"],
    "payout_audit_csv_header": csv_header(audit_csv_path) == ["created_at", "batch_id", "actor", "action", "details_json"],
}
print(f"payout_batch_count={len(batches) if isinstance(batches, list) else 'invalid'}")
print(f"payout_audit_event_count={len(events) if isinstance(events, list) else 'invalid'}")
for name, passed in checks.items():
    print(f"{name}={passed}")
overall = all(checks.values())
print(f"payout_controls_ok={overall}")
if not overall:
    print("operator payout controls must expose paused status, limit-aware preview, batch status list, audit JSON, and CSV exports before launch")
    sys.exit(1)
PY
  then
    ok "payout controls safety"
  else
    fail "payout controls safety; see $log_path"
  fi
}

check_operator_readiness_safety() {
  local log_path="$1"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: operator readiness safety"
    printf 'dry-run operator readiness safety check: require healthy operator samples and zero active alerts\n' >"$log_path"
    return
  fi
  if python3 - "$REPORT_DIR/http-operator-health.json" "$REPORT_DIR/http-operator-alerts.json" >"$log_path" 2>&1 <<'PY'
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
print(f"sample_count={len(samples)}")
print(f"node_sample_count={len(node_samples)}")
print(f"signer_sample_count={len(signer_samples)}")
print(f"active_alert_count={len(alerts) if isinstance(alerts, list) else 'invalid'}")
for name, passed in checks.items():
    print(f"{name}={passed}")
if not all(checks.values()):
    print("operator readiness must prove healthy node/signer samples and zero active alerts")
    sys.exit(1)
PY
  then
    ok "operator readiness safety"
  else
    fail "operator readiness safety; see $log_path"
  fi
}

check_payout_limit_safety() {
  local log_path="$1"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: payout limit safety"
    printf 'dry-run payout limit safety check: require positive ordered payout caps and manual approval threshold below max batch\n' >"$log_path"
    return
  fi
  if python3 - "$REPORT_DIR/config-snapshot.json" >"$log_path" 2>&1 <<'PY'
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
print(f"minimum_payout_base_units={minimum if minimum is not None else 'missing'}")
print(f"manual_payout_approval_base_units={manual if manual is not None else 'missing'}")
print(f"max_payout_batch_base_units={batch if batch is not None else 'missing'}")
print(f"max_daily_payout_base_units={daily if daily is not None else 'missing'}")
for name, passed in checks.items():
    print(f"{name}={passed}")
if not all(checks.values()):
    print("payout limits must prove positive ordered caps with manual approval below max batch")
    sys.exit(1)
PY
  then
    ok "payout limit safety"
  else
    fail "payout limit safety; see $log_path"
  fi
}

check_database_migration_safety() {
  local log_path="$1"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: database migration safety"
    printf 'dry-run database migration safety check: require complete schema_migrations coverage before launch\n' >"$log_path"
    return
  fi
  if python3 - "$REPORT_DIR/database-migration.json" >"$log_path" 2>&1 <<'PY'
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
print(f"known_versions={','.join(str(v) for v in sorted(known_set)) if known_set else 'missing'}")
print(f"database_versions={','.join(str(v) for v in sorted(database_set)) if database_set else 'missing'}")
print(f"latest_known_version={latest_known}")
print(f"latest_database_version={latest_database}")
print(f"known_migration_count={known_count}")
for name, passed in checks.items():
    print(f"{name}={passed}")
if not all(checks.values()):
    print("database schema migrations must be complete before go-live")
    sys.exit(1)
PY
  then
    ok "database migration safety"
  else
    fail "database migration safety; see $log_path"
  fi
}

check_systemd_runtime_safety() {
  local log_path="$1"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: systemd runtime safety"
    printf 'dry-run systemd runtime safety check: require enabled/active pool services and timers\n' >"$log_path"
    return
  fi
  if ! command -v systemctl >/dev/null 2>&1; then
    fail "systemd runtime safety: systemctl not installed"
    printf 'systemctl_installed=False\n' >"$log_path"
    return
  fi
  if python3 - "$log_path" >"$log_path.tmp" 2>&1 <<'PY'
import subprocess
import sys

log_path = sys.argv[1]
services = [
    "csd-pool-daemon.service",
    "csd-pool-signer.service",
]
timers = [
    "csd-pool-reconcile-blocks.timer",
    "csd-pool-rewards.timer",
    "csd-pool-payout-create.timer",
    "csd-pool-payout-sign.timer",
    "csd-pool-payout-submit.timer",
    "csd-pool-payout-reconcile.timer",
    "csd-pool-monitoring.timer",
    "csd-pool-backup.timer",
]

def run(*args):
    result = subprocess.run(args, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    return result.returncode, result.stdout.strip()

def key(unit):
    return unit.replace("-", "_").replace(".", "_")

checks = {"systemctl_installed": True}
lines = ["systemctl_installed=True"]
for unit in services + timers:
    code_enabled, enabled = run("systemctl", "is-enabled", unit)
    code_active, active = run("systemctl", "is-active", unit)
    enabled_ok = code_enabled == 0 and enabled == "enabled"
    active_ok = code_active == 0 and active == "active"
    prefix = key(unit)
    checks[f"{prefix}_enabled"] = enabled_ok
    checks[f"{prefix}_active"] = active_ok
    lines.append(f"{prefix}_enabled={enabled_ok}")
    lines.append(f"{prefix}_active={active_ok}")
    lines.append(f"{prefix}_is_enabled_output={enabled or 'missing'}")
    lines.append(f"{prefix}_is_active_output={active or 'missing'}")

overall = all(checks.values())
lines.append(f"systemd_runtime_ok={overall}")
with open(log_path, "w", encoding="utf-8") as f:
    f.write("\n".join(lines) + "\n")
if not overall:
    print("systemd services and timers must be enabled and active before go-live")
    for name, passed in checks.items():
        print(f"{name}={passed}")
    sys.exit(1)
PY
  then
    rm -f "$log_path.tmp"
    ok "systemd runtime safety"
  else
    cat "$log_path.tmp" >>"$log_path" 2>/dev/null || true
    rm -f "$log_path.tmp"
    fail "systemd runtime safety; see $log_path"
  fi
}

check_runtime_hardening_safety() {
  local log_path="$1"
  local daemon_user="${CSD_POOL_DAEMON_EXPECTED_USER:-csd-pool}"
  local daemon_group="${CSD_POOL_DAEMON_EXPECTED_GROUP:-csd-pool}"
  local signer_user="${CSD_POOL_SIGNER_EXPECTED_USER:-csd-signer}"
  local signer_group="${CSD_POOL_SIGNER_EXPECTED_GROUP:-csd-signer}"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: runtime hardening safety"
    {
      printf 'dry-run runtime hardening safety check\n'
      printf 'expected=daemon and signer loaded systemd properties keep service user/group and hardening enabled\n'
      printf 'daemon_expected_user=%s\n' "$daemon_user"
      printf 'daemon_expected_group=%s\n' "$daemon_group"
      printf 'signer_expected_user=%s\n' "$signer_user"
      printf 'signer_expected_group=%s\n' "$signer_group"
    } >"$log_path"
    return
  fi
  if ! command -v systemctl >/dev/null 2>&1; then
    fail "runtime hardening safety: systemctl not installed"
    printf 'systemctl_installed=False\n' >"$log_path"
    return
  fi
  if python3 - "$daemon_user" "$daemon_group" "$signer_user" "$signer_group" >"$log_path" 2>&1 <<'PY'
import subprocess
import sys

daemon_user, daemon_group, signer_user, signer_group = sys.argv[1:5]
units = [
    ("daemon", "csd-pool-daemon.service", daemon_user, daemon_group),
    ("signer", "csd-pool-signer.service", signer_user, signer_group),
]

def run(*args):
    result = subprocess.run(args, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    return result.returncode, result.stdout.strip()

def show(unit, prop):
    code, value = run("systemctl", "show", unit, f"--property={prop}", "--value")
    return value if code == 0 else ""

def truthy(value):
    return str(value).strip().lower() in {"yes", "true", "1"}

checks = {"systemctl_installed": True}
for label, unit, expected_user, expected_group in units:
    user = show(unit, "User")
    group = show(unit, "Group")
    no_new_privileges = show(unit, "NoNewPrivileges")
    private_tmp = show(unit, "PrivateTmp")
    protect_home = show(unit, "ProtectHome")
    protect_system = show(unit, "ProtectSystem")
    unit_checks = {
        f"{label}_user_ok": user == expected_user,
        f"{label}_group_ok": group == expected_group,
        f"{label}_no_new_privileges_ok": truthy(no_new_privileges),
        f"{label}_private_tmp_ok": truthy(private_tmp),
        f"{label}_protect_home_ok": truthy(protect_home),
        f"{label}_protect_system_strict": protect_system == "strict",
    }
    print(f"{label}_unit={unit}")
    print(f"{label}_expected_user={expected_user}")
    print(f"{label}_user={user or 'missing'}")
    print(f"{label}_expected_group={expected_group}")
    print(f"{label}_group={group or 'missing'}")
    print(f"{label}_no_new_privileges={no_new_privileges or 'missing'}")
    print(f"{label}_private_tmp={private_tmp or 'missing'}")
    print(f"{label}_protect_home={protect_home or 'missing'}")
    print(f"{label}_protect_system={protect_system or 'missing'}")
    for name, passed in unit_checks.items():
        print(f"{name}={passed}")
        checks[name] = passed

overall = all(checks.values())
print(f"runtime_hardening_ok={overall}")
if not overall:
    print("daemon and signer loaded systemd units must preserve service identity and hardening before go-live")
    sys.exit(1)
PY
  then
    ok "runtime hardening safety"
  else
    fail "runtime hardening safety; see $log_path"
  fi
}

check_resource_limit_safety() {
  local log_path="$1"
  local daemon_min="${CSD_POOL_DAEMON_MIN_NOFILE:-65536}"
  local signer_min="${CSD_POOL_SIGNER_MIN_NOFILE:-4096}"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: resource limit safety"
    {
      printf 'dry-run resource limit safety check\n'
      printf 'expected=systemd LimitNOFILE and live /proc limits meet go-live minimums\n'
      printf 'daemon_min_nofile=%s\n' "$daemon_min"
      printf 'signer_min_nofile=%s\n' "$signer_min"
    } >"$log_path"
    return
  fi
  if ! command -v systemctl >/dev/null 2>&1; then
    fail "resource limit safety: systemctl not installed"
    printf 'systemctl_installed=False\n' >"$log_path"
    return
  fi
  if python3 - "$daemon_min" "$signer_min" >"$log_path" 2>&1 <<'PY'
import re
import subprocess
import sys
from pathlib import Path

daemon_min = int(sys.argv[1])
signer_min = int(sys.argv[2])
units = [
    ("daemon", "csd-pool-daemon.service", daemon_min),
    ("signer", "csd-pool-signer.service", signer_min),
]

def run(*args):
    result = subprocess.run(args, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    return result.returncode, result.stdout.strip()

def parse_limit(value):
    value = (value or "").strip().lower()
    if value in {"infinity", "unlimited"}:
        return 2**63 - 1
    try:
        return int(value)
    except ValueError:
        return -1

def proc_nofile(pid):
    limits_path = Path("/proc") / str(pid) / "limits"
    if not limits_path.exists():
        return -1
    for line in limits_path.read_text(encoding="utf-8", errors="replace").splitlines():
        if line.startswith("Max open files"):
            parts = re.split(r"\s+", line.strip())
            if len(parts) >= 5:
                return parse_limit(parts[3])
    return -1

checks = {"systemctl_installed": True}
for label, unit, minimum in units:
    code_limit, limit_raw = run("systemctl", "show", unit, "--property=LimitNOFILE", "--value")
    code_pid, pid_raw = run("systemctl", "show", unit, "--property=MainPID", "--value")
    configured = parse_limit(limit_raw) if code_limit == 0 else -1
    try:
        pid = int(pid_raw)
    except ValueError:
        pid = 0
    live = proc_nofile(pid) if pid > 0 else -1
    configured_ok = configured >= minimum
    pid_ok = pid > 0
    live_ok = live >= minimum
    print(f"{label}_unit={unit}")
    print(f"{label}_min_nofile={minimum}")
    print(f"{label}_systemd_limit_nofile_raw={limit_raw or 'missing'}")
    print(f"{label}_systemd_limit_nofile={configured}")
    print(f"{label}_main_pid={pid}")
    print(f"{label}_proc_limit_nofile={live}")
    print(f"{label}_systemd_limit_nofile_ok={configured_ok}")
    print(f"{label}_main_pid_positive={pid_ok}")
    print(f"{label}_proc_limit_nofile_ok={live_ok}")
    checks[f"{label}_systemd_limit_nofile_ok"] = configured_ok
    checks[f"{label}_main_pid_positive"] = pid_ok
    checks[f"{label}_proc_limit_nofile_ok"] = live_ok

overall = all(checks.values())
print(f"resource_limits_ok={overall}")
if not overall:
    print("daemon and signer must have enough configured and live open-file capacity before go-live")
    sys.exit(1)
PY
  then
    ok "resource limit safety"
  else
    fail "resource limit safety; see $log_path"
  fi
}

check_service_provenance_safety() {
  local log_path="$1"
  local release_manifest
  release_manifest="$(resolve_release_manifest)"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: service provenance safety"
    {
      printf 'dry-run service provenance safety check\n'
      printf 'release_manifest=%s\n' "${release_manifest:-unresolved}"
      printf 'expected=daemon executable and signer entrypoint sha256 must match current release SHA256SUMS\n'
    } >"$log_path"
    return
  fi
  if [[ -z "$release_manifest" || ! -f "$release_manifest" ]]; then
    fail "service provenance safety: release manifest missing"
    printf 'release_manifest=%s\n' "${release_manifest:-unresolved}" >"$log_path"
    return
  fi
  if ! command -v systemctl >/dev/null 2>&1; then
    fail "service provenance safety: systemctl not installed"
    printf 'systemctl_installed=False\n' >"$log_path"
    return
  fi
  if python3 - "$release_manifest" >"$log_path" 2>&1 <<'PY'
import hashlib
import os
import subprocess
import sys

release_manifest = sys.argv[1]
release_dir = os.path.dirname(os.path.abspath(release_manifest))
sha256sums = os.path.join(release_dir, "SHA256SUMS")
release_name = ""
with open(release_manifest, "r", encoding="utf-8") as f:
    for line in f:
        if line.startswith("name="):
            release_name = line.strip().split("=", 1)[1]
            break

if os.path.basename(os.path.dirname(release_dir)) == "releases":
    opt_dir = os.path.dirname(os.path.dirname(release_dir))
else:
    opt_dir = release_dir
current_release_path = os.path.join(opt_dir, "CURRENT_RELEASE")
current_release_name = ""
if os.path.exists(current_release_path):
    with open(current_release_path, "r", encoding="utf-8") as f:
        current_release_name = f.readline().strip()

expected = {}
with open(sha256sums, "r", encoding="utf-8") as f:
    for line in f:
        parts = line.strip().split()
        if len(parts) >= 2:
            expected[parts[1].lstrip("./")] = parts[0]

def bool_text(value):
    return "True" if value else "False"

def run(*args):
    result = subprocess.run(args, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    return result.returncode, result.stdout.strip()

def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()

checks = {
    "release_manifest_present": os.path.isfile(release_manifest),
    "release_sha256sums_present": os.path.isfile(sha256sums),
    "current_release_matches_manifest": bool(release_name) and current_release_name == release_name,
}
lines = [
    f"release_manifest={release_manifest}",
    f"release_dir={release_dir}",
    f"release_name={release_name or 'missing'}",
    f"current_release_path={current_release_path}",
    f"current_release_name={current_release_name or 'missing'}",
]

services = [
    ("daemon", "csd-pool-daemon.service", "bin/csd-pool-daemon", "exe"),
    ("signer", "csd-pool-signer.service", "ops/wallet-signer/signer.mjs", "cmdline"),
]
for key, unit, artifact_key, artifact_source in services:
    expected_sha = expected.get(artifact_key, "")
    code, main_pid_raw = run("systemctl", "show", unit, "--property=MainPID", "--value")
    try:
        main_pid = int(main_pid_raw)
    except ValueError:
        main_pid = 0
    artifact_path = ""
    artifact_sha = ""
    artifact_exists = False
    if main_pid > 0:
        if artifact_source == "exe":
            proc_exe = f"/proc/{main_pid}/exe"
            try:
                artifact_path = os.readlink(proc_exe)
                artifact_exists = os.path.exists(artifact_path)
                artifact_sha = sha256_file(proc_exe)
            except OSError:
                artifact_path = ""
        else:
            expected_path = os.path.realpath(os.path.join(release_dir, artifact_key))
            try:
                with open(f"/proc/{main_pid}/cmdline", "rb") as f:
                    arguments = [part.decode("utf-8", "replace") for part in f.read().split(b"\0") if part]
                for argument in arguments:
                    if os.path.realpath(argument) == expected_path:
                        artifact_path = argument
                        artifact_exists = os.path.isfile(argument)
                        artifact_sha = sha256_file(argument)
                        break
            except OSError:
                artifact_path = ""
    matches = bool(expected_sha) and artifact_sha == expected_sha
    checks[f"{key}_main_pid_positive"] = code == 0 and main_pid > 0
    checks[f"{key}_artifact_readable"] = bool(artifact_sha)
    checks[f"{key}_expected_sha_present"] = bool(expected_sha)
    checks[f"{key}_binary_matches_release"] = matches
    lines.extend([
        f"{key}_unit={unit}",
        f"{key}_main_pid={main_pid}",
        f"{key}_artifact_key={artifact_key}",
        f"{key}_artifact_source={artifact_source}",
        f"{key}_artifact_path={artifact_path or 'missing'}",
        f"{key}_artifact_path_exists={bool_text(artifact_exists)}",
        f"{key}_artifact_sha256={artifact_sha or 'missing'}",
        f"{key}_expected_sha256={expected_sha or 'missing'}",
        f"{key}_binary_matches_release={bool_text(matches)}",
    ])

overall = all(checks.values())
for name, passed in checks.items():
    lines.append(f"{name}={bool_text(passed)}")
lines.append(f"service_provenance_ok={bool_text(overall)}")
print("\n".join(lines))
if not overall:
    print("daemon executable and signer entrypoint must match the current release checksums")
    sys.exit(1)
PY
  then
    ok "service provenance safety"
  else
    fail "service provenance safety; see $log_path"
  fi
}

check_backup_artifact_safety() {
  local log_path="$1"
  local backup_path="${CSD_POOL_BACKUP_PATH:-}"
  local min_bytes="${CSD_POOL_BACKUP_MIN_BYTES:-1024}"
  local max_age_days="${CSD_POOL_BACKUP_MAX_AGE_DAYS:-2}"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: backup artifact safety"
    {
      printf 'dry-run backup artifact safety check\n'
      printf 'expected_backup_path=%s\n' "${backup_path:-missing}"
      printf 'expected_min_bytes=%s\n' "$min_bytes"
      printf 'expected_max_age_days=%s\n' "$max_age_days"
    } >"$log_path"
    return
  fi
  if python3 - "$backup_path" "$min_bytes" "$max_age_days" >"$log_path" 2>&1 <<'PY'
import hashlib
import os
import sys
import time

backup_path, min_bytes_raw, max_age_days_raw = sys.argv[1:4]

def bool_text(value):
    return "True" if value else "False"

def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()

checks = {
    "backup_path_present": bool(backup_path),
    "min_bytes_valid": min_bytes_raw.isdigit(),
    "max_age_days_valid": max_age_days_raw.isdigit() and int(max_age_days_raw or "0") >= 1,
}
min_bytes = int(min_bytes_raw) if checks["min_bytes_valid"] else 0
max_age_days = int(max_age_days_raw) if checks["max_age_days_valid"] else 0
exists = bool(backup_path) and os.path.exists(backup_path)
regular = bool(backup_path) and os.path.isfile(backup_path)
size = os.path.getsize(backup_path) if regular else 0
mtime = os.path.getmtime(backup_path) if regular else 0
age_seconds = int(max(0, time.time() - mtime)) if regular else -1
fresh = regular and age_seconds <= max_age_days * 86400
checks.update({
    "backup_file_exists": exists,
    "backup_regular_file": regular,
    "backup_size_at_or_above_min": regular and size >= min_bytes,
    "backup_fresh": fresh,
})
print(f"backup_path={backup_path or 'missing'}")
print(f"backup_path_present={bool_text(checks['backup_path_present'])}")
print(f"backup_file_exists={bool_text(exists)}")
print(f"backup_regular_file={bool_text(regular)}")
print(f"backup_size_bytes={size}")
print(f"backup_min_bytes={min_bytes}")
print(f"backup_age_seconds={age_seconds}")
print(f"backup_max_age_days={max_age_days}")
print(f"backup_fresh={bool_text(fresh)}")
if regular:
    print(f"backup_sha256={sha256_file(backup_path)}")
for name, passed in checks.items():
    print(f"{name}={bool_text(passed)}")
if not all(checks.values()):
    print("backup artifact must be present, regular, large enough, and fresh before go-live")
    sys.exit(1)
PY
  then
    ok "backup artifact safety"
  else
    fail "backup artifact safety; see $log_path"
  fi
}

check_restore_api_safety() {
  local log_path="$1"
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: restore API safety"
    printf 'dry-run restore API safety check: require restored API probes and operator payout status check\n' >"$log_path"
    return
  fi
  if python3 - "$REPORT_DIR/restore-drill.log" "${CSD_POOL_RESTORE_API_URL:-}" >"$log_path" 2>&1 <<'PY'
import sys

log_path, restore_api_url = sys.argv[1:3]
with open(log_path, "r", encoding="utf-8") as f:
    log = f.read()
checks = {
    "restore_api_url_present": bool(restore_api_url),
    "restore_complete": "restore drill complete" in log,
    "restore_health_ok": "ok: restore API /health" in log,
    "restore_pool_ok": "ok: restore API /api/pool" in log,
    "restore_blocks_ok": "ok: restore API /api/blocks" in log,
    "restore_payments_ok": "ok: restore API /api/payments" in log,
    "restore_operator_payout_status_ok": "ok: restore API operator payout status" in log,
    "restore_api_not_skipped": "skip: restore API checks disabled" not in log,
    "restore_operator_not_skipped": "skip: restore operator API check disabled" not in log,
}
print(f"restore_api_url={'present' if restore_api_url else 'missing'}")
for name, passed in checks.items():
    print(f"{name}={passed}")
if not all(checks.values()):
    print("restore drill must prove restored API and operator payout status against the restored database")
    sys.exit(1)
PY
  then
    ok "restore API safety"
  else
    fail "restore API safety; see $log_path"
  fi
}

write_dry_run_restore_http_reports() {
  if [[ "$DRY_RUN" != "1" ]]; then
    return
  fi
  mkdir -p "$REPORT_DIR"
  printf '{"dry_run":true,"planned":"/health"}\n' >"$REPORT_DIR/restore-http-health.json"
  printf '{"dry_run":true,"planned":"/api/pool"}\n' >"$REPORT_DIR/restore-http-pool.json"
  printf '{"dry_run":true,"planned":"/api/blocks","blocks":[]}\n' >"$REPORT_DIR/restore-http-blocks.json"
  printf '{"dry_run":true,"planned":"/api/payments","payments":[]}\n' >"$REPORT_DIR/restore-http-payments.json"
  printf '{"dry_run":true,"planned":"/api/operator/payouts/status","payouts_enabled":false}\n' >"$REPORT_DIR/restore-http-operator-payout-status.json"
}

check_operator_http() {
  local path="$1"
  local label="$2"
  local log_path="$3"
  if [[ -z "${CSD_POOL_OPERATOR_TOKEN:-}" ]]; then
    fail "$label: CSD_POOL_OPERATOR_TOKEN missing"
    printf 'operator token missing for %s\n' "$path" >"$log_path"
    return
  fi
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: $label"
    printf 'dry-run operator probe: %s%s\n' "$API_URL" "$path" >"$log_path"
    return
  fi
  if command -v curl >/dev/null 2>&1; then
    if curl --fail --silent --show-error --max-time 5 \
      -H "Authorization: Bearer $CSD_POOL_OPERATOR_TOKEN" \
      "$API_URL$path" >"$log_path" 2>&1; then
      ok "$label"
    else
      fail "$label; see $log_path"
    fi
  else
    fail "$label: curl not installed"
    printf 'curl not installed for %s\n' "$path" >"$log_path"
  fi
}

split_host_port() {
  local addr="$1"
  if [[ "$addr" =~ ^\[([^]]+)\]:([0-9]+)$ ]]; then
    printf '%s\n%s\n' "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}"
  elif [[ "$addr" =~ ^([^:]+):([0-9]+)$ ]]; then
    printf '%s\n%s\n' "${BASH_REMATCH[1]}" "${BASH_REMATCH[2]}"
  else
    return 1
  fi
}

check_tcp() {
  local addr="$1"
  local label="$2"
  local log_path="$3"
  local host port parsed
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: $label"
    printf 'dry-run TCP probe: %s\n' "$addr" >"$log_path"
    return
  fi
  if ! parsed="$(split_host_port "$addr")"; then
    fail "$label: address must be host:port or [ipv6]:port"
    printf 'invalid TCP address: %s\n' "$addr" >"$log_path"
    return
  fi
  host="$(printf '%s\n' "$parsed" | sed -n '1p')"
  port="$(printf '%s\n' "$parsed" | sed -n '2p')"
  if command -v python3 >/dev/null 2>&1; then
    if python3 - "$host" "$port" >"$log_path" 2>&1 <<'PY'
import socket
import sys

host, port = sys.argv[1], int(sys.argv[2])
with socket.create_connection((host, port), timeout=5):
    print(f"connected {host}:{port}")
PY
    then
      ok "$label"
    else
      fail "$label; see $log_path"
    fi
  elif command -v nc >/dev/null 2>&1; then
    if nc -z -w 5 "$host" "$port" >"$log_path" 2>&1; then
      ok "$label"
    else
      fail "$label; see $log_path"
    fi
  else
    fail "$label: python3 or nc not installed"
  fi
}

check_public_tcp() {
  local label="$1"
  local log_path="$2"
  if [[ -z "$PUBLIC_STRATUM_ADDR" ]]; then
    if [[ "$TARGET" == "public-beta" || "$TARGET" == "production" ]]; then
      fail "$label: public Stratum probe address missing"
      printf 'public Stratum probe address missing\n' >"$log_path"
    else
      skip "$label disabled; set CSD_POOL_GO_LIVE_PUBLIC_STRATUM_ADDR"
      printf 'public Stratum TCP probe disabled\n' >"$log_path"
    fi
    return
  fi
  check_tcp "$PUBLIC_STRATUM_ADDR" "$label" "$log_path"
}

check_public_port_tiers_safety() {
  local log_path="$1"
  if [[ "$TARGET" != "public-beta" && "$TARGET" != "production" ]]; then
    skip "public Stratum port tiers not required for $TARGET"
    printf 'public Stratum port tiers not required for %s\n' "$TARGET" >"$log_path"
    return
  fi
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: public Stratum port tiers"
    {
      printf 'dry-run public Stratum port tiers check\n'
      printf 'public_stratum_probe_addr=%s\n' "${PUBLIC_STRATUM_ADDR:-missing}"
      printf 'public_port_tiers_raw=%s\n' "${CSD_POOL_PUBLIC_PORT_TIERS:-missing}"
      printf 'expected=every enabled public Stratum port tier accepts TCP and probe port is included in enabled tiers\n'
    } >"$log_path"
    return
  fi
  if python3 - "${PUBLIC_STRATUM_ADDR:-}" "${CSD_POOL_PUBLIC_PORT_TIERS:-}" >"$log_path" 2>&1 <<'PY'
import socket
import sys

probe_addr, tiers_raw = sys.argv[1:3]

def split_host_port(value):
    value = value.strip()
    if value.startswith("["):
        end = value.find("]")
        if end != -1 and value[end + 1 : end + 2] == ":":
            return value[1:end], int(value[end + 2 :])
    if ":" in value:
        host, port = value.rsplit(":", 1)
        return host, int(port)
    raise ValueError("expected host:port or [ipv6]:port")

def parse_tiers(raw):
    tiers = []
    if not raw.strip():
        return tiers
    for index, part in enumerate(raw.split(","), start=1):
        fields = [field.strip() for field in part.split(":")]
        if len(fields) < 3:
            raise ValueError(f"tier {index} must be port:name:difficulty[:disabled]")
        port = int(fields[0])
        name = fields[1]
        difficulty = fields[2]
        flags = {field.lower() for field in fields[3:]}
        disabled = "disabled" in flags
        tiers.append({
            "index": index,
            "port": port,
            "name": name,
            "difficulty": difficulty,
            "disabled": disabled,
        })
    return tiers

checks = {}
try:
    host, probe_port = split_host_port(probe_addr)
    checks["public_stratum_probe_addr_valid"] = True
except Exception as exc:
    host, probe_port = "", 0
    print(f"probe_parse_error={exc}")
    checks["public_stratum_probe_addr_valid"] = False

try:
    tiers = parse_tiers(tiers_raw)
    checks["public_port_tiers_parse_ok"] = True
except Exception as exc:
    tiers = []
    print(f"tiers_parse_error={exc}")
    checks["public_port_tiers_parse_ok"] = False

enabled = [tier for tier in tiers if not tier["disabled"]]
disabled = [tier for tier in tiers if tier["disabled"]]
checks["public_port_tier_enabled_count_positive"] = len(enabled) > 0
checks["probe_port_in_enabled_tiers"] = any(tier["port"] == probe_port for tier in enabled)

print(f"public_stratum_probe_addr={probe_addr or 'missing'}")
print(f"public_stratum_probe_host={host or 'missing'}")
print(f"public_stratum_probe_port={probe_port or 'missing'}")
print(f"public_port_tiers_raw={tiers_raw or 'missing'}")
print(f"public_port_tier_count={len(tiers)}")
print(f"public_port_tier_enabled_count={len(enabled)}")
print(f"public_port_tier_disabled_count={len(disabled)}")

all_tcp_ok = True
for tier in tiers:
    prefix = f"tier_{tier['index']}"
    print(f"{prefix}_port={tier['port']}")
    print(f"{prefix}_name={tier['name']}")
    print(f"{prefix}_difficulty={tier['difficulty']}")
    print(f"{prefix}_disabled={tier['disabled']}")
    connected = False
    if not tier["disabled"] and host:
        try:
            with socket.create_connection((host, tier["port"]), timeout=5):
                connected = True
        except Exception as exc:
            print(f"{prefix}_tcp_error={exc}")
    elif tier["disabled"]:
        connected = True
    print(f"{prefix}_tcp_connected={connected}")
    if not connected:
        all_tcp_ok = False

checks["enabled_tiers_tcp_connected"] = all_tcp_ok
for name, passed in checks.items():
    print(f"{name}={passed}")
overall = all(checks.values())
print(f"public_port_tiers_ok={overall}")
if not overall:
    print("every enabled public Stratum port tier must be reachable and include the configured probe port")
    sys.exit(1)
PY
  then
    ok "public Stratum port tiers"
  else
    fail "public Stratum port tiers; see $log_path"
  fi
}

check_public_stratum_smoke() {
  local log_path="$1"
  if [[ -z "$PUBLIC_STRATUM_ADDR" ]]; then
    if [[ "$TARGET" == "public-beta" || "$TARGET" == "production" ]]; then
      fail "external public stratum smoke: public Stratum probe address missing"
      printf '{"skipped":false,"error":"public Stratum probe address missing"}\n' >"$log_path"
    else
      skip "external public stratum smoke disabled; set CSD_POOL_GO_LIVE_PUBLIC_STRATUM_ADDR"
      printf '{"skipped":true,"reason":"public Stratum probe address not configured"}\n' >"$log_path"
    fi
    return
  fi
  run_workers_or_plan "external public stratum smoke" "$log_path" \
    stratum-smoke "$PUBLIC_STRATUM_ADDR"
}

check_public_stratum_load() {
  local log_path="$1"
  if [[ "$TARGET" != "public-beta" && "$TARGET" != "production" ]]; then
    skip "external public stratum load not required for $TARGET"
    printf '{"skipped":true,"reason":"external public Stratum load not required for target","target":' >"$log_path"
    json_string "$TARGET" >>"$log_path"
    printf '}\n' >>"$log_path"
    return
  fi
  if [[ -z "$PUBLIC_STRATUM_ADDR" ]]; then
    fail "external public stratum load: public Stratum probe address missing"
    printf '{"skipped":false,"error":"public Stratum probe address missing"}\n' >"$log_path"
    return
  fi
  run_workers_or_plan "external public stratum load" "$log_path" \
    stratum-load-test "$PUBLIC_STRATUM_ADDR"
}

check_public_port_tiers_smoke() {
  local log_path="$1"
  if [[ "$TARGET" != "public-beta" && "$TARGET" != "production" ]]; then
    skip "public Stratum port tier smoke not required for $TARGET"
    printf '{"skipped":true,"reason":"public Stratum port tier smoke not required for target","target":' >"$log_path"
    json_string "$TARGET" >>"$log_path"
    printf '}\n' >>"$log_path"
    return
  fi
  if [[ "$DRY_RUN" == "1" ]]; then
    skip "dry-run: public Stratum port tier smoke"
    {
      printf '{\n'
      printf '  "skipped": true,\n'
      printf '  "reason": "dry-run public Stratum port tier smoke",\n'
      printf '  "public_stratum_probe_addr": '; json_string "${PUBLIC_STRATUM_ADDR:-missing}"; printf ',\n'
      printf '  "public_port_tiers_raw": '; json_string "${CSD_POOL_PUBLIC_PORT_TIERS:-missing}"; printf ',\n'
      printf '  "expected": "every enabled public Stratum port tier completes stratum-smoke"\n'
      printf '}\n'
    } >"$log_path"
    return
  fi

  local tiers_tsv
  local temp_dir
  temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/csd-pool-port-tier-smoke.XXXXXX")"
  if ! tiers_tsv="$(python3 - "${PUBLIC_STRATUM_ADDR:-}" "${CSD_POOL_PUBLIC_PORT_TIERS:-}" <<'PY'
import sys

probe_addr, tiers_raw = sys.argv[1:3]

def split_host_port(value):
    value = value.strip()
    if value.startswith("["):
        end = value.find("]")
        if end != -1 and value[end + 1 : end + 2] == ":":
            return value[1:end], int(value[end + 2 :])
    if ":" in value:
        host, port = value.rsplit(":", 1)
        return host, int(port)
    raise ValueError("expected host:port or [ipv6]:port")

def parse_tiers(raw):
    tiers = []
    if not raw.strip():
        return tiers
    for index, part in enumerate(raw.split(","), start=1):
        fields = [field.strip() for field in part.split(":")]
        if len(fields) < 3:
            raise ValueError(f"tier {index} must be port:name:difficulty[:disabled]")
        flags = {field.lower() for field in fields[3:]}
        tiers.append((index, int(fields[0]), fields[1], fields[2], "disabled" in flags))
    return tiers

host, _probe_port = split_host_port(probe_addr)
endpoint_host = f"[{host}]" if ":" in host and not host.startswith("[") else host
enabled = [tier for tier in parse_tiers(tiers_raw) if not tier[4]]
if not enabled:
    raise SystemExit("no enabled public Stratum port tiers configured")
for index, port, name, difficulty, _disabled in enabled:
    print(f"{index}\t{port}\t{name}\t{difficulty}\t{endpoint_host}:{port}")
PY
)"; then
    fail "public Stratum port tier smoke: unable to parse tier configuration"
    {
      printf '{\n'
      printf '  "ok": false,\n'
      printf '  "error": "unable to parse public Stratum port tier configuration",\n'
      printf '  "public_stratum_probe_addr": '; json_string "${PUBLIC_STRATUM_ADDR:-missing}"; printf ',\n'
      printf '  "public_port_tiers_raw": '; json_string "${CSD_POOL_PUBLIC_PORT_TIERS:-missing}"; printf '\n'
      printf '}\n'
    } >"$log_path"
    rm -rf "$temp_dir"
    return
  fi

  local tier_count=0
  local failed_count=0
  local index port name difficulty endpoint report
  while IFS=$'\t' read -r index port name difficulty endpoint; do
    [[ -z "$index" ]] && continue
    tier_count=$((tier_count + 1))
    report="$temp_dir/tier-${index}.json"
    if CSD_POOL_CONFIG="$CONFIG_PATH" run_workers_command stratum-smoke "$endpoint" >"$report" 2>&1; then
      ok "public Stratum port tier smoke: $name:$port"
    else
      failed_count=$((failed_count + 1))
      fail "public Stratum port tier smoke: $name:$port; see $report"
    fi
    printf '%s\t%s\t%s\t%s\t%s\n' "$index" "$port" "$name" "$difficulty" "$endpoint" >>"$temp_dir/tiers.tsv"
  done <<<"$tiers_tsv"

  python3 - "$temp_dir" "$tier_count" "$failed_count" "${PUBLIC_STRATUM_ADDR:-}" "${CSD_POOL_PUBLIC_PORT_TIERS:-}" >"$log_path" <<'PY'
import json
import pathlib
import sys

temp_dir = pathlib.Path(sys.argv[1])
tier_count = int(sys.argv[2])
failed_count = int(sys.argv[3])
probe_addr = sys.argv[4]
tiers_raw = sys.argv[5]

tiers = []
tiers_tsv = temp_dir / "tiers.tsv"
if tiers_tsv.exists():
    for line in tiers_tsv.read_text(encoding="utf-8").splitlines():
        index, port, name, difficulty, endpoint = line.split("\t", 4)
        report_path = temp_dir / f"tier-{index}.json"
        report = None
        raw = ""
        if report_path.exists():
            raw = report_path.read_text(encoding="utf-8", errors="replace")
            try:
                report = json.loads(raw)
            except json.JSONDecodeError:
                report = {"json_valid": False, "raw_excerpt": raw[:500]}
        requested = report.get("requested_clients") if isinstance(report, dict) else None
        succeeded = report.get("succeeded_clients") if isinstance(report, dict) else None
        failed = report.get("failed_clients") if isinstance(report, dict) else None
        tiers.append({
            "index": int(index),
            "port": int(port),
            "name": name,
            "difficulty": difficulty,
            "endpoint": endpoint,
            "requested_clients": requested,
            "succeeded_clients": succeeded,
            "failed_clients": failed,
            "passed": isinstance(requested, int) and requested > 0 and succeeded == requested and failed == 0,
            "report": report,
        })

ok = tier_count > 0 and failed_count == 0 and all(tier["passed"] for tier in tiers)
print(json.dumps({
    "ok": ok,
    "public_stratum_probe_addr": probe_addr,
    "public_port_tiers_raw": tiers_raw,
    "enabled_tier_count": tier_count,
    "failed_tier_count": failed_count,
    "tiers": tiers,
}, indent=2, sort_keys=True))
PY
  rm -rf "$temp_dir"
}

printf 'CSD Pool go-live check\n'
printf 'target=%s\n' "$TARGET"
printf 'root=%s\n' "$ROOT_DIR"
printf 'env=%s\n' "$ENV_PATH"
printf 'config=%s\n' "$CONFIG_PATH"
printf 'api=%s\n' "$API_URL"
printf 'stratum=%s\n' "$STRATUM_ADDR"
printf 'report_dir=%s\n' "$REPORT_DIR"

case "$TARGET" in
  private-beta|public-beta|production)
    ok "go-live target accepted: $TARGET"
    ;;
  *)
    fail "unsupported CSD_POOL_GO_LIVE_TARGET: $TARGET"
    ;;
esac

normalize_report_paths
require_real_file "$ENV_PATH" "environment file"
require_real_file "$CONFIG_PATH" "config file"
require_executable_or_cargo "$WORKERS_BIN" "workers"
require_file "$ROOT_DIR/ops/bin/csd-pool-preflight.sh" "preflight script"
require_file "$ROOT_DIR/ops/bin/csd-pool-verify.sh" "verify script"
require_file "$ROOT_DIR/ops/bin/csd-pool-restore-drill.sh" "restore drill script"
load_env_file

require_env CSD_POOL_DATABASE_URL
require_env CSD_POOL_OPERATOR_TOKEN
require_env CSD_POOL_SIGNER_TOKEN
require_env CSD_POOL_NODE_TOKEN
require_env CSD_POOL_SIGNER_URL
require_env CSD_POOL_SIGNER_WALLET_ADDRESS
require_env CSD_POOL_WATCH_NODE_URL
require_env CSD_POOL_SUBMIT_NODE_URL
require_env CSD_POOL_PAYOUT_NODE_URL
require_env CSD_POOL_PUBLIC_STRATUM_ADDR
require_env CSD_POOL_RESTORE_DATABASE_URL
require_no_placeholder_env
require_public_endpoint_config
write_env_snapshot "$REPORT_DIR/env-snapshot.txt"
check_secrets_permissions_safety "$REPORT_DIR/secrets-permissions-safety.log"
write_real_env_readiness "$REPORT_DIR/real-env-readiness.log"
check_clock_safety "$REPORT_DIR/clock-safety.log"
check_disk_safety "$REPORT_DIR/disk-safety.log"
write_config_snapshot "$REPORT_DIR/config-snapshot.json"
check_bind_safety "$REPORT_DIR/bind-safety.log"
check_edge_proxy_safety "$REPORT_DIR/edge-proxy-safety.log"
check_payout_limit_safety "$REPORT_DIR/payout-limit-safety.log"
run_workers_or_plan "database migration status" "$REPORT_DIR/database-migration.json" \
  migrate
check_database_migration_safety "$REPORT_DIR/database-migration-safety.log"
run_workers_or_plan "database runtime contract" "$REPORT_DIR/database-runtime.json" \
  check-database-runtime

run_or_plan "production preflight" "$REPORT_DIR/preflight.log" \
  env \
  CSD_POOL_ENV_FILE="$ENV_PATH" \
  CSD_POOL_PREFLIGHT_CONFIG="$CONFIG_PATH" \
  CSD_POOL_PREFLIGHT_NODE=1 \
  CSD_POOL_PREFLIGHT_SIGNER=1 \
  CSD_POOL_PREFLIGHT_MIGRATE=1 \
  CSD_POOL_PREFLIGHT_VERIFY=1 \
  CSD_POOL_PREFLIGHT_VERIFY_HTTP=1 \
  CSD_POOL_PREFLIGHT_REPORT_DIR="$REPORT_DIR/preflight" \
  "$ROOT_DIR/ops/bin/csd-pool-preflight.sh"

check_release_integrity "$REPORT_DIR/release-integrity.log"

run_or_plan "deployment verification with live gates" "$REPORT_DIR/verify.log" \
  env \
  CSD_POOL_BIN_DIR="$BIN_DIR" \
  CSD_POOL_CONFIG="$CONFIG_PATH" \
  CSD_POOL_ENV_FILE="$ENV_PATH" \
  CSD_POOL_VERIFY_API_URL="$API_URL" \
  CSD_POOL_VERIFY_STRATUM_ADDR="$STRATUM_ADDR" \
  CSD_POOL_VERIFY_RELEASE=1 \
  CSD_POOL_VERIFY_BACKUP=1 \
  CSD_POOL_VERIFY_MIGRATE=1 \
  CSD_POOL_VERIFY_HTTP=1 \
  CSD_POOL_VERIFY_SMOKE=1 \
  CSD_POOL_VERIFY_LOAD=1 \
  "$ROOT_DIR/ops/bin/csd-pool-verify.sh"

check_systemd_runtime_safety "$REPORT_DIR/systemd-runtime-safety.log"
check_runtime_hardening_safety "$REPORT_DIR/runtime-hardening-safety.log"
check_resource_limit_safety "$REPORT_DIR/resource-limit-safety.log"
check_service_provenance_safety "$REPORT_DIR/service-provenance-safety.log"
check_backup_artifact_safety "$REPORT_DIR/backup-artifact-safety.log"

run_or_plan "restore drill from latest backup" "$REPORT_DIR/restore-drill.log" \
  env \
  CSD_POOL_BIN_DIR="$BIN_DIR" \
  CSD_POOL_WORKERS_BIN="$WORKERS_BIN" \
  CSD_POOL_BACKUP_PATH="${CSD_POOL_BACKUP_PATH:-}" \
  CSD_POOL_RESTORE_DATABASE_URL="${CSD_POOL_RESTORE_DATABASE_URL:-}" \
  CSD_POOL_RESTORE_API_URL="${CSD_POOL_RESTORE_API_URL:-}" \
  CSD_POOL_RESTORE_REPORT_DIR="$REPORT_DIR" \
  CSD_POOL_OPERATOR_TOKEN="${CSD_POOL_OPERATOR_TOKEN:-}" \
  CSD_POOL_RESTORE_DRILL_CONFIRM=restore-drill \
  "$ROOT_DIR/ops/bin/csd-pool-restore-drill.sh"
write_dry_run_restore_http_reports
check_restore_api_safety "$REPORT_DIR/restore-api-safety.log"

run_workers_or_plan "live CSD node template contract" "$REPORT_DIR/check-node-template.json" \
  check-node-template
run_workers_or_plan "live CSD node runtime quorum" "$REPORT_DIR/node-runtime.json" \
  check-node-runtime
check_node_endpoint_safety "$REPORT_DIR/node-endpoint-safety.log"

run_workers_or_plan "isolated signer contract" "$REPORT_DIR/check-signer.json" \
  check-signer
check_signer_safety "$REPORT_DIR/signer-safety.log"

run_workers_or_plan "runtime health sample" "$REPORT_DIR/sample-health.json" \
  sample-health

run_workers_or_plan "payout preview safety report" "$REPORT_DIR/payout-preview.json" \
  payout-preview

check_http "$API_URL/health" "api health endpoint" "$REPORT_DIR/http-health.txt"
check_http "$API_URL/status" "public status page" "$REPORT_DIR/http-status-page.txt"
check_http "$API_URL/getting-started" "getting started page" "$REPORT_DIR/http-getting-started-page.txt"
check_http "$API_URL/api/status" "public status endpoint" "$REPORT_DIR/http-api-status.json"
check_status_release_binding "$REPORT_DIR/status-release-binding.log"
check_runtime_status_binding "$REPORT_DIR/runtime-status-binding.log"
check_runtime_config_binding "$REPORT_DIR/runtime-config-binding.log"
check_http "$API_URL/api/pool" "public pool endpoint" "$REPORT_DIR/http-api-pool.json"
check_pool_endpoint_binding "$REPORT_DIR/http-api-pool.json" "$REPORT_DIR/http-api-status.json" "$REPORT_DIR/pool-endpoint-binding.log" "local"
check_http "$API_URL/api/metrics" "public metrics endpoint" "$REPORT_DIR/http-api-metrics.json"
check_http "$API_URL/metrics" "prometheus metrics endpoint" "$REPORT_DIR/http-prometheus-metrics.txt"
check_metrics_surface_safety "$REPORT_DIR/metrics-surface-safety.log"
check_http "$API_URL/api/blocks" "public blocks endpoint" "$REPORT_DIR/http-api-blocks.json"
check_http "$API_URL/api/payments" "public payments endpoint" "$REPORT_DIR/http-api-payments.json"
check_http "$API_URL/api/getting-started" "getting started endpoint" "$REPORT_DIR/http-api-getting-started.json"
check_getting_started_binding "$REPORT_DIR/http-api-getting-started.json" "$REPORT_DIR/getting-started-binding.log" "local"
check_public_dns_safety "$REPORT_DIR/public-dns-safety.log"
check_public_api_tls_safety "$REPORT_DIR/public-api-tls-safety.log"
check_public_api_headers_safety "$REPORT_DIR/public-api-headers-safety.log"
check_public_api_surface_safety "$REPORT_DIR/public-api-surface-safety.log"
check_public_operator_auth_boundary "$REPORT_DIR/public-operator-auth-boundary.log"
check_public_http "/api/status" "external public status endpoint" "$REPORT_DIR/http-public-api-status.json"
check_external_public_status_binding "$REPORT_DIR/external-public-status-binding.log"
check_external_public_config_binding "$REPORT_DIR/external-public-config-binding.log"
check_public_http "/api/pool" "external public pool endpoint" "$REPORT_DIR/http-public-api-pool.json"
check_pool_endpoint_binding "$REPORT_DIR/http-public-api-pool.json" "$REPORT_DIR/http-public-api-status.json" "$REPORT_DIR/external-public-pool-binding.log" "external public"
check_public_http "/api/getting-started" "external public getting started endpoint" "$REPORT_DIR/http-public-api-getting-started.json"
check_getting_started_binding "$REPORT_DIR/http-public-api-getting-started.json" "$REPORT_DIR/external-public-getting-started-binding.log" "external public"
check_public_http "/getting-started" "external getting started page" "$REPORT_DIR/http-public-getting-started.txt"
check_tcp "$STRATUM_ADDR" "public stratum TCP connect" "$REPORT_DIR/stratum-tcp.log"
check_public_tcp "external public stratum TCP connect" "$REPORT_DIR/public-stratum-tcp.log"
check_public_port_tiers_safety "$REPORT_DIR/public-port-tiers-safety.log"
check_public_port_tiers_smoke "$REPORT_DIR/public-port-tiers-smoke.json"
check_public_stratum_smoke "$REPORT_DIR/public-stratum-smoke.json"
check_public_stratum_load "$REPORT_DIR/public-stratum-load.json"

check_operator_http "/api/operator/health" "operator health endpoint" "$REPORT_DIR/http-operator-health.json"
check_operator_http "/api/operator/alerts?status=active&limit=20" "operator alerts endpoint" "$REPORT_DIR/http-operator-alerts.json"
check_operator_readiness_safety "$REPORT_DIR/operator-readiness-safety.log"
check_operator_http "/api/operator/payouts" "operator payout batches endpoint" "$REPORT_DIR/http-operator-payout-batches.json"
check_operator_http "/api/operator/payouts/export.csv" "operator payout batches CSV export" "$REPORT_DIR/http-operator-payout-batches.csv"
check_operator_http "/api/operator/payouts/audit?limit=20" "operator payout audit endpoint" "$REPORT_DIR/http-operator-payout-audit.json"
check_operator_http "/api/operator/payouts/audit/export.csv?limit=1000" "operator payout audit CSV export" "$REPORT_DIR/http-operator-payout-audit.csv"
check_operator_http "/api/operator/payouts/preview" "operator payout preview endpoint" "$REPORT_DIR/http-operator-payout-preview.json"
check_operator_http "/api/operator/payouts/status" "operator payout status endpoint" "$REPORT_DIR/http-operator-payout-status.json"
check_payout_safety "$REPORT_DIR/payout-safety.log"
check_payout_controls_safety "$REPORT_DIR/payout-controls-safety.log"
check_evidence_redaction_safety "$REPORT_DIR/evidence-redaction-safety.log"

printf 'reports=%s\n' "$REPORT_DIR"
write_final_reports
write_evidence_archive
printf 'go_live_report=%s\n' "$REPORT_TXT"
printf 'go_live_summary=%s\n' "$SUMMARY_JSON"
printf 'go_live_evidence=%s\n' "$EVIDENCE_ARCHIVE"
printf 'go_live_evidence_sha256=%s\n' "$EVIDENCE_SHA256"
printf 'summary: pass=%s fail=%s skip=%s\n' "$PASS" "$FAIL" "$SKIP"
if [[ "$FAIL" -gt 0 ]]; then
  exit 1
fi

#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
TARGET="${CSD_POOL_GO_LIVE_TARGET:-public-beta}"
TIMESTAMP="${CSD_POOL_REAL_GO_LIVE_TIMESTAMP:-$(date -u +%Y%m%dT%H%M%SZ)}"
REPORT_DIR="${CSD_POOL_GO_LIVE_REPORT_DIR:-/var/tmp/csd-pool-${TARGET}-go-live-${TIMESTAMP}}"
EVIDENCE_NAME="${CSD_POOL_GO_LIVE_EVIDENCE_NAME:-csd-pool-${TARGET}-go-live-${TIMESTAMP}}"
GO_LIVE_SCRIPT="$ROOT_DIR/ops/bin/csd-pool-go-live-check.sh"
VERIFY_SCRIPT="$ROOT_DIR/ops/bin/csd-pool-verify-go-live-evidence.sh"
SIGNOFF_SCRIPT="$ROOT_DIR/ops/bin/csd-pool-generate-signoff.sh"
RECEIPT_SCRIPT="$ROOT_DIR/ops/bin/csd-pool-export-real-go-live-receipt.sh"
DOCTOR_SCRIPT="$ROOT_DIR/ops/bin/csd-pool-real-env-doctor.sh"

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
}

require_file() {
  local path="$1"
  local label="$2"
  [[ -n "$path" ]] || fail "$label is required"
  [[ -f "$path" ]] || fail "$label not found: $path"
}

require_executable() {
  local path="$1"
  local label="$2"
  [[ -x "$path" ]] || fail "$label not executable: $path"
}

require_value() {
  local value="$1"
  local label="$2"
  [[ -n "$value" ]] || fail "$label is required"
}

sha256_value() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$path" | awk '{print $1}'
  else
    fail "sha256 tool missing"
  fi
}

check_real_go_live_postcheck() {
  local log_path="$1"
  local summary_path="$REPORT_DIR/go-live-summary.json"
  local report_path="$REPORT_DIR/GO-LIVE-REPORT.txt"
  local signoff_path="$REPORT_DIR/GO-LIVE-SIGNOFF.md"
  local archive_sha
  local sha_file_line
  archive_sha="$(sha256_value "$EVIDENCE_ARCHIVE")"
  sha_file_line="$(sed -n '1p' "$EVIDENCE_SHA256" 2>/dev/null || true)"

  if python3 - \
    "$summary_path" \
    "$report_path" \
    "$signoff_path" \
    "$EVIDENCE_ARCHIVE" \
    "$EVIDENCE_SHA256" \
    "$archive_sha" \
    "$sha_file_line" \
    "$TARGET" \
    >"$log_path" 2>&1 <<'PY'
import json
import pathlib
import sys

summary_path, report_path, signoff_path, archive_path, sha_path, archive_sha, sha_file_line, target = sys.argv[1:9]
summary_p = pathlib.Path(summary_path)
report_p = pathlib.Path(report_path)
signoff_p = pathlib.Path(signoff_path)
archive_p = pathlib.Path(archive_path)
sha_p = pathlib.Path(sha_path)

checks = {
    "summary_exists": summary_p.is_file(),
    "report_exists": report_p.is_file(),
    "signoff_exists": signoff_p.is_file(),
    "evidence_archive_exists": archive_p.is_file(),
    "evidence_sha256_exists": sha_p.is_file(),
}

summary = {}
if checks["summary_exists"]:
    with summary_p.open("r", encoding="utf-8") as f:
        summary = json.load(f)

checks.update(
    {
        "summary_status_passed": (summary.get("summary") or {}).get("status") == "passed",
        "summary_fail_zero": (summary.get("summary") or {}).get("fail") == 0,
        "summary_dry_run_false": summary.get("dry_run") is False,
        "summary_target_matches": summary.get("target") == target,
        "summary_evidence_archive_matches": (summary.get("evidence") or {}).get("archive") == archive_path,
        "summary_evidence_sha256_matches": (summary.get("evidence") or {}).get("sha256") == sha_path,
        "sha256_line_matches_archive": sha_file_line.startswith(archive_sha) and archive_p.name in sha_file_line,
    }
)

signoff_text = signoff_p.read_text(encoding="utf-8", errors="replace") if signoff_p.is_file() else ""
checks["signoff_records_dry_run_false"] = "- dry_run: `False`" in signoff_text or "- dry_run: `false`" in signoff_text
checks["signoff_records_passed_status"] = "- summary_status: `passed`" in signoff_text

for name, passed in checks.items():
    print(f"{name}={passed}")
print(f"archive_sha256={archive_sha}")
print(f"sha256_file_line={sha_file_line or 'missing'}")
print(f"real_go_live_postcheck_ok={all(checks.values())}")
if not all(checks.values()):
    sys.exit(1)
PY
  then
    printf 'real_go_live_postcheck=%s\n' "$log_path"
  else
    fail "real go-live postcheck failed; see $log_path"
  fi
}

write_launch_toolchain_manifest() {
  local path="$1"
  python3 - \
    "$path" \
    "$TARGET" \
    "$ROOT_DIR" \
    "$BIN_DIR/csd-pool-workers" \
    "$GO_LIVE_SCRIPT" \
    "$VERIFY_SCRIPT" \
    "$SIGNOFF_SCRIPT" \
    "$RECEIPT_SCRIPT" \
    "$DOCTOR_SCRIPT" <<'PY'
import hashlib
import json
import pathlib
import sys

output, target, root, *paths = sys.argv[1:]
entries = []
for raw in paths:
    path = pathlib.Path(raw)
    entries.append(
        {
            "path": str(path),
            "basename": path.name,
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "executable": bool(path.stat().st_mode & 0o111),
        }
    )
payload = {
    "target": target,
    "root": root,
    "entries": entries,
    "required_basenames": [
        "csd-pool-workers",
        "csd-pool-go-live-check.sh",
        "csd-pool-verify-go-live-evidence.sh",
        "csd-pool-generate-signoff.sh",
        "csd-pool-export-real-go-live-receipt.sh",
        "csd-pool-real-env-doctor.sh",
    ],
}
pathlib.Path(output).write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY
}

reject_example_path() {
  local path="$1"
  local label="$2"
  case "$(basename "$path")" in
    *.example|*.example.*|example.*|config.example.toml|csd-pool.env.example)
      fail "$label must be a real environment file, not an example/template: $path"
      ;;
  esac
}

bool_text() {
  if [[ "$1" == "1" ]]; then
    printf 'True'
  else
    printf 'False'
  fi
}

is_example_path() {
  local path="$1"
  case "$(basename "$path")" in
    *.example|*.example.*|example.*|config.example.toml|csd-pool.env.example)
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

write_real_go_live_inputs_report() {
  local log_path="$1"
  local public_required=0
  local public_api_https=0
  local env_example=0
  local config_example=0
  local workers_present=0
  local go_live_present=0
  local verifier_present=0
  local signoff_present=0
  local dry_run_false=0
  local inputs_ok=0

  if [[ "$TARGET" == "public-beta" || "$TARGET" == "production" ]]; then
    public_required=1
  fi
  case "$PUBLIC_API_URL" in
    https://*) public_api_https=1 ;;
  esac
  if is_example_path "$ENV_PATH"; then env_example=1; fi
  if is_example_path "$CONFIG_PATH"; then config_example=1; fi
  [[ -x "$BIN_DIR/csd-pool-workers" ]] && workers_present=1
  [[ -x "$GO_LIVE_SCRIPT" ]] && go_live_present=1
  [[ -x "$VERIFY_SCRIPT" ]] && verifier_present=1
  [[ -x "$SIGNOFF_SCRIPT" ]] && signoff_present=1
  [[ "${CSD_POOL_GO_LIVE_DRY_RUN:-0}" != "1" ]] && dry_run_false=1

  if [[ -f "$ENV_PATH" && -f "$CONFIG_PATH" && \
        "$env_example" == "0" && "$config_example" == "0" && \
        "$workers_present" == "1" && "$go_live_present" == "1" && \
        "$verifier_present" == "1" && "$signoff_present" == "1" && \
        -n "$API_URL" && -n "$STRATUM_ADDR" && "$dry_run_false" == "1" ]]; then
    if [[ "$public_required" == "0" || ( -n "$PUBLIC_API_URL" && -n "$PUBLIC_STRATUM_ADDR" && "$public_api_https" == "1" ) ]]; then
      inputs_ok=1
    fi
  fi

  {
    printf 'target=%s\n' "$TARGET"
    printf 'report_dir=%s\n' "$REPORT_DIR"
    printf 'evidence_name=%s\n' "$EVIDENCE_NAME"
    printf 'env_path=%s\n' "$ENV_PATH"
    printf 'env_sha256=%s\n' "$(sha256_value "$ENV_PATH")"
    printf 'env_example_path=%s\n' "$(bool_text "$env_example")"
    printf 'config_path=%s\n' "$CONFIG_PATH"
    printf 'config_sha256=%s\n' "$(sha256_value "$CONFIG_PATH")"
    printf 'config_example_path=%s\n' "$(bool_text "$config_example")"
    printf 'bin_dir=%s\n' "$BIN_DIR"
    printf 'workers_bin=%s\n' "$BIN_DIR/csd-pool-workers"
    printf 'workers_bin_sha256=%s\n' "$(sha256_value "$BIN_DIR/csd-pool-workers")"
    printf 'go_live_script=%s\n' "$GO_LIVE_SCRIPT"
    printf 'go_live_script_sha256=%s\n' "$(sha256_value "$GO_LIVE_SCRIPT")"
    printf 'verify_script=%s\n' "$VERIFY_SCRIPT"
    printf 'verify_script_sha256=%s\n' "$(sha256_value "$VERIFY_SCRIPT")"
    printf 'signoff_script=%s\n' "$SIGNOFF_SCRIPT"
    printf 'signoff_script_sha256=%s\n' "$(sha256_value "$SIGNOFF_SCRIPT")"
    printf 'api_url=%s\n' "$API_URL"
    printf 'stratum_addr=%s\n' "$STRATUM_ADDR"
    printf 'public_required=%s\n' "$(bool_text "$public_required")"
    printf 'public_api_url=%s\n' "${PUBLIC_API_URL:-missing}"
    printf 'public_api_https=%s\n' "$(bool_text "$public_api_https")"
    printf 'public_stratum_addr=%s\n' "${PUBLIC_STRATUM_ADDR:-missing}"
    printf 'dry_run_env_false=%s\n' "$(bool_text "$dry_run_false")"
    printf 'workers_bin_executable=%s\n' "$(bool_text "$workers_present")"
    printf 'go_live_script_executable=%s\n' "$(bool_text "$go_live_present")"
    printf 'verify_script_executable=%s\n' "$(bool_text "$verifier_present")"
    printf 'signoff_script_executable=%s\n' "$(bool_text "$signoff_present")"
    printf 'real_go_live_inputs_ok=%s\n' "$(bool_text "$inputs_ok")"
  } >"$log_path"

  if [[ "$inputs_ok" != "1" ]]; then
    fail "real go-live inputs report failed; see $log_path"
  fi
}

case "$TARGET" in
  private-beta|public-beta|production) ;;
  *) fail "CSD_POOL_GO_LIVE_TARGET must be private-beta, public-beta, or production" ;;
esac

if [[ "${CSD_POOL_GO_LIVE_DRY_RUN:-0}" == "1" ]]; then
  fail "real go-live wrapper refuses CSD_POOL_GO_LIVE_DRY_RUN=1"
fi

ENV_PATH="${CSD_POOL_ENV_FILE:-}"
CONFIG_PATH="${CSD_POOL_GO_LIVE_CONFIG:-${CSD_POOL_CONFIG:-}}"
BIN_DIR="${CSD_POOL_BIN_DIR:-/opt/csd-pool/bin}"
API_URL="${CSD_POOL_GO_LIVE_API_URL:-}"
STRATUM_ADDR="${CSD_POOL_GO_LIVE_STRATUM_ADDR:-}"
PUBLIC_API_URL="${CSD_POOL_GO_LIVE_PUBLIC_API_URL:-${CSD_POOL_PUBLIC_API_URL:-}}"
PUBLIC_STRATUM_ADDR="${CSD_POOL_GO_LIVE_PUBLIC_STRATUM_ADDR:-${CSD_POOL_PUBLIC_STRATUM_ADDR:-}}"

require_file "$ENV_PATH" "CSD_POOL_ENV_FILE"
require_file "$CONFIG_PATH" "CSD_POOL_CONFIG or CSD_POOL_GO_LIVE_CONFIG"
reject_example_path "$ENV_PATH" "CSD_POOL_ENV_FILE"
reject_example_path "$CONFIG_PATH" "CSD_POOL_CONFIG"
require_executable "$BIN_DIR/csd-pool-workers" "csd-pool-workers"
require_executable "$GO_LIVE_SCRIPT" "go-live check script"
require_executable "$VERIFY_SCRIPT" "go-live evidence verifier"
require_executable "$SIGNOFF_SCRIPT" "go-live signoff generator"
require_executable "$RECEIPT_SCRIPT" "real go-live receipt exporter"
require_executable "$DOCTOR_SCRIPT" "real environment doctor"
require_value "$API_URL" "CSD_POOL_GO_LIVE_API_URL"
require_value "$STRATUM_ADDR" "CSD_POOL_GO_LIVE_STRATUM_ADDR"

if [[ "$TARGET" == "public-beta" || "$TARGET" == "production" ]]; then
  require_value "$PUBLIC_API_URL" "CSD_POOL_GO_LIVE_PUBLIC_API_URL"
  require_value "$PUBLIC_STRATUM_ADDR" "CSD_POOL_GO_LIVE_PUBLIC_STRATUM_ADDR"
  case "$PUBLIC_API_URL" in
    https://*) ;;
    *) fail "public-beta/production public API URL must use https: $PUBLIC_API_URL" ;;
  esac
fi

mkdir -p "$REPORT_DIR"
REPORT_DIR="$(cd "$REPORT_DIR" && pwd)"
EVIDENCE_ARCHIVE="$REPORT_DIR/${EVIDENCE_NAME}.tar.gz"
EVIDENCE_SHA256="$EVIDENCE_ARCHIVE.sha256"
INPUTS_LOG="$REPORT_DIR/real-go-live-inputs.log"
TOOLCHAIN_MANIFEST="$REPORT_DIR/launch-toolchain-manifest.json"
DOCTOR_DIR="$REPORT_DIR/real-env-doctor"
DOCTOR_REPORT="$DOCTOR_DIR/REAL-ENVIRONMENT-DOCTOR.txt"
DOCTOR_SUMMARY="$DOCTOR_DIR/real-environment-doctor-summary.json"

printf 'CSD Pool real go-live wrapper\n'
printf 'target=%s\n' "$TARGET"
printf 'root=%s\n' "$ROOT_DIR"
printf 'env=%s\n' "$ENV_PATH"
printf 'config=%s\n' "$CONFIG_PATH"
printf 'bin_dir=%s\n' "$BIN_DIR"
printf 'api=%s\n' "$API_URL"
printf 'stratum=%s\n' "$STRATUM_ADDR"
printf 'public_api=%s\n' "${PUBLIC_API_URL:-not-required}"
printf 'public_stratum=%s\n' "${PUBLIC_STRATUM_ADDR:-not-required}"
printf 'report_dir=%s\n' "$REPORT_DIR"
printf 'evidence=%s\n' "$EVIDENCE_ARCHIVE"

write_real_go_live_inputs_report "$INPUTS_LOG"
write_launch_toolchain_manifest "$TOOLCHAIN_MANIFEST"

CSD_POOL_DOCTOR_OUTPUT_DIR="$DOCTOR_DIR" \
CSD_POOL_GO_LIVE_TARGET="$TARGET" \
  "$DOCTOR_SCRIPT" "$ENV_PATH" "$CONFIG_PATH"

CSD_POOL_GO_LIVE_DRY_RUN=0 \
CSD_POOL_GO_LIVE_TARGET="$TARGET" \
CSD_POOL_ENV_FILE="$ENV_PATH" \
CSD_POOL_CONFIG="$CONFIG_PATH" \
CSD_POOL_GO_LIVE_CONFIG="$CONFIG_PATH" \
CSD_POOL_BIN_DIR="$BIN_DIR" \
CSD_POOL_GO_LIVE_API_URL="$API_URL" \
CSD_POOL_GO_LIVE_STRATUM_ADDR="$STRATUM_ADDR" \
CSD_POOL_GO_LIVE_PUBLIC_API_URL="$PUBLIC_API_URL" \
CSD_POOL_GO_LIVE_PUBLIC_STRATUM_ADDR="$PUBLIC_STRATUM_ADDR" \
CSD_POOL_GO_LIVE_REPORT_DIR="$REPORT_DIR" \
CSD_POOL_GO_LIVE_EVIDENCE_NAME="$EVIDENCE_NAME" \
  "$GO_LIVE_SCRIPT"

"$VERIFY_SCRIPT" "$EVIDENCE_ARCHIVE"
"$SIGNOFF_SCRIPT" "$EVIDENCE_ARCHIVE" "$REPORT_DIR/GO-LIVE-SIGNOFF.md"
check_real_go_live_postcheck "$REPORT_DIR/real-go-live-postcheck.log"

archive_sha="$(sha256_value "$EVIDENCE_ARCHIVE")"
inputs_sha="$(sha256_value "$INPUTS_LOG")"
toolchain_sha="$(sha256_value "$TOOLCHAIN_MANIFEST")"
doctor_report_sha="$(sha256_value "$DOCTOR_REPORT")"
doctor_summary_sha="$(sha256_value "$DOCTOR_SUMMARY")"
summary_sha="$(sha256_value "$REPORT_DIR/go-live-summary.json")"
report_sha="$(sha256_value "$REPORT_DIR/GO-LIVE-REPORT.txt")"
signoff_sha="$(sha256_value "$REPORT_DIR/GO-LIVE-SIGNOFF.md")"

{
  printf 'status=passed\n'
  printf 'target=%s\n' "$TARGET"
  printf 'timestamp_utc=%s\n' "$TIMESTAMP"
  printf 'report_dir=%s\n' "$REPORT_DIR"
  printf 'real_go_live_inputs=%s\n' "$INPUTS_LOG"
  printf 'real_go_live_inputs_sha256=%s\n' "$inputs_sha"
  printf 'launch_toolchain_manifest=%s\n' "$TOOLCHAIN_MANIFEST"
  printf 'launch_toolchain_manifest_sha256=%s\n' "$toolchain_sha"
  printf 'real_environment_doctor_report=%s\n' "$DOCTOR_REPORT"
  printf 'real_environment_doctor_report_sha256=%s\n' "$doctor_report_sha"
  printf 'real_environment_doctor_summary=%s\n' "$DOCTOR_SUMMARY"
  printf 'real_environment_doctor_summary_sha256=%s\n' "$doctor_summary_sha"
  printf 'go_live_report=%s\n' "$REPORT_DIR/GO-LIVE-REPORT.txt"
  printf 'go_live_report_sha256=%s\n' "$report_sha"
  printf 'go_live_summary=%s\n' "$REPORT_DIR/go-live-summary.json"
  printf 'go_live_summary_sha256=%s\n' "$summary_sha"
  printf 'go_live_signoff=%s\n' "$REPORT_DIR/GO-LIVE-SIGNOFF.md"
  printf 'go_live_signoff_sha256=%s\n' "$signoff_sha"
  printf 'real_go_live_postcheck=%s\n' "$REPORT_DIR/real-go-live-postcheck.log"
  printf 'evidence_archive=%s\n' "$EVIDENCE_ARCHIVE"
  printf 'evidence_sha256=%s\n' "$EVIDENCE_SHA256"
  printf 'evidence_archive_sha256=%s\n' "$archive_sha"
} >"$REPORT_DIR/REAL-GO-LIVE-SUMMARY.txt"

"$RECEIPT_SCRIPT" "$REPORT_DIR/REAL-GO-LIVE-SUMMARY.txt" "$REPORT_DIR"

printf 'real_go_live_summary=%s\n' "$REPORT_DIR/REAL-GO-LIVE-SUMMARY.txt"
printf 'go_live_signoff=%s\n' "$REPORT_DIR/GO-LIVE-SIGNOFF.md"
printf 'summary: real go-live evidence verified\n'

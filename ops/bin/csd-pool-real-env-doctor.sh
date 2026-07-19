#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
ENV_PATH="${1:-${CSD_POOL_ENV_FILE:-/etc/csd-pool/csd-pool.env}}"
CONFIG_PATH="${2:-${CSD_POOL_CONFIG:-/etc/csd-pool/config.toml}}"
OUTPUT_DIR="${CSD_POOL_DOCTOR_OUTPUT_DIR:-/tmp/csd-pool-real-env-doctor}"
TARGET="${CSD_POOL_GO_LIVE_TARGET:-public-beta}"
ALLOW_OPEN="${CSD_POOL_DOCTOR_ALLOW_OPEN:-0}"

fail() {
  printf 'fail: %s\n' "$1" >&2
  exit 1
}

usage() {
  printf 'usage: %s /path/to/csd-pool.env /path/to/config.toml\n' "$(basename "$0")" >&2
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

mkdir -p "$OUTPUT_DIR"
OUTPUT_DIR="$(cd "$OUTPUT_DIR" && pwd)"

python3 - \
  "$ENV_PATH" \
  "$CONFIG_PATH" \
  "$OUTPUT_DIR/real-environment-doctor-summary.json" \
  "$OUTPUT_DIR/REAL-ENVIRONMENT-DOCTOR.txt" \
  "$TARGET" <<'PY'
import json
import os
import pathlib
import re
import shutil
import socket
import stat
import subprocess
import sys
from datetime import datetime, timezone
from decimal import Decimal, InvalidOperation
from urllib.parse import urlparse
import ipaddress

env_path, config_path, summary_out, report_out, target = sys.argv[1:6]

PLACEHOLDERS = (
    "change-me",
    "placeholder",
    "replace-me",
    "redacted",
    "<redacted>",
    "example.com",
    "example.net",
    "example.org",
    "pool.example",
)
REQUIRED_ENV = [
    "CSD_POOL_DATABASE_URL",
    "CSD_POOL_RESTORE_DATABASE_URL",
    "CSD_POOL_TEMPLATE_MODE",
    "CSD_POOL_SUBMIT_CANDIDATES",
    "CSD_POOL_NODE_TOKEN",
    "CSD_POOL_OPERATOR_TOKEN",
    "CSD_POOL_SIGNER_URL",
    "CSD_POOL_SIGNER_TOKEN",
    "CSD_POOL_SIGNER_WALLET_ADDRESS",
    "CSD_POOL_SIGNER_PRIVATE_KEY_FILE",
    "CSD_POOL_SIGNER_NODE_URL",
    "CSD_POOL_WATCH_NODE_URL",
    "CSD_POOL_SUBMIT_NODE_URL",
    "CSD_POOL_PAYOUT_NODE_URL",
    "CSD_POOL_PUBLIC_API_URL",
    "CSD_POOL_PUBLIC_STRATUM_ADDR",
]
EXAMPLE_WALLET_ADDRESS = "0123456789abcdef0123456789abcdef01234567"


def add(checks, key, passed, severity, detail="", remediation=""):
    checks.append(
        {
            "key": key,
            "passed": bool(passed),
            "severity": severity,
            "detail": detail,
            "remediation": remediation,
        }
    )


def is_example_path(path):
    name = pathlib.Path(path).name.lower()
    return name.endswith(".example") or ".example." in name or name in {"config.example.toml", "csd-pool.env.example"}


def read_text(path):
    try:
        return pathlib.Path(path).read_text(encoding="utf-8")
    except FileNotFoundError:
        return ""


def parse_env(path):
    env = {}
    pattern = re.compile(r"^(?:export\s+)?([A-Za-z_][A-Za-z0-9_]*)=(.*)$")
    for raw in read_text(path).splitlines():
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        match = pattern.match(line)
        if not match:
            continue
        key, value = match.groups()
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "'\"":
            value = value[1:-1]
        env[key] = value
    return env


def has_placeholder(value):
    lowered = str(value or "").lower()
    return any(marker in lowered for marker in PLACEHOLDERS)


def token_ok(value):
    return bool(value) and len(value) >= 32 and not has_placeholder(value)


def parsed_url(value):
    try:
        return urlparse(value or "")
    except Exception:
        return urlparse("")


def redact_url(value):
    raw = str(value or "")
    if not raw:
        return "missing"
    parsed = parsed_url(raw)
    if not parsed.scheme or not parsed.netloc:
        return raw
    hostname = parsed.hostname or ""
    netloc = hostname
    if parsed.username:
        netloc = parsed.username
        if parsed.password is not None:
            netloc += ":<redacted>"
        netloc += f"@{hostname}"
    if parsed.port is not None:
        netloc += f":{parsed.port}"
    return parsed._replace(netloc=netloc).geturl()


def host_is_global(host):
    if not host:
        return False, "missing host"
    try:
        ip = ipaddress.ip_address(host)
        return ip.is_global, f"literal_ip={ip}"
    except ValueError:
        pass
    try:
        infos = socket.getaddrinfo(host, None)
    except OSError as exc:
        return False, f"dns_lookup_failed={exc}"
    addresses = sorted({info[4][0] for info in infos})
    global_addresses = []
    blocked = []
    for address in addresses:
        try:
            ip = ipaddress.ip_address(address)
        except ValueError:
            blocked.append(address)
            continue
        if ip.is_global:
            global_addresses.append(address)
        else:
            blocked.append(address)
    if global_addresses:
        return True, "global_addresses=" + ",".join(global_addresses)
    return False, "non_global_addresses=" + ",".join(blocked or addresses)


def endpoint_non_mock_public_url(value):
    url = parsed_url(value)
    if url.scheme not in {"http", "https"} or not url.hostname:
        return False, "must be http(s) URL with host"
    global_ok, detail = host_is_global(url.hostname)
    if has_placeholder(value):
        return False, "contains fixture/example marker"
    return global_ok, detail


def endpoint_internal_url(value):
    url = parsed_url(value)
    if url.scheme not in {"http", "https"} or not url.hostname:
        return False, "must be http(s) URL with host"
    if has_placeholder(value):
        return False, "contains fixture/example marker"
    global_ok, detail = host_is_global(url.hostname)
    return not global_ok, detail


def toml_scalar_value(text, section, wanted_key):
    in_section = False
    for raw in text.splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line:
            continue
        if line.startswith("[") and line.endswith("]"):
            in_section = line == f"[{section}]"
            continue
        if not in_section or "=" not in line:
            continue
        key, value = line.split("=", 1)
        if key.strip() != wanted_key:
            continue
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "'\"":
            value = value[1:-1]
        return value
    return ""


def toml_listen_value(text, section):
    return toml_scalar_value(text, section, "listen")


def csd_amount(value):
    try:
        amount = Decimal(str(value or ""))
    except InvalidOperation:
        return None
    if not amount.is_finite() or amount.as_tuple().exponent < -8:
        return None
    return amount


def listen_loopback(value):
    raw = str(value or "")
    if not raw:
        return False, "missing"
    if raw.startswith("[") and "]:" in raw:
        host, port = raw[1:].split("]:", 1)
    elif ":" in raw:
        host, port = raw.rsplit(":", 1)
    else:
        return False, "must be host:port"
    host = host.strip().lower()
    if not port.isdigit() or int(port) <= 0 or int(port) > 65535:
        return False, "port invalid"
    if host == "localhost":
        return True, f"host={host} port={port}"
    try:
        ip = ipaddress.ip_address(host)
        return ip.is_loopback, f"host={host} port={port}"
    except ValueError:
        return False, f"host={host} is not a loopback literal"


def stratum_host_port(value):
    raw = value or ""
    if has_placeholder(raw):
        return False, "contains fixture/example marker"
    if ":" not in raw:
        return False, "must be host:port"
    host, port = raw.rsplit(":", 1)
    if not host or not port.isdigit():
        return False, "must be host:port with numeric port"
    if int(port) <= 0 or int(port) > 65535:
        return False, "port out of range"
    global_ok, detail = host_is_global(host.strip("[]"))
    return global_ok, detail


checks = []
env_file = pathlib.Path(env_path)
config_file = pathlib.Path(config_path)
env_exists = env_file.is_file()
config_exists = config_file.is_file()
env_text = read_text(env_path)
config_text = read_text(config_path)
env = parse_env(env_path) if env_exists else {}

add(checks, "env_file_exists", env_exists, "hard", env_path, "Create the production env file before real go-live.")
add(checks, "config_file_exists", config_exists, "hard", config_path, "Install the reviewed production config.toml before real go-live.")
add(checks, "env_file_not_example", env_exists and not is_example_path(env_path), "hard", env_path, "Use a real env file, not ops/env/csd-pool.env.example.")
add(checks, "config_file_not_example", config_exists and not is_example_path(config_path), "hard", config_path, "Use a real config file, not config.example.toml.")

if env_exists:
    mode = stat.S_IMODE(env_file.stat().st_mode)
    add(checks, "env_file_not_world_readable", (mode & 0o007) == 0, "hard", oct(mode), "Set env permissions to 0640 or stricter.")
else:
    add(checks, "env_file_not_world_readable", False, "hard", "missing", "Create and restrict the env file.")

missing_keys = [key for key in REQUIRED_ENV if not env.get(key)]
add(checks, "required_env_keys_present", not missing_keys, "hard", "missing=" + ",".join(missing_keys) if missing_keys else "all required keys present", "Populate all go-live env keys.")

placeholder_keys = [key for key, value in env.items() if has_placeholder(value)]
add(checks, "env_has_no_placeholder_values", not placeholder_keys, "hard", "placeholder_keys=" + ",".join(placeholder_keys) if placeholder_keys else "no placeholder env values", "Replace placeholder/example env values.")
add(checks, "config_has_no_placeholder_values", config_exists and not has_placeholder(config_text), "hard", "config placeholder scan", "Replace placeholder/example config values.")
add(checks, "template_mode_live", env.get("CSD_POOL_TEMPLATE_MODE", "").strip().lower() == "live", "hard", f"mode={env.get('CSD_POOL_TEMPLATE_MODE', 'missing')}", "Set CSD_POOL_TEMPLATE_MODE=live for a real launch.")
add(checks, "submit_candidates_enabled", env.get("CSD_POOL_SUBMIT_CANDIDATES", "").strip().lower() == "true", "hard", f"enabled={env.get('CSD_POOL_SUBMIT_CANDIDATES', 'missing')}", "Set CSD_POOL_SUBMIT_CANDIDATES=true so solved candidates reach the CSD network.")

for section, key in (
    ("stratum", "config_stratum_listen_loopback"),
    ("api", "config_api_listen_loopback"),
    ("signer", "config_signer_listen_loopback"),
):
    listen_value = toml_listen_value(config_text, section) if config_exists else ""
    listen_ok, listen_detail = listen_loopback(listen_value)
    add(
        checks,
        key,
        listen_ok,
        "hard",
        listen_detail,
        f"Set [{section}].listen to a loopback host:port and expose public traffic only through the edge proxy.",
    )

mining_address = toml_scalar_value(config_text, "pool", "mining_address") if config_exists else ""
mining_address_normalized = mining_address.lower()
mining_address_valid = bool(re.fullmatch(r"[0-9a-fA-F]{40}", mining_address))
add(checks, "config_mining_address_valid", mining_address_valid, "hard", f"len={len(mining_address)}", "Set [pool].mining_address to the real 40-hex CSD reward address.")
add(checks, "config_mining_address_not_example", mining_address_valid and mining_address_normalized != EXAMPLE_WALLET_ADDRESS, "hard", "example=false" if mining_address_valid and mining_address_normalized != EXAMPLE_WALLET_ADDRESS else "example_or_invalid=true", "Replace the bundled example [pool].mining_address with the production reward address.")

payout_values = {
    key: csd_amount(toml_scalar_value(config_text, "pool", key)) if config_exists else None
    for key in (
        "minimum_payout_csd",
        "manual_payout_approval_csd",
        "max_payout_batch_csd",
        "max_daily_payout_csd",
    )
}
minimum = payout_values["minimum_payout_csd"]
manual = payout_values["manual_payout_approval_csd"]
batch = payout_values["max_payout_batch_csd"]
daily = payout_values["max_daily_payout_csd"]
payout_limits_valid = (
    None not in (minimum, manual, batch, daily)
    and minimum > 0
    and minimum <= manual < batch <= daily
)
add(
    checks,
    "config_payout_limits_valid",
    payout_limits_valid,
    "hard",
    "minimum<=manual<batch<=daily" if payout_limits_valid else "missing, invalid, non-positive, or unordered payout limits",
    "Set positive [pool] payout limits ordered as minimum <= manual approval < max batch <= max daily, with at most 8 decimals.",
)

add(checks, "operator_token_production_length", token_ok(env.get("CSD_POOL_OPERATOR_TOKEN")), "hard", f"len={len(env.get('CSD_POOL_OPERATOR_TOKEN', ''))}", "Use a fresh operator token of at least 32 characters.")
add(checks, "signer_token_production_length", token_ok(env.get("CSD_POOL_SIGNER_TOKEN")), "hard", f"len={len(env.get('CSD_POOL_SIGNER_TOKEN', ''))}", "Use a fresh signer token of at least 32 characters.")
add(checks, "node_token_production_length", token_ok(env.get("CSD_POOL_NODE_TOKEN")), "hard", f"len={len(env.get('CSD_POOL_NODE_TOKEN', ''))}", "Use a fresh node adapter token of at least 32 characters and configure it as CSD_POOL_ADAPTER_TOKEN on adapter nodes.")

db_url = parsed_url(env.get("CSD_POOL_DATABASE_URL"))
restore_url = parsed_url(env.get("CSD_POOL_RESTORE_DATABASE_URL"))
add(checks, "database_url_is_postgres", db_url.scheme in {"postgres", "postgresql"}, "hard", redact_url(env.get("CSD_POOL_DATABASE_URL")), "Set CSD_POOL_DATABASE_URL to a PostgreSQL URL.")
add(checks, "restore_database_url_is_postgres", restore_url.scheme in {"postgres", "postgresql"}, "hard", redact_url(env.get("CSD_POOL_RESTORE_DATABASE_URL")), "Set CSD_POOL_RESTORE_DATABASE_URL to a PostgreSQL URL.")
add(checks, "database_url_has_password", bool(db_url.password) and not has_placeholder(db_url.password), "hard", redact_url(env.get("CSD_POOL_DATABASE_URL")), "Set CSD_POOL_DATABASE_URL with a real database password, not a redacted placeholder.")
add(checks, "restore_database_url_has_password", bool(restore_url.password) and not has_placeholder(restore_url.password), "hard", redact_url(env.get("CSD_POOL_RESTORE_DATABASE_URL")), "Set CSD_POOL_RESTORE_DATABASE_URL with a real database password, not a redacted placeholder.")
add(checks, "restore_database_is_separate", bool(db_url.geturl()) and bool(restore_url.geturl()) and db_url.geturl() != restore_url.geturl(), "hard", "database and restore URLs compared", "Use a separate restore database URL.")

public_api = env.get("CSD_POOL_PUBLIC_API_URL", "")
public_api_url = parsed_url(public_api)
add(checks, "public_api_is_https", public_api_url.scheme == "https" and bool(public_api_url.hostname), "hard", redact_url(public_api), "Expose the public API via HTTPS.")
api_global, api_detail = host_is_global(public_api_url.hostname) if public_api_url.hostname else (False, "missing host")
add(checks, "public_api_dns_global", api_global and not has_placeholder(public_api), "hard", api_detail, "Point public API DNS at a globally routable address.")
stratum_ok, stratum_detail = stratum_host_port(env.get("CSD_POOL_PUBLIC_STRATUM_ADDR"))
add(checks, "public_stratum_addr_global", stratum_ok, "hard", stratum_detail, "Set public Stratum to a globally routable host:port.")

watch_ok, watch_detail = endpoint_non_mock_public_url(env.get("CSD_POOL_WATCH_NODE_URL"))
submit_ok, submit_detail = endpoint_non_mock_public_url(env.get("CSD_POOL_SUBMIT_NODE_URL"))
add(checks, "watch_node_url_real_non_loopback", watch_ok, "hard", watch_detail, "Use a real non-loopback CSD watch node URL.")
add(checks, "submit_node_url_real_non_loopback", submit_ok, "hard", submit_detail, "Use a real non-loopback CSD submit node URL.")
payout_node_ok, payout_node_detail = endpoint_internal_url(env.get("CSD_POOL_PAYOUT_NODE_URL"))
add(checks, "payout_node_url_internal_not_public", payout_node_ok, "hard", payout_node_detail, "Set CSD_POOL_PAYOUT_NODE_URL to a direct local or private official CSD RPC base.")

signer_url = parsed_url(env.get("CSD_POOL_SIGNER_URL", ""))
add(checks, "signer_url_present", signer_url.scheme in {"http", "https"} and bool(signer_url.hostname), "hard", redact_url(env.get("CSD_POOL_SIGNER_URL")), "Set CSD_POOL_SIGNER_URL to the isolated signer health/sign endpoint.")
signer_internal_ok, signer_internal_detail = endpoint_internal_url(env.get("CSD_POOL_SIGNER_URL"))
add(checks, "signer_url_internal_not_public", signer_internal_ok, "hard", signer_internal_detail, "Keep CSD_POOL_SIGNER_URL on loopback or private network; never expose the payout signer on a public address.")
signer_node_ok, signer_node_detail = endpoint_internal_url(env.get("CSD_POOL_SIGNER_NODE_URL"))
add(checks, "signer_node_url_internal_not_public", signer_node_ok, "hard", signer_node_detail, "Set CSD_POOL_SIGNER_NODE_URL to a direct local or private CSD RPC base used by the official SDK.")
wallet = env.get("CSD_POOL_SIGNER_WALLET_ADDRESS", "")
wallet_valid = bool(re.fullmatch(r"[0-9a-fA-F]{40}", wallet or ""))
add(checks, "signer_wallet_address_shape", wallet_valid, "hard", f"len={len(wallet)}", "Set the expected 40-hex signer wallet address.")
add(checks, "signer_wallet_address_not_example", wallet_valid and wallet.lower() != EXAMPLE_WALLET_ADDRESS, "hard", "example=false" if wallet_valid and wallet.lower() != EXAMPLE_WALLET_ADDRESS else "example_or_invalid=true", "Replace the bundled example signer wallet address with the production payout wallet.")

key_path_raw = env.get("CSD_POOL_SIGNER_PRIVATE_KEY_FILE", "")
key_path = pathlib.Path(key_path_raw) if key_path_raw else None
key_exists = bool(key_path and key_path.is_file())
add(checks, "signer_private_key_file_exists", key_exists, "hard", key_path_raw or "missing", "Provision the signer key file on the target host before go-live.")
if key_exists:
    key_mode = stat.S_IMODE(key_path.stat().st_mode)
    key_text = read_text(key_path).strip()
    normalized_key = key_text[2:] if key_text.lower().startswith("0x") else key_text
    key_shape_ok = bool(re.fullmatch(r"[0-9a-fA-F]{64}", normalized_key)) and int(normalized_key, 16) != 0
    add(checks, "signer_private_key_permissions_restricted", (key_mode & 0o077) == 0, "hard", oct(key_mode), "Set the signer private key file to mode 0600 or stricter.")
    add(checks, "signer_private_key_shape_valid", key_shape_ok, "hard", "32-byte nonzero hex" if key_shape_ok else "invalid", "Store one valid nonzero 32-byte CSD private key as hex.")
else:
    add(checks, "signer_private_key_permissions_restricted", False, "hard", "missing", "Set the signer private key file to mode 0600 or stricter.")
    add(checks, "signer_private_key_shape_valid", False, "hard", "missing", "Store one valid nonzero 32-byte CSD private key as hex.")

node_binary = shutil.which("node")
node_major = 0
node_detail = "missing"
if node_binary:
    try:
        version_text = subprocess.run([node_binary, "--version"], check=True, capture_output=True, text=True, timeout=5).stdout.strip()
        version_match = re.fullmatch(r"v(\d+)(?:\.\d+){2}", version_text)
        node_major = int(version_match.group(1)) if version_match else 0
        node_detail = version_text or "invalid version"
    except (OSError, subprocess.SubprocessError):
        node_detail = "version probe failed"
add(checks, "signer_node_runtime_supported", node_major >= 18, "hard", node_detail, "Install Node.js 18 or newer on the signer host.")

hard_failures = [check for check in checks if check["severity"] == "hard" and not check["passed"]]
status = "ready_for_real_go_live" if not hard_failures else "needs_real_inputs"
summary = {
    "status": status,
    "target": "real-environment-doctor",
    "go_live_target": target,
    "generated_at_utc": datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z"),
    "env_path": env_path,
    "config_path": config_path,
    "hard_failures": len(hard_failures),
    "checks": checks,
}
pathlib.Path(summary_out).write_text(json.dumps(summary, indent=2, sort_keys=True) + "\n", encoding="utf-8")

lines = [
    "CSD Pool Real Environment Doctor",
    f"status={status}",
    f"go_live_target={target}",
    f"hard_failures={len(hard_failures)}",
    f"env_path={env_path}",
    f"config_path={config_path}",
    "",
    "Hard Checks",
]
for check in checks:
    prefix = "ok" if check["passed"] else "missing"
    lines.append(f"{prefix}: {check['key']} - {check['detail']}")
    if not check["passed"]:
        lines.append(f"  next={check['remediation']}")
lines.extend(["", f"summary_json={summary_out}"])
pathlib.Path(report_out).write_text("\n".join(lines) + "\n", encoding="utf-8")
PY

printf 'real_environment_doctor_report=%s\n' "$OUTPUT_DIR/REAL-ENVIRONMENT-DOCTOR.txt"
printf 'real_environment_doctor_summary=%s\n' "$OUTPUT_DIR/real-environment-doctor-summary.json"
status="$(python3 - "$OUTPUT_DIR/real-environment-doctor-summary.json" <<'PY'
import json
import sys
with open(sys.argv[1], "r", encoding="utf-8") as handle:
    print(json.load(handle).get("status", "unknown"))
PY
)"
printf 'status=%s\n' "$status"
printf 'summary: real environment doctor completed\n'

if [[ "$status" != "ready_for_real_go_live" && "$ALLOW_OPEN" != "1" ]]; then
  exit 1
fi

#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${CSD_POOL_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
BIN_DIR="${CSD_POOL_BIN_DIR:-$ROOT_DIR/target/release}"

for binary in csd-pool-api csd-pool-daemon csd-pool-bridge; do
  if [[ ! -x "$BIN_DIR/$binary" ]]; then
    printf 'fail: live startup policy binary missing: %s\n' "$BIN_DIR/$binary" >&2
    exit 1
  fi
done

printf 'CSD Pool live startup policy self-test\n'
printf 'bin_dir=%s\n' "$BIN_DIR"

python3 - "$BIN_DIR" <<'PY'
import os
import pathlib
import subprocess
import sys

bin_dir = pathlib.Path(sys.argv[1])
base_env = os.environ.copy()
for key in (
    "CSD_POOL_CONFIG",
    "CSD_POOL_DATABASE_URL",
    "CSD_POOL_REQUIRE_DATABASE",
    "CSD_POOL_SUBMIT_CANDIDATES",
):
    base_env.pop(key, None)

pool_address = "123456789abcdef0123456789abcdef012345678"
cases = [
    (
        "api rejects live mode without PostgreSQL",
        bin_dir / "csd-pool-api",
        {
            "CSD_POOL_TEMPLATE_MODE": "live",
            "CSD_POOL_API_LISTEN": "127.0.0.1:0",
        },
        "persistent PostgreSQL is required in live mode",
    ),
    (
        "daemon rejects live mode without PostgreSQL",
        bin_dir / "csd-pool-daemon",
        {
            "CSD_POOL_TEMPLATE_MODE": "live",
            "CSD_POOL_API_LISTEN": "127.0.0.1:0",
            "CSD_POOL_STRATUM_LISTEN": "127.0.0.1:0",
        },
        "persistent PostgreSQL is required in live mode",
    ),
    (
        "bridge rejects live mode without PostgreSQL",
        bin_dir / "csd-pool-bridge",
        {
            "CSD_POOL_TEMPLATE_MODE": "live",
            "CSD_POOL_SUBMIT_CANDIDATES": "true",
            "CSD_POOL_NODE_URL": "http://127.0.0.1:8790",
            "CSD_POOL_MINING_ADDRESS": pool_address,
            "CSD_POOL_STRATUM_LISTEN": "127.0.0.1:0",
        },
        "persistent PostgreSQL is required in live mode",
    ),
    (
        "bridge rejects live mode with candidate submission disabled",
        bin_dir / "csd-pool-bridge",
        {
            "CSD_POOL_TEMPLATE_MODE": "live",
            "CSD_POOL_DATABASE_URL": "postgres://unused:unused@127.0.0.1:1/unused",
            "CSD_POOL_NODE_URL": "http://127.0.0.1:8790",
            "CSD_POOL_MINING_ADDRESS": pool_address,
            "CSD_POOL_STRATUM_LISTEN": "127.0.0.1:0",
        },
        "candidate block submission is required in live mode",
    ),
]

for label, binary, overrides, expected in cases:
    env = base_env.copy()
    env.update(overrides)
    try:
        result = subprocess.run(
            [str(binary)],
            env=env,
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
    except subprocess.TimeoutExpired as exc:
        print(f"fail: {label}: process did not fail closed before timeout", file=sys.stderr)
        raise SystemExit(1) from exc
    output = result.stdout + result.stderr
    if result.returncode == 0:
        print(f"fail: {label}: process exited successfully", file=sys.stderr)
        raise SystemExit(1)
    if expected not in output:
        print(f"fail: {label}: expected error marker missing", file=sys.stderr)
        print(output, file=sys.stderr)
        raise SystemExit(1)
    print(f"ok: {label}")

print("summary: live startup policy self-test passed")
PY

#!/usr/bin/env python3
"""Fail-closed verifier for local stateless-candidate canary evidence."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import stat
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


SCHEMA = "csd-pool-stateless-candidate-canary-gate/v1"
EXPECTED_COMMIT = "d2884dd7d8dbcdb6322af66afa0f0f833a9ab98c"
LOCKED_FILES = {
    "adapter_patch": (
        "ops/csd-node-adapter/compute-substrate-pool-adapter.patch",
        "265dfdc620453606b5ff6d4222327056c9bcdf713a3f28827f6932c6aae67dc4",
    ),
    "p2p_patch": (
        "ops/csd-node-adapter/compute-substrate-p2p-backoff.patch",
        "51dd08cc9cfc0a4539afa67558c1ae005c165d615223e825e75130352eec2075",
    ),
    "apply_script": (
        "ops/csd-node-adapter/apply-and-build.sh",
        "80370b68b1f68bce8b88c7496ac081b27c0352f4cbf9eda1eaca9278357815f9",
    ),
}
EXPECTED_TESTS = {
    "pool_candidate_tests": 12,
    "pool_header_publish_tests": 6,
    "dial_backoff_tests": 8,
    "bridge_tests": 68,
    "db_tests": 26,
    "node_tests": 15,
}
EXPECTED_PORTS = {"3333": 413, "3334": 2, "3335": 1}
EXPECTED_UNITS = {"daemon", "node_a", "node_b", "signer"}
MAX_CPU_PERCENT = 20.0
MAX_RSS_KB = 1_048_576
MAX_FD = 1_000
MAX_TASKS = 256
MAX_PG_CONNECTIONS = 60
MAX_BASELINE_AGE_SECONDS = 300
MAX_FUTURE_CLOCK_SKEW_SECONDS = 5
SNAPSHOT_TIMESTAMP_FORMAT = "%Y-%m-%dT%H:%M:%S.%fZ"
SNAPSHOT_TIMESTAMP_PATTERN = re.compile(
    r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{6}Z"
)
REQUIRED_REPLAY_FLAGS = {
    "cargo_check_passed",
    "two_official_http_adapters",
    "primary_template_only",
    "secondary_empty_job_cache",
    "divergent_mempools",
    "malformed_rejected",
    "duplicate_idempotent",
    "p2p_first",
    "secondary_timeout",
    "health_drift",
    "secondary_only_accepted_persisted",
}
REQUIRED_ROLLBACK_STEPS = {
    "release_sha_verified",
    "config_sha_verified",
    "stateless_disabled",
    "latch_preserved",
    "daemon_active",
    "nrestarts_zero",
    "production_counts",
    "ports",
    "public_operator_health",
    "ab_converged",
}
REQUIRED_ROLLBACK_CONFIG = {
    "CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_ENABLED": "false",
    "CSD_POOL_STATELESS_PARALLEL_CANDIDATE_SUBMIT_ENABLED": "false",
    "CSD_POOL_PRIMARY_SUBMIT_NODE_NAME": "node-a",
}


class Gate:
    def __init__(self) -> None:
        self.failures: list[str] = []

    def require(self, condition: bool, message: str) -> None:
        if not condition:
            self.failures.append(message)


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def is_sha256(value: Any) -> bool:
    return isinstance(value, str) and re.fullmatch(r"[0-9a-f]{64}", value) is not None


def mapping(value: Any) -> dict[str, Any]:
    return value if isinstance(value, dict) else {}


def integer(value: Any) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def number(value: Any) -> float | None:
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return float(value)
    return None


def verify_locked_file(gate: Gate, repo_root: Path, label: str, evidence: dict[str, Any]) -> None:
    relative, expected = LOCKED_FILES[label]
    path = repo_root / relative
    gate.require(path.is_file() and not path.is_symlink(), f"{label}: locked file missing or symlink")
    if path.is_file() and not path.is_symlink():
        gate.require(sha256_file(path) == expected, f"{label}: repository sha256 mismatch")
    gate.require(evidence.get("path") == relative, f"{label}: evidence path mismatch")
    gate.require(evidence.get("sha256") == expected, f"{label}: evidence sha256 mismatch")


def verify_local_snapshot(gate: Gate, label: str, evidence: dict[str, Any]) -> Path | None:
    raw_path = evidence.get("path")
    expected = evidence.get("sha256")
    gate.require(isinstance(raw_path, str) and os.path.isabs(raw_path), f"{label}: path must be absolute")
    gate.require(is_sha256(expected), f"{label}: sha256 must be lowercase hex")
    if not isinstance(raw_path, str) or not os.path.isabs(raw_path):
        return None
    path = Path(raw_path)
    valid_file = path.is_file() and not path.is_symlink()
    gate.require(valid_file, f"{label}: file missing or symlink")
    if valid_file and is_sha256(expected):
        gate.require(sha256_file(path) == expected, f"{label}: file sha256 mismatch")
    return path if valid_file else None


def parse_env_snapshot(path: Path) -> tuple[dict[str, str], set[str], list[int]]:
    values: dict[str, str] = {}
    duplicates: set[str] = set()
    unsupported_lines: list[int] = []
    text = path.read_text(encoding="utf-8")
    if any(ord(character) < 32 and character not in "\n\r\t" for character in text):
        raise ValueError("contains non-whitespace control characters")
    assignment = re.compile(
        r"(?:export[ \t]+)?([A-Za-z_][A-Za-z0-9_]*)[ \t]*=[ \t]*(.*)"
    )
    for line_number, raw_line in enumerate(text.splitlines(), start=1):
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        match = assignment.fullmatch(line)
        if match is None:
            unsupported_lines.append(line_number)
            continue
        key, value = match.groups()
        value = value.strip()
        if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
            value = value[1:-1]
        if key in values:
            duplicates.add(key)
        else:
            values[key] = value
    return values, duplicates, unsupported_lines


def verify_rollback_config_snapshot(gate: Gate, path: Path) -> None:
    mode = stat.S_IMODE(path.stat().st_mode)
    gate.require(mode == 0o600, f"rollback.config_snapshot: mode must be 0600, got {mode:04o}")
    try:
        values, duplicates, unsupported_lines = parse_env_snapshot(path)
    except (OSError, UnicodeError, ValueError) as exc:
        gate.require(False, f"rollback.config_snapshot: unreadable UTF-8 env snapshot: {exc}")
        return
    gate.require(
        not unsupported_lines,
        "rollback.config_snapshot: unsupported syntax at lines "
        + ",".join(str(line) for line in unsupported_lines),
    )
    for key, expected in REQUIRED_ROLLBACK_CONFIG.items():
        gate.require(key not in duplicates, f"rollback.config_snapshot: duplicate {key}")
        gate.require(
            values.get(key) == expected,
            f"rollback.config_snapshot: {key} must equal {expected}",
        )


def verify_snapshot_freshness(gate: Gate, snapshot: dict[str, Any]) -> None:
    captured_at = snapshot.get("captured_at_utc")
    gate.require(
        snapshot.get("max_age_seconds") == MAX_BASELINE_AGE_SECONDS,
        f"snapshot.max_age_seconds: must be {MAX_BASELINE_AGE_SECONDS}",
    )
    gate.require(
        snapshot.get("future_clock_skew_seconds") == MAX_FUTURE_CLOCK_SKEW_SECONDS,
        f"snapshot.future_clock_skew_seconds: must be {MAX_FUTURE_CLOCK_SKEW_SECONDS}",
    )
    if not isinstance(captured_at, str) or SNAPSHOT_TIMESTAMP_PATTERN.fullmatch(captured_at) is None:
        gate.require(False, "snapshot.captured_at_utc: must be strict UTC with six fractional digits")
        return
    try:
        captured = datetime.strptime(captured_at, SNAPSHOT_TIMESTAMP_FORMAT).replace(
            tzinfo=timezone.utc
        )
    except ValueError:
        gate.require(False, "snapshot.captured_at_utc: invalid calendar timestamp")
        return
    age_seconds = (datetime.now(timezone.utc) - captured).total_seconds()
    gate.require(
        age_seconds >= -MAX_FUTURE_CLOCK_SKEW_SECONDS,
        "snapshot.captured_at_utc: exceeds allowed future clock skew",
    )
    gate.require(
        age_seconds <= MAX_BASELINE_AGE_SECONDS,
        "snapshot.captured_at_utc: production baseline is stale",
    )


def verify_evidence(repo_root: Path, evidence_path: Path, evidence: dict[str, Any]) -> list[str]:
    gate = Gate()
    gate.require(evidence_path.is_file() and not evidence_path.is_symlink(), "evidence: file missing or symlink")
    if evidence_path.is_file() and not evidence_path.is_symlink():
        mode = stat.S_IMODE(evidence_path.stat().st_mode)
        gate.require(mode == 0o600, f"evidence: mode must be 0600, got {mode:04o}")

    gate.require(evidence.get("schema") == SCHEMA, "schema: unsupported or missing")
    gate.require(evidence.get("scope") == "local_evidence_only", "scope: must be local_evidence_only")

    boundary = mapping(evidence.get("execution_boundary"))
    for field in (
        "production_connections",
        "production_writes",
        "production_restarts",
        "production_config_changes",
        "miner_connections",
    ):
        gate.require(boundary.get(field) == 0, f"execution_boundary.{field}: must be 0")

    verify_snapshot_freshness(gate, mapping(evidence.get("snapshot")))

    artifacts = mapping(evidence.get("artifacts"))
    gate.require(artifacts.get("official_commit") == EXPECTED_COMMIT, "artifacts.official_commit: mismatch")
    for label in LOCKED_FILES:
        verify_locked_file(gate, repo_root, label, mapping(artifacts.get(label)))

    tests = mapping(artifacts.get("tests"))
    for name, passed in EXPECTED_TESTS.items():
        result = mapping(tests.get(name))
        gate.require(result.get("passed") == passed, f"artifacts.tests.{name}.passed: expected {passed}")
        gate.require(result.get("failed") == 0, f"artifacts.tests.{name}.failed: must be 0")
    replay = mapping(artifacts.get("replay"))
    for name in REQUIRED_REPLAY_FLAGS:
        gate.require(replay.get(name) is True, f"artifacts.replay.{name}: must be true")

    baseline = mapping(evidence.get("production_baseline"))
    gate.require(baseline.get("candidate_mode") == "A-only", "production_baseline.candidate_mode: must be A-only")
    gate.require(baseline.get("legacy_parallel_enabled") is False, "production_baseline.legacy_parallel_enabled: must be false")
    gate.require(baseline.get("stateless_parallel_enabled") is False, "production_baseline.stateless_parallel_enabled: must be false")
    gate.require(baseline.get("production_sessions") == 413, "production_baseline.production_sessions: expected 413")
    gate.require(baseline.get("production_workers") == 413, "production_baseline.production_workers: expected 413")
    gate.require(mapping(baseline.get("ports")) == EXPECTED_PORTS, "production_baseline.ports: expected frozen 413/2/1")
    gate.require(baseline.get("jn12_active") == 0, "production_baseline.jn12_active: expected frozen incident value 0")
    health = mapping(baseline.get("health"))
    gate.require(health.get("public") is True, "production_baseline.health.public: must be true")
    gate.require(health.get("operator") is True, "production_baseline.health.operator: must be true")

    node_a = mapping(baseline.get("node_a"))
    node_b = mapping(baseline.get("node_b"))
    for label, node in (("node_a", node_a), ("node_b", node_b)):
        gate.require(node.get("healthy") is True, f"production_baseline.{label}.healthy: must be true")
        gate.require((integer(node.get("height")) or 0) > 0, f"production_baseline.{label}.height: must be positive")
        gate.require(isinstance(node.get("tip"), str) and node.get("tip", "").startswith("0x"), f"production_baseline.{label}.tip: invalid")
        gate.require(isinstance(node.get("chainwork"), str) and node.get("chainwork", "").isdigit(), f"production_baseline.{label}.chainwork: invalid")
        peers = integer(node.get("peers"))
        gate.require(peers is not None and peers > 0, f"production_baseline.{label}.peers: must be positive")
    gate.require(node_a.get("height") == node_b.get("height"), "production_baseline: A/B height mismatch")
    gate.require(node_a.get("tip") == node_b.get("tip"), "production_baseline: A/B tip mismatch")
    gate.require(node_a.get("chainwork") == node_b.get("chainwork"), "production_baseline: A/B chainwork mismatch")

    resources = mapping(evidence.get("resources"))
    pg_connections = integer(resources.get("pg_connections"))
    gate.require(pg_connections is not None and 0 <= pg_connections <= MAX_PG_CONNECTIONS, "resources.pg_connections: outside 0..60")
    units = resources.get("units")
    gate.require(isinstance(units, list), "resources.units: must be a list")
    units_by_name: dict[str, dict[str, Any]] = {}
    if isinstance(units, list):
        for item in units:
            unit = mapping(item)
            name = unit.get("name")
            if isinstance(name, str) and name not in units_by_name:
                units_by_name[name] = unit
            else:
                gate.require(False, "resources.units: duplicate or invalid unit name")
    gate.require(set(units_by_name) == EXPECTED_UNITS, "resources.units: expected exactly daemon,node_a,node_b,signer")
    for name in EXPECTED_UNITS:
        unit = units_by_name.get(name, {})
        gate.require(unit.get("active") is True, f"resources.{name}.active: must be true")
        gate.require(unit.get("running") is True, f"resources.{name}.running: must be true")
        gate.require(unit.get("nrestarts") == 0, f"resources.{name}.nrestarts: must be 0")
        cpu = number(unit.get("cpu_percent"))
        rss = integer(unit.get("rss_kb"))
        fd = integer(unit.get("fd"))
        tasks = integer(unit.get("tasks"))
        gate.require(cpu is not None and 0 <= cpu <= MAX_CPU_PERCENT, f"resources.{name}.cpu_percent: outside 0..20")
        gate.require(rss is not None and 0 < rss <= MAX_RSS_KB, f"resources.{name}.rss_kb: outside 1..1048576")
        gate.require(fd is not None and 0 < fd <= MAX_FD, f"resources.{name}.fd: outside 1..1000")
        gate.require(tasks is not None and 0 < tasks <= MAX_TASKS, f"resources.{name}.tasks: outside 1..256")

    canary = mapping(evidence.get("canary"))
    gate.require(canary.get("secondary_budget") == 1, "canary.secondary_budget: must be 1")
    latch_path = canary.get("latch_path")
    latch_ok = isinstance(latch_path, str) and os.path.isabs(latch_path)
    gate.require(latch_ok, "canary.latch_path: must be absolute")
    if latch_ok:
        gate.require(Path(latch_path).parent == Path("/var/lib/csd-pool"), "canary.latch_path: parent must be /var/lib/csd-pool")
    gate.require(canary.get("one_shot") is True, "canary.one_shot: must be true")
    gate.require(canary.get("restart_persistence_test_passed") is True, "canary.restart_persistence_test_passed: must be true")
    gate.require(canary.get("existing_latch_fail_closed") is True, "canary.existing_latch_fail_closed: must be true")
    gate.require(canary.get("latch_error_fail_closed") is True, "canary.latch_error_fail_closed: must be true")
    gate.require(canary.get("unsafe_parent_fail_closed") is True, "canary.unsafe_parent_fail_closed: must be true")
    gate.require(canary.get("broad_latch_mode_fail_closed") is True, "canary.broad_latch_mode_fail_closed: must be true")

    rollback = mapping(evidence.get("rollback"))
    verify_local_snapshot(gate, "rollback.previous_release_artifact", mapping(rollback.get("previous_release_artifact")))
    config_snapshot_path = verify_local_snapshot(
        gate, "rollback.config_snapshot", mapping(rollback.get("config_snapshot"))
    )
    if config_snapshot_path is not None:
        verify_rollback_config_snapshot(gate, config_snapshot_path)
    verify_local_snapshot(gate, "rollback.rollback_script", mapping(rollback.get("rollback_script")))
    rollback_script = mapping(rollback.get("rollback_script"))
    expected_rollback_script = (repo_root / "ops/bin/csd-pool-rollback-release.sh").resolve()
    raw_rollback_path = rollback_script.get("path")
    if isinstance(raw_rollback_path, str) and os.path.isabs(raw_rollback_path):
        gate.require(Path(raw_rollback_path).resolve() == expected_rollback_script, "rollback.rollback_script: must be repository rollback script")
    gate.require(rollback.get("disable_stateless_before_restart") is True, "rollback.disable_stateless_before_restart: must be true")
    gate.require(rollback.get("preserve_latch") is True, "rollback.preserve_latch: must be true")
    rollback_env = mapping(rollback.get("environment"))
    gate.require(rollback_env.get("candidate_mode") == "A-only", "rollback.environment.candidate_mode: must be A-only")
    gate.require(rollback_env.get("legacy_parallel_enabled") is False, "rollback.environment.legacy_parallel_enabled: must be false")
    gate.require(rollback_env.get("stateless_parallel_enabled") is False, "rollback.environment.stateless_parallel_enabled: must be false")
    gate.require(rollback_env.get("primary_submit_node") == "node-a", "rollback.environment.primary_submit_node: must be node-a")
    steps = rollback.get("verification_steps")
    gate.require(isinstance(steps, list) and set(steps) == REQUIRED_ROLLBACK_STEPS, "rollback.verification_steps: incomplete or unexpected")

    return gate.failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("evidence", type=Path)
    parser.add_argument("--repo-root", type=Path, default=Path(__file__).resolve().parents[2])
    args = parser.parse_args()

    evidence_path = args.evidence.absolute()
    try:
        with evidence_path.open("r", encoding="utf-8") as handle:
            evidence = json.load(handle)
    except (OSError, json.JSONDecodeError) as exc:
        print("status=FAIL_CLOSED")
        print(f"failure=evidence unreadable: {exc}")
        return 1
    if not isinstance(evidence, dict):
        print("status=FAIL_CLOSED")
        print("failure=evidence root must be an object")
        return 1

    failures = verify_evidence(args.repo_root.resolve(), evidence_path, evidence)
    if failures:
        print("status=FAIL_CLOSED")
        print(f"failure_count={len(failures)}")
        for failure in failures:
            print(f"failure={failure}")
        return 1
    print("status=PASS_LOCAL_EVIDENCE_GATE")
    print("production_change_authorized=false")
    print("candidate_mode=A-only")
    print("secondary_budget=1")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

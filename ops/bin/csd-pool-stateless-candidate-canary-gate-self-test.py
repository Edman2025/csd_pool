#!/usr/bin/env python3
"""Negative-matrix tests for the local stateless candidate canary gate."""

from __future__ import annotations

import copy
import hashlib
import json
import os
import subprocess
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any


ROOT = Path(__file__).resolve().parents[2]
GATE = ROOT / "ops/bin/csd-pool-stateless-candidate-canary-gate.py"


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


class CanaryGateTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.work = Path(self.temp.name)
        self.release = self.work / "previous-release.tar.gz"
        self.config = self.work / "production.env.snapshot"
        self.release.write_bytes(b"reviewed previous release\n")
        self.config.write_bytes(
            b"CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_ENABLED=false\n"
            b"CSD_POOL_STATELESS_PARALLEL_CANDIDATE_SUBMIT_ENABLED=false\n"
            b"CSD_POOL_PRIMARY_SUBMIT_NODE_NAME=node-a\n"
        )
        os.chmod(self.release, 0o600)
        os.chmod(self.config, 0o600)
        rollback = ROOT / "ops/bin/csd-pool-rollback-release.sh"
        self.evidence: dict[str, Any] = {
            "schema": "csd-pool-stateless-candidate-canary-gate/v1",
            "scope": "local_evidence_only",
            "snapshot": {
                "captured_at_utc": datetime.now(timezone.utc).strftime(
                    "%Y-%m-%dT%H:%M:%S.%fZ"
                ),
                "max_age_seconds": 300,
                "future_clock_skew_seconds": 5,
            },
            "execution_boundary": {
                "production_connections": 0,
                "production_writes": 0,
                "production_restarts": 0,
                "production_config_changes": 0,
                "miner_connections": 0,
            },
            "artifacts": {
                "official_commit": "d2884dd7d8dbcdb6322af66afa0f0f833a9ab98c",
                "adapter_patch": {
                    "path": "ops/csd-node-adapter/compute-substrate-pool-adapter.patch",
                    "sha256": "265dfdc620453606b5ff6d4222327056c9bcdf713a3f28827f6932c6aae67dc4",
                },
                "p2p_patch": {
                    "path": "ops/csd-node-adapter/compute-substrate-p2p-backoff.patch",
                    "sha256": "51dd08cc9cfc0a4539afa67558c1ae005c165d615223e825e75130352eec2075",
                },
                "apply_script": {
                    "path": "ops/csd-node-adapter/apply-and-build.sh",
                    "sha256": "80370b68b1f68bce8b88c7496ac081b27c0352f4cbf9eda1eaca9278357815f9",
                },
                "tests": {
                    "pool_candidate_tests": {"passed": 12, "failed": 0},
                    "pool_header_publish_tests": {"passed": 6, "failed": 0},
                    "dial_backoff_tests": {"passed": 8, "failed": 0},
                    "bridge_tests": {"passed": 68, "failed": 0},
                    "db_tests": {"passed": 26, "failed": 0},
                    "node_tests": {"passed": 15, "failed": 0},
                },
                "replay": {
                    "cargo_check_passed": True,
                    "two_official_http_adapters": True,
                    "primary_template_only": True,
                    "secondary_empty_job_cache": True,
                    "divergent_mempools": True,
                    "malformed_rejected": True,
                    "duplicate_idempotent": True,
                    "p2p_first": True,
                    "secondary_timeout": True,
                    "health_drift": True,
                    "secondary_only_accepted_persisted": True,
                },
            },
            "production_baseline": {
                "candidate_mode": "A-only",
                "legacy_parallel_enabled": False,
                "stateless_parallel_enabled": False,
                "production_sessions": 413,
                "production_workers": 413,
                "ports": {"3333": 413, "3334": 2, "3335": 1},
                "jn12_active": 0,
                "health": {"public": True, "operator": True},
                "node_a": {"healthy": True, "height": 59092, "tip": "0xabc", "chainwork": "160000", "peers": 8},
                "node_b": {"healthy": True, "height": 59092, "tip": "0xabc", "chainwork": "160000", "peers": 8},
            },
            "resources": {
                "pg_connections": 25,
                "units": [
                    {"name": "daemon", "active": True, "running": True, "nrestarts": 0, "cpu_percent": 1.5, "rss_kb": 330000, "fd": 430, "tasks": 9},
                    {"name": "node_a", "active": True, "running": True, "nrestarts": 0, "cpu_percent": 1.5, "rss_kb": 220000, "fd": 33, "tasks": 19},
                    {"name": "node_b", "active": True, "running": True, "nrestarts": 0, "cpu_percent": 1.3, "rss_kb": 235000, "fd": 32, "tasks": 20},
                    {"name": "signer", "active": True, "running": True, "nrestarts": 0, "cpu_percent": 0.0, "rss_kb": 59000, "fd": 19, "tasks": 11},
                ],
            },
            "canary": {
                "secondary_budget": 1,
                "latch_path": "/var/lib/csd-pool/stateless-candidate-canary.latch",
                "one_shot": True,
                "restart_persistence_test_passed": True,
                "existing_latch_fail_closed": True,
                "latch_error_fail_closed": True,
                "unsafe_parent_fail_closed": True,
                "broad_latch_mode_fail_closed": True,
            },
            "rollback": {
                "previous_release_artifact": {"path": str(self.release), "sha256": sha256_file(self.release)},
                "config_snapshot": {"path": str(self.config), "sha256": sha256_file(self.config)},
                "rollback_script": {"path": str(rollback), "sha256": sha256_file(rollback)},
                "disable_stateless_before_restart": True,
                "preserve_latch": True,
                "environment": {
                    "candidate_mode": "A-only",
                    "legacy_parallel_enabled": False,
                    "stateless_parallel_enabled": False,
                    "primary_submit_node": "node-a",
                },
                "verification_steps": [
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
                ],
            },
        }

    def tearDown(self) -> None:
        self.temp.cleanup()

    def run_gate(self, evidence: dict[str, Any]) -> subprocess.CompletedProcess[str]:
        path = self.work / "evidence.json"
        path.write_text(json.dumps(evidence, sort_keys=True), encoding="utf-8")
        os.chmod(path, 0o600)
        return subprocess.run(
            [str(GATE), "--repo-root", str(ROOT), str(path)],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
        )

    def replace_config(self, content: str) -> None:
        self.config.write_text(content, encoding="utf-8")
        os.chmod(self.config, 0o600)
        self.evidence["rollback"]["config_snapshot"]["sha256"] = sha256_file(self.config)

    def assert_rejected(self, mutate, expected: str) -> None:
        evidence = copy.deepcopy(self.evidence)
        mutate(evidence)
        result = self.run_gate(evidence)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(expected, result.stdout)

    def test_valid_evidence_passes(self) -> None:
        result = self.run_gate(self.evidence)
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("status=PASS_LOCAL_EVIDENCE_GATE", result.stdout)
        self.assertIn("production_change_authorized=false", result.stdout)

    def test_world_readable_evidence_fails(self) -> None:
        path = self.work / "world-readable.json"
        path.write_text(json.dumps(self.evidence), encoding="utf-8")
        os.chmod(path, 0o644)
        result = subprocess.run(
            [str(GATE), "--repo-root", str(ROOT), str(path)],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("mode must be 0600", result.stdout)

    def test_symlink_evidence_fails(self) -> None:
        target = self.work / "target.json"
        link = self.work / "evidence-link.json"
        target.write_text(json.dumps(self.evidence), encoding="utf-8")
        os.chmod(target, 0o600)
        link.symlink_to(target)
        result = subprocess.run(
            [str(GATE), "--repo-root", str(ROOT), str(link)],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
        )
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("evidence: file missing or symlink", result.stdout)

    def test_stateless_pre_enabled_fails(self) -> None:
        self.assert_rejected(lambda e: e["production_baseline"].update(stateless_parallel_enabled=True), "stateless_parallel_enabled")

    def test_expired_production_snapshot_fails(self) -> None:
        expired = (datetime.now(timezone.utc) - timedelta(minutes=10)).strftime(
            "%Y-%m-%dT%H:%M:%S.%fZ"
        )
        self.assert_rejected(
            lambda e: e["snapshot"].update(captured_at_utc=expired),
            "production baseline is stale",
        )

    def test_future_production_snapshot_fails(self) -> None:
        future = (datetime.now(timezone.utc) + timedelta(seconds=30)).strftime(
            "%Y-%m-%dT%H:%M:%S.%fZ"
        )
        self.assert_rejected(
            lambda e: e["snapshot"].update(captured_at_utc=future),
            "exceeds allowed future clock skew",
        )

    def test_malformed_production_snapshot_fails(self) -> None:
        self.assert_rejected(
            lambda e: e["snapshot"].update(captured_at_utc="2026-07-22 09:00:00Z"),
            "must be strict UTC with six fractional digits",
        )

    def test_widened_snapshot_age_policy_fails(self) -> None:
        self.assert_rejected(
            lambda e: e["snapshot"].update(max_age_seconds=3600),
            "snapshot.max_age_seconds: must be 300",
        )

    def test_legacy_pre_enabled_fails(self) -> None:
        self.assert_rejected(lambda e: e["production_baseline"].update(legacy_parallel_enabled=True), "legacy_parallel_enabled")

    def test_frozen_port_count_mismatch_fails(self) -> None:
        self.assert_rejected(lambda e: e["production_baseline"]["ports"].update({"3335": 2}), "expected frozen 413/2/1")

    def test_jn12_unexpected_active_fails(self) -> None:
        self.assert_rejected(lambda e: e["production_baseline"].update(jn12_active=1), "jn12_active")

    def test_ab_tip_mismatch_fails(self) -> None:
        self.assert_rejected(lambda e: e["production_baseline"]["node_b"].update(tip="0xdef"), "A/B tip mismatch")

    def test_zero_node_peers_fails(self) -> None:
        self.assert_rejected(
            lambda e: e["production_baseline"]["node_b"].update(peers=0),
            "production_baseline.node_b.peers: must be positive",
        )

    def test_restart_fails(self) -> None:
        self.assert_rejected(lambda e: e["resources"]["units"][0].update(nrestarts=1), "resources.daemon.nrestarts")

    def test_high_cpu_fails(self) -> None:
        self.assert_rejected(lambda e: e["resources"]["units"][1].update(cpu_percent=20.1), "resources.node_a.cpu_percent")

    def test_high_rss_fails(self) -> None:
        self.assert_rejected(lambda e: e["resources"]["units"][2].update(rss_kb=1_048_577), "resources.node_b.rss_kb")

    def test_high_fd_fails(self) -> None:
        self.assert_rejected(lambda e: e["resources"]["units"][0].update(fd=1001), "resources.daemon.fd")

    def test_high_tasks_fails(self) -> None:
        self.assert_rejected(lambda e: e["resources"]["units"][3].update(tasks=257), "resources.signer.tasks")

    def test_high_pg_connections_fails(self) -> None:
        self.assert_rejected(lambda e: e["resources"].update(pg_connections=61), "resources.pg_connections")

    def test_relative_latch_fails(self) -> None:
        self.assert_rejected(lambda e: e["canary"].update(latch_path="state/canary.latch"), "canary.latch_path")

    def test_wrong_latch_parent_fails(self) -> None:
        self.assert_rejected(lambda e: e["canary"].update(latch_path="/tmp/canary.latch"), "parent must be /var/lib/csd-pool")

    def test_budget_above_one_fails(self) -> None:
        self.assert_rejected(lambda e: e["canary"].update(secondary_budget=2), "secondary_budget")

    def test_restart_persistence_gap_fails(self) -> None:
        self.assert_rejected(lambda e: e["canary"].update(restart_persistence_test_passed=False), "restart_persistence_test_passed")

    def test_unsafe_latch_parent_gap_fails(self) -> None:
        self.assert_rejected(lambda e: e["canary"].update(unsafe_parent_fail_closed=False), "unsafe_parent_fail_closed")

    def test_broad_latch_mode_gap_fails(self) -> None:
        self.assert_rejected(lambda e: e["canary"].update(broad_latch_mode_fail_closed=False), "broad_latch_mode_fail_closed")

    def test_replay_gap_fails(self) -> None:
        self.assert_rejected(lambda e: e["artifacts"]["replay"].update(p2p_first=False), "artifacts.replay.p2p_first")

    def test_test_count_regression_fails(self) -> None:
        self.assert_rejected(lambda e: e["artifacts"]["tests"]["pool_candidate_tests"].update(passed=11), "expected 12")

    def test_missing_release_artifact_fails(self) -> None:
        self.assert_rejected(lambda e: e["rollback"]["previous_release_artifact"].update(path=str(self.work / "missing.tgz")), "file missing")

    def test_tampered_config_snapshot_fails(self) -> None:
        evidence = copy.deepcopy(self.evidence)
        self.config.write_bytes(b"tampered\n")
        result = self.run_gate(evidence)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("config_snapshot: file sha256 mismatch", result.stdout)

    def test_rollback_snapshot_stateless_enabled_fails(self) -> None:
        self.replace_config(
            "CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_ENABLED=false\n"
            "CSD_POOL_STATELESS_PARALLEL_CANDIDATE_SUBMIT_ENABLED=true\n"
            "CSD_POOL_PRIMARY_SUBMIT_NODE_NAME=node-a\n"
        )
        result = self.run_gate(self.evidence)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(
            "CSD_POOL_STATELESS_PARALLEL_CANDIDATE_SUBMIT_ENABLED must equal false",
            result.stdout,
        )

    def test_rollback_snapshot_legacy_parallel_enabled_fails(self) -> None:
        self.replace_config(
            "CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_ENABLED=true\n"
            "CSD_POOL_STATELESS_PARALLEL_CANDIDATE_SUBMIT_ENABLED=false\n"
            "CSD_POOL_PRIMARY_SUBMIT_NODE_NAME=node-a\n"
        )
        result = self.run_gate(self.evidence)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(
            "CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_ENABLED must equal false",
            result.stdout,
        )

    def test_rollback_snapshot_primary_label_mismatch_fails(self) -> None:
        self.replace_config(
            "CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_ENABLED=false\n"
            "CSD_POOL_STATELESS_PARALLEL_CANDIDATE_SUBMIT_ENABLED=false\n"
            "CSD_POOL_PRIMARY_SUBMIT_NODE_NAME=node-b\n"
        )
        result = self.run_gate(self.evidence)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("CSD_POOL_PRIMARY_SUBMIT_NODE_NAME must equal node-a", result.stdout)

    def test_rollback_snapshot_duplicate_control_fails(self) -> None:
        self.replace_config(
            "CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_ENABLED=false\n"
            "CSD_POOL_STATELESS_PARALLEL_CANDIDATE_SUBMIT_ENABLED=false\n"
            "CSD_POOL_STATELESS_PARALLEL_CANDIDATE_SUBMIT_ENABLED=true\n"
            "CSD_POOL_PRIMARY_SUBMIT_NODE_NAME=node-a\n"
        )
        result = self.run_gate(self.evidence)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(
            "duplicate CSD_POOL_STATELESS_PARALLEL_CANDIDATE_SUBMIT_ENABLED",
            result.stdout,
        )

    def test_rollback_snapshot_world_readable_fails(self) -> None:
        os.chmod(self.config, 0o644)
        result = self.run_gate(self.evidence)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("rollback.config_snapshot: mode must be 0600", result.stdout)

    def test_rollback_snapshot_command_line_fails(self) -> None:
        self.replace_config(
            "CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_ENABLED=false\n"
            "CSD_POOL_STATELESS_PARALLEL_CANDIDATE_SUBMIT_ENABLED=false\n"
            "CSD_POOL_PRIMARY_SUBMIT_NODE_NAME=node-a\n"
            "source /tmp/unsafe-override.env\n"
        )
        result = self.run_gate(self.evidence)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("unsupported syntax at lines 4", result.stdout)

    def test_rollback_snapshot_control_character_fails(self) -> None:
        self.replace_config(
            "CSD_POOL_PARALLEL_CANDIDATE_SUBMIT_ENABLED=false\n"
            "CSD_POOL_STATELESS_PARALLEL_CANDIDATE_SUBMIT_ENABLED=false\n"
            "CSD_POOL_PRIMARY_SUBMIT_NODE_NAME=node-a\n"
            "UNRELATED=value\x00override\n"
        )
        result = self.run_gate(self.evidence)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn("contains non-whitespace control characters", result.stdout)

    def test_rollback_does_not_disable_stateless_fails(self) -> None:
        self.assert_rejected(lambda e: e["rollback"]["environment"].update(stateless_parallel_enabled=True), "rollback.environment.stateless_parallel_enabled")

    def test_rollback_latch_not_preserved_fails(self) -> None:
        self.assert_rejected(lambda e: e["rollback"].update(preserve_latch=False), "rollback.preserve_latch")

    def test_incomplete_rollback_verification_fails(self) -> None:
        self.assert_rejected(lambda e: e["rollback"]["verification_steps"].remove("ab_converged"), "verification_steps")

    def test_nonzero_production_connection_fails(self) -> None:
        self.assert_rejected(lambda e: e["execution_boundary"].update(production_connections=1), "production_connections")


if __name__ == "__main__":
    unittest.main(verbosity=2)

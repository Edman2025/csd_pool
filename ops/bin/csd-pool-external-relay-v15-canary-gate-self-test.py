#!/usr/bin/env python3
"""Self-tests for the local-only V15 deployment guard."""

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
GATE = ROOT / "ops/bin/csd-pool-external-relay-v15-canary-gate.py"
SPEC = importlib.util.spec_from_file_location("v15_gate", GATE)
assert SPEC and SPEC.loader
gate = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = gate
SPEC.loader.exec_module(gate)


def relay(slot: str, marker: str, region: str) -> dict[str, object]:
    return {
        "slot": slot,
        "host_fingerprint": marker * 16,
        "region": region,
        "network_provider": f"provider-{marker}",
        "network_asn_class": f"asn-{marker}",
        "operator_account": f"account-{marker}",
        "fault_domain": f"fault-{marker}",
        "public_key": marker * 64,
        "endpoint_secret_ref_present": True,
        "full_node_validation": True,
        "dedicated_ack_key": True,
        "min_vcpu": 4,
        "memory_gib": 8,
        "storage_gib": 250,
        "network_mbps": 100,
    }


def manifest() -> dict[str, object]:
    return {
        "protocol_version": 15,
        "delivery_mode": "ANNOUNCE_THEN_PULL",
        "adapter_capabilities": {
            "push_full_block": False,
            "pull_get_block": True,
            "delayed_response_channel": True,
            "application_accept_hook": True,
        },
        "relays": [relay("D", "a", "region-a"), relay("E", "b", "region-b")],
        "not_a_b_c_miner_wallet_hosts": True,
    }


def lane(**changes: int) -> dict[str, int]:
    value = {
        "scheduled": 1,
        "ack_verified": 1,
        "negative": 0,
        "timeout": 0,
        "transport": 0,
        "drop": 0,
        "abnormal": 0,
        "outstanding": 0,
    }
    value.update(changes)
    return value


class V15GateTests(unittest.TestCase):
    def test_01_default_is_zero_and_blocked(self) -> None:
        result = gate.default_zero()
        self.assertEqual(result["status"], "BLOCKED_EXTERNAL_INPUTS")
        for key in (
            "production_connections",
            "ssh_connections",
            "db_connections",
            "http_connections",
            "nodec_connections",
            "miner_connections",
            "writes",
            "uploads",
            "reloads",
            "restarts",
            "config_changes",
        ):
            self.assertEqual(result[key], 0)
        self.assertFalse(result["feature_enabled"])
        self.assertFalse(result["deployment_authorized"])

    def test_02_manifest_accepts_two_independent_relays(self) -> None:
        result = gate.validate_manifest(manifest())
        self.assertEqual(result["relay_count"], 2)
        self.assertFalse(result["deployment_authorized"])

    def test_03_current_official_interface_is_blocked(self) -> None:
        value = manifest()
        value["adapter_capabilities"]["delayed_response_channel"] = False
        with self.assertRaisesRegex(gate.GateError, "blocked_official_adapter_interface"):
            gate.validate_manifest(value)

    def test_04_push_cannot_be_claimed_without_proof(self) -> None:
        value = manifest()
        value["delivery_mode"] = "PUSH"
        with self.assertRaisesRegex(gate.GateError, "delivery_mode_not_proven"):
            gate.validate_manifest(value)

    def test_05_requires_exact_d_and_e(self) -> None:
        value = manifest()
        value["relays"][1]["slot"] = "D"
        with self.assertRaises(gate.GateError):
            gate.validate_manifest(value)

    def test_06_rejects_shared_region_provider_asn_account_fault_host_or_key(self) -> None:
        for field in (
            "host_fingerprint",
            "region",
            "network_provider",
            "network_asn_class",
            "operator_account",
            "fault_domain",
            "public_key",
        ):
            value = manifest()
            value["relays"][1][field] = value["relays"][0][field]
            with self.assertRaisesRegex(gate.GateError, f"relay_independence_{field}"):
                gate.validate_manifest(value)

    def test_07_rejects_missing_full_node_dedicated_key_or_endpoint_ref(self) -> None:
        for field in (
            "endpoint_secret_ref_present",
            "full_node_validation",
            "dedicated_ack_key",
        ):
            value = manifest()
            value["relays"][0][field] = False
            with self.assertRaises(gate.GateError):
                gate.validate_manifest(value)

    def test_08_rejects_underprovisioned_relay(self) -> None:
        for field, value_under in (
            ("min_vcpu", 3),
            ("memory_gib", 7),
            ("storage_gib", 249),
            ("network_mbps", 99),
        ):
            value = manifest()
            value["relays"][0][field] = value_under
            with self.assertRaises(gate.GateError):
                gate.validate_manifest(value)

    def test_09_rejects_non_integer_or_boolean_resource_values(self) -> None:
        for field in ("min_vcpu", "memory_gib", "storage_gib", "network_mbps"):
            for invalid in ("250", True, False, 8.0, None):
                value = manifest()
                value["relays"][0][field] = invalid
                with self.assertRaisesRegex(gate.GateError, f"invalid_{field}"):
                    gate.validate_manifest(value)

    def test_10_four_lane_success_is_pending_below_ten(self) -> None:
        sample = {"lanes": [lane() for _ in range(4)], "mature_candidates": 9}
        self.assertEqual(
            gate.classify_canary(sample),
            "PENDING_NOT_ENOUGH_MATURE_V15_CANDIDATES",
        )

    def test_11_ten_mature_is_bounded_not_effect_pass(self) -> None:
        sample = {"lanes": [lane() for _ in range(4)], "mature_candidates": 10}
        self.assertEqual(
            gate.classify_canary(sample),
            "BOUNDED_V15_OBSERVATION_NO_CAUSAL_IMPROVEMENT_CLAIM",
        )

    def test_12_each_terminal_error_stops_only_v15(self) -> None:
        for terminal in ("negative", "timeout", "transport", "drop", "abnormal"):
            broken = lane(ack_verified=0, **{terminal: 1})
            sample = {"lanes": [broken, lane(), lane(), lane()], "mature_candidates": 10}
            self.assertEqual(
                gate.classify_canary(sample),
                "STOP_AND_ROLL_BACK_V15_FLAGS_KEEP_V13",
            )

    def test_13_nonconservation_fails_closed(self) -> None:
        sample = {"lanes": [lane(scheduled=2), lane(), lane(), lane()], "mature_candidates": 10}
        with self.assertRaisesRegex(gate.GateError, "lane_nonconservation"):
            gate.classify_canary(sample)

    def test_14_health_redlines_stop_v15(self) -> None:
        for field in (
            "consecutive_two_orphans",
            "persistent_ab_lag",
            "submit_redline",
            "hashrate_redline",
            "generation_redline",
        ):
            sample = {"lanes": [lane() for _ in range(4)], "mature_candidates": 10, field: True}
            self.assertEqual(
                gate.classify_canary(sample),
                "STOP_AND_ROLL_BACK_V15_FLAGS_KEEP_V13",
            )

    def test_15_cli_default_exact(self) -> None:
        completed = subprocess.run(
            ["python3", str(GATE)],
            check=True,
            capture_output=True,
            text=True,
        )
        lines = completed.stdout.splitlines()
        self.assertEqual(lines[0], "PASS_LOCAL_ONLY_DEFAULT_ZERO")
        self.assertEqual(json.loads(lines[1])["production_connections"], 0)

    def test_16_source_has_no_network_or_process_launcher(self) -> None:
        source = GATE.read_text()
        for forbidden in (
            "import socket",
            "import requests",
            "import urllib",
            "import subprocess",
            "paramiko",
            "sshpass",
            "Popen(",
        ):
            self.assertNotIn(forbidden, source)

    def test_17_cli_manifest_is_local_only(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "manifest.json"
            path.write_text(json.dumps(manifest()))
            completed = subprocess.run(
                ["python3", str(GATE), "--manifest", str(path)],
                check=True,
                capture_output=True,
                text=True,
            )
            self.assertFalse(json.loads(completed.stdout)["manifest"]["deployment_authorized"])


if __name__ == "__main__":
    unittest.main(verbosity=2)

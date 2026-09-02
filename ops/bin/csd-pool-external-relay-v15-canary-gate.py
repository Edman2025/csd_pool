#!/usr/bin/env python3
"""Local-only V15 external-relay application-ACK deployment guard."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any


PROTOCOL_VERSION = 15
V13_TARGET_SHA256 = "d1ff89b5812fd4cd2bdf995aecb1e2a0ba838b7c2de50536155b13beaab36bba"
DAEMON_SHA256 = "a4f147d48d48689d22936b79dfec31d095ad63a7f3d2a4e9b66cd74f4469cdf1"
MIN_MATURE_CANDIDATES = 10
HOST_FINGERPRINT = re.compile(r"^[0-9a-f]{16}$")
PUBLIC_KEY = re.compile(r"^[0-9a-f]{64}$")
SAFE_LABEL = re.compile(r"^[A-Za-z0-9_.-]{1,64}$")


class GateError(ValueError):
    pass


@dataclass(frozen=True)
class Relay:
    slot: str
    host_fingerprint: str
    region: str
    network_provider: str
    network_asn_class: str
    operator_account: str
    fault_domain: str
    public_key: str
    endpoint_secret_ref_present: bool
    full_node_validation: bool
    dedicated_ack_key: bool
    min_vcpu: int
    memory_gib: int
    storage_gib: int
    network_mbps: int


def _safe_label(value: Any, field: str) -> str:
    if not isinstance(value, str) or not SAFE_LABEL.fullmatch(value):
        raise GateError(f"invalid_{field}")
    return value


def _strict_int(value: Any, field: str) -> int:
    if not isinstance(value, int) or isinstance(value, bool):
        raise GateError(f"invalid_{field}")
    return value


def parse_relay(raw: Any) -> Relay:
    if not isinstance(raw, dict):
        raise GateError("relay_not_object")
    slot = raw.get("slot")
    if slot not in {"D", "E"}:
        raise GateError("relay_slot")
    fingerprint = raw.get("host_fingerprint")
    if not isinstance(fingerprint, str) or not HOST_FINGERPRINT.fullmatch(fingerprint):
        raise GateError("relay_host_fingerprint")
    public_key = raw.get("public_key")
    if not isinstance(public_key, str) or not PUBLIC_KEY.fullmatch(public_key):
        raise GateError("relay_public_key")
    return Relay(
        slot=slot,
        host_fingerprint=fingerprint,
        region=_safe_label(raw.get("region"), "region"),
        network_provider=_safe_label(raw.get("network_provider"), "network_provider"),
        network_asn_class=_safe_label(raw.get("network_asn_class"), "network_asn_class"),
        operator_account=_safe_label(raw.get("operator_account"), "operator_account"),
        fault_domain=_safe_label(raw.get("fault_domain"), "fault_domain"),
        public_key=public_key,
        endpoint_secret_ref_present=raw.get("endpoint_secret_ref_present") is True,
        full_node_validation=raw.get("full_node_validation") is True,
        dedicated_ack_key=raw.get("dedicated_ack_key") is True,
        min_vcpu=_strict_int(raw.get("min_vcpu"), "min_vcpu"),
        memory_gib=_strict_int(raw.get("memory_gib"), "memory_gib"),
        storage_gib=_strict_int(raw.get("storage_gib"), "storage_gib"),
        network_mbps=_strict_int(raw.get("network_mbps"), "network_mbps"),
    )


def validate_manifest(data: Any) -> dict[str, Any]:
    if not isinstance(data, dict):
        raise GateError("manifest_not_object")
    if data.get("protocol_version") != PROTOCOL_VERSION:
        raise GateError("protocol_version")
    if data.get("delivery_mode") != "ANNOUNCE_THEN_PULL":
        raise GateError("delivery_mode_not_proven")
    capabilities = data.get("adapter_capabilities")
    if not isinstance(capabilities, dict):
        raise GateError("adapter_capabilities")
    if capabilities.get("push_full_block") is not False:
        raise GateError("push_capability_unproven")
    required = (
        "pull_get_block",
        "delayed_response_channel",
        "application_accept_hook",
    )
    if any(capabilities.get(name) is not True for name in required):
        raise GateError("blocked_official_adapter_interface")
    relays_raw = data.get("relays")
    if not isinstance(relays_raw, list) or len(relays_raw) != 2:
        raise GateError("relay_count")
    relays = sorted((parse_relay(item) for item in relays_raw), key=lambda item: item.slot)
    if [relay.slot for relay in relays] != ["D", "E"]:
        raise GateError("relay_slots")
    uniqueness = (
        "host_fingerprint",
        "region",
        "network_provider",
        "network_asn_class",
        "operator_account",
        "fault_domain",
        "public_key",
    )
    for field in uniqueness:
        if getattr(relays[0], field) == getattr(relays[1], field):
            raise GateError(f"relay_independence_{field}")
    for relay in relays:
        if not relay.endpoint_secret_ref_present:
            raise GateError("relay_endpoint_secret_ref")
        if not relay.full_node_validation:
            raise GateError("relay_not_full_node_validator")
        if not relay.dedicated_ack_key:
            raise GateError("relay_ack_key_not_dedicated")
        if relay.min_vcpu < 4 or relay.memory_gib < 8:
            raise GateError("relay_compute_below_minimum")
        if relay.storage_gib < 250 or relay.network_mbps < 100:
            raise GateError("relay_io_below_minimum")
    if data.get("not_a_b_c_miner_wallet_hosts") is not True:
        raise GateError("relay_role_independence")
    return {
        "status": "READY_FOR_SEPARATE_INDEPENDENT_REVIEW_NOT_AUTHORIZED",
        "protocol_version": PROTOCOL_VERSION,
        "delivery_mode": "ANNOUNCE_THEN_PULL",
        "relay_count": 2,
        "independence_fields": len(uniqueness),
        "remote_application_ack_semantics": "SIGNED_RELAY_FULL_NODE_APPLICATION_ACCEPT",
        "deployment_authorized": False,
    }


def default_zero() -> dict[str, Any]:
    return {
        "status": "BLOCKED_EXTERNAL_INPUTS",
        "reason": "two_independent_relays_and_official_adapter_ack_hooks_not_sealed",
        "protocol_version": PROTOCOL_VERSION,
        "delivery_mode": "ANNOUNCE_THEN_PULL",
        "current_v13_target_sha256": V13_TARGET_SHA256,
        "current_daemon_sha256": DAEMON_SHA256,
        "feature_enabled": False,
        "deployment_authorized": False,
        "production_connections": 0,
        "ssh_connections": 0,
        "db_connections": 0,
        "http_connections": 0,
        "nodec_connections": 0,
        "miner_connections": 0,
        "writes": 0,
        "uploads": 0,
        "reloads": 0,
        "restarts": 0,
        "config_changes": 0,
        "candidate_submit_changes": 0,
        "peer_policy_changes": 0,
        "remote_application_receipt_observed": False,
        "delivery_effect_pass": False,
        "orphan_effect_pass": False,
    }


def validate_lane(lane: Any) -> None:
    if not isinstance(lane, dict):
        raise GateError("lane_not_object")
    names = (
        "scheduled",
        "ack_verified",
        "negative",
        "timeout",
        "transport",
        "drop",
        "abnormal",
        "outstanding",
    )
    values = {}
    for name in names:
        value = lane.get(name)
        if not isinstance(value, int) or isinstance(value, bool) or value < 0:
            raise GateError(f"lane_{name}")
        values[name] = value
    terminal = sum(values[name] for name in names if name != "scheduled")
    if values["scheduled"] != terminal:
        raise GateError("lane_nonconservation")


def classify_canary(sample: Any) -> str:
    if not isinstance(sample, dict):
        raise GateError("sample_not_object")
    lanes = sample.get("lanes")
    if not isinstance(lanes, list) or len(lanes) != 4:
        raise GateError("two_by_two_lanes")
    for lane in lanes:
        validate_lane(lane)
    redlines = (
        "consecutive_two_orphans",
        "persistent_ab_lag",
        "submit_redline",
        "hashrate_redline",
        "generation_redline",
    )
    any_terminal_error = any(
        sum(int(lane.get(name, 0)) for name in ("negative", "timeout", "transport", "drop", "abnormal"))
        > 0
        for lane in lanes
    )
    if any_terminal_error or any(sample.get(name) is True for name in redlines):
        return "STOP_AND_ROLL_BACK_V15_FLAGS_KEEP_V13"
    mature = sample.get("mature_candidates")
    if not isinstance(mature, int) or isinstance(mature, bool) or mature < 0:
        raise GateError("mature_candidates")
    if mature < MIN_MATURE_CANDIDATES:
        return "PENDING_NOT_ENOUGH_MATURE_V15_CANDIDATES"
    return "BOUNDED_V15_OBSERVATION_NO_CAUSAL_IMPROVEMENT_CLAIM"


def artifact(path: Path) -> dict[str, Any]:
    body = path.read_bytes()
    return {
        "mode": f"{path.stat().st_mode & 0o777:04o}",
        "bytes": len(body),
        "sha256": hashlib.sha256(body).hexdigest(),
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path)
    parser.add_argument("--sample", type=Path)
    parser.add_argument("--artifact", type=Path)
    args = parser.parse_args()
    if args.manifest is None and args.sample is None and args.artifact is None:
        print("PASS_LOCAL_ONLY_DEFAULT_ZERO")
        print(json.dumps(default_zero(), sort_keys=True))
        return 0
    output: dict[str, Any] = {}
    if args.manifest is not None:
        output["manifest"] = validate_manifest(json.loads(args.manifest.read_text()))
    if args.sample is not None:
        output["canary"] = classify_canary(json.loads(args.sample.read_text()))
    if args.artifact is not None:
        output["artifact"] = artifact(args.artifact)
    print(json.dumps(output, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

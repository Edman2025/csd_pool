#!/usr/bin/env python3
"""Local-only tests for the temporary Node A dependency decoupling design."""

from __future__ import annotations

import configparser
import copy
import os
import stat
import tempfile
import unittest
from dataclasses import dataclass, field
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
NODE_A = "csd-pool-migrated-node.service"
DAEMON = "csd-pool-migrated-daemon.service"
SIGNER = "csd-pool-migrated-signer.service"
NODE_B = "csd-pool-migrated-node-b.service"

DAEMON_BASE = ROOT / "ops/systemd/csd-pool-migrated-daemon.service"
DAEMON_DROPIN = (
    ROOT / "ops/systemd/nodea-switch-decoupling/daemon-temporary.conf"
)
SIGNER_DROPIN = (
    ROOT / "ops/systemd/nodea-switch-decoupling/signer-temporary.conf"
)

# The signer fixture contains only non-sensitive directives captured by the
# current-systemd dependency audit. ExecStart and environment values are
# intentionally absent.
SIGNER_BASE_TEXT = """\
[Unit]
After=network-online.target csd-pool-migrated-node.service
Wants=network-online.target
Requires=csd-pool-migrated-node.service

[Service]
Restart=on-failure
RestartSec=5
"""

LIST_KEYS = ("After", "Wants", "Requires", "BindsTo")
A_UNAVAILABLE_PER_RESTART_SECONDS = 30
A_UNAVAILABLE_CUMULATIVE_SECONDS = 60
TARGET_CONVERGENCE_SECONDS = 180
DECOUPLED_WINDOW_SECONDS = 300


def unit_directives(text: str) -> list[tuple[str, str]]:
    section = ""
    result: list[tuple[str, str]] = []
    for raw_line in text.splitlines():
        line = raw_line.strip()
        if not line or line.startswith(("#", ";")):
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1]
            continue
        if section != "Unit" or "=" not in line:
            continue
        key, value = line.split("=", 1)
        result.append((key.strip(), value.strip()))
    return result


def effective_dependencies(fragments: list[str]) -> dict[str, set[str]]:
    values = {key: set() for key in LIST_KEYS}
    for fragment in fragments:
        for key, value in unit_directives(fragment):
            if key not in values:
                continue
            if value == "":
                values[key].clear()
            else:
                values[key].update(value.split())
    return values


def validate_dropin_shape(path: Path, reset_key: str) -> None:
    assert path.is_file() and not path.is_symlink(), f"{path}: regular file required"
    assert stat.S_IMODE(path.stat().st_mode) == 0o600, f"{path}: mode must be 0600"
    parser = configparser.ConfigParser(
        interpolation=None,
        strict=False,
        empty_lines_in_values=False,
    )
    parser.optionxform = str
    parser.read_string(path.read_text(encoding="ascii"))
    assert parser.sections() == ["Unit"], f"{path}: only [Unit] is allowed"
    assert set(parser["Unit"]) == {
        reset_key,
        "Wants",
        "After",
    }, f"{path}: unexpected directive"
    assert parser["Unit"][reset_key] == "", f"{path}: {reset_key}= must reset"
    assert parser["Unit"]["Wants"] == NODE_A
    assert parser["Unit"]["After"] == NODE_A
    forbidden = ("ExecStart", "Environment", "EnvironmentFile")
    text = path.read_text(encoding="ascii")
    assert all(item not in text for item in forbidden)


@dataclass
class UnitGeneration:
    generation: int = 1
    active: bool = True
    nrestarts: int = 0
    sha: str = "baseline"


@dataclass
class FakeSystemd:
    base: dict[str, str]
    installed_dropins: dict[str, str] = field(default_factory=dict)
    loaded_dropins: dict[str, str] = field(default_factory=dict)
    units: dict[str, UnitGeneration] = field(
        default_factory=lambda: {
            NODE_A: UnitGeneration(),
            DAEMON: UnitGeneration(),
            SIGNER: UnitGeneration(),
            NODE_B: UnitGeneration(),
        }
    )

    def clone_generations(self) -> dict[str, UnitGeneration]:
        return copy.deepcopy(self.units)

    def install_dropin(self, unit: str, text: str) -> None:
        self.installed_dropins[unit] = text

    def remove_dropin(self, unit: str) -> None:
        self.installed_dropins.pop(unit, None)

    def daemon_reload(self) -> None:
        # systemd reloads manager configuration but does not restart running
        # services. This invariant is explicitly checked by every path test.
        before = self.clone_generations()
        self.loaded_dropins = dict(self.installed_dropins)
        assert self.units == before

    def graph(self, unit: str) -> dict[str, set[str]]:
        fragments = [self.base[unit]]
        if unit in self.loaded_dropins:
            fragments.append(self.loaded_dropins[unit])
        return effective_dependencies(fragments)

    def node_reverse_edges(self) -> tuple[set[str], set[str]]:
        bound_by = {
            unit for unit in (DAEMON, SIGNER) if NODE_A in self.graph(unit)["BindsTo"]
        }
        required_by = {
            unit for unit in (DAEMON, SIGNER) if NODE_A in self.graph(unit)["Requires"]
        }
        return bound_by, required_by

    def restart_node_a(self, sha: str, successful: bool = True) -> None:
        strong_dependents = {
            unit
            for unit in (DAEMON, SIGNER)
            if NODE_A in self.graph(unit)["BindsTo"]
            or NODE_A in self.graph(unit)["Requires"]
        }
        for unit in strong_dependents:
            self.units[unit].generation += 1
        node = self.units[NODE_A]
        node.generation += 1
        node.sha = sha
        node.active = successful

    def ensure_node_a_active(self, sha: str = "old") -> None:
        if not self.units[NODE_A].active or self.units[NODE_A].sha != sha:
            self.restart_node_a(sha=sha, successful=True)


class DependencyDecouplingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.daemon_base = DAEMON_BASE.read_text(encoding="ascii")
        cls.daemon_dropin = DAEMON_DROPIN.read_text(encoding="ascii")
        cls.signer_dropin = SIGNER_DROPIN.read_text(encoding="ascii")

    def new_systemd(self) -> FakeSystemd:
        return FakeSystemd(
            base={
                DAEMON: self.daemon_base,
                SIGNER: SIGNER_BASE_TEXT,
            }
        )

    def install_both_and_reload(self, manager: FakeSystemd) -> None:
        manager.install_dropin(DAEMON, self.daemon_dropin)
        manager.install_dropin(SIGNER, self.signer_dropin)
        before = manager.clone_generations()
        manager.daemon_reload()
        self.assertEqual(manager.units, before)

    def restore_graph(self, manager: FakeSystemd) -> None:
        manager.remove_dropin(DAEMON)
        manager.remove_dropin(SIGNER)
        before = manager.clone_generations()
        manager.daemon_reload()
        self.assertEqual(manager.units, before)

    def assert_decoupled(self, manager: FakeSystemd) -> None:
        daemon = manager.graph(DAEMON)
        signer = manager.graph(SIGNER)
        self.assertNotIn(NODE_A, daemon["BindsTo"])
        self.assertNotIn(NODE_A, signer["Requires"])
        self.assertIn(NODE_A, daemon["Wants"])
        self.assertIn(NODE_A, signer["Wants"])
        self.assertIn(NODE_A, daemon["After"])
        self.assertIn(NODE_A, signer["After"])
        bound_by, required_by = manager.node_reverse_edges()
        self.assertNotIn(DAEMON, bound_by)
        self.assertNotIn(SIGNER, required_by)

    def assert_original_graph(self, manager: FakeSystemd) -> None:
        daemon = manager.graph(DAEMON)
        signer = manager.graph(SIGNER)
        self.assertIn(NODE_A, daemon["BindsTo"])
        self.assertIn(NODE_A, signer["Requires"])
        bound_by, required_by = manager.node_reverse_edges()
        self.assertIn(DAEMON, bound_by)
        self.assertIn(SIGNER, required_by)

    def test_dropin_files_are_narrow_and_mode_0600(self) -> None:
        validate_dropin_shape(DAEMON_DROPIN, "BindsTo")
        validate_dropin_shape(SIGNER_DROPIN, "Requires")

    def test_list_reset_semantics_remove_only_strong_node_a_edges(self) -> None:
        manager = self.new_systemd()
        self.assert_original_graph(manager)
        self.install_both_and_reload(manager)
        self.assert_decoupled(manager)

    def test_daemon_reload_does_not_change_any_generation(self) -> None:
        manager = self.new_systemd()
        before = manager.clone_generations()
        self.install_both_and_reload(manager)
        self.assertEqual(manager.units, before)
        self.restore_graph(manager)
        self.assertEqual(manager.units, before)

    def test_success_path_restarts_only_node_a_and_restores_graph(self) -> None:
        manager = self.new_systemd()
        self.install_both_and_reload(manager)
        self.assert_decoupled(manager)
        dependent_before = {
            DAEMON: copy.deepcopy(manager.units[DAEMON]),
            SIGNER: copy.deepcopy(manager.units[SIGNER]),
            NODE_B: copy.deepcopy(manager.units[NODE_B]),
        }
        manager.restart_node_a(sha="target", successful=True)
        self.assertTrue(manager.units[NODE_A].active)
        self.assertEqual(manager.units[NODE_A].sha, "target")
        for unit, snapshot in dependent_before.items():
            self.assertEqual(manager.units[unit], snapshot)
        self.restore_graph(manager)
        self.assert_original_graph(manager)
        for unit, snapshot in dependent_before.items():
            self.assertEqual(manager.units[unit], snapshot)

    def test_target_failure_rolls_back_while_decoupled_then_restores(self) -> None:
        manager = self.new_systemd()
        self.install_both_and_reload(manager)
        dependent_before = {
            DAEMON: copy.deepcopy(manager.units[DAEMON]),
            SIGNER: copy.deepcopy(manager.units[SIGNER]),
            NODE_B: copy.deepcopy(manager.units[NODE_B]),
        }
        manager.restart_node_a(sha="target", successful=False)
        self.assertFalse(manager.units[NODE_A].active)
        manager.ensure_node_a_active(sha="old")
        self.assertTrue(manager.units[NODE_A].active)
        self.assertEqual(manager.units[NODE_A].sha, "old")
        for unit, snapshot in dependent_before.items():
            self.assertEqual(manager.units[unit], snapshot)
        self.restore_graph(manager)
        self.assert_original_graph(manager)

    def test_install_interruption_before_reload_leaves_effective_graph_unchanged(self) -> None:
        manager = self.new_systemd()
        before = manager.clone_generations()
        manager.install_dropin(DAEMON, self.daemon_dropin)
        # The second atomic rename failed. No daemon-reload was issued, so the
        # manager still has the original graph. Cleanup is file-only.
        self.assert_original_graph(manager)
        manager.remove_dropin(DAEMON)
        self.assertEqual(manager.units, before)

    def test_interruption_after_reload_restores_graph_without_restart(self) -> None:
        manager = self.new_systemd()
        before = manager.clone_generations()
        self.install_both_and_reload(manager)
        self.assert_decoupled(manager)
        # A failed before it was touched. Recovery removes both temporary
        # drop-ins and reloads; all processes remain in their original generation.
        self.restore_graph(manager)
        self.assert_original_graph(manager)
        self.assertEqual(manager.units, before)

    def test_restore_interruption_is_resumable_with_node_a_active(self) -> None:
        manager = self.new_systemd()
        self.install_both_and_reload(manager)
        manager.restart_node_a(sha="target", successful=True)
        dependent_before = {
            DAEMON: copy.deepcopy(manager.units[DAEMON]),
            SIGNER: copy.deepcopy(manager.units[SIGNER]),
        }
        manager.remove_dropin(DAEMON)
        manager.daemon_reload()
        self.assertTrue(manager.units[NODE_A].active)
        self.assertIn(NODE_A, manager.graph(DAEMON)["BindsTo"])
        self.assertNotIn(NODE_A, manager.graph(SIGNER)["Requires"])
        manager.remove_dropin(SIGNER)
        manager.daemon_reload()
        self.assert_original_graph(manager)
        for unit, snapshot in dependent_before.items():
            self.assertEqual(manager.units[unit], snapshot)

    def test_pid_drift_before_node_restart_restores_graph_without_touching_a(self) -> None:
        manager = self.new_systemd()
        before = manager.clone_generations()
        self.install_both_and_reload(manager)
        manager.units[SIGNER].generation += 1
        # The executor must detect this redline before restart A. It restores
        # the graph, but must not attempt to hide or undo the signer drift.
        node_a_before = copy.deepcopy(manager.units[NODE_A])
        manager.remove_dropin(DAEMON)
        manager.remove_dropin(SIGNER)
        manager.daemon_reload()
        self.assertEqual(manager.units[NODE_A], node_a_before)
        self.assertEqual(manager.units[DAEMON], before[DAEMON])
        self.assertNotEqual(manager.units[SIGNER], before[SIGNER])
        self.assert_original_graph(manager)

    def test_dependent_drift_after_target_keeps_a_active_before_restore(self) -> None:
        manager = self.new_systemd()
        self.install_both_and_reload(manager)
        manager.restart_node_a(sha="target", successful=True)
        node_a_target = copy.deepcopy(manager.units[NODE_A])
        manager.units[DAEMON].generation += 1
        # A is already active. A core-generation drift is reported, not hidden
        # by another A restart. Restoring the graph is reload-only.
        self.restore_graph(manager)
        self.assertEqual(manager.units[NODE_A], node_a_target)
        self.assertTrue(manager.units[NODE_A].active)
        self.assert_original_graph(manager)

    def test_availability_budgets_are_strict_and_rollback_aware(self) -> None:
        self.assertEqual(A_UNAVAILABLE_PER_RESTART_SECONDS, 30)
        self.assertEqual(A_UNAVAILABLE_CUMULATIVE_SECONDS, 60)
        self.assertEqual(TARGET_CONVERGENCE_SECONDS, 180)
        self.assertEqual(DECOUPLED_WINDOW_SECONDS, 300)
        self.assertLessEqual(
            2 * A_UNAVAILABLE_PER_RESTART_SECONDS,
            A_UNAVAILABLE_CUMULATIVE_SECONDS,
        )
        self.assertLess(TARGET_CONVERGENCE_SECONDS, DECOUPLED_WINDOW_SECONDS)

    def test_original_strong_graph_would_reproduce_the_incident(self) -> None:
        manager = self.new_systemd()
        self.assert_original_graph(manager)
        daemon_before = manager.units[DAEMON].generation
        signer_before = manager.units[SIGNER].generation
        manager.restart_node_a(sha="target", successful=True)
        self.assertEqual(manager.units[DAEMON].generation, daemon_before + 1)
        self.assertEqual(manager.units[SIGNER].generation, signer_before + 1)


if __name__ == "__main__":
    unittest.main(verbosity=2)

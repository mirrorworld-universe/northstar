#!/usr/bin/env python3
"""Sonic: Recover L1 into new directories without replacing retained ER state."""
import json
import os
from pathlib import Path
import pwd
import runpy
import unittest

import continuity_devnet as rollout
from continuity_rollout_state import atomic_json, private_json, retained_fingerprint
from native_upgrade import digest
import native_l1_recovery as recovery

UPGRADE = runpy.run_path(str(Path(__file__).with_name("test-native-upgrade.py")))
FAILED_SLOT = 519_264_000
FEATURE = "8whkDenELnfwYLirBc4Wb4H2Z1z5K7tiykpdxJS7cWg1"
FEATURE_OWNER = "Feature111111111111111111111111111111111111"


class RecoveryHost(UPGRADE["UpgradeHost"]):
    def __init__(self, world):
        super().__init__(world)
        original = self.l1.call
        def call(method, parameters=None):
            if method == "getHighestSnapshotSlot":
                return {"full": world.recovery_snapshot_slot, "incremental": None}
            return original(method, parameters)
        self.l1.call = call

    def execute(self, arguments, timeout=120, allowed=False):
        arguments = list(map(str, arguments))
        self.world.recovery_commands.append(arguments)
        properties = {
            "FragmentPath": str(self.world.fragment), "DropInPaths": str(rollout.DROPIN),
            "NeedDaemonReload": "no", "User": pwd.getpwuid(os.getuid()).pw_name,
            "Group": "", "DynamicUser": "no", "Restart": "no",
        }
        if arguments[:2] == ["systemctl", "show"]:
            for name, value in properties.items():
                if "--property=" + name in arguments:
                    return value
        return super().execute(arguments, timeout, allowed)


class RecoveryTests(unittest.TestCase):
    def setUp(self):
        fixture = UPGRADE["UpgradeTests"]()
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        self.fixture, self.root, self.world = fixture, fixture.root, fixture.world
        self.base, self.upgrade = fixture.base, fixture.upgrade
        self.world.active = False
        self.world.args.recover_failed_l1_slot = FAILED_SLOT
        self.world.native_upstream_slot = FAILED_SLOT + 300
        self.world.native_local_slot = FAILED_SLOT + 300
        self.world.recovery_snapshot_slot = FAILED_SLOT + 100
        self.world.recovery_commands = []
        self.world.accounts[FEATURE] = {"owner": FEATURE_OWNER, "executable": False,
                                       "raw": b"\x01" + FAILED_SLOT.to_bytes(8, "little")}
        self.world.fragment = self.root / "unit/validator.service"
        self.world.fragment.write_text("[Service]\nUser=" + pwd.getpwuid(os.getuid()).pw_name + "\n")
        reservation_path = self.base / "control/reservation.json"
        reservation = private_json(reservation_path)
        reservation["original"]["retained_paths"].append(str(self.world.fragment))
        plan_path = self.base / "control/plan.json"
        plan = private_json(plan_path)
        plan["original_sha256"] = digest(reservation["original"])
        atomic_json(plan_path, plan)
        reservation["plan_sha256"] = digest(plan)
        atomic_json(reservation_path, reservation)
        atomic_json(self.base / "control/retained.json", retained_fingerprint(reservation["original"]["retained_paths"]))
        binding = dict(self.world.session_identity, source_revision=self.base.name,
                       sdk_revision=rollout.SDK_REVISION, er_genesis="11111111111111111111111111111111",
                       l1_rpc=self.world.args.l1_rpc, er_rpc=self.world.args.er_rpc)
        atomic_json(self.base / "control/sdk-smoke/binding.json", binding)
        self.old_ledger = Path(rollout.flag_value(self.world.current["arguments"], "--ledger"))
        self.old_ledger.mkdir(parents=True)
        (self.old_ledger / "preserved.bin").write_bytes(b"retained L1 data")
        self.before = retained_fingerprint([self.base, self.old_ledger, self.world.fragment])
        self.starts, self.uploads, self.keys = self.world.starts, self.world.uploads[:], self.world.key_generations

    def assert_no_new_start(self):
        self.assertEqual(self.world.starts, self.starts)
        self.assertEqual(self.world.uploads, self.uploads)
        self.assertEqual(self.world.key_generations, self.keys)

    def test_failed_l1_recovery_uses_new_data_directories_and_preserves_protocol_state(self):
        self.world.args.funder_keypair.unlink()
        RecoveryHost(self.world).run()
        self.assertEqual(self.world.starts, self.starts + 1)
        self.assertEqual(self.world.uploads, self.uploads)
        self.assertEqual(self.world.key_generations, self.keys)
        self.assertEqual(retained_fingerprint([self.base, self.old_ledger, self.world.fragment]), self.before)
        arguments = self.world.current["arguments"]
        self.assertEqual(rollout.flag_value(arguments, "--ledger"), str(self.upgrade / "runtime/l1/ledger"))
        self.assertEqual(rollout.flag_value(arguments, "--accounts"), str(self.upgrade / "runtime/l1/accounts"))
        self.assertIn("--no-voting", arguments)
        self.assertEqual(self.world.current["state_environment"], self.fixture.state_environment)
        report = private_json(self.upgrade / "control/report.json")
        self.assertEqual(report["qualification_kind"], "native-l1-recovery")
        self.assertEqual(report["controller_onchain_transactions"], 0)
        self.assertEqual(report["recovery_failed_slot"], FAILED_SLOT)
        self.assertTrue(report["prior_l1_data_preserved"])

    def test_qualified_recovery_retry_does_not_start_again(self):
        RecoveryHost(self.world).run()
        RecoveryHost(self.world).run()
        self.assertEqual(self.world.starts, self.starts + 1)
        self.assertEqual(self.world.uploads, self.uploads)
        self.assertEqual(retained_fingerprint([self.base, self.old_ledger, self.world.fragment]), self.before)

    def test_recovery_argument_rewrite_removes_all_old_state_paths(self):
        original = ["validator", "--ledger", "/old/ledger", "--accounts", "/old/a", "/old/b",
                    "--accounts-index-path=/old/index", "--no-snapshot-fetch", "--no-genesis-fetch",
                    "--no-incremental-snapshots", "--snapshots", "/old/snapshots", "--rpc-port", "8899"]
        actual = recovery.arguments(original, self.upgrade / "runtime")
        self.assertFalse(any(value.startswith("/old/") for value in actual))
        self.assertEqual(rollout.flag_value(actual, "--rpc-port"), "8899")
        self.assertIn("--no-voting", actual)
        self.assertNotIn("--no-snapshot-fetch", actual)
        self.assertEqual(rollout.flag_value(actual, "--expected-genesis-hash"), rollout.DEVNET)
        self.assertEqual(rollout.flag_value(actual, "--use-snapshot-archives-at-startup"), "when-newest")

    def test_recovery_never_discards_explicit_replay_constraints(self):
        with self.assertRaisesRegex(ValueError, "explicit replay constraints"):
            recovery.arguments(["validator", "--ledger", "/old", "--expected-bank-hash", "bound"], self.upgrade / "runtime")

    def test_recovery_fingerprint_failure_does_not_restart_the_original(self):
        self.world.retention_error = True
        with self.assertRaisesRegex(ValueError, "retention snapshot"):
            RecoveryHost(self.world).run()
        self.assertFalse(self.world.active)
        self.assert_no_new_start()

    def test_recovery_refuses_unrecognized_new_l1_data_before_start(self):
        class InjectedData(RecoveryHost):
            def execute(host, arguments, timeout=120, allowed=False):
                result = super().execute(arguments, timeout, allowed)
                if list(map(str, arguments)) == ["sudo", "-n", "systemctl", "daemon-reload"]:
                    (self.upgrade / "runtime/l1/ledger/unknown.bin").write_bytes(b"unrecognized")
                return result
        with self.assertRaisesRegex(ValueError, "unrecognized pre-start data"):
            InjectedData(self.world).run()
        self.assert_no_new_start()
        self.assertEqual(retained_fingerprint([self.base, self.old_ledger, self.world.fragment]), self.before)

    def test_recovery_requires_retained_bootstrap_binding(self):
        (self.base / "control/sdk-smoke/binding.json").unlink()
        with self.assertRaises(FileNotFoundError):
            RecoveryHost(self.world).run()
        self.assert_no_new_start()

    def test_recovery_without_explicit_slot_keeps_the_inactive_guard(self):
        self.world.args.recover_failed_l1_slot = None
        with self.assertRaisesRegex(ValueError, "original active service"):
            RecoveryHost(self.world).run()
        self.assert_no_new_start()

    def test_recovery_requires_the_reviewed_native_fix_in_target_history(self):
        class UnsupportedSource(RecoveryHost):
            def execute(host, arguments, timeout=120, allowed=False):
                if "--is-ancestor" in list(map(str, arguments)):
                    raise ValueError("reviewed native fix missing")
                return super().execute(arguments, timeout, allowed)
        with self.assertRaisesRegex(ValueError, "reviewed native fix missing"):
            UnsupportedSource(self.world).run()
        self.assert_no_new_start()

    def test_recovery_rejects_a_different_feature_activation(self):
        self.world.args.recover_failed_l1_slot -= 1
        with self.assertRaisesRegex(ValueError, "activation"):
            RecoveryHost(self.world).run()
        self.assert_no_new_start()

    def test_recovery_rejects_changed_retained_session_binding(self):
        path = self.base / "control/sdk-smoke/binding.json"
        binding = private_json(path)
        binding["context"] = "cd" * 32
        atomic_json(path, binding)
        with self.assertRaisesRegex(ValueError, "binding|identity"):
            RecoveryHost(self.world).run()
        self.assert_no_new_start()

    def test_recovery_rejects_changed_original_service_fragment(self):
        self.world.fragment.write_text("changed unit")
        with self.assertRaisesRegex(ValueError, "service.*changed|service.*differs"):
            RecoveryHost(self.world).run()
        self.assert_no_new_start()

    def test_recovery_rejects_unknown_service_overrides(self):
        class OtherOverride(RecoveryHost):
            def execute(host, arguments, timeout=120, allowed=False):
                if "--property=DropInPaths" in list(map(str, arguments)):
                    return str(rollout.DROPIN) + " /unknown/override.conf"
                return super().execute(arguments, timeout, allowed)
        with self.assertRaisesRegex(ValueError, "override|service"):
            OtherOverride(self.world).run()
        self.assert_no_new_start()

    def test_recovery_does_not_repeat_a_failed_start_or_restore_old_data(self):
        self.world.checkpoint_error = "checkpoint_lineage_recovery_failed"
        with self.assertRaisesRegex(ValueError, "quarantine"):
            RecoveryHost(self.world).run()
        self.assertFalse(self.world.active)
        self.assertEqual(self.world.starts, self.starts + 1)
        with self.assertRaisesRegex(ValueError, "already attempted"):
            RecoveryHost(self.world).run()
        self.assertEqual(self.world.starts, self.starts + 1)
        self.assertEqual(retained_fingerprint([self.base, self.old_ledger, self.world.fragment]), self.before)

    def test_recovery_rejects_a_snapshot_at_or_before_the_failed_slot(self):
        self.world.recovery_snapshot_slot = FAILED_SLOT
        with self.assertRaisesRegex(ValueError, "snapshot"):
            RecoveryHost(self.world).run()
        self.assertFalse(self.world.active)
        self.assertEqual(self.world.starts, self.starts + 1)


if __name__ == "__main__":
    unittest.main()

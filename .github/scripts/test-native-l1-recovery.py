#!/usr/bin/env python3
"""Sonic: Recover L1 into new directories without replacing retained ER state."""
import json
import os
from pathlib import Path
import pwd
import runpy
import subprocess
import sys
import unittest
from unittest import mock

import continuity_devnet as rollout
from continuity_rollout_state import atomic_json, private_json, retained_fingerprint
from native_upgrade import NativeUpgrade, digest
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
        if "empty-l1" in arguments:
            return json.dumps(recovery.inspect_empty_paths(private_json(arguments[-1])["paths"]))
        properties = {
            "FragmentPath": str(self.world.fragment), "DropInPaths": str(rollout.DROPIN),
            "NeedDaemonReload": "no", "User": pwd.getpwuid(os.getuid()).pw_name,
            "Group": "", "DynamicUser": "no", "Restart": "no", "SubState": "dead" if not self.world.active else "running",
            "Result": "success", "ExecMainCode": "2", "ExecMainStatus": "15", "NRestarts": "0",
            "ExecMainExitTimestamp": "Sun 2026-11-08 23:43:05 UTC",
        }
        properties.update(getattr(self.world, "retry_stop_overrides", {}))
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

    def interrupt_first_recovery(self):
        with mock.patch.object(NativeUpgrade, "wait_ready", side_effect=RuntimeError("native upgrade readiness timed out")):
            with self.assertRaisesRegex(RuntimeError, "readiness timed out"):
                RecoveryHost(self.world).run()
        self.assertFalse(self.world.active)
        self.world.args.reconcile_failed_l1 = True
        self.world.args.reconcile_stopped_at = "Sun 2026-11-08 23:43:05 UTC"

    def test_explicit_reconciliation_allows_only_one_start_and_preserves_first_attempt(self):
        self.interrupt_first_recovery()
        first_barrier = (self.upgrade / "control/start-attempted.json").read_bytes()
        cached = self.upgrade / "runtime/l1/ledger/snapshot-cache.bin"
        cached.write_bytes(b"retained downloaded L1 data")
        before = retained_fingerprint([self.base, self.old_ledger, self.world.fragment, self.upgrade / "runtime"])
        commands_before = len(self.world.recovery_commands)
        self.world.args.funder_keypair.unlink()
        RecoveryHost(self.world).run()
        self.assertEqual(self.world.starts, self.starts + 2)
        self.assertEqual(self.world.uploads, self.uploads)
        self.assertEqual(self.world.key_generations, self.keys)
        self.assertEqual((self.upgrade / "control/start-attempted.json").read_bytes(), first_barrier)
        self.assertEqual(before, retained_fingerprint([self.base, self.old_ledger, self.world.fragment, self.upgrade / "runtime"]))
        after = self.world.recovery_commands[commands_before:]
        self.assertFalse(any("install" in command or "mv" in command or "empty-l1" in command for command in after))
        report = private_json(self.upgrade / "control/l1-retry/report.json")
        self.assertEqual(report["qualification_kind"], "native-l1-recovery-retry")
        self.assertEqual(report["readiness_seconds"], 3600)
        self.assertEqual(report["controller_onchain_transactions"], 0)
        self.assertEqual(report["retry_authorization_sha256"], digest(private_json(self.upgrade / "control/l1-retry/authorization.json")))
        with self.assertRaises(ValueError):
            RecoveryHost(self.world).run()
        self.world.args.reconcile_failed_l1 = False
        RecoveryHost(self.world).run()
        self.assertEqual(self.world.starts, self.starts + 2)

    def test_reconciliation_cli_rejects_an_explicit_replacement_stage(self):
        result = subprocess.run([sys.executable, "-B", rollout.__file__, "run", "--revision", "b" * 40,
            "--sdk-checkout", str(self.world.args.sdk_checkout), "--stage", str(self.world.args.stage),
            "--upgrade-existing", "--recover-failed-l1-slot", str(FAILED_SLOT), "--reconcile-failed-l1",
            "--reconcile-stopped-at", "Sun 2026-11-08 23:43:05 UTC"], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("reconciliation requires retained artifacts", result.stderr)
        self.assert_no_new_start()

    def test_reconciliation_refuses_an_active_service_without_stopping_it(self):
        self.interrupt_first_recovery()
        self.world.active = True
        with self.assertRaisesRegex(ValueError, "unqualified stopped"):
            RecoveryHost(self.world).run()
        self.assertTrue(self.world.active)
        self.assertEqual(self.world.starts, self.starts + 1)

    def test_reconciliation_rejects_unreviewed_stopped_state(self):
        self.interrupt_first_recovery()
        for name, value in (("ExecMainExitTimestamp", "different stop"), ("ExecMainStatus", "1"),
                            ("Result", "exit-code"), ("Restart", "always"), ("NRestarts", "1"),
                            ("NeedDaemonReload", "yes"), ("ExecMainCode", "1")):
            with self.subTest(property=name):
                self.world.retry_stop_overrides = {name: value}
                with self.assertRaisesRegex(ValueError, "service|properties"):
                    RecoveryHost(self.world).run()
                self.assertEqual(self.world.starts, self.starts + 1)

    def test_reconciliation_rejects_changed_protocol_controls(self):
        self.interrupt_first_recovery()
        (self.base / "control/new-metadata").write_bytes(b"changed control state")
        with self.assertRaisesRegex(ValueError, "protocol control records"):
            RecoveryHost(self.world).run()
        self.assertEqual(self.world.starts, self.starts + 1)

    def test_reconciliation_rejects_state_changes_after_authorization(self):
        self.interrupt_first_recovery()
        class ChangedAfterAuthorization(RecoveryHost):
            def execute(host, arguments, timeout=120, allowed=False):
                result = super().execute(arguments, timeout, allowed)
                if "flush" in list(map(str, arguments)) and (self.upgrade / "control/l1-retry/authorization.json").exists():
                    (self.base / "runtime/checkpoints/retained.bin").write_bytes(b"changed after authorization")
                return result
        with self.assertRaisesRegex(ValueError, "state changed"):
            ChangedAfterAuthorization(self.world).run()
        self.assertEqual(self.world.starts, self.starts + 1)
        self.assertFalse((self.upgrade / "control/l1-retry/start-attempted.json").exists())

    def test_failed_reconciled_start_is_stopped_and_cannot_repeat(self):
        self.interrupt_first_recovery()
        self.world.checkpoint_error = "checkpoint_lineage_recovery_failed"
        with self.assertRaisesRegex(ValueError, "quarantine"):
            RecoveryHost(self.world).run()
        self.assertFalse(self.world.active)
        failure = private_json(self.upgrade / "control/l1-retry/failure.json")
        self.assertTrue(failure["stop_verified"])
        self.assertEqual(self.world.starts, self.starts + 2)
        with self.assertRaisesRegex(ValueError, "already attempted"):
            RecoveryHost(self.world).run()
        self.assertEqual(self.world.starts, self.starts + 2)

    def test_reconciliation_cannot_reuse_a_consumed_start_barrier(self):
        self.interrupt_first_recovery()
        class InterruptedAdditionalStart(RecoveryHost):
            def execute(host, arguments, timeout=120, allowed=False):
                if list(map(str, arguments)) == ["sudo", "-n", "systemctl", "start", rollout.SERVICE]:
                    raise RuntimeError("additional start interrupted")
                return super().execute(arguments, timeout, allowed)
        with self.assertRaisesRegex(RuntimeError, "start interrupted"):
            InterruptedAdditionalStart(self.world).run()
        self.assertEqual(self.world.starts, self.starts + 1)
        with self.assertRaisesRegex(ValueError, "already attempted"):
            RecoveryHost(self.world).run()
        self.assertEqual(self.world.starts, self.starts + 1)

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

    def test_empty_l1_helper_cli_checks_only_the_requested_empty_directory(self):
        directory = self.root / "empty-l1-check"
        directory.mkdir(mode=0o700)
        request = self.root / "empty-l1-check.json"
        atomic_json(request, {"paths": [str(directory)]})
        result = subprocess.check_output([sys.executable, "-B", Path(rollout.__file__), "empty-l1", "--input", request], text=True)
        self.assertIs(json.loads(result), True)

    def test_empty_l1_helper_rejects_directory_aliases(self):
        directory = self.root / "empty-l1-check"
        directory.mkdir(mode=0o700)
        alias = directory / "alias"
        alias.symlink_to(self.old_ledger, target_is_directory=True)
        with self.assertRaises(OSError):
            recovery.inspect_empty_paths([str(directory), str(alias)])
        self.assertEqual((self.old_ledger / "preserved.bin").read_bytes(), b"retained L1 data")

    def test_prestart_empty_check_runs_with_service_directory_privileges(self):
        RecoveryHost(self.world).run()
        checks = [command for command in self.world.recovery_commands if "empty-l1" in command]
        self.assertEqual(len(checks), 1)
        self.assertEqual(checks[0][:4], ["sudo", "-n", "python3", "-B"])

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

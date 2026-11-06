#!/usr/bin/env python3
"""Sonic: Native-only upgrades retain protocol identities and journal state."""
import copy
import json
from pathlib import Path
import runpy
import shutil
import unittest
from unittest import mock

from native_upgrade import digest

import continuity_devnet as rollout
from continuity_rollout_state import ARTIFACTS, atomic_json, file_hash, private_json

MODEL = runpy.run_path(str(Path(__file__).with_name("test-continuity-devnet-model.py")))


class UpgradeHost(MODEL["FakeHost"]):
    def __init__(self, world):
        super().__init__(world)
        original = self.er.call
        def call(method, params=None):
            if method == "northstarSysGetSyncStatus":
                slot = getattr(world, "native_local_slot", 100)
                return {"isSyncing": False, "latestSyncedSlot": slot, "latestL1Slot": slot,
                        "checkpointError": getattr(world, "checkpoint_error", None)}
            return original(method, params)
        self.er.call = call
        for rpc, attribute in ((self.l1, "native_local_slot"), (self.upstream, "native_upstream_slot")):
            original_call = rpc.call
            def slot_call(method, params=None, original=original_call, name=attribute):
                return getattr(world, name, 100) if method == "getSlot" else original(method, params)
            rpc.call = slot_call

    def execute(self, arguments, timeout=120, allowed=False):
        arguments = list(map(str, arguments))
        if arguments[0] == "node" and arguments[1].endswith("native-upgrade-snapshot.mjs"):
            self.world.snapshots += 1
            if self.world.snapshot_error:
                raise ValueError(self.world.snapshot_error)
            return json.dumps({"identity": self.world.session_identity,
                               "er_genesis": "11111111111111111111111111111111" if arguments[-1] == "online" else None})
        return super().execute(arguments, timeout, allowed)


class UpgradeTests(unittest.TestCase):
    def setUp(self):
        fixture = MODEL["ModelTests"]()
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        self.root, self.world = fixture.root, fixture.world
        self.world.args.upgrade_existing = True
        MODEL["FakeHost"](self.world).run()
        self.base = self.world.args.state_root / self.world.args.revision
        plan = private_json(self.base / "control/plan.json")
        self.world.session_identity = {
            "version": 2, "genesis": rollout.DEVNET, "portal": plan["programs"]["portal"],
            "token_bridge": plan["programs"]["token_bridge"], "validator": plan["validator"],
            "session": rollout.public_text(bytes([8]) * 32), "context": "ab" * 32,
            "profile": plan["profile"], "authority": plan["funder"], "nonce": "1", "grid_id": "0",
            "ttl_slots": "78840000", "fee_cap": "1000000000", "settlement_interval_slots": "75",
        }
        self.world.snapshots = 0
        self.world.snapshot_error = None
        self.state_environment = copy.deepcopy(self.world.current["state_environment"])
        self.portal = rollout.flag_value(self.world.current["arguments"], "--portal")
        for name in ("checkpoints", "unsettled"):
            (self.base / "runtime" / name / "retained.bin").write_bytes(b"retained protocol state")
        self.old_binary = Path(self.world.current["arguments"][0])
        self.old_binary_bytes = self.old_binary.read_bytes()
        stage = self.root / "upgrade-stage"
        shutil.copytree(self.world.args.stage, stage)
        for role in ("validator", "geyser_events"):
            target = stage / ARTIFACTS[role]
            target.chmod(0o700)
            target.write_bytes(b"\x7fELFupgraded-" + role.encode())
        manifest = private_json(stage / "manifest.json")
        manifest["source_revision"] = "b" * 40
        manifest["sha256"] = {role: file_hash(stage / relative) for role, relative in ARTIFACTS.items()}
        atomic_json(stage / "manifest.json", manifest)
        self.world.args.stage = stage
        self.world.args.revision = "b" * 40
        self.world.args.upgrade_existing = True
        self.uploads = self.world.uploads[:]
        self.keys = self.world.key_generations
        self.starts = self.world.starts

    def test_native_upgrade_preserves_program_session_and_journal_bindings(self):
        UpgradeHost(self.world).run()
        self.assertEqual(self.world.uploads, self.uploads)
        self.assertEqual(self.world.key_generations, self.keys)
        self.assertEqual(self.world.starts, self.starts + 1)
        self.assertEqual(self.world.current["state_environment"], self.state_environment)
        self.assertEqual(rollout.flag_value(self.world.current["arguments"], "--portal"), self.portal)
        self.assertNotEqual(Path(self.world.current["arguments"][0]), self.old_binary)
        self.assertEqual(self.old_binary.read_bytes(), self.old_binary_bytes)
        for name in ("checkpoints", "unsettled"):
            self.assertEqual((self.base / "runtime" / name / "retained.bin").read_bytes(), b"retained protocol state")
        self.assertGreaterEqual(self.world.snapshots, 3)
        self.assertEqual(len(list((self.base / "control/keys").iterdir())), 4)
        report = private_json(self.upgrade / "control/report.json")
        self.assertEqual(report["qualification_kind"], "native-upgrade")
        self.assertEqual(report["controller_onchain_transactions"], 0)
        self.assertFalse(report["session_replaced"])
        self.assertIn("not a fresh SOL/SPL", report["scope"])

    @property
    def upgrade(self):
        return self.world.args.state_root / "upgrades" / self.world.args.revision

    def assert_no_native_start(self):
        self.assertEqual(self.world.starts, self.starts)
        self.assertEqual(self.world.uploads, self.uploads)
        self.assertEqual(self.world.key_generations, self.keys)

    def interrupt_before_start(self):
        class InterruptedHost(UpgradeHost):
            def execute(host, arguments, timeout=120, allowed=False):
                if list(map(str, arguments)) == ["sudo", "-n", "systemctl", "daemon-reload"]:
                    raise RuntimeError("simulated pre-start interruption")
                return super().execute(arguments, timeout, allowed)
        with self.assertRaisesRegex(RuntimeError, "pre-start interruption"):
            InterruptedHost(self.world).run()
        self.assertFalse(self.world.active)
        self.assertFalse((self.upgrade / "control/start-attempted.json").exists())
        self.assert_no_native_start()

    def test_guarded_upgrade_requires_explicit_opt_in(self):
        self.world.args.upgrade_existing = False
        with self.assertRaisesRegex(ValueError, "explicit review"):
            UpgradeHost(self.world).run()
        self.assert_no_native_start()

    def test_opt_in_preserves_same_source_first_activation_retries(self):
        self.world.args.revision = self.base.name
        self.world.args.stage = Path(private_json(self.base / "control/reservation.json")["stage"])
        UpgradeHost(self.world).run()
        self.assertEqual(private_json(self.base / "control/report.json")["qualification_kind"], "first-activation")
        self.assertFalse(self.upgrade.exists())
        self.assertEqual(self.world.snapshots, 0)
        self.assertEqual(self.world.uploads, self.uploads)

    def test_native_upgrade_does_not_require_a_funder_keypair(self):
        self.world.args.funder_keypair.unlink()
        UpgradeHost(self.world).run()
        self.assertEqual(self.world.uploads, self.uploads)
        self.assertEqual(self.world.key_generations, self.keys)

    def test_native_upgrade_accepts_a_bound_pre_plugin_activation(self):
        reservation_path = self.base / "control/reservation.json"
        reservation = private_json(reservation_path)
        stage = Path(reservation["stage"])
        manifest = private_json(stage / "manifest.json")
        del manifest["sha256"]["geyser_events"]
        atomic_json(stage / "manifest.json", manifest)
        plan_path = self.base / "control/plan.json"
        plan = private_json(plan_path)
        plan["artifacts"] = manifest["sha256"]
        plan["profile"] = digest({"version": 2, "source": self.base.name, "artifacts": plan["artifacts"],
                                  "sdk": rollout.SDK_REVISION, "proof_acceptance_qualified": False})
        atomic_json(plan_path, plan)
        reservation["plan_sha256"] = digest(plan)
        atomic_json(reservation_path, reservation)
        activation_path = self.base / "control/activation.json"
        activation = private_json(activation_path)
        self.assertEqual(activation["arguments"][-2], "--er-geyser-plugin-config")
        activation["arguments"] = activation["arguments"][:-2]
        activation["artifacts"] = plan["artifacts"]
        del activation["geyser_config_sha256"]
        atomic_json(activation_path, activation)
        self.world.current["arguments"] = activation["arguments"][:]
        text = rollout.service_dropin(activation["arguments"], *activation["state_environment"].values())
        rollout.DROPIN.write_text(text)
        atomic_json(self.base / "control/activation-installed.json", {"sha256": rollout.hashlib.sha256(text.encode()).hexdigest()})
        (self.base / "runtime/geyser-events.json").unlink()
        (self.base / "runtime" / ARTIFACTS["geyser_events"]).unlink()
        self.world.session_identity["profile"] = plan["profile"]
        with self.assertRaisesRegex(ValueError, "artifact roles"):
            rollout.verify_artifacts(stage, self.base.name)
        UpgradeHost(self.world).run()
        self.assertEqual(self.world.current["state_environment"], self.state_environment)
        self.assertEqual(self.world.uploads, self.uploads)

    def test_changed_onchain_artifact_is_rejected_before_stopping(self):
        portal = self.world.args.stage / ARTIFACTS["portal"]
        portal.write_bytes(b"\x7fELFdifferent-program")
        manifest = private_json(self.world.args.stage / "manifest.json")
        manifest["sha256"]["portal"] = file_hash(portal)
        atomic_json(self.world.args.stage / "manifest.json", manifest)
        with self.assertRaisesRegex(ValueError, "cannot change on-chain"):
            UpgradeHost(self.world).run()
        self.assertTrue(self.world.active)
        self.assert_no_native_start()

    def test_quarantined_preflight_is_rejected_before_stopping(self):
        self.world.snapshot_error = "checkpoint quarantine blocks qualification"
        with self.assertRaisesRegex(ValueError, "checkpoint quarantine"):
            UpgradeHost(self.world).run()
        self.assertTrue(self.world.active)
        self.assert_no_native_start()

    def test_changed_managed_config_is_rejected_before_stopping(self):
        config = self.base / "runtime/geyser-events.json"
        config.chmod(0o600)
        config.write_text('{}')
        with self.assertRaisesRegex(ValueError, "event plugin binding"):
            UpgradeHost(self.world).run()
        self.assertTrue(self.world.active)
        self.assert_no_native_start()

    def test_prestart_retry_resumes_only_the_same_frozen_state(self):
        self.interrupt_before_start()
        UpgradeHost(self.world).run()
        self.assertEqual(self.world.starts, self.starts + 1)
        self.assertEqual(self.world.uploads, self.uploads)

    def test_prestart_retry_rejects_changed_journals(self):
        self.interrupt_before_start()
        (self.base / "runtime/unsettled/retained.bin").write_bytes(b"changed outside this upgrade")
        with self.assertRaisesRegex(ValueError, "changed while stopped"):
            UpgradeHost(self.world).run()
        self.assert_no_native_start()

    def test_qualified_retry_never_restarts_the_runtime(self):
        UpgradeHost(self.world).run()
        UpgradeHost(self.world).run()
        self.assertEqual(self.world.starts, self.starts + 1)
        self.assertEqual(self.world.uploads, self.uploads)

    def test_changed_session_after_start_stops_without_rollback(self):
        class ChangedSessionHost(UpgradeHost):
            def execute(host, arguments, timeout=120, allowed=False):
                result = super().execute(arguments, timeout, allowed)
                if list(map(str, arguments)) == ["sudo", "-n", "systemctl", "start", rollout.SERVICE]:
                    self.world.session_identity["context"] = "cd" * 32
                return result
        with self.assertRaisesRegex(ValueError, "session or ER genesis differs"):
            ChangedSessionHost(self.world).run()
        self.assertFalse(self.world.active)
        self.assertNotEqual(Path(self.world.current["arguments"][0]), self.old_binary)
        self.assertTrue((self.upgrade / "control/start-attempted.json").exists())
        with self.assertRaisesRegex(ValueError, "already attempted"):
            UpgradeHost(self.world).run()
        self.assertEqual(self.world.starts, self.starts + 1)
        self.assertEqual(self.world.uploads, self.uploads)

    def test_uncertain_start_never_falls_back_to_the_old_runtime(self):
        class UncertainStartHost(UpgradeHost):
            def execute(host, arguments, timeout=120, allowed=False):
                result = super().execute(arguments, timeout, allowed)
                if list(map(str, arguments)) == ["sudo", "-n", "systemctl", "start", rollout.SERVICE]:
                    raise RuntimeError("simulated uncertain start")
                return result
        with self.assertRaisesRegex(RuntimeError, "uncertain start"):
            UncertainStartHost(self.world).run()
        self.assertFalse(self.world.active)
        self.assertNotEqual(Path(self.world.current["arguments"][0]), self.old_binary)
        with self.assertRaisesRegex(ValueError, "already attempted"):
            UpgradeHost(self.world).run()
        self.assertEqual(self.world.starts, self.starts + 1)

    def test_failed_qualification_cancels_pending_systemd_restarts(self):
        class RestartingHost(UpgradeHost):
            def execute(host, arguments, timeout=120, allowed=False):
                command = list(map(str, arguments))
                if getattr(self.world, "restarting", False):
                    if command == ["systemctl", "show", "--property=ActiveState", "--value", rollout.SERVICE]:
                        return "activating"
                    if command == ["systemctl", "show", "--property=MainPID", "--value", rollout.SERVICE]:
                        return "0"
                    if command == ["sudo", "-n", "systemctl", "stop", rollout.SERVICE]:
                        self.world.restarting = False
                if command[0] == "node" and command[1].endswith("native-upgrade-snapshot.mjs") and command[-1] == "online" and self.world.starts > self.starts:
                    self.world.restarting = True
                    raise RuntimeError("simulated interrupted qualification")
                return super().execute(arguments, timeout, allowed)
        with self.assertRaisesRegex(RuntimeError, "interrupted qualification"):
            RestartingHost(self.world).run()
        self.assertFalse(self.world.active)
        self.assertFalse(self.world.restarting)
        self.assertEqual(self.world.starts, self.starts + 1)

    def test_post_start_quarantine_stops_without_clearing_state(self):
        class QuarantinedHost(UpgradeHost):
            def execute(host, arguments, timeout=120, allowed=False):
                result = super().execute(arguments, timeout, allowed)
                if list(map(str, arguments)) == ["sudo", "-n", "systemctl", "start", rollout.SERVICE]:
                    self.world.checkpoint_error = "checkpoint_lineage_mismatch"
                return result
        with self.assertRaisesRegex(ValueError, "checkpoint quarantine"):
            QuarantinedHost(self.world).run()
        self.assertFalse(self.world.active)
        self.assertEqual((self.base / "runtime/unsettled/retained.bin").read_bytes(), b"retained protocol state")
        self.assertEqual(self.world.uploads, self.uploads)
        self.assertEqual(self.world.starts, self.starts + 1)

    def test_later_systemd_override_is_rejected_before_native_start(self):
        class OverrideHost(UpgradeHost):
            def execute(host, arguments, timeout=120, allowed=False):
                if list(map(str, arguments)) == ["systemctl", "show", "--property=ExecStart", "--value", rollout.SERVICE]:
                    return "ExecStart=/different/validator"
                return super().execute(arguments, timeout, allowed)
        with self.assertRaisesRegex(ValueError, "another systemd override"):
            OverrideHost(self.world).run()
        self.assertFalse(self.world.active)
        self.assert_no_native_start()

    def test_changed_active_library_is_not_reported_as_qualified(self):
        UpgradeHost(self.world).run()
        library = self.upgrade / "runtime" / ARTIFACTS["geyser_events"]
        library.chmod(0o700)
        library.write_bytes(b"different retained library")
        with self.assertRaisesRegex(ValueError, "upgrade artifact differs"):
            UpgradeHost(self.world).run()
        self.assertFalse(self.world.active)
        self.assertEqual(self.world.starts, self.starts + 1)

    def test_missing_established_control_records_never_trigger_a_new_start(self):
        UpgradeHost(self.world).run()
        for name in ("progress.json", "start-attempted.json", "installed.json", "frozen.json", "report.json", "plan.json"):
            path = self.upgrade / "control" / name
            value = private_json(path)
            path.unlink()
            with self.subTest(name=name), self.assertRaises((ValueError, OSError)):
                UpgradeHost(self.world).run()
            self.assertTrue(self.world.active)
            self.assertEqual(self.world.starts, self.starts + 1)
            atomic_json(path, value)

    def test_readiness_waits_for_the_refreshed_prestart_floor(self):
        self.world.native_upstream_slot = 120
        class AdvancingHeadHost(UpgradeHost):
            def execute(host, arguments, timeout=120, allowed=False):
                result = super().execute(arguments, timeout, allowed)
                if list(map(str, arguments)) == ["sudo", "-n", "systemctl", "daemon-reload"]:
                    self.world.native_upstream_slot = 125
                return result
        def catch_up(_duration):
            self.world.native_local_slot = 125
        with mock.patch("native_upgrade.time.sleep", side_effect=catch_up) as poll:
            AdvancingHeadHost(self.world).run()
        self.assertEqual(private_json(self.upgrade / "control/plan.json")["l1_floor"], 120)
        self.assertEqual(private_json(self.upgrade / "control/start-attempted.json")["l1_floor"], 125)
        self.assertEqual(poll.call_count, 1)
        self.assertEqual(self.world.starts, self.starts + 1)

    def test_next_upgrade_uses_the_qualified_native_binding(self):
        UpgradeHost(self.world).run()
        previous = Path(self.world.current["arguments"][0])
        stage = self.root / "next-upgrade-stage"
        shutil.copytree(self.world.args.stage, stage)
        manifest = private_json(stage / "manifest.json")
        manifest["source_revision"] = "c" * 40
        atomic_json(stage / "manifest.json", manifest)
        self.world.args.stage = stage
        self.world.args.revision = "c" * 40
        UpgradeHost(self.world).run()
        self.assertNotEqual(Path(self.world.current["arguments"][0]), previous)
        self.assertTrue(previous.exists())
        self.assertEqual(self.world.current["state_environment"], self.state_environment)
        self.assertEqual(self.world.starts, self.starts + 2)
        self.assertEqual(self.world.uploads, self.uploads)


if __name__ == "__main__":
    unittest.main()

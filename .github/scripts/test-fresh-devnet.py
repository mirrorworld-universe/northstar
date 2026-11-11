#!/usr/bin/env python3
"""Sonic: Explicit fresh cutover archives the old stack without modifying its programs."""
import copy
import json
from pathlib import Path
import runpy
import shutil
import unittest

import continuity_devnet as rollout
from continuity_rollout_state import ARTIFACTS, atomic_json, file_hash, private_json, retained_fingerprint
from native_upgrade import STATE_KEYS

UPGRADE = runpy.run_path(str(Path(__file__).with_name("test-native-upgrade.py")))


class FreshHost(UPGRADE["UpgradeHost"]):
    def __init__(self, world):
        super().__init__(world)
        original = self.er.call
        def er_call(method, params=None):
            if method == "getSessionPda":
                portal = rollout.flag_value(world.current["arguments"], "--portal")
                return world.session_identity["session"] if portal == world.session_identity["portal"] else rollout.public_text(bytes([9]) * 32)
            return original(method, params)
        self.er.call = er_call

    def execute(self, arguments, timeout=120, allowed=False):
        args = list(map(str, arguments))
        if args[0] == "solana-keygen" and "pubkey" in args and Path(args[-1]).parent.name == "keys":
            labels = {"portal": 43, "token_bridge": 44, "portal-buffer": 45, "token_bridge-buffer": 46}
            return rollout.public_text(bytes([labels[Path(args[-1]).stem]]) * 32)
        if args[0] == "node" and args[1].endswith("native-upgrade-snapshot.mjs"):
            config = private_json(args[-2])
            identity = copy.deepcopy(self.world.session_identity)
            if config["portal"] != identity["portal"]:
                identity.update({key: config[key] for key in ("genesis", "portal", "token_bridge", "validator", "profile")})
                identity.update(context="cd" * 32, session=rollout.public_text(bytes([9]) * 32))
            return json.dumps({"identity": identity, "er_genesis": "11111111111111111111111111111111" if args[-1] == "online" else None})
        if args[0] == "node" and args[1].endswith("devnet-continuity-smoke.mjs"):
            if self.world.fresh_smoke_error:
                raise RuntimeError("fresh qualification unavailable")
            config, phase = private_json(args[-2]), args[-1]
            directory = Path(config["smoke_directory"])
            directory.mkdir(mode=0o700, exist_ok=True)
            result = {"phase": phase, "context": "cd" * 32}
            atomic_json(directory / (phase + ".json"), result)
            return json.dumps(result)
        if "program" in args and "deploy" in args:
            role = Path(args[args.index("--program-id") + 1]).stem
            self.world.uploads.append(role)
            key = bytes([53 if role == "portal" else 54]) * 32
            self.world.accounts[self.plan["programs"][role]] = {"owner": rollout.LOADER, "executable": True,
                "raw": (2).to_bytes(4, "little") + key}
            header = (3).to_bytes(4, "little") + bytes(8) + b"\x01" + rollout.public_bytes(self.plan["funder"])
            self.world.accounts[rollout.public_text(key)] = {"owner": rollout.LOADER, "executable": False,
                "raw": header + (self.stage / ARTIFACTS[role]).read_bytes()}
            return ""
        return super().execute(arguments, timeout, allowed)


class FreshTests(unittest.TestCase):
    def setUp(self):
        fixture = UPGRADE["UpgradeTests"]()
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        self.root, self.world, self.base = fixture.root, fixture.world, fixture.base
        UPGRADE["UpgradeHost"](self.world).run()
        self.previous = self.world.args.state_root / "upgrades" / self.world.args.revision
        self.old_context = self.world.session_identity["context"]
        self.old_session = self.world.session_identity["session"]
        self.old_accounts = copy.deepcopy(self.world.accounts)
        self.world.accounts[self.old_session] = {"owner": self.world.session_identity["portal"], "executable": False,
                                                "raw": b"NSSESS02" + bytes(500)}
        self.old_accounts = copy.deepcopy(self.world.accounts)
        self.old_arguments = copy.deepcopy(self.world.current["arguments"])
        self.old_environment = copy.deepcopy(self.world.current["state_environment"])
        self.retained_paths = [self.base / "control", self.previous / "control", *map(Path, self.old_environment.values())]
        self.before = retained_fingerprint(self.retained_paths)
        stage = self.root / "fresh-stage"
        shutil.copytree(self.world.args.stage, stage)
        for role, relative in ARTIFACTS.items():
            path = stage / relative
            path.chmod(0o700)
            path.write_bytes(b"\x7fELFfresh-" + role.encode())
        manifest = private_json(stage / "manifest.json")
        manifest.update(source_revision="c" * 40, sha256={role: file_hash(stage / relative) for role, relative in ARTIFACTS.items()})
        atomic_json(stage / "manifest.json", manifest)
        self.world.args.stage, self.world.args.revision = stage, "c" * 40
        self.world.args.upgrade_existing = False
        self.world.args.replace_context = self.old_context
        self.world.args.abandon_previous_state = True
        self.world.fresh_smoke_error = False
        self.starts, self.keys, self.uploads = self.world.starts, self.world.key_generations, list(self.world.uploads)

    @property
    def target(self):
        return self.world.args.state_root / self.world.args.revision

    def assert_no_side_effects(self):
        self.assertEqual(self.world.starts, self.starts)
        self.assertEqual(self.world.key_generations, self.keys)
        self.assertEqual(self.world.uploads, self.uploads)
        self.assertTrue(self.world.active)
        self.assertEqual(self.world.current["arguments"], self.old_arguments)

    def test_explicit_fresh_cutover_archives_old_state_and_uses_new_identities(self):
        FreshHost(self.world).run()
        report = private_json(self.target / "control/report.json")
        self.assertEqual(report["qualification_kind"], "fresh-stack")
        self.assertEqual(report["previous_context"], self.old_context)
        self.assertNotEqual(report["context"], self.old_context)
        self.assertTrue(report["previous_state_abandoned"])
        self.assertTrue(report["archived_state_unchanged"])
        self.assertFalse(report["old_programs_modified"])
        self.assertEqual(report["event_source"], "l1")
        self.assertEqual(self.world.starts, self.starts + 3)
        self.assertNotEqual(self.world.current["state_environment"], self.old_environment)
        self.assertEqual(retained_fingerprint(self.retained_paths), self.before)
        for address, account in self.old_accounts.items():
            self.assertEqual(self.world.accounts[address], account)
        starts = self.world.starts
        FreshHost(self.world).run()
        self.assertEqual(self.world.starts, starts)
        self.assertEqual(self.world.uploads, self.uploads + ["portal", "token_bridge"])

    def test_context_change_during_upload_blocks_native_cutover(self):
        class ChangedContextHost(FreshHost):
            def execute(host, arguments, timeout=120, allowed=False):
                result = super().execute(arguments, timeout, allowed)
                if "program" in list(map(str, arguments)) and "deploy" in list(map(str, arguments)):
                    host.world.session_identity["context"] = "ef" * 32
                return result
        with self.assertRaises(ValueError):
            ChangedContextHost(self.world).run()
        self.assertEqual(self.world.starts, self.starts)
        self.assertTrue(self.world.active)
        self.assertEqual(self.world.current["arguments"], self.old_arguments)

    def test_fresh_cutover_requires_explicit_abandonment(self):
        self.world.args.abandon_previous_state = False
        with self.assertRaises(ValueError):
            FreshHost(self.world).run()
        self.assert_no_side_effects()

    def test_fresh_cutover_requires_the_reviewed_context(self):
        self.world.args.replace_context = "ef" * 32
        with self.assertRaises(ValueError):
            FreshHost(self.world).run()
        self.assert_no_side_effects()

    def test_first_activation_without_qualification_cannot_be_replaced(self):
        activation = private_json(self.base / "control/activation.json")
        self.world.current["arguments"] = activation["arguments"]
        self.world.current["state_environment"] = activation["state_environment"]
        self.world.current["binary_sha256"] = activation["artifacts"]["validator"]
        self.old_arguments = copy.deepcopy(activation["arguments"])
        rollout.DROPIN.write_text(rollout.service_dropin(activation["arguments"],
            *[activation["state_environment"][key] for key in STATE_KEYS]))
        (self.base / "control/report.json").unlink()
        with self.assertRaises(ValueError):
            FreshHost(self.world).run()
        self.assert_no_side_effects()

    def test_unqualified_previous_start_cannot_be_adopted(self):
        path = self.previous / "control/progress.json"
        progress = private_json(path)
        progress["phase"] = "start-attempted"
        atomic_json(path, progress)
        with self.assertRaises(ValueError):
            FreshHost(self.world).run()
        self.assert_no_side_effects()

    def test_post_start_failure_stops_only_the_verified_new_runtime(self):
        self.world.fresh_smoke_error = True
        with self.assertRaisesRegex(RuntimeError, "fresh qualification"):
            FreshHost(self.world).run()
        self.assertFalse(self.world.active)
        self.assertNotEqual(self.world.current["arguments"], self.old_arguments)
        self.assertEqual(retained_fingerprint(self.retained_paths), self.before)
        starts = self.world.starts
        with self.assertRaises(ValueError):
            FreshHost(self.world).run()
        self.assertEqual(self.world.starts, starts)

    def test_uncertain_running_record_is_stopped_without_another_start(self):
        FreshHost(self.world).run()
        progress = self.target / "control/fresh-progress.json"
        state = private_json(progress)
        state["phase"] = "running"
        atomic_json(progress, state)
        (self.target / "control/report.json").unlink()
        starts = self.world.starts
        with self.assertRaisesRegex(ValueError, "operator reconciliation"):
            FreshHost(self.world).run()
        self.assertFalse(self.world.active)
        self.assertEqual(self.world.starts, starts)

    def test_missing_start_readiness_evidence_cannot_remain_qualified(self):
        FreshHost(self.world).run()
        (self.target / "control/fresh-starts/resume-sol-ready.json").unlink()
        starts = self.world.starts
        with self.assertRaises((ValueError, OSError)):
            FreshHost(self.world).run()
        self.assertFalse(self.world.active)
        self.assertEqual(self.world.starts, starts)

    def test_fresh_program_cannot_reuse_the_funder_identity(self):
        class AliasHost(FreshHost):
            def execute(host, arguments, timeout=120, allowed=False):
                args = list(map(str, arguments))
                if args[0] == "solana-keygen" and "pubkey" in args and Path(args[-1]).stem == "portal":
                    return rollout.public_text(bytes([1]) * 32)
                return super().execute(arguments, timeout, allowed)
        with self.assertRaisesRegex(ValueError, "reuse"):
            AliasHost(self.world).run()
        self.assertEqual(self.world.uploads, self.uploads)
        self.assertEqual(self.world.starts, self.starts)
        self.assertTrue(self.world.active)

    def test_fresh_program_cannot_reuse_a_previous_upload_key(self):
        old_buffer = private_json(self.base / "control/plan.json")["programs"]["portal-buffer"]
        class AliasHost(FreshHost):
            def execute(host, arguments, timeout=120, allowed=False):
                args = list(map(str, arguments))
                if args[0] == "solana-keygen" and "pubkey" in args and Path(args[-1]).stem == "portal":
                    return old_buffer
                return super().execute(arguments, timeout, allowed)
        with self.assertRaisesRegex(ValueError, "reuse"):
            AliasHost(self.world).run()
        self.assertEqual(self.world.uploads, self.uploads)
        self.assertEqual(self.world.starts, self.starts)
        self.assertTrue(self.world.active)

    def test_missing_installation_marker_cannot_remain_qualified(self):
        FreshHost(self.world).run()
        (self.target / "control/activation-installed.json").unlink()
        starts = self.world.starts
        with self.assertRaises((ValueError, OSError)):
            FreshHost(self.world).run()
        self.assertFalse(self.world.active)
        self.assertEqual(self.world.starts, starts)

    def test_post_start_failure_cancels_a_verified_pending_restart(self):
        class RestartingHost(FreshHost):
            def execute(host, arguments, timeout=120, allowed=False):
                args = list(map(str, arguments))
                if args[0] == "node" and args[1].endswith("devnet-continuity-smoke.mjs"):
                    host.world.active = False
                    host.world.pending_restart = True
                    raise RuntimeError("fresh qualification pending restart")
                if getattr(host.world, "pending_restart", False) and "systemctl" in args:
                    if "--property=ActiveState" in args:
                        return "activating"
                    if "stop" in args:
                        host.world.pending_restart = False
                return super().execute(arguments, timeout, allowed)
        with self.assertRaisesRegex(RuntimeError, "pending restart"):
            RestartingHost(self.world).run()
        self.assertFalse(self.world.pending_restart)
        self.assertFalse(self.world.active)
        self.assertTrue(private_json(self.target / "control/fresh-progress.json")["quarantined"])

    def test_changed_post_start_binding_requires_reconciliation_without_a_stop(self):
        class ChangedHost(FreshHost):
            def execute(host, arguments, timeout=120, allowed=False):
                args = list(map(str, arguments))
                if args[0] == "node" and args[1].endswith("devnet-continuity-smoke.mjs"):
                    host.world.current["arguments"].extend(["--changed-setting", "value"])
                    raise RuntimeError("fresh qualification binding changed")
                return super().execute(arguments, timeout, allowed)
        with self.assertRaisesRegex(RuntimeError, "binding changed"):
            ChangedHost(self.world).run()
        self.assertTrue(self.world.active)
        state = private_json(self.target / "control/fresh-progress.json")
        self.assertFalse(state["quarantined"])
        self.assertTrue(state["operator_reconciliation_required"])


if __name__ == "__main__":
    unittest.main()

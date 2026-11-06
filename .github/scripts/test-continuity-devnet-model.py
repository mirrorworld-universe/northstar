#!/usr/bin/env python3
"""Sonic: Run first activation/retry against a fake host; no services or chain access."""
import copy
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import continuity_devnet as rollout
from continuity_rollout_state import ARTIFACTS, DEVNET, SDK_REVISION, atomic_json, file_hash, inspect_paths, private_json, retained_fingerprint


class FakeRpc:
    def __init__(self, world, layer):
        self.world, self.layer = world, layer

    def call(self, method, parameters=None):
        if method == "getGenesisHash":
            return "11111111111111111111111111111111" if self.layer == "er" else DEVNET
        if method == "getIdentity":
            return {"identity": self.world.validator}
        if method == "getSlot":
            return 100
        if method == "getHealth":
            return "ok"
        if method == "getSessionPda":
            return None
        if method == "getMinimumBalanceForRentExemption":
            return 1000
        if method == "getBalance":
            return {"value": 20_000_000_000}
        raise AssertionError(method)

    def account(self, address):
        return self.world.accounts.get(address)


class FakeHost(rollout.Deployment):
    def __init__(self, world):
        super().__init__(world.args)
        self.world = world
        self.upstream, self.l1, self.er = (FakeRpc(world, layer) for layer in ("upstream", "l1", "er"))

    def event_listener_ready(self):
        self.world.event_probes += 1
        arguments = self.world.current["arguments"]
        config_path = Path(arguments[arguments.index("--er-geyser-plugin-config") + 1])
        configuration = json.loads(config_path.read_text())
        assert configuration["listen_addr"] == "0.0.0.0:9101"
        assert file_hash(configuration["libpath"]) == self.manifest["sha256"]["geyser_events"]
        return True

    def execute(self, arguments, timeout=120, allowed=False):
        arguments = list(map(str, arguments))
        if arguments[0] == "git":
            return SDK_REVISION + "\n" if "rev-parse" in arguments else ""
        if arguments[0] == "solana-keygen":
            if "new" in arguments:
                filename = Path(arguments[arguments.index("--outfile") + 1])
                atomic_json(filename, [0] * 64)
                self.world.key_generations += 1
                return ""
            role = Path(arguments[-1]).stem
            labels = {"funder": 1, "portal": 3, "token_bridge": 4, "portal-buffer": 5, "token_bridge-buffer": 6}
            return rollout.public_text(bytes([labels[role]]) * 32)
        if arguments[0] == "node" and "-e" in arguments:
            return rollout.public_text(bytes([8]) * 32)
        if arguments[0] == "node":
            config, phase = private_json(arguments[-2]), arguments[-1]
            directory = Path(config["smoke_directory"])
            directory.mkdir(mode=0o700, exist_ok=True)
            filename = directory / (phase + ".json")
            if filename.exists():
                return json.dumps(private_json(filename))
            result = {"phase": phase, "context": "ab" * 32}
            atomic_json(filename, result)
            return json.dumps(result)
        if "stop-vrf-service.py" in " ".join(arguments):
            self.world.worker_stops += 1
            return ""
        if "inventory" in arguments:
            return json.dumps(self.world.current)
        if "flush" in arguments:
            rollout.sync_paths(private_json(arguments[-1])["paths"])
            return "null"
        if "fingerprint" in arguments or "inspect" in arguments:
            if "fingerprint" in arguments and self.world.retention_error and not self.world.active:
                raise ValueError("retention snapshot unavailable")
            function = retained_fingerprint if "fingerprint" in arguments else inspect_paths
            return json.dumps(function(private_json(arguments[-1])["paths"]))
        if "install" in arguments:
            if "-d" in arguments:
                directory = Path(arguments[-1])
                directory.mkdir(parents=True, exist_ok=True)
                directory.chmod(int(arguments[arguments.index("-m") + 1], 8))
            else:
                shutil.copyfile(arguments[-2], arguments[-1])
                Path(arguments[-1]).chmod(int(arguments[arguments.index("-m") + 1], 8))
            return ""
        if "mv" in arguments:
            os.replace(arguments[-2], arguments[-1])
            return ""
        if "systemctl" in arguments:
            if "stop" in arguments:
                self.world.active = False
                return ""
            if "start" in arguments:
                self.world.active = True
                self.world.starts += 1
                self.world.current = copy.deepcopy(self.world.original)
                if rollout.DROPIN.exists():
                    command = shlex.split(rollout.DROPIN.read_text().split("ExecStart=")[-1])
                    self.world.current["arguments"] = command[3:]
                    self.world.current["state_environment"] = dict(item.split("=", 1) for item in command[1:3])
                    self.world.current["binary_sha256"] = file_hash(command[3])
                return ""
            if "--property=ActiveState" in arguments:
                return "active" if self.world.active else "inactive"
            if "--property=MainPID" in arguments:
                return "42" if self.world.active else "0"
            if "--property=ExecStart" in arguments:
                return "{ path=/usr/bin/env ; argv[]=" + rollout.DROPIN.read_text().split("ExecStart=")[-1] + "; }"
            if "daemon-reload" in arguments:
                return ""
        if "program" in arguments and "deploy" in arguments:
            assert self.world.worker_stops > 0, "worker must be quiesced before native uploads"
            role = Path(arguments[arguments.index("--program-id") + 1]).stem
            self.world.uploads.append(role)
            if self.world.upload_failure == "not-observed":
                raise RuntimeError("test interrupted upload")
            program = self.plan["programs"][role]
            data_key = bytes([13 if role == "portal" else 14]) * 32
            self.world.accounts[program] = {"owner": rollout.LOADER, "executable": True, "raw": (2).to_bytes(4, "little") + data_key}
            header = (3).to_bytes(4, "little") + bytes(8) + b"\x01" + rollout.public_bytes(self.plan["funder"])
            self.world.accounts[rollout.public_text(data_key)] = {"owner": rollout.LOADER, "executable": False,
                                                               "raw": header + (self.stage / ARTIFACTS[role]).read_bytes()}
            if self.world.upload_failure == "included":
                raise RuntimeError("test lost upload response")
            return ""
        raise AssertionError(arguments)


class ModelTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        stage = self.root / "stage"
        checksums = {}
        for role, relative in ARTIFACTS.items():
            filename = stage / relative
            filename.parent.mkdir(parents=True, exist_ok=True)
            filename.write_bytes(b"\x7fELF" + role.encode())
            checksums[role] = file_hash(filename)
        atomic_json(stage / "manifest.json", {"version": 2, "protocol": "continuity-v2", "source_revision": "a" * 40,
                    "platform_tools": "v1.56", "arch": "v0", "sha256": checksums, "activation_performed": False,
                    "hosted_qualified": False, "proof_acceptance_qualified": False})
        sdk = self.root / "sdk"
        sdk.mkdir()
        (sdk / "package-lock.json").write_text("fixture dependency lock")
        funder = self.root / "funder.json"
        atomic_json(funder, [0] * 64)
        old_binary = self.root / "old/agave-validator"
        old_binary.parent.mkdir()
        old_binary.write_bytes(b"\x7fELForiginal")
        original = {"arguments": [str(old_binary), "--ledger", str(self.root / "old/ledger"), "--portal", rollout.public_text(bytes([7]) * 32)],
                    "pid": 41, "start_time": "1", "binary_sha256": file_hash(old_binary), "uid": os.getuid(), "gid": os.getgid(),
                    "retained_paths": [str(old_binary)], "state_environment": {}}
        args = SimpleNamespace(stage=stage, revision="a" * 40, state_root=self.root / "state", sdk_checkout=sdk,
                               funder_keypair=funder, l1_rpc="http://localhost:8899", er_rpc="http://localhost:8910",
                               devnet_rpc="https://api.devnet.solana.com", max_estimated_cost_lamports=10_000_000_000)
        self.world = SimpleNamespace(args=args, original=original, current=copy.deepcopy(original), active=True, starts=0,
                                     worker_stops=0, event_probes=0, uploads=[], accounts={}, upload_failure=None, key_generations=0, retention_error=False,
                                     validator=rollout.public_text(bytes([2]) * 32))
        lock = sdk / "package-lock.json"
        self.addCleanup(patch.stopall)
        patch.object(rollout, "DROPIN", self.root / "unit/managed.conf").start()
        patch.object(rollout, "print", create=True).start()
        patch.object(rollout, "file_hash", side_effect=lambda name: "c9c3eab30574777558d737d043060eadfec8c156b41ee4b313bbab2d05910ded" if Path(name) == lock else file_hash(name)).start()
        patch.dict(os.environ, {"GITHUB_OUTPUT": str(self.root / "outputs")}).start()

    def test_first_activation_and_repeat_preserve_legacy_and_do_not_upload_again(self):
        before = retained_fingerprint(self.world.original["retained_paths"])
        FakeHost(self.world).run()
        self.assertEqual(self.world.uploads, ["portal", "token_bridge"])
        self.assertEqual(self.world.starts, 3)
        self.assertEqual(self.world.key_generations, 4)
        FakeHost(self.world).run()
        self.assertEqual(self.world.uploads, ["portal", "token_bridge"])
        self.assertEqual(self.world.key_generations, 4)
        self.assertEqual(retained_fingerprint(self.world.original["retained_paths"]), before)

    def test_activation_starts_with_the_public_er_event_plugin(self):
        FakeHost(self.world).run()
        arguments = self.world.current["arguments"]
        self.assertIn("--er-geyser-plugin-config", arguments)
        config_path = Path(arguments[arguments.index("--er-geyser-plugin-config") + 1])
        config = json.loads(config_path.read_text())
        self.assertEqual(config["listen_addr"], "0.0.0.0:9101")
        self.assertEqual(config["commitment"], "confirmed")
        plugin = Path(config["libpath"])
        self.assertTrue(plugin.is_absolute())
        self.assertEqual(plugin, self.world.args.state_root / self.world.args.revision / "runtime/lib/libnorthstar_geyser_events.so")
        self.assertEqual(file_hash(plugin), file_hash(self.world.args.stage / "lib/libnorthstar_geyser_events.so"))
        self.assertEqual(config_path.stat().st_mode & 0o777, 0o400)
        self.assertEqual(plugin.stat().st_mode & 0o777, 0o500)
        self.assertEqual(self.world.event_probes, self.world.starts)

    def test_changed_installed_event_artifacts_are_not_replaced_or_started(self):
        for relative in ("lib/libnorthstar_geyser_events.so", "geyser-events.json"):
            with self.subTest(relative=relative):
                if self.world.starts == 0:
                    FakeHost(self.world).run()
                target = self.world.args.state_root / self.world.args.revision / "runtime" / relative
                original = target.read_bytes()
                mode = target.stat().st_mode & 0o777
                target.chmod(0o600)
                target.write_bytes(b"changed")
                target.chmod(mode)
                starts = self.world.starts
                with self.assertRaisesRegex(ValueError, "event plugin.*differ"):
                    FakeHost(self.world).run()
                self.assertEqual(self.world.starts, starts)
                self.assertEqual(target.read_bytes(), b"changed")
                target.chmod(0o600)
                target.write_bytes(original)
                target.chmod(mode)

    def test_retention_failure_restores_unchanged_original_service_before_activation(self):
        self.world.retention_error = True
        with self.assertRaisesRegex(ValueError, "retention snapshot"):
            FakeHost(self.world).run()
        self.assertTrue(self.world.active)
        self.assertEqual(self.world.current, self.world.original)
        self.assertFalse((self.world.args.state_root / self.world.args.revision / "control/activation.json").exists())

    def test_changed_running_service_is_not_stopped_on_retry(self):
        FakeHost(self.world).run()
        starts = self.world.starts
        self.world.current["arguments"].extend(["--additional-setting", "changed"])
        with self.assertRaisesRegex(ValueError, "running.*changed"):
            FakeHost(self.world).run()
        self.assertEqual(self.world.starts, starts)
        self.assertTrue(self.world.active)

    def test_existing_er_plugin_requires_review_before_uploads_or_service_changes(self):
        self.world.original["arguments"].extend(["--er-geyser-plugin-config", "/retained/er-events.json"])
        self.world.current = copy.deepcopy(self.world.original)
        with self.assertRaisesRegex(ValueError, "explicit deployment review"):
            FakeHost(self.world).run()
        self.assertEqual(self.world.uploads, [])
        self.assertEqual(self.world.starts, 0)
        self.assertEqual(self.world.key_generations, 0)
        self.assertTrue(self.world.active)

    def test_established_plan_loss_does_not_generate_keys_or_restart(self):
        FakeHost(self.world).run()
        starts, keys = self.world.starts, self.world.key_generations
        (self.world.args.state_root / self.world.args.revision / "control/plan.json").unlink()
        with self.assertRaises(OSError):
            FakeHost(self.world).run()
        self.assertEqual((self.world.starts, self.world.key_generations), (starts, keys))
        self.assertTrue(self.world.active)

    def test_included_upload_reconciles_without_reupload(self):
        self.world.upload_failure = "included"
        with self.assertRaisesRegex(RuntimeError, "lost upload"):
            FakeHost(self.world).run()
        self.world.upload_failure = None
        FakeHost(self.world).run()
        self.assertEqual(self.world.uploads, ["portal", "token_bridge"])
        self.assertEqual(self.world.key_generations, 4)

    def test_unobserved_upload_remains_a_barrier(self):
        self.world.upload_failure = "not-observed"
        with self.assertRaisesRegex(RuntimeError, "interrupted"):
            FakeHost(self.world).run()
        self.world.upload_failure = None
        with self.assertRaisesRegex(ValueError, "unresolved"):
            FakeHost(self.world).run()
        self.assertEqual(self.world.uploads, ["portal"])
        self.assertEqual(self.world.starts, 0)
        self.assertEqual(self.world.current, self.world.original)


if __name__ == "__main__":
    unittest.main()

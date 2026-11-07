#!/usr/bin/env python3
"""Sonic: Diagnostics observe lifecycle/metadata without mutating or publishing private input."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

import native_runtime_diagnostics as diagnostics
from continuity_devnet import public_text
from continuity_rollout_state import atomic_json, file_hash, retained_fingerprint, service_dropin

SECRET = "PRIVATE_DIAGNOSTIC_SENTINEL"


class DiagnosticTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.state_root = self.root / "state"
        self.base = self.state_root / ("a" * 40)
        self.control = self.base / "control"
        self.control.mkdir(parents=True, mode=0o700)
        self.runtime = self.base / "runtime"
        (self.runtime / "bin").mkdir(parents=True)
        for role in ("checkpoints", "unsettled"):
            (self.runtime / role).mkdir(mode=0o700)
            (self.runtime / role / "retained.bin").write_bytes(SECRET.encode())
        binary = self.runtime / "bin/agave-validator"
        binary.write_bytes(b"\x7fELFretained-validator")
        binary.chmod(0o500)
        programs = {role: public_text(bytes([index]) * 32) for index, role in enumerate(("portal", "token_bridge"), 1)}
        arguments = [str(binary), "--portal", programs["portal"], "--private-option", SECRET]
        original = {"uid": os.getuid(), "gid": os.getgid(), "arguments": arguments}
        artifacts = {"validator": file_hash(binary), "cli": "b" * 64, "portal": "c" * 64, "token_bridge": "d" * 64}
        profile = diagnostics.digest({"version": 2, "source": self.base.name, "artifacts": artifacts,
                                      "sdk": diagnostics.SDK_REVISION, "proof_acceptance_qualified": False})
        self.plan = {"version": 2, "source_revision": self.base.name, "sdk_revision": diagnostics.SDK_REVISION,
                     "genesis": diagnostics.DEVNET, "programs": programs, "validator": public_text(bytes([7]) * 32),
                     "profile": profile, "artifacts": artifacts, "original_sha256": diagnostics.digest(original), "private": SECRET}
        reservation = {"version": 2, "source_revision": self.base.name, "original": original, "plan_sha256": diagnostics.digest(self.plan)}
        environment = {"NORTHSTAR_CHECKPOINT_PLAN_DIR": str(self.runtime / "checkpoints"),
                       "NORTHSTAR_UNSETTLED_STATE_DIR": str(self.runtime / "unsettled")}
        activation = {"version": 2, "arguments": arguments, "state_environment": environment, "artifacts": artifacts}
        text = service_dropin(arguments, *environment.values())
        self.dropin = self.root / "managed.conf"
        self.dropin.write_text(text)
        atomic_json(self.control / "plan.json", self.plan)
        atomic_json(self.control / "reservation.json", reservation)
        atomic_json(self.control / "activation.json", activation)
        atomic_json(self.control / "activation-installed.json", {"sha256": hashlib.sha256(text.encode()).hexdigest()})
        self.lock = self.root / "deploy.lock"
        self.lock.write_bytes(b"")
        self.lock.chmod(0o600)
        self.start = "{ path=/usr/bin/env; argv[]=" + text.split("ExecStart=")[-1].strip() + " ; }"
        self.commands = []
        self.messages = ["thread 'main' panicked at private/path.rs:1:1: permission denied " + SECRET,
                         "Program log: panicked at " + SECRET, "ordinary private data " + SECRET]

    def run_command(self, arguments):
        self.commands.append(arguments)
        if arguments[0] == "systemctl":
            body = "LoadState=loaded\nActiveState=inactive\nSubState=dead\nMainPID=0\nResult=success\nExecMainStatus=0\nExecStart=" + self.start + "\n"
        elif arguments[0] == "journalctl":
            body = "\n".join(json.dumps({"MESSAGE": value}) for value in self.messages)
        else:
            raise AssertionError("unexpected diagnostic operation")
        return SimpleNamespace(returncode=0, stdout=body.encode(), stderr=SECRET.encode())

    def collect(self):
        return diagnostics.collect_report(self.run_command, self.state_root, self.dropin, self.lock)

    def test_stopped_service_and_known_binding_are_reported_without_private_content(self):
        result = self.collect()
        self.assertTrue(result["read_only"])
        self.assertEqual(result["systemd"]["ActiveState"], "inactive")
        self.assertEqual(result["status"], "snapshot_collected")
        binding = result["managed_records"]
        self.assertTrue(binding["recorded_plan_identity_matches"])
        self.assertTrue(binding["original_dropin_record_matches"])
        self.assertTrue(binding["managed_dropin_matches_original"])
        self.assertTrue(binding["original_binary_matches"])
        self.assertFalse(binding["onchain_state_checked"])
        self.assertEqual(result["journal"]["marker_counts"], {"runtime_panic": 1, "permission_denied": 1})
        encoded = json.dumps(result)
        for private in (SECRET, "ExecStart", "private/path.rs", str(self.root)):
            self.assertNotIn(private, encoded)

    def test_every_retained_byte_and_mode_is_unchanged(self):
        before = retained_fingerprint([self.root])
        self.collect()
        self.assertEqual(before, retained_fingerprint([self.root]))
        self.assertEqual({command[0] for command in self.commands}, {"systemctl", "journalctl"})
        for command in self.commands:
            self.assertFalse(set(command) & {"start", "stop", "restart", "enable", "disable", "reset-failed", "daemon-reload"})

    def test_missing_lock_is_not_created(self):
        self.lock.unlink()
        result = self.collect()
        self.assertEqual(result["status"], "deployment_lock_missing")
        self.assertNotIn("managed_records", result)
        self.assertFalse(self.lock.exists())

    def test_active_deployment_blocks_private_metadata_inspection(self):
        descriptor = os.open(self.lock, os.O_RDWR)
        self.addCleanup(os.close, descriptor)
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        result = self.collect()
        self.assertEqual(result["status"], "deployment_busy")
        self.assertNotIn("managed_records", result)

    def test_private_control_alias_is_reported_without_following_it(self):
        plan_path = self.control / "plan.json"
        original = self.control / "original.json"
        plan_path.rename(original)
        plan_path.symlink_to(original)
        result = self.collect()
        self.assertEqual(result["managed_records"]["status"], "invalid_or_unreadable_record")
        self.assertNotIn(SECRET, json.dumps(result))

    def test_changed_plan_cannot_publish_a_trusted_identity(self):
        atomic_json(self.control / "plan.json", dict(self.plan, profile="0" * 64))
        result = self.collect()["managed_records"]
        self.assertFalse(result["recorded_plan_identity_matches"])
        self.assertNotIn("profile", result)
        self.assertNotIn("portal", result)

    def test_unknown_effective_binding_does_not_claim_a_running_binding(self):
        self.start = "{ path=/unknown/program; argv[]=" + SECRET + " ; }"
        result = self.collect()["managed_records"]
        self.assertEqual(result["status"], "inspected")
        self.assertFalse(result["effective_start_mentions_original_binary"])
        self.assertNotIn(SECRET, json.dumps(result))

    def test_changed_dropin_is_observed_but_never_repaired(self):
        self.dropin.write_text(SECRET)
        result = self.collect()["managed_records"]
        self.assertFalse(result["managed_dropin_matches_original"])
        self.assertEqual(self.dropin.read_text(), SECRET)

    def test_command_errors_never_publish_stderr(self):
        def failed(_arguments):
            return SimpleNamespace(returncode=1, stdout=b"", stderr=SECRET.encode())
        result = diagnostics.collect_report(failed, self.state_root, self.dropin, self.lock)
        self.assertEqual(result["status"], "service_query_unavailable")
        self.assertNotIn(SECRET, json.dumps(result))

    def test_missing_unit_still_reports_known_retained_records(self):
        self.start = ""
        def absent_unit(arguments):
            result = self.run_command(arguments)
            if arguments[0] == "systemctl":
                result.returncode = 1
                result.stdout = result.stdout.replace(b"LoadState=loaded", b"LoadState=not-found")
            return result
        result = diagnostics.collect_report(absent_unit, self.state_root, self.dropin, self.lock)
        self.assertEqual(result["systemd"]["LoadState"], "not-found")
        self.assertEqual(result["systemd_query_exit_code"], 1)
        self.assertTrue(result["managed_records"]["recorded_plan_identity_matches"])
        self.assertFalse(result["managed_records"]["effective_start_mentions_original_binary"])

    def test_unknown_upgrade_fields_never_publish_private_values(self):
        revision = "b" * 40
        control = self.state_root / "upgrades" / revision / "control"
        control.mkdir(parents=True, mode=0o700)
        plan = {"version": SECRET, "kind": "native-upgrade", "source_revision": revision,
                "base_revision": self.base.name, "base_plan_sha256": diagnostics.digest(self.plan)}
        atomic_json(control / "plan.json", plan)
        atomic_json(control / "progress.json", {"version": SECRET, "phase": SECRET, "plan_sha256": diagnostics.digest(plan)})
        self.start += " " + str(self.state_root / "upgrades" / revision / "runtime/bin/agave-validator")
        result = self.collect()
        self.assertEqual(result["managed_records"]["upgrade_record"]["version"], "unknown")
        self.assertFalse(result["managed_records"]["upgrade_record"]["identity_matches"])
        self.assertFalse(result["managed_records"]["upgrade_record"]["progress_matches"])
        self.assertNotIn(SECRET, json.dumps(result))

    def test_journal_timeout_does_not_hide_service_state(self):
        def timed_out(arguments):
            if arguments[0] == "journalctl":
                raise diagnostics.subprocess.TimeoutExpired(arguments, 30, stderr=SECRET.encode())
            return self.run_command(arguments)
        result = diagnostics.collect_report(timed_out, self.state_root, self.dropin, self.lock)
        self.assertEqual(result["systemd"]["ActiveState"], "inactive")
        self.assertEqual(result["journal"]["status"], "unavailable")
        self.assertNotIn(SECRET, json.dumps(result))

    def test_journal_output_is_bounded(self):
        result = diagnostics.journal_summary(lambda _args: SimpleNamespace(returncode=0, stdout=b"a" * (2 * 1024 * 1024 + 1)))
        self.assertEqual(result, {"status": "unavailable"})


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Sonic: Diagnostics observe lifecycle/metadata without mutating or publishing private input."""
import fcntl
import gzip
import hashlib
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

import native_runtime_diagnostics as diagnostics
from continuity_devnet import public_text
from continuity_rollout_state import atomic_json, file_hash, private_json, retained_fingerprint, service_dropin

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
        self.ledger = self.root / "ledger"
        self.ledger.mkdir()
        self.logfile = self.root / "validator.log"
        self.logfile.write_text("ordinary private log data " + SECRET)
        arguments = [str(binary), "--portal", programs["portal"], "--ledger", str(self.ledger),
                     "--log", str(self.logfile), "--private-option", SECRET]
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
        self.unit_text = ""
        self.messages = ["thread 'main' panicked at private/path.rs:1:1: permission denied " + SECRET,
                         "Program log: panicked at " + SECRET, "ordinary private data " + SECRET]

    def run_command(self, arguments):
        self.commands.append(arguments)
        if arguments[0] == "systemctl" and arguments[1] == "cat":
            body = self.unit_text
        elif arguments[0] == "systemctl":
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

    def test_configured_file_log_is_bounded_and_never_exposes_content_or_paths(self):
        self.logfile.write_bytes(b"x" * 80_000 + b"\nERROR No space left on device " + SECRET.encode()
                                 + b"\nProgram log: permission denied " + SECRET.encode())
        before = retained_fingerprint([self.root])
        result = self.collect()
        log = result["operational"]["file_logs"][0]
        self.assertEqual(log["source"], "validator_flag")
        self.assertEqual(log["status"], "classified")
        self.assertLessEqual(log["bytes_examined"], 64 * 1024)
        self.assertEqual(log["marker_counts"], {"disk_full": 1})
        self.assertEqual(result["operational"]["filesystems"]["ledger"]["status"], "observed")
        self.assertNotIn(SECRET, json.dumps(result))
        self.assertNotIn(str(self.logfile), json.dumps(result))
        self.assertEqual(before, retained_fingerprint([self.root]))

    def test_private_directories_are_not_accepted_as_log_sinks(self):
        log = self.control / "private.log"
        log.write_text(SECRET)
        result = diagnostics.file_log_summary(log)
        self.assertEqual(result["status"], "not_an_approved_log_sink")
        self.assertNotIn(SECRET, json.dumps(result))

    def test_rotated_sibling_log_is_read_but_cross_directory_alias_is_rejected(self):
        self.logfile.unlink()
        rotated = self.root / "validator.log.1"
        rotated.write_text("No space left on device " + SECRET)
        self.logfile.symlink_to(rotated.name)
        result = diagnostics.file_log_summary(self.logfile)
        self.assertTrue(result["rotated_sibling"])
        self.assertEqual(result["marker_counts"], {"disk_full": 1})
        self.logfile.unlink()
        self.logfile.symlink_to(self.control / "plan.json")
        result = diagnostics.file_log_summary(self.logfile)
        self.assertEqual(result["status"], "unapproved_log_alias")
        self.assertNotIn(SECRET, json.dumps(result))

    def test_validator_default_log_uses_the_validated_identity_and_working_directory(self):
        activation_path = self.control / "activation.json"
        activation = private_json(activation_path)
        position = activation["arguments"].index("--log")
        del activation["arguments"][position:position + 2]
        atomic_json(activation_path, activation)
        default = self.root / ("agave-validator-" + self.plan["validator"] + ".log")
        default.write_text("bank hash mismatch " + SECRET)
        records = {"recorded_plan_identity_matches": True, "managed_dropin_matches_original": True,
                   "original_binary_matches": True, "original_revisions": [self.base.name], "validator": self.plan["validator"]}
        result = diagnostics.operational_details({"WorkingDirectory": str(self.root)}, self.state_root, records)
        self.assertEqual(result["file_logs"][0]["source"], "validator_default")
        self.assertEqual(result["file_logs"][0]["marker_counts"], {"bank_hash_mismatch": 1})
        self.assertNotIn(SECRET, json.dumps(result))
        self.assertNotIn(str(default), json.dumps(result))

    def test_short_validator_log_flag_is_discovered_without_a_working_directory(self):
        activation_path = self.control / "activation.json"
        activation = private_json(activation_path)
        position = activation["arguments"].index("--log")
        activation["arguments"][position] = "-o"
        atomic_json(activation_path, activation)
        self.logfile.write_text("too many open files " + SECRET)
        records = {"recorded_plan_identity_matches": True, "managed_dropin_matches_original": True,
                   "original_binary_matches": True, "original_revisions": [self.base.name], "validator": self.plan["validator"]}
        result = diagnostics.operational_details({}, self.state_root, records)
        self.assertEqual(len(result["file_logs"]), 1)
        self.assertEqual(result["file_logs"][0]["marker_counts"], {"file_descriptor_limit": 1})
        self.assertNotIn(SECRET, json.dumps(result))

    def test_unknown_managed_binding_cannot_read_file_logs(self):
        atomic_json(self.control / "plan.json", dict(self.plan, profile="0" * 64))
        result = self.collect()
        self.assertEqual(result["operational"]["file_logs"], [])

    def test_nonregular_log_never_blocks_or_reads_a_stream(self):
        self.logfile.unlink()
        os.mkfifo(self.logfile, 0o600)
        result = diagnostics.file_log_summary(self.logfile)
        self.assertEqual(result["status"], "not_a_regular_log")

    def test_systemd_file_sink_is_not_copied_into_public_properties(self):
        secondary = self.root / "stderr.err"
        secondary.write_text("too many open files " + SECRET)
        def with_sink(arguments):
            result = self.run_command(arguments)
            if arguments[0] == "systemctl":
                result.stdout += ("StandardError=append\nStandardErrorFile=" + str(secondary) + "\n").encode()
            return result
        result = diagnostics.collect_report(with_sink, self.state_root, self.dropin, self.lock)
        self.assertEqual(result["operational"]["file_logs"][-1]["marker_counts"], {"file_descriptor_limit": 1})
        for private in (SECRET, str(secondary), "StandardErrorFile"):
            self.assertNotIn(private, json.dumps(result))

    def test_namespaced_journal_routes_privately_and_reports_no_namespace_value(self):
        result = diagnostics.journal_summary(self.run_command, SECRET)
        self.assertTrue(result["namespaced"])
        self.assertIn("--namespace=" + SECRET, self.commands[-1])
        self.assertNotIn(SECRET, json.dumps(result))

    def test_systemd_append_path_is_resolved_from_unit_files_without_public_content(self):
        target = self.root / "validator-output"
        target.write_text("No space left on device " + SECRET)
        self.unit_text = "[Service]\nEnvironment=TOKEN=" + SECRET + "\nStandardOutput=append:" + str(target) + "\n"
        def append_sink(arguments):
            result = self.run_command(arguments)
            if arguments[0] == "systemctl" and arguments[1] == "show":
                result.stdout += b"StandardOutput=append\n"
            return result
        result = diagnostics.collect_report(append_sink, self.state_root, self.dropin, self.lock)
        files = result["operational"]["file_logs"]
        self.assertTrue(any(item["source"] == "StandardOutput" and item.get("marker_counts") == {"disk_full": 1} for item in files))
        for private in (SECRET, str(target), "Environment"):
            self.assertNotIn(private, json.dumps(result))

    def test_unit_sink_resets_and_unloaded_modes_are_not_used(self):
        self.unit_text = "[Service]\nStandardOutput=append:/private/validator.log\n[Service]\nStandardOutput=null\n"
        self.assertEqual(diagnostics.unit_log_sinks(self.run_command, {"StandardOutput": "append"}), {})
        self.assertEqual(diagnostics.unit_log_sinks(self.run_command, {"StandardOutput": "null"}), {"StandardOutput": "null"})

    def test_configured_credential_and_database_paths_are_not_log_sinks(self):
        for path in (self.root / "identity.json", self.root / "id_ed25519", self.root / "000123.log", Path("/proc/self/environ")):
            self.assertFalse(diagnostics.approved_log_path(path))

    def test_empty_current_log_reads_a_bounded_previous_rotation(self):
        self.logfile.write_text("")
        previous = self.root / (self.logfile.name + ".1")
        previous.write_text("duplicate running instances of the same validator node: " + SECRET)
        result = self.collect()
        rotations = [item for item in result["operational"]["file_logs"] if item["source"].endswith("_rotation")]
        self.assertEqual(len(rotations), 1)
        self.assertEqual(rotations[0]["marker_counts"], {"duplicate_validator_instance": 1})
        self.assertNotIn(SECRET, json.dumps(result))
        self.assertNotIn(str(previous), json.dumps(result))

    def test_compressed_rotation_is_classified_without_exposing_messages(self):
        previous = self.root / (self.logfile.name + ".1.gz")
        previous.write_bytes(gzip.compress(("permission denied " + SECRET).encode()))
        result = diagnostics.gzip_log_summary(previous)
        self.assertEqual(result["status"], "classified")
        self.assertEqual(result["marker_counts"], {"permission_denied": 1})
        self.assertNotIn(SECRET, json.dumps(result))

    def test_compressed_rotation_decode_limit_is_explicit(self):
        previous = self.root / (self.logfile.name + ".1.gz")
        previous.write_bytes(gzip.compress(b"x" * 1000))
        result = diagnostics.gzip_log_summary(previous, max_decoded_bytes=32)
        self.assertEqual(result["status"], "rotation_over_limit")
        self.assertNotIn("marker_counts", result)

    def test_archive_between_old_and_approved_compressed_budget_is_read(self):
        previous = self.root / (self.logfile.name + ".1.gz")
        previous.write_bytes(gzip.compress(b"permission denied"))
        actual = previous.stat()
        larger = SimpleNamespace(st_mode=actual.st_mode, st_mtime=actual.st_mtime, st_size=128 * 1024 * 1024)
        with mock.patch.object(diagnostics.os, "fstat", return_value=larger):
            result = diagnostics.gzip_log_summary(previous)
        self.assertEqual(result["status"], "classified")
        self.assertEqual(result["compressed_bytes"], larger.st_size)
        self.assertEqual(result["marker_counts"], {"permission_denied": 1})

    def test_archive_limit_reports_which_budget_was_reached(self):
        previous = self.root / (self.logfile.name + ".1.gz")
        previous.write_bytes(gzip.compress(b"x" * 1000))
        result = diagnostics.gzip_log_summary(previous, max_decoded_bytes=32)
        self.assertEqual(result["status"], "rotation_over_limit")
        self.assertEqual(result["limit_kind"], "decoded_bytes")
        self.assertEqual(result["limits"]["decoded_bytes"], 32)
        self.assertNotIn("marker_counts", result)

    def test_compressed_and_time_limits_are_reported_without_partial_markers(self):
        previous = self.root / (self.logfile.name + ".1.gz")
        previous.write_bytes(gzip.compress(b"permission denied"))
        for arguments, kind in (({"max_compressed_bytes": 1}, "compressed_bytes"), ({"max_seconds": -1}, "elapsed_seconds")):
            result = diagnostics.gzip_log_summary(previous, **arguments)
            self.assertEqual(result["status"], "rotation_over_limit")
            self.assertEqual(result["limit_kind"], kind)
            self.assertEqual(result["compressed_bytes"], previous.stat().st_size)
            self.assertNotIn("marker_counts", result)

    def test_rotation_discovery_is_bounded_and_excludes_aliases_and_private_files(self):
        for number in range(1, 5):
            target = self.root / (self.logfile.name + "." + str(number))
            target.write_text("ordinary")
            os.utime(target, (number, number))
        alias = self.root / (self.logfile.name + ".5")
        alias.symlink_to(self.control / "plan.json")
        (self.root / (self.logfile.name + ".6.json")).write_text(SECRET)
        found = diagnostics.previous_log_rotations(self.logfile)
        self.assertEqual(len(found), 2)
        self.assertNotIn(alias, found)
        self.assertTrue(all(path.suffix != ".json" for path in found))

    def test_journal_output_is_bounded(self):
        result = diagnostics.journal_summary(lambda _args: SimpleNamespace(returncode=0, stdout=b"a" * (2 * 1024 * 1024 + 1)))
        self.assertEqual(result, {"status": "unavailable"})


if __name__ == "__main__":
    unittest.main()

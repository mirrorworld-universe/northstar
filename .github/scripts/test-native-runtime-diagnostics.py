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
from continuity_rollout_state import atomic_json, file_hash, geyser_events_config, private_json, retained_fingerprint, service_dropin

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
        reservation = {"version": 2, "source_revision": self.base.name, "sdk_revision": diagnostics.SDK_REVISION,
                       "original": original, "plan_sha256": diagnostics.digest(self.plan)}
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

    def install_upgrade(self, stderr=False, event_source=None, retained_l1_revision=None):
        revision = "b" * 40
        namespace = self.state_root / "upgrades" / revision
        control, runtime = namespace / "control", namespace / "runtime"
        control.mkdir(parents=True, mode=0o700)
        (runtime / "bin").mkdir(parents=True)
        (runtime / "lib").mkdir()
        binary = runtime / "bin/agave-validator"
        binary.write_bytes(b"\x7fELFupgraded-validator")
        binary.chmod(0o500)
        plugin = runtime / diagnostics.ARTIFACTS["geyser_events"]
        plugin.write_bytes(b"\x7fELFupgraded-plugin")
        plugin.chmod(0o500)
        configuration = runtime / "geyser-events.json"
        atomic_json(configuration, geyser_events_config(plugin, event_source or "er"))
        configuration.chmod(0o400)
        activation = private_json(self.control / "activation.json")
        flag = "--geyser-plugin-config" if event_source == "l1" else "--er-geyser-plugin-config"
        arguments = [*activation["arguments"], flag, str(configuration)]
        if event_source == "l1":
            arguments.extend(["--geyser-plugin-config", str(self.root / "other-l1-plugin.json")])
        arguments[0] = str(binary)
        if stderr:
            arguments[arguments.index("--log") + 1] = "-"
        ledger_runtime = self.state_root / "upgrades" / retained_l1_revision / "runtime" if retained_l1_revision else runtime
        ledger = ledger_runtime / "l1/ledger"
        ledger.mkdir(parents=True)
        arguments[arguments.index("--ledger") + 1] = str(ledger)
        desired = {"arguments": arguments, "binary_sha256": file_hash(binary),
                   "state_environment": activation["state_environment"], "uid": os.getuid(), "gid": os.getgid()}
        plan = {"version": 1, "kind": "native-upgrade", "source_revision": revision,
                "base_revision": self.base.name, "base_plan_sha256": diagnostics.digest(self.plan),
                "desired": desired, "before": {**desired, "arguments": activation["arguments"]},
                "artifacts": {**self.plan["artifacts"], "validator": file_hash(binary), "geyser_events": file_hash(plugin)},
                "l1_floor": 1, "recovery_failed_slot": 519264000, "private": SECRET}
        if event_source is not None:
            plan["event_source"] = event_source
        text = service_dropin(arguments, *activation["state_environment"].values())
        self.dropin.write_text(text)
        self.dropin.chmod(0o600)
        self.start = "{ path=/usr/bin/env; argv[]=" + text.split("ExecStart=")[-1].strip() + " ; }"
        atomic_json(control / "plan.json", plan)
        atomic_json(control / "progress.json", {"version": 1, "plan_sha256": diagnostics.digest(plan), "phase": "start-attempted"})
        atomic_json(control / "frozen.json", {"plan_sha256": diagnostics.digest(plan)})
        atomic_json(control / "installed.json", {"sha256": hashlib.sha256(text.encode()).hexdigest()})
        atomic_json(control / "start-attempted.json", {"plan_sha256": diagnostics.digest(plan), "l1_floor": 2})
        return control

    def test_l1_runtime_binding_preserves_other_standard_geyser_plugins(self):
        self.install_upgrade(event_source="l1")
        before = retained_fingerprint([self.root])
        result = self.collect()
        self.assertTrue(result["managed_records"]["upgrade_record"]["runtime_binding_matches"])
        self.assertEqual(before, retained_fingerprint([self.root]))
        self.assertNotIn(SECRET, json.dumps(result))

    def test_active_upgrade_binding_ignores_retained_l1_namespace(self):
        self.install_upgrade(event_source="l1", retained_l1_revision="c" * 40)
        before = retained_fingerprint([self.root])
        result = self.collect()
        records = result["managed_records"]
        self.assertEqual(records["status"], "inspected")
        self.assertEqual(records["upgrade_revisions"], ["b" * 40])
        self.assertTrue(records["upgrade_record"]["runtime_binding_matches"])
        self.assertEqual(before, retained_fingerprint([self.root]))
        self.assertNotIn(SECRET, json.dumps(result))

    def test_l1_binding_rejects_an_er_plugin_configuration(self):
        control = self.install_upgrade(event_source="l1")
        configuration = control.parent / "runtime/geyser-events.json"
        value = private_json(configuration)
        value["name"] = "er-events"
        atomic_json(configuration, value)
        configuration.chmod(0o400)
        result = self.collect()
        self.assertFalse(result["managed_records"]["upgrade_record"]["runtime_binding_matches"])
        self.assertEqual(result["operational"]["file_logs"], [])

    def test_unqualified_upgrade_logs_require_bound_service_and_start_barriers(self):
        self.install_upgrade()
        self.logfile.write_text("snapshot hash mismatch " + SECRET)
        before = retained_fingerprint([self.root])
        result = self.collect()
        self.assertTrue(result["operational"]["file_logs"], "bound upgrade logs must remain available before qualification")
        self.assertTrue(result["managed_records"]["upgrade_record"]["runtime_binding_matches"])
        self.assertEqual(result["operational"]["file_logs"][0]["marker_counts"], {"snapshot_hash_mismatch": 1})
        self.assertEqual(result["managed_records"]["upgrade_record"]["phase"], "start-attempted")
        self.assertFalse(result["managed_records"]["onchain_state_checked"])
        for private in (SECRET, str(self.root), "arguments", "state_environment"):
            self.assertNotIn(private, json.dumps(result))
        self.assertEqual(before, retained_fingerprint([self.root]))

    def test_upgrade_systemd_append_sink_is_resolved_without_private_unit_content(self):
        self.install_upgrade(stderr=True)
        target = self.root / "validator-output"
        target.write_text("No snapshots available " + SECRET)
        self.unit_text = "[Service]\nEnvironment=TOKEN=" + SECRET + "\nStandardOutput=append:" + str(target) + "\n"
        def append_sink(arguments):
            result = self.run_command(arguments)
            if arguments[0] == "systemctl" and arguments[1] == "show":
                result.stdout += b"StandardOutput=append\n"
            return result
        result = diagnostics.collect_report(append_sink, self.state_root, self.dropin, self.lock)
        self.assertEqual(result["operational"]["file_logs"][0]["marker_counts"], {"snapshot_unavailable": 1})
        self.assertNotIn(SECRET, json.dumps(result))
        self.assertNotIn(str(target), json.dumps(result))

    def test_upgrade_binding_rejects_changed_frozen_record(self):
        control = self.install_upgrade()
        atomic_json(control / "frozen.json", {"plan_sha256": "0" * 64})
        self.assertEqual(self.collect()["operational"]["file_logs"], [])

    def test_bound_recovery_reports_only_directory_metadata_and_retry_barrier_presence(self):
        self.install_upgrade()
        before = retained_fingerprint([self.root])
        result = self.collect()
        upgrade = result["managed_records"]["upgrade_record"]
        self.assertIn("l1_directories", upgrade)
        self.assertEqual(upgrade["l1_directories"]["ledger"]["mode"], "0755")
        self.assertTrue(upgrade["l1_directories"]["ledger"]["owner_matches_record"])
        self.assertTrue(upgrade["l1_directories"]["tower"]["absent"])
        self.assertEqual(upgrade["retry_records"], {"authorization": False, "start_attempted": False,
            "outcome": False, "failure": False})
        self.assertEqual(before, retained_fingerprint([self.root]))
        for private in (SECRET, str(self.root), "retained.bin", "snapshot-cache.bin"):
            self.assertNotIn(private, json.dumps(result))

    def test_upgrade_binding_rejects_missing_barrier_or_changed_effective_arguments(self):
        control = self.install_upgrade()
        self.logfile.write_text("snapshot hash mismatch " + SECRET)
        original = self.start
        self.start = self.start.replace("--portal", "--different-portal")
        self.assertEqual(self.collect()["operational"]["file_logs"], [])
        self.start = original
        (control / "start-attempted.json").unlink()
        self.assertEqual(self.collect()["operational"]["file_logs"], [])

    def test_upgrade_binding_rejects_changed_native_artifact(self):
        control = self.install_upgrade()
        plan = private_json(control / "plan.json")
        binary = Path(plan["desired"]["arguments"][0])
        binary.unlink()
        binary.write_bytes(b"different")
        binary.chmod(0o500)
        self.assertEqual(self.collect()["operational"]["file_logs"], [])

    def test_bootstrap_and_resume_markers_are_fixed_and_exclude_program_logs(self):
        evidence = diagnostics.tail_evidence(("Searching for an RPC service with shred version 1 " + SECRET
            + "\nNo snapshots available " + SECRET
            + "\nFailed to download a snapshot archive for slot 42 from " + SECRET
            + "\nLoading bank from full snapshot archive: " + SECRET
            + "\nCannot resume continuity session: " + SECRET
            + "\nProgram log: No snapshots available").encode())
        self.assertEqual(evidence["marker_counts"], {"rpc_peer_search": 1, "snapshot_unavailable": 1,
            "snapshot_download_failed": 1, "snapshot_load_started": 1, "continuity_resume_failed": 1})
        self.assertNotIn(SECRET, json.dumps(evidence))

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

    def test_recorded_sdk_revision_not_current_tooling_binds_historical_profile(self):
        plan = dict(self.plan, sdk_revision="c" * 40)
        plan["profile"] = diagnostics.digest({"version": 2, "source": self.base.name, "artifacts": plan["artifacts"],
                                              "sdk": plan["sdk_revision"], "proof_acceptance_qualified": False})
        reservation = private_json(self.control / "reservation.json")
        reservation.update(sdk_revision=plan["sdk_revision"], plan_sha256=diagnostics.digest(plan))
        atomic_json(self.control / "plan.json", plan)
        atomic_json(self.control / "reservation.json", reservation)
        before = retained_fingerprint([self.root])
        result = self.collect()["managed_records"]
        self.assertTrue(result["recorded_plan_identity_matches"])
        self.assertEqual(result["profile"], plan["profile"])
        self.assertEqual(retained_fingerprint([self.root]), before)

    def test_mismatched_recorded_sdk_revisions_cannot_publish_identity(self):
        reservation = private_json(self.control / "reservation.json")
        reservation["sdk_revision"] = "c" * 40
        atomic_json(self.control / "reservation.json", reservation)
        result = self.collect()["managed_records"]
        self.assertFalse(result["recorded_plan_identity_matches"])
        self.assertNotIn("profile", result)

    def test_verified_service_reports_configured_rpc_indexes_without_private_arguments(self):
        before = retained_fingerprint([self.root])
        result = self.collect()
        self.assertEqual(result["operational"]["rpc_account_indexes"],
                         {"status": "observed", "configured": [], "key_filter": "unrestricted"})
        self.assertNotIn(SECRET, json.dumps(result))
        self.assertEqual(retained_fingerprint([self.root]), before)

    def test_rpc_index_reporting_accepts_cli_forms_but_never_discloses_keys_or_unknown_values(self):
        configured = ["validator", "--account-index", "program-id", "spl-token-owner",
                      "--account-index=spl-token-mint", "--private-option", SECRET]
        self.assertEqual(diagnostics.rpc_index_configuration(configured), {"status": "observed",
                         "configured": ["program-id", "spl-token-mint", "spl-token-owner"], "key_filter": "unrestricted"})
        filtered = configured + ["--account-index-include-key", public_text(bytes([13]) * 32)]
        report = diagnostics.rpc_index_configuration(filtered)
        self.assertEqual(report["key_filter"], "include-only")
        self.assertNotIn(public_text(bytes([13]) * 32), json.dumps(report))
        invalid = diagnostics.rpc_index_configuration(["validator", "--account-index", SECRET])
        self.assertEqual(invalid, {"status": "ambiguous"})
        self.assertNotIn(SECRET, json.dumps(invalid))
        unbound = diagnostics.operational_details({}, self.state_root, {}, [])
        self.assertNotIn("rpc_account_indexes", unbound)

    def install_pending_upload(self):
        revision = "c" * 40
        control = self.state_root / revision / "control"
        control.mkdir(parents=True, mode=0o700)
        old = private_json(self.control / "reservation.json")["original"]
        activation = private_json(self.control / "activation.json")
        service = {"arguments": activation["arguments"], "state_environment": activation["state_environment"],
                   "binary_sha256": self.plan["artifacts"]["validator"], "uid": old["uid"], "gid": old["gid"]}
        replacement = {"version": 1, "service": service, "snapshot": {"identity": {"context": "ab" * 32}}}
        artifacts = self.plan["artifacts"] | {"geyser_events": "e" * 64}
        programs = {role: public_text(bytes([index]) * 32) for index, role in
                    enumerate(("portal", "token_bridge", "portal-buffer", "token_bridge-buffer"), 21)}
        plan = {"version": 2, "mode": "fresh-stack", "source_revision": revision, "sdk_revision": diagnostics.SDK_REVISION,
                "genesis": diagnostics.DEVNET, "artifacts": artifacts, "programs": programs,
                "original_sha256": diagnostics.digest(service), "replacement_sha256": diagnostics.digest(replacement)}
        plan["profile"] = diagnostics.digest({"version": 2, "source": revision, "artifacts": artifacts,
                                             "sdk": diagnostics.SDK_REVISION, "proof_acceptance_qualified": False})
        reservation = {"version": 2, "source_revision": revision, "sdk_revision": diagnostics.SDK_REVISION,
                       "plan_sha256": diagnostics.digest(plan), "original": service, "replacement": replacement,
                       "l1_rpc": "http://127.0.0.1:8899"}
        atomic_json(control / "plan.json", plan)
        atomic_json(control / "reservation.json", reservation)
        atomic_json(control / "fresh-progress.json", {"version": 1, "plan_sha256": diagnostics.digest(plan), "phase": "prepared"})
        atomic_json(control / "programs.json", {"version": 2, "roles": {"portal": {"elf_sha256": artifacts["portal"], "complete": None}}})
        (control / "operations.log").write_text("Error: 834 write transactions failed\nHTTP 429 Too Many Requests\n" + SECRET)
        (control / "operations.log").chmod(0o600)
        return revision, control

    def collect_pending(self, revision):
        def active_run(arguments):
            result = self.run_command(arguments)
            result.stdout = result.stdout.replace(b"ActiveState=inactive", b"ActiveState=active").replace(b"MainPID=0", b"MainPID=123")
            return result
        return diagnostics.collect_report(active_run, self.state_root, self.dropin, self.lock, pending_revision=revision)

    def test_pending_upload_evidence_is_bound_read_only_and_redacted(self):
        revision, control = self.install_pending_upload()
        before = retained_fingerprint([self.root])
        report = self.collect_pending(revision)
        pending = report["pending_fresh_upload"]
        self.assertEqual(pending["status"], "inspected")
        self.assertEqual(pending["phase"], "prepared")
        self.assertEqual(pending["uploads"], {"portal": "unresolved", "token_bridge": "not-recorded"})
        self.assertEqual(pending["operation_errors"]["write_transactions_failed"], [834])
        self.assertEqual(pending["operation_errors"]["markers"].get("http_rate_limit"), 1)
        self.assertFalse(pending["onchain_state_checked"])
        self.assertNotIn(SECRET, json.dumps(report))
        self.assertEqual(retained_fingerprint([self.root]), before)

    def test_pending_upload_classifies_the_native_uploader_terminal_retry_error(self):
        revision, control = self.install_pending_upload()
        (control / "operations.log").write_text("Error: Data writes to account failed: Max retries exceeded\n" + SECRET)
        pending = self.collect_pending(revision)["pending_fresh_upload"]
        self.assertEqual(pending["source_revision"], revision)
        self.assertEqual(pending["operation_errors"]["markers"], {"upload_write_failure": 1, "retry_budget_exhausted": 1})
        self.assertNotIn(SECRET, json.dumps(pending))

    def test_pending_upload_reports_absent_or_bound_reconciliation_attempts(self):
        revision, control = self.install_pending_upload()
        pending = self.collect_pending(revision)["pending_fresh_upload"]
        self.assertEqual(pending["upload_reconciliation"], {"status": "not-recorded", "attempt_barrier_present": False})
        plan = private_json(control / "plan.json")
        saved = private_json(control / "reservation.json")
        marker = {"version": 1, "kind": "retained-program-upload", "plan_sha256": diagnostics.digest(plan),
                  "controller_revision": "d" * 40, "source_revision": revision, "submission_rpc": saved["l1_rpc"],
                  "previous_service_sha256": diagnostics.digest(saved["replacement"]["service"]),
                  "pending_roles": ["portal"], "attempt_budget": 1, "phase": "attempted"}
        atomic_json(control / "program-upload-reconcile.json", marker)
        observed = self.collect_pending(revision)["pending_fresh_upload"]["upload_reconciliation"]
        self.assertEqual(observed["phase"], "attempted")
        self.assertTrue(observed["attempt_barrier_present"])
        marker["plan_sha256"] = "e" * 64
        atomic_json(control / "program-upload-reconcile.json", marker)
        self.assertEqual(self.collect_pending(revision)["pending_fresh_upload"]["upload_reconciliation"]["status"], "unavailable")

    def test_pending_upload_binds_additional_pass_to_the_unchanged_failed_record(self):
        revision, control = self.install_pending_upload()
        plan = private_json(control / "plan.json")
        saved = private_json(control / "reservation.json")
        primary = {"version": 1, "kind": "retained-program-upload", "plan_sha256": diagnostics.digest(plan),
                   "controller_revision": "d" * 40, "source_revision": revision, "submission_rpc": saved["l1_rpc"],
                   "previous_service_sha256": diagnostics.digest(saved["replacement"]["service"]),
                   "pending_roles": ["portal"], "attempt_budget": 1, "phase": "failed", "operator_reconciliation_required": True}
        atomic_json(control / "program-upload-reconcile.json", primary)
        retry = {key: value for key, value in primary.items() if key != "operator_reconciliation_required"}
        retry.update(version=2, phase="attempted", read_rpc=saved["l1_rpc"], submission_roles=["portal"],
                     previous_failed_record_sha256=diagnostics.digest(primary))
        atomic_json(control / "program-upload-reconcile-retry.json", retry)
        summary = self.collect_pending(revision)["pending_fresh_upload"]["upload_reconciliation"]
        self.assertEqual(summary["record_sha256"], diagnostics.digest(primary))
        self.assertEqual(summary["additional_pass"]["phase"], "attempted")
        self.assertEqual(summary["additional_pass"]["submission_roles"], ["portal"])
        retry["previous_failed_record_sha256"] = "f" * 64
        atomic_json(control / "program-upload-reconcile-retry.json", retry)
        self.assertEqual(self.collect_pending(revision)["pending_fresh_upload"]["upload_reconciliation"]["additional_pass"]["status"], "unavailable")

    def test_pending_upload_does_not_infer_unused_budget_from_an_orphan_additional_pass(self):
        revision, control = self.install_pending_upload()
        atomic_json(control / "program-upload-reconcile-retry.json", {"private": SECRET})
        summary = self.collect_pending(revision)["pending_fresh_upload"]["upload_reconciliation"]
        self.assertEqual(summary["status"], "unavailable")
        self.assertNotIn(SECRET, json.dumps(summary))

    def test_pending_upload_never_claims_absent_budget_when_a_report_remains(self):
        revision, control = self.install_pending_upload()
        atomic_json(control / "program-upload-reconcile-report.json", {"result": "failed", "private": SECRET})
        observed = self.collect_pending(revision)["pending_fresh_upload"]["upload_reconciliation"]
        self.assertEqual(observed["status"], "unavailable")
        self.assertNotIn(SECRET, json.dumps(observed))

    def test_pending_upload_logs_are_suppressed_for_changed_plan_or_any_start(self):
        revision, control = self.install_pending_upload()
        plan = private_json(control / "plan.json")
        changed = dict(plan, profile="f" * 64)
        atomic_json(control / "plan.json", changed)
        self.assertNotIn("operation_errors", self.collect_pending(revision)["pending_fresh_upload"])
        atomic_json(control / "plan.json", plan)
        (control / "fresh-starts").mkdir(mode=0o700)
        self.assertNotIn("operation_errors", self.collect_pending(revision)["pending_fresh_upload"])

    def test_pending_upload_refuses_changed_previous_service_even_with_valid_hashes(self):
        revision, control = self.install_pending_upload()
        reservation = private_json(control / "reservation.json")
        reservation["replacement"]["service"]["arguments"] = ["/unbound/validator", SECRET]
        plan = private_json(control / "plan.json")
        plan["replacement_sha256"] = diagnostics.digest(reservation["replacement"])
        reservation["plan_sha256"] = diagnostics.digest(plan)
        atomic_json(control / "plan.json", plan)
        atomic_json(control / "reservation.json", reservation)
        atomic_json(control / "fresh-progress.json", {"version": 1, "plan_sha256": diagnostics.digest(plan), "phase": "prepared"})
        pending = self.collect_pending(revision)["pending_fresh_upload"]
        self.assertNotIn("operation_errors", pending)
        self.assertNotIn(SECRET, json.dumps(pending))

    def test_pending_upload_requires_private_bounded_operation_log(self):
        revision, control = self.install_pending_upload()
        log = control / "operations.log"
        log.chmod(0o644)
        self.assertEqual(self.collect_pending(revision)["pending_fresh_upload"]["operation_errors"]["status"], "unavailable")
        log.chmod(0o600)
        with log.open("wb") as output:
            output.truncate(65 * 1024 * 1024)
        self.assertEqual(self.collect_pending(revision)["pending_fresh_upload"]["operation_errors"]["status"], "file_limit_exceeded")

    def test_pending_upload_refuses_symlinked_operation_log(self):
        revision, control = self.install_pending_upload()
        log = control / "operations.log"
        log.unlink()
        log.symlink_to(self.logfile)
        pending = self.collect_pending(revision)["pending_fresh_upload"]
        self.assertNotIn("write_transactions_failed", pending.get("operation_errors", {}))
        self.assertNotIn(SECRET, json.dumps(pending))

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

    def test_startup_stage_reports_only_fixed_names_timestamps_and_bounded_progress(self):
        text = ("[2026-11-08T23:23:10.000000Z INFO agave_snapshots::unarchive] Extracting " + SECRET
            + "\n[2026-11-08T23:28:00.000000Z INFO agave_snapshots::unarchive] Extracted " + SECRET
            + "\n[2026-11-08T23:28:01.000000Z INFO solana_runtime::serde_snapshot] Building accounts index..."
            + "\n[2026-11-08T23:42:59.000000Z INFO solana_accounts_db::accounts_db] generating index: processed 42/100 slots... " + SECRET
            + "\nProgram log: [2026-11-08T23:43:00Z INFO solana_rpc::rpc_service] rpc bound to " + SECRET)
        result = diagnostics.tail_evidence(text.encode())
        startup = result.get("startup", {})
        self.assertEqual(startup.get("last_stage"), "accounts_index_progress")
        self.assertEqual(startup["stage_counts"], {"snapshot_extract_started": 1, "snapshot_extract_completed": 1,
            "accounts_index_started": 1, "accounts_index_progress": 1})
        self.assertEqual(startup["last_stage_at"], "2026-11-08T23:42:59.000000Z")
        self.assertEqual(startup["index_progress"], {"processed": 42, "total": 100})
        self.assertNotIn(SECRET, json.dumps(result))
        self.assertNotIn("agave_snapshots::", json.dumps(result))

    def test_startup_stage_rejects_unknown_modules_and_invalid_progress(self):
        text = ("[2026-11-08T23:42:59Z INFO unknown_module] generating index: processed 42/100 slots... " + SECRET
            + "\n[2026-11-08T23:42:59Z INFO solana_accounts_db::accounts_db] generating index: processed 101/100 slots..."
            + "\n[2026-11-08T23:42:59Z INFO solana_accounts_db::accounts_db] generating index: processed 1/0 slots...")
        result = diagnostics.tail_evidence(text.encode())
        self.assertEqual(result.get("startup", {}).get("stage_counts", {}), {})
        self.assertNotIn("index_progress", result.get("startup", {}))
        self.assertNotIn(SECRET, json.dumps(result))

    def test_panic_evidence_includes_only_committed_source_and_fixed_reason(self):
        previous = self.root / (self.logfile.name + ".1.gz")
        text = ("thread 'solReplayStage' panicked at /" + SECRET + "/core/src/replay_stage.rs:2167:25:\n"
                + "We have tried to repair duplicate slot: 42 more than 10 times " + SECRET)
        previous.write_bytes(gzip.compress(text.encode()))
        result = diagnostics.gzip_log_summary(previous)
        self.assertEqual(result["panic_sites"], [{"file": "core/src/replay_stage.rs", "line": 2167}])
        self.assertEqual(result["marker_counts"]["duplicate_slot_repair_exhausted"], 1)
        self.assertNotIn(SECRET, json.dumps(result))
        self.assertNotIn("solReplayStage", json.dumps(result))

    def test_panic_site_survives_repeated_repository_directory_names(self):
        message = "thread 'worker' panicked at /private/northstar/northstar/core/src/replay_stage.rs:2167:25:"
        self.assertEqual(diagnostics.panic_sites([message]), [{"file": "core/src/replay_stage.rs", "line": 2167}])

    def test_panic_site_is_found_in_multiline_metric_location(self):
        messages = ['datapoint: panic message="panicked at',
                    'location="/private/northstar/northstar/core/src/replay_stage.rs:2167:25"']
        self.assertEqual(diagnostics.panic_sites(messages), [{"file": "core/src/replay_stage.rs", "line": 2167}])

    def test_crate_relative_panic_source_is_resolved_only_when_unique(self):
        self.assertEqual(diagnostics.panic_sites(["panicked at src/replay_stage.rs:2167:25"]),
                         [{"file": "core/src/replay_stage.rs", "line": 2167}])
        self.assertEqual(diagnostics.panic_sites(["panicked at src/lib.rs:1:1"]), [])

    def test_panic_evidence_reports_fixed_failure_kinds_and_numeric_slots(self):
        text = b"thread 'worker' panicked at src/replay_stage.rs:2167:25:\ncalled `Result::unwrap()` on an `Err` value: bank hash mismatch at slot 123: expected PRIVATE, got PRIVATE"
        result = diagnostics.tail_evidence(text)
        self.assertEqual(result["panic_kinds"], ["bank_hash_mismatch", "result_unwrap"])
        self.assertEqual(result["bank_hash_slots"], [123])
        self.assertNotIn("PRIVATE", json.dumps(result))

    def test_panic_sites_reject_program_logs_and_non_repository_paths(self):
        messages = ["Program log: panicked at core/src/replay_stage.rs:2167:1",
                    "panicked at /private/keys/secret.rs:1:1", "panicked at core/src/not_a_committed_file.rs:1:1"]
        self.assertEqual(diagnostics.panic_sites(messages), [])

    def test_journal_output_is_bounded(self):
        result = diagnostics.journal_summary(lambda _args: SimpleNamespace(returncode=0, stdout=b"a" * (2 * 1024 * 1024 + 1)))
        self.assertEqual(result, {"status": "unavailable"})


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
"""Guard retained program-upload reconciliation without native service actions."""
import hashlib
import io
import os
from contextlib import redirect_stdout
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock

import continuity_devnet as rollout
from continuity_rollout_state import ARTIFACTS, DEVNET, atomic_json, private_json
from continuity_upload_resume import failure_summary, resume_uploads
from fresh_devnet import digest


class UploadResumeTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        args = SimpleNamespace(stage=self.root / "stage", revision="a" * 40, state_root=self.root / "state",
                               devnet_rpc="https://api.devnet.solana.com", l1_rpc="http://127.0.0.1:8899",
                               er_rpc="http://127.0.0.1:8910", max_estimated_cost_lamports=10_000_000_000,
                               funder_keypair=self.root / "funder.json", replace_context="ab" * 32)
        value = self.ops = rollout.Deployment(args)
        value.control.mkdir(mode=0o700, parents=True)
        value.stage = args.stage
        checksums = {}
        for role, relative in ARTIFACTS.items():
            filename = value.stage / relative
            filename.parent.mkdir(parents=True, exist_ok=True)
            filename.write_bytes(b"\x7fELF" + role.encode())
            checksums[role] = hashlib.sha256(filename.read_bytes()).hexdigest()
        value.manifest = {"sha256": checksums}
        value.plan = {"version": 2, "mode": "fresh-stack", "source_revision": args.revision, "genesis": DEVNET,
                      "funder": rollout.public_text(bytes([1]) * 32), "validator": rollout.public_text(bytes([2]) * 32),
                      "programs": {name: rollout.public_text(bytes([index]) * 32) for index, name in enumerate(
                          ("portal", "token_bridge", "portal-buffer", "token_bridge-buffer"), 3)}, "artifacts": checksums}
        value.original = {"arguments": ["/retained/agave-validator", "--portal", "old"], "binary_sha256": "old",
                          "uid": os.getuid(), "gid": os.getgid(), "state_environment": {}}
        snapshot = {"identity": {"context": args.replace_context}, "er_genesis": "retained"}
        previous = {"service": value.original, "snapshot": snapshot}
        value.fresh = SimpleNamespace(service="test-validator.service", authorize=Mock(), state=Mock(return_value="active"), effective=Mock(), previous=previous,
                                      archived_binding=Mock(return_value=(SimpleNamespace(base_plan={}), {})),
                                      snapshot=Mock(return_value=snapshot))
        value.prepare = Mock()
        value.inventory = Mock(return_value=value.original)
        value.flush = Mock()
        value.log = value.control / "operations.log"
        value.log.write_text("Error: Data writes to account failed: Max retries exceeded\n")
        value.log.chmod(0o600)
        atomic_json(value.control / "plan.json", value.plan)
        atomic_json(value.control / "reservation.json", {"version": 2, "source_revision": args.revision,
                    "plan_sha256": digest(value.plan), "l1_rpc": args.l1_rpc, "replacement": previous})
        atomic_json(value.control / "fresh-progress.json", {"version": 1, "plan_sha256": digest(value.plan), "phase": "prepared"})
        atomic_json(value.control / "programs.json", {"version": 2, "roles": {"portal": {
                    "elf_sha256": checksums["portal"], "complete": None}}})
        elf = (value.stage / ARTIFACTS["portal"]).read_bytes()
        self.buffer = {"owner": rollout.LOADER, "executable": False,
                       "raw": (1).to_bytes(4, "little") + b"\x01" + rollout.public_bytes(value.plan["funder"]) + elf[:4] + bytes(len(elf) - 4)}
        self.accounts = {value.plan["programs"]["portal-buffer"]: self.buffer}
        self.views = {}
        value.upstream = SimpleNamespace(
            call=lambda method, *_: 1000 if method == "getMinimumBalanceForRentExemption" else {"value": 20_000_000_000},
            account=lambda key: self.accounts.get(key))
        def local_call(method, *_):
            if method == "getMinimumBalanceForRentExemption":
                return 1000
            if method == "getBalance":
                return {"value": 20_000_000_000}
            return {"getGenesisHash": DEVNET, "getHealth": "ok", "getIdentity": {"identity": value.plan["validator"]}}[method]
        value.l1 = SimpleNamespace(call=local_call, account=lambda key: self.accounts.get(key))
        value.program_view = Mock(side_effect=lambda role: self.views.get(role))
        self.commands = []
        self.pid = "123"
        def execute(arguments, **_):
            self.commands.append(arguments)
            if arguments[:2] == ["systemctl", "show"]:
                return self.pid
            if "program" not in arguments or "deploy" not in arguments:
                raise AssertionError("only signed program submission is permitted")
            retry = value.control / "program-upload-reconcile-retry.json"
            record = private_json(retry if retry.exists() else value.control / "program-upload-reconcile.json")
            self.assertEqual(record["phase"], "attempted")
            self.assertEqual(record["plan_sha256"], digest(value.plan))
            self.assertTrue(value.flush.called)
            role = next(role for role in ("portal", "token_bridge") if str(value.stage / ARTIFACTS[role]) in map(str, arguments))
            self.assertIn(role, record["submission_roles"])
            self.views[role] = {"programdata": role, "deployment_slot": 50, "sha256": checksums[role]}
            self.accounts.pop(value.plan["programs"][role + "-buffer"], None)
            return ""
        value.execute = execute

    def run_resume(self, failed_record_sha256=None):
        options = {} if failed_record_sha256 is None else {"failed_record_sha256": failed_record_sha256}
        with redirect_stdout(io.StringIO()):
            return resume_uploads(self.ops, "d" * 40, **options)

    def prior_failed_record(self):
        record = {"version": 1, "kind": "retained-program-upload", "plan_sha256": digest(self.ops.plan),
                  "controller_revision": "e83cadadd9ad4003a2d5303f395e92244ebb0362", "source_revision": self.ops.args.revision,
                  "submission_rpc": self.ops.args.l1_rpc, "previous_service_sha256": digest(self.ops.fresh.previous["service"]),
                  "pending_roles": ["portal"], "attempt_budget": 1, "phase": "failed", "operator_reconciliation_required": True}
        atomic_json(self.ops.control / "program-upload-reconcile.json", record)
        atomic_json(self.ops.control / "program-upload-reconcile-report.json", {
            "version": 1, "result": "failed", "source_revision": self.ops.args.revision,
            "controller_revision": record["controller_revision"], "native_cutover_performed": False,
            "operator_reconciliation_required": True})
        return digest(record)

    def signed_calls(self):
        return [call for call in self.commands if "deploy" in call]

    def test_one_pass_reuses_original_keys_and_leaves_native_progress_prepared(self):
        before = private_json(self.ops.control / "plan.json")
        result = self.run_resume()
        self.assertEqual(result["result"], "uploads-completed")
        self.assertFalse(result["native_cutover_performed"])
        self.assertEqual(len(self.signed_calls()), 2)
        for call in self.signed_calls():
            self.assertEqual(call[call.index("--url") + 1], self.ops.args.l1_rpc)
            self.assertIn(str(self.ops.control / "keys"), str(call))
        self.assertEqual(private_json(self.ops.control / "plan.json"), before)
        self.assertEqual(private_json(self.ops.control / "fresh-progress.json")["phase"], "prepared")
        self.assertEqual(private_json(self.ops.control / "program-upload-reconcile.json")["phase"], "completed")
        self.assertFalse(any("stop" in call or "restart" in call or "stop-vrf-service.py" in str(call) for call in self.commands))
        with self.assertRaises(ValueError):
            self.run_resume()
        self.assertEqual(len(self.signed_calls()), 2)

    def test_failure_summary_exposes_only_allowlisted_phase_and_type(self):
        secret = "private-command-output-must-not-be-published"
        summary = failure_summary(FileNotFoundError(2, secret), "source-prepare")
        self.assertEqual(summary["failure_type"], "FileNotFoundError")
        self.assertEqual(summary["phase"], "source-prepare")
        self.assertEqual(summary["errno"], 2)
        self.assertNotIn(secret, str(summary))
        self.assertFalse(summary["raw_error_included"])

    def test_program_reads_and_submission_share_the_verified_recorded_l1(self):
        from urllib.error import HTTPError
        public_reads = Mock(side_effect=HTTPError("https://api.devnet.solana.com", 429, "private response", {}, None))
        self.ops.upstream.call = public_reads
        result = self.run_resume()
        self.assertEqual(result["result"], "uploads-completed")
        public_reads.assert_not_called()
        self.assertEqual(len(self.signed_calls()), 2)

    def test_acknowledged_failed_record_gets_a_separate_single_budget(self):
        approved = self.prior_failed_record()
        path = self.ops.control / "program-upload-reconcile.json"
        before = path.read_bytes()
        report_path = self.ops.control / "program-upload-reconcile-report.json"
        report_before = report_path.read_bytes()
        result = self.run_resume(approved)
        self.assertEqual(result["result"], "uploads-completed")
        self.assertEqual(path.read_bytes(), before)
        self.assertEqual(report_path.read_bytes(), report_before)
        retry = private_json(self.ops.control / "program-upload-reconcile-retry.json")
        self.assertEqual(retry["previous_failed_record_sha256"], approved)
        self.assertEqual(retry["read_rpc"], self.ops.args.l1_rpc)
        self.assertEqual(retry["submission_roles"], ["portal", "token_bridge"])
        with self.assertRaises(ValueError):
            self.run_resume(approved)
        self.assertEqual(len(self.signed_calls()), 2)

    def test_acknowledgement_cannot_replace_failed_record_binding_or_reviewed_controller(self):
        self.prior_failed_record()
        with self.assertRaises(ValueError):
            self.run_resume("e" * 64)
        marker = self.ops.control / "program-upload-reconcile.json"
        changed = private_json(marker)
        changed["controller_revision"] = "b" * 40
        atomic_json(marker, changed)
        with self.assertRaises(ValueError):
            self.run_resume(digest(changed))
        self.assertEqual(self.signed_calls(), [])
        self.assertFalse((self.ops.control / "program-upload-reconcile-retry.json").exists())

    def test_failed_additional_pass_keeps_both_barriers_and_cannot_cascade(self):
        approved = self.prior_failed_record()
        primary = (self.ops.control / "program-upload-reconcile.json").read_bytes()
        original = self.ops.execute
        def fail(arguments, **kwargs):
            if "deploy" in arguments:
                raise ValueError("private command response")
            return original(arguments, **kwargs)
        self.ops.execute = fail
        with self.assertRaises(ValueError):
            self.run_resume(approved)
        marker = private_json(self.ops.control / "program-upload-reconcile-retry.json")
        self.assertEqual(marker["phase"], "failed")
        self.assertEqual(marker["submission_roles"], ["portal"])
        self.assertEqual((self.ops.control / "program-upload-reconcile.json").read_bytes(), primary)
        with self.assertRaises(ValueError):
            self.run_resume(approved)

    def test_http_status_summary_never_exposes_url_or_response_text(self):
        from urllib.error import HTTPError
        summary = failure_summary(HTTPError("https://private.invalid/token", 429, "private response", {}, None), "submission")
        self.assertEqual(summary["http_status"], 429)
        self.assertNotIn("private", str(summary))

    def test_unknown_error_cannot_authorize_a_signed_retry(self):
        self.ops.log.write_text("unclassified private message")
        with self.assertRaisesRegex(ValueError, "retry"):
            self.run_resume()
        self.assertEqual(self.signed_calls(), [])

    def test_recorded_local_rpc_genesis_and_validator_identity_are_required(self):
        self.ops.l1.call = lambda *_: "unexpected-network"
        with self.assertRaises(ValueError):
            self.run_resume()
        self.assertEqual(self.signed_calls(), [])
        self.assertFalse((self.ops.control / "program-upload-reconcile.json").exists())

    def test_missing_plan_never_reaches_prepare_or_identity_generation(self):
        (self.ops.control / "plan.json").unlink()
        with self.assertRaises((ValueError, OSError)):
            self.run_resume()
        self.ops.prepare.assert_not_called()
        self.assertEqual(self.signed_calls(), [])

    def test_changed_plan_cannot_reach_a_signed_submission(self):
        plan = private_json(self.ops.control / "plan.json")
        atomic_json(self.ops.control / "plan.json", dict(plan, genesis="changed"))
        with self.assertRaises(ValueError):
            self.run_resume()
        self.assertEqual(self.signed_calls(), [])

    def test_native_freeze_progress_cannot_resume_uploads(self):
        atomic_json(self.ops.control / "fresh-progress.json", {"version": 1, "plan_sha256": digest(self.ops.plan), "phase": "freezing"})
        with self.assertRaises(ValueError):
            self.run_resume()
        self.assertEqual(self.signed_calls(), [])

    def test_any_native_start_barrier_suppresses_upload_reconciliation(self):
        (self.ops.control / "fresh-starts").mkdir(mode=0o700)
        with self.assertRaises(ValueError):
            self.run_resume()
        self.assertEqual(self.signed_calls(), [])

    def test_buffer_authority_and_contents_are_bound_before_submission(self):
        original = self.buffer["raw"]
        for changed in (original[:5] + bytes([9]) * 32 + original[37:], original[:-1] + b"X"):
            self.buffer["raw"] = changed
            with self.assertRaises(ValueError):
                self.run_resume()
            self.assertEqual(self.signed_calls(), [])
        self.buffer["raw"] = original

    def test_unrecorded_bridge_buffer_is_not_overwritten(self):
        self.accounts[self.ops.plan["programs"]["token_bridge-buffer"]] = self.buffer
        with self.assertRaises(ValueError):
            self.run_resume()
        self.assertEqual(self.signed_calls(), [])

    def test_interrupted_attempt_is_not_resubmitted(self):
        atomic_json(self.ops.control / "program-upload-reconcile.json", {"version": 1, "phase": "attempted"})
        with self.assertRaises(ValueError):
            self.run_resume()
        self.assertEqual(self.signed_calls(), [])

    def test_upload_failure_retains_one_shot_barrier_without_stopping_old_service(self):
        original = self.ops.execute
        def fail(arguments, **kwargs):
            if "deploy" in arguments:
                raise ValueError("private command failure")
            return original(arguments, **kwargs)
        self.ops.execute = fail
        with self.assertRaises(ValueError):
            self.run_resume()
        marker = private_json(self.ops.control / "program-upload-reconcile.json")
        self.assertEqual(marker["phase"], "failed")
        self.assertTrue(marker["operator_reconciliation_required"])
        with self.assertRaises(ValueError):
            self.run_resume()
        self.assertFalse(any("stop" in call or "restart" in call for call in self.commands))

    def test_service_pid_change_is_never_reported_as_success(self):
        original = self.ops.execute
        def change(arguments, **kwargs):
            result = original(arguments, **kwargs)
            if "deploy" in arguments:
                self.pid = "124"
            return result
        self.ops.execute = change
        with self.assertRaises(ValueError):
            self.run_resume()
        self.assertEqual(private_json(self.ops.control / "program-upload-reconcile.json")["phase"], "failed")


if __name__ == "__main__":
    unittest.main()

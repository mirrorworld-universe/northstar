#!/usr/bin/env python3
"""Guard continuation of one fully uploaded, never-started fresh activation."""
import io
from contextlib import redirect_stdout
from pathlib import Path
import runpy
import unittest
from unittest.mock import Mock

from continuity_rollout_state import atomic_json, private_json
from fresh_devnet import digest
from fresh_prepared_continuation import continue_prepared_fresh

_fixtures = runpy.run_path(str(Path(__file__).with_name("test-continuity-upload-resume.py")))


class PreparedContinuationTests(unittest.TestCase):
    def setUp(self):
        _fixtures["UploadResumeTests"].setUp(self)
        self.ops.fresh.fail = Mock()
        self.ops.inventory = Mock(return_value={**self.ops.original, "pid": 123, "start_time": "42"})
        saved = private_json(self.ops.control / "reservation.json")
        saved["original"] = self.ops.inventory()
        self.ops.plan["original_sha256"] = digest(saved["original"])
        self.ops.plan["replacement_sha256"] = digest(saved["replacement"])
        self.ops.plan["profile"] = "ef" * 32
        saved["plan_sha256"] = digest(self.ops.plan)
        atomic_json(self.ops.control / "plan.json", self.ops.plan)
        atomic_json(self.ops.control / "fresh-progress.json", {"version": 1, "plan_sha256": digest(self.ops.plan), "phase": "prepared"})
        atomic_json(self.ops.control / "reservation.json", saved)
        for role in ("portal", "token_bridge"):
            self.views[role] = {"programdata": role, "deployment_slot": 50, "sha256": self.ops.manifest["sha256"][role]}
        self.accounts.clear()
        atomic_json(self.ops.control / "programs.json", {"version": 2, "roles": {
            role: {"elf_sha256": self.ops.manifest["sha256"][role], "complete": view} for role, view in self.views.items()}})
        self.original_upstream = self.ops.upstream
        def activate():
            self.assertIs(self.ops.upstream, self.original_upstream)
            intent = private_json(self.ops.control / "fresh-controller-continuation.json")
            self.assertEqual(intent["phase"], "attempted")
            self.assertEqual(intent["plan_sha256"], digest(self.ops.plan))
            self.assertTrue(self.ops.flush.called)
            report = {"result": "passed", "qualification_kind": "fresh-stack", "fresh_plan_sha256": digest(self.ops.plan),
                      "portal": self.ops.plan["programs"]["portal"], "token_bridge": self.ops.plan["programs"]["token_bridge"],
                      "profile": self.ops.plan["profile"], "context": "cd" * 32}
            atomic_json(self.ops.control / "fresh-progress.json", {
                "version": 1, "plan_sha256": digest(self.ops.plan), "phase": "qualified"})
            atomic_json(self.ops.control / "report.json", report)
            return report
        self.ops.run = Mock(side_effect=activate)

    def signed_calls(self):
        return _fixtures["UploadResumeTests"].signed_calls(self)

    def run_continuation(self, plan_hash=None):
        with redirect_stdout(io.StringIO()):
            return continue_prepared_fresh(self.ops, "d" * 40, plan_hash or digest(self.ops.plan))

    def test_qualified_continuation_keeps_original_plan_keys_and_programs(self):
        before_plan = (self.ops.control / "plan.json").read_bytes()
        before_programs = private_json(self.ops.control / "programs.json")
        result = self.run_continuation()
        self.assertEqual(result["qualification_kind"], "fresh-stack")
        self.ops.run.assert_called_once()
        self.assertEqual((self.ops.control / "plan.json").read_bytes(), before_plan)
        self.assertEqual(private_json(self.ops.control / "programs.json"), before_programs)
        self.assertEqual(self.signed_calls(), [])
        self.assertEqual(private_json(self.ops.control / "fresh-controller-continuation.json")["phase"], "qualified")
        with self.assertRaises(ValueError):
            self.run_continuation()
        self.assertEqual(self.ops.run.call_count, 1)

    def test_missing_plan_never_enters_identity_preparation(self):
        (self.ops.control / "plan.json").unlink()
        with self.assertRaises(OSError):
            self.run_continuation()
        self.ops.prepare.assert_not_called()
        self.ops.run.assert_not_called()

    def test_partial_upload_or_changed_plan_never_enters_prepare_or_native_run(self):
        with self.assertRaises(ValueError):
            self.run_continuation("f" * 64)
        self.ops.prepare.assert_not_called()
        state = private_json(self.ops.control / "programs.json")
        state["roles"]["portal"]["complete"] = None
        atomic_json(self.ops.control / "programs.json", state)
        with self.assertRaises(ValueError):
            self.run_continuation()
        self.ops.prepare.assert_not_called()
        self.ops.run.assert_not_called()

    def test_existing_native_start_or_activation_prevents_continuation(self):
        (self.ops.control / "fresh-starts").mkdir(mode=0o700)
        with self.assertRaises(ValueError):
            self.run_continuation()
        self.ops.run.assert_not_called()

    def test_changed_old_process_is_not_adopted(self):
        self.pid = "124"
        with self.assertRaises(ValueError):
            self.run_continuation()
        self.ops.run.assert_not_called()
        self.assertFalse((self.ops.control / "fresh-controller-continuation.json").exists())

    def test_failed_native_run_keeps_a_nonrepeatable_continuation_record(self):
        self.ops.run.side_effect = RuntimeError("private diagnostics retained")
        with self.assertRaises(RuntimeError):
            self.run_continuation()
        record = private_json(self.ops.control / "fresh-controller-continuation.json")
        self.assertEqual(record["phase"], "failed")
        self.assertTrue(record["operator_reconciliation_required"])
        with self.assertRaises(ValueError):
            self.run_continuation()
        self.assertEqual(self.ops.run.call_count, 1)

    def test_post_start_evidence_failure_requests_verified_new_runtime_quarantine(self):
        original = self.ops.run.side_effect
        def changed_report():
            report = original()
            report["portal"] = "unexpected"
            atomic_json(self.ops.control / "report.json", report)
            return report
        self.ops.run.side_effect = changed_report
        with self.assertRaises(ValueError):
            self.run_continuation()
        self.ops.fresh.fail.assert_called_once_with(force=True)
        self.assertEqual(private_json(self.ops.control / "fresh-controller-continuation.json")["phase"], "failed")

    def test_ready_report_without_qualified_progress_is_rejected(self):
        def incomplete():
            report = {"result": "passed", "qualification_kind": "fresh-stack", "fresh_plan_sha256": digest(self.ops.plan)}
            atomic_json(self.ops.control / "report.json", report)
            return report
        self.ops.run.side_effect = incomplete
        with self.assertRaises(ValueError):
            self.run_continuation()
        self.assertEqual(private_json(self.ops.control / "fresh-controller-continuation.json")["phase"], "failed")


if __name__ == "__main__":
    unittest.main()

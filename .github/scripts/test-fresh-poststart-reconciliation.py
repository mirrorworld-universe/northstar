#!/usr/bin/env python3
"""Reconcile one reviewed settlement-pending resume without replacing state or signed intent."""
import io
from contextlib import redirect_stdout
from pathlib import Path
import runpy
from types import SimpleNamespace
import unittest
from unittest.mock import Mock

from continuity_rollout_state import atomic_json, file_hash, private_json
from fresh_poststart_reconciliation import reconcile_fresh_resume
import native_runtime_diagnostics as diagnostics

_fixture_source = runpy.run_path(str(Path(__file__).with_name("test-native-runtime-diagnostics.py")))


class PoststartReconciliationTests(unittest.TestCase):
    def setUp(self):
        fixture = _fixture_source["DiagnosticTests"]()
        fixture.setUp()
        self.addCleanup(fixture.doCleanups)
        fixture.install_failed_resume_review()
        self.fixture = fixture
        self.values = dict(fixture.failed_values)
        self.values["ExecStart"] = fixture.start
        plan = private_json(fixture.control / "plan.json")
        self.ops = SimpleNamespace(control=fixture.control, namespace=fixture.base, runtime=fixture.runtime,
            args=SimpleNamespace(revision=fixture.base.name, state_root=fixture.state_root), plan=plan,
            original=private_json(fixture.control / "reservation.json")["original"], prepare=Mock(), flush=Mock())
        self.ops.execute = Mock(side_effect=lambda *_args, **_kwargs: "\n".join(key + "=" + value for key, value in self.values.items()))
        self.ops.inspect = Mock(return_value={str(fixture.dropin): {"sha256": file_hash(fixture.dropin), "mode": 0o600}})
        self.ops.program_view = Mock(side_effect=lambda role: private_json(fixture.control / "programs.json")["roles"][role]["complete"])
        self.genesis = private_json(fixture.control / "sdk-config.json")["er_genesis"]
        self.ops.upstream = SimpleNamespace(call=Mock(side_effect=lambda method, *_: diagnostics.DEVNET if method == "getGenesisHash" else 1000))
        self.ops.l1 = SimpleNamespace(call=Mock(return_value=1000))
        self.ops.er = SimpleNamespace(call=Mock(return_value=self.genesis))
        self.ops.fresh = SimpleNamespace(service="northstar-validator.service", dropin=fixture.dropin, authorize=Mock(),
            verify_desired=Mock(return_value=fixture.failed_specification), archive_unchanged=Mock(),
            snapshot=Mock(return_value={"identity": {"context": "e" * 64}, "er_genesis": self.genesis}))
        def advance(phase, **extra):
            atomic_json(fixture.control / "fresh-progress.json", {"version": 1, "plan_sha256": diagnostics.digest(plan), "phase": phase, **extra})
        self.ops.fresh.advance = Mock(side_effect=advance)
        def start():
            record = private_json(fixture.control / "fresh-poststart-reconciliation.json")
            self.assertEqual(record["phase"], "start-attempted")
            self.assertTrue((fixture.control / "fresh-reconciliation/start-attempted.json").exists())
            self.assertTrue(self.ops.flush.called)
            self.values.update(ActiveState="active", MainPID="456")
        self.ops.start_runtime = Mock(side_effect=start)
        def smoke(*, resumed_phase):
            self.assertEqual(resumed_phase, "resume-sol")
            self.assertTrue((fixture.control / "fresh-reconciliation/ready.json").exists())
            atomic_json(fixture.control / "report.json", {"result": "passed", "qualification_kind": "fresh-stack",
                "source_revision": plan["source_revision"], "fresh_plan_sha256": diagnostics.digest(plan), "context": "e" * 64,
                "portal": plan["programs"]["portal"], "token_bridge": plan["programs"]["token_bridge"], "profile": plan["profile"]})
        self.ops.smoke = Mock(side_effect=smoke)
        def fail(**_):
            self.values.update(ActiveState="inactive", MainPID="0")
            advance("failed", quarantined=True, operator_reconciliation_required=True)
        self.ops.fresh.fail = Mock(side_effect=fail)
        self.review = diagnostics.fresh_resume_review(fixture.state_root, fixture.base.name, fixture.failed_specification, self.values)
        self.assertEqual(self.review["status"], "reviewable")

    def run_reconciliation(self, token=None):
        with redirect_stdout(io.StringIO()):
            return reconcile_fresh_resume(self.ops, "d" * 40, token or self.review["review_sha256"], "e" * 64)

    def test_one_extra_start_preserves_prior_records_and_saved_signed_transaction(self):
        names = ("fresh-controller-continuation.json", "fresh-controller-continuation-retry.json", "fresh-starts/resume-sol.json",
                 "sdk-smoke/controls/sol-withdraw/intent.json", "sdk-smoke/keys/sol-payer.json")
        before = {name: (self.ops.control / name).read_bytes() for name in names}
        report = self.run_reconciliation()
        self.assertEqual(report["qualification_kind"], "fresh-stack")
        self.assertEqual(private_json(self.ops.control / "fresh-progress.json")["phase"], "qualified")
        self.assertEqual(private_json(self.ops.control / "fresh-poststart-reconciliation.json")["phase"], "qualified")
        self.assertEqual(before, {name: (self.ops.control / name).read_bytes() for name in names})
        with self.assertRaises(ValueError):
            self.run_reconciliation()
        self.ops.start_runtime.assert_called_once()
        self.ops.program_view.assert_any_call("portal")

    def test_changed_review_or_exact_stop_never_starts(self):
        with self.assertRaises(ValueError):
            self.run_reconciliation("f" * 64)
        self.values["InactiveEnterTimestamp"] = "Thu 2026-11-12 01:30:00 UTC"
        with self.assertRaises(ValueError):
            self.run_reconciliation()
        self.ops.start_runtime.assert_not_called()
        self.ops.prepare.assert_not_called()

    def test_changed_program_observation_never_records_or_starts(self):
        self.ops.program_view.return_value = None
        self.ops.program_view.side_effect = None
        with self.assertRaises(ValueError):
            self.run_reconciliation()
        self.ops.start_runtime.assert_not_called()
        self.assertFalse((self.ops.control / "fresh-poststart-reconciliation.json").exists())

    def test_qualification_failure_quarantines_without_rollback_or_another_start(self):
        self.ops.smoke.side_effect = ValueError("qualification remains incomplete")
        with self.assertRaises(ValueError):
            self.run_reconciliation()
        self.ops.fresh.fail.assert_called_once_with(force=True)
        self.assertEqual(private_json(self.ops.control / "fresh-poststart-reconciliation.json")["phase"], "failed")
        with self.assertRaises(ValueError):
            self.run_reconciliation()
        self.ops.start_runtime.assert_called_once()

    def test_stop_evidence_changed_during_prepare_never_records_or_starts(self):
        self.ops.prepare.side_effect = lambda: self.values.update(InactiveEnterTimestamp="Thu 2026-11-12 01:30:00 UTC")
        with self.assertRaises(ValueError):
            self.run_reconciliation()
        self.ops.start_runtime.assert_not_called()
        self.assertFalse((self.ops.control / "fresh-poststart-reconciliation.json").exists())

    def test_changed_native_specification_never_starts(self):
        self.ops.fresh.verify_desired.return_value = self.fixture.failed_specification | {"binary_sha256": "f" * 64}
        with self.assertRaises(ValueError):
            self.run_reconciliation()
        self.ops.start_runtime.assert_not_called()

    def test_incomplete_success_report_never_qualifies_and_quarantines(self):
        self.ops.smoke.side_effect = lambda **_: atomic_json(self.ops.control / "report.json", {"result": "passed"})
        with self.assertRaises(ValueError):
            self.run_reconciliation()
        self.ops.fresh.fail.assert_called_once_with(force=True)
        self.assertEqual(private_json(self.ops.control / "fresh-progress.json")["phase"], "failed")

    def test_changed_er_genesis_quarantines_before_sdk_actions(self):
        self.ops.er.call.return_value = "different-genesis"
        with self.assertRaises(ValueError):
            self.run_reconciliation()
        self.ops.fresh.fail.assert_called_once_with(force=True)
        self.ops.smoke.assert_not_called()


if __name__ == "__main__":
    unittest.main()

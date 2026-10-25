import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location(
    "timing", Path(__file__).with_name("summarize-timing.py")
)
timing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(timing)


class TimingTests(unittest.TestCase):
    def test_duration_units_and_invalid_value(self):
        for value, expected in [("1ns", 1e-9), ("2.5µs", 2.5e-6), ("3us", 3e-6),
                                ("4ms", .004), ("5s", 5)]:
            self.assertAlmostEqual(timing.seconds(value), expected)
        with self.assertRaises(ValueError):
            timing.seconds("not-a-duration")

    def test_overlapping_spans_are_not_added(self):
        log = "\n".join([
            "2026-01-01T00:00:02Z DEBUG Parent{id=one}:child: sp1_prover::worker: close time.busy=1s time.idle=1s",
            "2026-01-01T00:00:03Z DEBUG Parent{id=two}:child: sp1_prover::worker: close time.busy=1s time.idle=1s",
        ])
        report = timing.summarize(log, {})
        stage, = report["stages"]
        self.assertEqual(stage["span"], "child")
        self.assertEqual(stage["count"], 2)
        self.assertEqual(stage["busy_s_sum"], 2)
        self.assertEqual(stage["idle_s_sum"], 2)
        self.assertEqual(stage["interval_union_s"], 3)

    def test_empty_or_failed_attempts_do_not_pass(self):
        for state in [{}, {"status": "completed", "proof_attempts_started": 0},
                      {"status": "started", "proof_attempts_started": 1}]:
            report = timing.summarize("", state)
            self.assertFalse(report["all_attempts_verified"])
            self.assertFalse(report["all_recorded_proofs_within_120s"])

    def test_generated_proof_is_not_counted_twice(self):
        state = {
            "status": "completed", "proof_attempts_started": 1,
            "phases": [
                {"phase": "groth16-generated-unverified", "prove_and_wrap_ms": 100000},
                {"phase": "groth16", "prove_and_wrap_ms": 100000, "sdk_verified": True},
            ],
        }
        report = timing.summarize("", state)
        self.assertEqual(len(report["proofs"]), 1)
        self.assertTrue(report["all_recorded_proofs_within_120s"])
        self.assertFalse(report["fresh_session_acceptance_established"])
        self.assertFalse(report["production_acceptance"])
        state["proof_attempts_started"] = 2
        self.assertFalse(timing.summarize("", state)["all_attempts_verified"])

    def test_slow_and_unverified_results_stay_visible(self):
        state = {"status": "completed", "proof_attempts_started": 2, "phases": [
            {"phase": "groth16", "prove_and_wrap_ms": 120001, "sdk_verified": True},
            {"phase": "groth16", "prove_and_wrap_ms": 90000, "sdk_verified": False},
        ]}
        report = timing.summarize("", state)
        self.assertEqual(len(report["proofs"]), 2)
        self.assertEqual(report["recorded_verified_proofs"], 1)
        self.assertFalse(report["all_recorded_proofs_within_120s"])

    def test_invalid_or_missing_time_never_passes(self):
        for value in [None, -1, 0, True, "10"]:
            state = {"status": "completed", "proof_attempts_started": 1, "phases": [
                {"phase": "groth16", "prove_and_wrap_ms": value, "sdk_verified": True},
            ]}
            self.assertFalse(timing.summarize("", state)["all_recorded_proofs_within_120s"])


if __name__ == "__main__":
    unittest.main()

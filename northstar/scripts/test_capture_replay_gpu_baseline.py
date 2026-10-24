#!/usr/bin/env python3
"""Offline measurement-parser invariants. Generated proof bytes are synthetic."""

import copy
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    "capture_replay_gpu_baseline", Path(__file__).with_name("capture-replay-gpu-baseline.py")
)
CAPTURE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CAPTURE)
REPLAY = Path(__file__).resolve().parents[1] / "zkvm-replay"


class MeasurementTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.root = Path(self.work.name)
        self.path = self.root / "measurements.json"
        self.candidate = json.loads((REPLAY / "partial-candidate-v1.json").read_text())
        self.data = {
            "schema": "northstar-sp1-benchmark-v2",
            "program_vkey_hash": self.candidate["program_vkey_hash"],
            "public_inputs": bytes(256).hex(),
            "client_init_ms": 16,
            "phases": [{"phase": "setup", "wall_ms": 42}],
        }
        for index in range(3):
            proof = self.root / f"proof-{index}.bin"
            public = self.root / f"public-{index}.bin"
            proof.write_bytes(bytes.fromhex("4388a21c") + bytes(352))
            public.write_bytes(bytes(256))
            self.data["phases"].append({
                "phase": "groth16", "sample": index,
                "prove_and_wrap_ms": 90_000 if index == 0 else 41_000,
                "verify_ms": 500, "onchain_proof_bytes": 356,
                "onchain_proof_path": proof.name, "public_inputs_path": public.name,
            })

    def summarize(self):
        self.path.write_text(json.dumps(self.data))
        return CAPTURE.summarize(self.path, self.candidate)

    def test_separate_timing_domains_and_no_acceptance(self):
        summary = self.summarize()
        self.assertEqual(summary["samples"][0]["prove_wrap_and_verify_ms"], 90_500)
        self.assertEqual(summary["samples"][0]["prove_and_wrap_ms"], 90_000)
        self.assertEqual([row["process_cold"] for row in summary["samples"]], [True, False, False])
        self.assertEqual(summary["setup_ms"], 42)
        self.assertEqual(summary["validation_level"], "measurement-structure-only")
        self.assertIsNone(summary["portal_resolution_compute_units"])
        for name in ("production_acceptance", "vrf_execution_proof", "l40s_acceptance"):
            self.assertIs(summary[name], False)

    def test_retains_over_limit_measurements_as_failures_not_zero(self):
        self.data["phases"][1]["prove_and_wrap_ms"] = 190_803
        sample = self.summarize()["samples"][0]
        self.assertFalse(sample["within_120s_prove_wrap"])
        self.assertEqual(sample["prove_wrap_and_verify_ms"], 191_303)

    def test_rejects_bad_sample_count_order_or_duplicate_paths(self):
        original = copy.deepcopy(self.data)
        for change in ("missing", "duplicate_index", "bool_index", "duplicate_path", "extra_phase"):
            with self.subTest(change=change):
                self.data = copy.deepcopy(original)
                if change == "missing":
                    self.data["phases"].pop()
                elif change == "duplicate_index":
                    self.data["phases"][2]["sample"] = 0
                elif change == "bool_index":
                    self.data["phases"][1]["sample"] = False
                elif change == "duplicate_path":
                    self.data["phases"][2]["onchain_proof_path"] = "proof-0.bin"
                else:
                    self.data["phases"].append({"phase": "execute"})
                with self.assertRaises(ValueError):
                    self.summarize()

    def test_rejects_noninteger_or_negative_measurements(self):
        original = copy.deepcopy(self.data)
        for field, values in (("prove_and_wrap_ms", (0, -1, True, 1.5, "41000")),
                              ("verify_ms", (-1, False, None))):
            for value in values:
                with self.subTest(field=field, value=value):
                    self.data = copy.deepcopy(original)
                    self.data["phases"][1][field] = value
                    with self.assertRaises(ValueError):
                        self.summarize()

    def test_missing_measurements_are_not_zero(self):
        del self.data["client_init_ms"]
        with self.assertRaises(KeyError):
            self.summarize()

    def test_rejects_new_candidate_or_unknown_legacy_version(self):
        for field, value in (("proof_kind", 3), ("proof_version", 2),
                             ("proof_version", True), ("schema", "new-candidate")):
            with self.subTest(field=field, value=value):
                saved = self.candidate[field]
                self.candidate[field] = value
                with self.assertRaisesRegex(ValueError, "only the legacy"):
                    self.summarize()
                self.candidate[field] = saved

    def test_rejects_wrong_key_or_schema(self):
        for field in ("program_vkey_hash", "schema"):
            with self.subTest(field=field):
                saved = self.data[field]
                self.data[field] = "other"
                with self.assertRaises(ValueError):
                    self.summarize()
                self.data[field] = saved

    def test_rejects_public_mismatch_and_proof_envelope(self):
        (self.root / "public-0.bin").write_bytes(bytes(255))
        with self.assertRaisesRegex(ValueError, "public input bytes"):
            self.summarize()
        (self.root / "public-0.bin").write_bytes(bytes(256))
        for proof in (bytes(355), bytes(356), bytes.fromhex("4388a21c01") + bytes(351)):
            with self.subTest(proof=proof[:5]):
                (self.root / "proof-0.bin").write_bytes(proof)
                with self.assertRaisesRegex(ValueError, "proof envelope"):
                    self.summarize()

    def test_rejects_absolute_and_escaping_paths(self):
        with tempfile.TemporaryDirectory() as external:
            outside = Path(external) / "proof.bin"
            outside.write_bytes(bytes.fromhex("4388a21c") + bytes(352))
            (self.root / "link.bin").symlink_to(outside)
            for name in (str(outside), "link.bin", "../missing.bin"):
                with self.subTest(name=name):
                    self.data["phases"][1]["onchain_proof_path"] = name
                    with self.assertRaises((ValueError, FileNotFoundError)):
                        self.summarize()

    def test_current_retained_real_measurements(self):
        summary = CAPTURE.summarize(
            REPLAY / "evidence/g5-a10g-sha-v1/final-client/measurements.json", self.candidate
        )
        self.assertEqual([row["prove_wrap_and_verify_ms"] for row in summary["samples"]],
                         [93_578, 42_453, 41_664])
        self.assertFalse(summary["production_acceptance"])

    def test_capture_requires_explicit_review(self):
        class Args:
            execute_reviewed_launcher = False
        with patch.object(CAPTURE.subprocess, "Popen") as process:
            with self.assertRaisesRegex(ValueError, "coordinator review"):
                CAPTURE.capture(Args())
            process.assert_not_called()

    def test_hash_mismatch_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "client hash mismatch"):
            CAPTURE.checked_hash(self.root / "proof-0.bin", "0" * 64, "client")

    def test_failed_command_preserves_log_and_failure(self):
        with self.assertRaises(subprocess.CalledProcessError) as failure:
            CAPTURE.run([sys.executable, "-c", "print('synthetic failure'); raise SystemExit(7)"],
                        self.root, "failed.log", 10)
        self.assertEqual(failure.exception.returncode, 7)
        self.assertIn("synthetic failure", (self.root / "failed.log").read_text())

    def capture_args(self):
        self.path.write_text(json.dumps(self.candidate))
        retained = self.root / "retained.json"
        retained.write_text(json.dumps({
            "program_vkey_hash": self.candidate["program_vkey_hash"],
            "run": {"prover_binary_sha256": CAPTURE.digest(self.root / "proof-0.bin"),
                    "server_binary_sha256": CAPTURE.digest(self.root / "proof-1.bin")},
        }))
        fixture = self.root / "fixture.bin"
        fixture.write_bytes(b"synthetic fixture")
        self.candidate["baseline_fixture_sha256"] = CAPTURE.digest(fixture)
        self.path.write_text(json.dumps(self.candidate))
        return SimpleNamespace(
            execute_reviewed_launcher=True, candidate=str(self.path),
            retained_client=str(retained), client=str(self.root / "proof-0.bin"),
            server=str(self.root / "proof-1.bin"), fixture=str(fixture),
            launcher=str(self.root / "public-0.bin"),
            launcher_sha256=CAPTURE.digest(self.root / "public-0.bin"),
            launcher_env=str(self.root / "public-1.bin"),
            launcher_env_sha256=CAPTURE.digest(self.root / "public-1.bin"),
            output_parent=str(self.root),
        )

    def test_capture_key_mismatch_retains_failure_and_never_proves(self):
        args = self.capture_args()

        def fake_run(command, directory, log, timeout):
            if "key" in command:
                (directory / "key.json").write_text(json.dumps({"program_vkey_hash": "wrong"}))

        with patch.object(CAPTURE, "run", side_effect=fake_run) as run:
            with patch("builtins.print"), self.assertRaisesRegex(ValueError, "embedded key mismatch"):
                CAPTURE.capture(args)
        self.assertEqual(run.call_count, 2)
        out = next(self.root.glob("replay-development-*"))
        metadata = json.loads((out / "capture.json").read_text())
        self.assertEqual(metadata["exit_code"], 1)
        self.assertFalse(metadata["production_acceptance"])
        self.assertFalse((out / "measurements.json").exists())

    def test_capture_success_records_metadata_without_acceptance(self):
        args = self.capture_args()

        def fake_run(command, directory, log, timeout):
            if "key" in command:
                (directory / "key.json").write_text(json.dumps({
                    "program_vkey_hash": self.candidate["program_vkey_hash"]}))
            if "groth16" in command:
                (directory / "measurements.json").write_text(json.dumps(self.data))
                for path in self.root.glob("*.bin"):
                    (directory / path.name).write_bytes(path.read_bytes())

        with patch.object(CAPTURE, "run", side_effect=fake_run), patch("builtins.print"):
            CAPTURE.capture(args)
        out = next(self.root.glob("replay-development-*"))
        metadata = json.loads((out / "capture.json").read_text())
        self.assertEqual(metadata["exit_code"], 0)
        self.assertFalse(metadata["production_acceptance"])
        self.assertEqual(metadata["launcher_env_sha256"], args.launcher_env_sha256)
        self.assertEqual((out / "exit").read_text(), "0\n")
        self.assertTrue((out / "summary.json").is_file())

    def test_cleanup_signals_descendants_even_after_parent_exits(self):
        with patch.object(CAPTURE.subprocess, "Popen") as popen, patch.object(CAPTURE.os, "killpg") as kill:
            process = popen.return_value
            process.pid = 123
            process.wait.side_effect = [subprocess.TimeoutExpired(["synthetic"], 1), 0, 0]
            with self.assertRaises(subprocess.TimeoutExpired):
                CAPTURE.run(["synthetic"], self.root, "synthetic.log", 1)
            self.assertEqual([call.args for call in kill.call_args_list],
                             [(123, CAPTURE.signal.SIGTERM), (123, CAPTURE.signal.SIGKILL)])

    def test_timeout_preserves_failure(self):
        with self.assertRaises(subprocess.TimeoutExpired):
            CAPTURE.run([sys.executable, "-c", "import time; time.sleep(60)"],
                        self.root, "timeout.log", 0.1)


if __name__ == "__main__":
    unittest.main()

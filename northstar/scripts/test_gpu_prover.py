import importlib.util
from pathlib import Path
import subprocess
import sys
import unittest

spec = importlib.util.spec_from_file_location("gpu_prover", Path(__file__).with_name("gpu-prover.py"))
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)


class GpuRunnerTests(unittest.TestCase):
    def test_timeout_is_bounded(self):
        with self.assertRaises(subprocess.TimeoutExpired):
            runner.run([sys.executable, "-c", "import time; time.sleep(60)"], 0.1)

    def test_child_failure_propagates(self):
        with self.assertRaises(RuntimeError):
            runner.run([sys.executable, "-c", "raise SystemExit(17)"], 5)

    def test_failed_parent_stops_descendants(self):
        import os
        import signal
        import tempfile
        import time
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "child"
            program = f"""
import os, time
from pathlib import Path
marker = Path({str(marker)!r})
if os.fork() == 0:
    marker.write_text(str(os.getpid()))
    time.sleep(60)
else:
    while not marker.exists():
        time.sleep(0.01)
    os._exit(17)
"""
            with self.assertRaises(RuntimeError):
                runner.run([sys.executable, "-c", program], 5)
            pid = int(marker.read_text())
            stat = Path(f"/proc/{pid}/stat")
            try:
                for _ in range(100):
                    if not stat.exists() or stat.read_text().split()[2] == "Z":
                        break
                    time.sleep(0.01)
                else:
                    self.fail("descendant still running after parent failure")
            finally:
                try:
                    os.kill(pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass

    def test_timeout_stops_descendants(self):
        import tempfile
        import time
        with tempfile.TemporaryDirectory() as directory:
            marker = Path(directory) / "child"
            program = (
                "import os, signal, time; from pathlib import Path; "
                "child = os.fork(); "
                "signal.signal(signal.SIGTERM, signal.SIG_IGN) if child == 0 else None; "
                f"Path({str(marker)!r}).write_text(str(os.getpid())) if child == 0 else None; "
                "time.sleep(60)"
            )
            with self.assertRaises(subprocess.TimeoutExpired):
                runner.run([sys.executable, "-c", program], 1)
            stat = Path(f"/proc/{int(marker.read_text())}/stat")
            for _ in range(100):
                if not stat.exists() or stat.read_text().split()[2] == "Z":
                    break
                time.sleep(0.01)
            else:
                self.fail("descendant still running after timeout")

    def test_retained_artifacts_and_mutations(self):
        import copy
        import json
        replay = Path(__file__).resolve().parents[1] / "zkvm-replay"
        manifest = json.loads((replay / "partial-candidate-v1.json").read_text())
        case = replay / "evidence/l40s-v1/live-01"
        measurements = json.loads((case / "measurements.json").read_text())
        proof = (case / "northstar-sp1-groth16-onchain.bin").read_bytes()
        public = (case / "northstar-sp1-public-inputs.bin").read_bytes()
        runner.validate(measurements, proof, public, manifest)
        for field, value in [("program_vkey_hash", "0x00"), ("public_inputs", "00")]:
            changed = copy.deepcopy(measurements)
            changed[field] = value
            with self.assertRaises(ValueError):
                runner.validate(changed, proof, public, manifest)
        for duration in [0, 120001]:
            changed = copy.deepcopy(measurements)
            next(p for p in changed["phases"] if p["phase"] == "groth16")["prove_and_wrap_ms"] = duration
            with self.assertRaises(ValueError):
                runner.validate(changed, proof, public, manifest)
        with self.assertRaises(ValueError):
            runner.validate(measurements, proof[:-1], public, manifest)


if __name__ == "__main__":
    unittest.main()

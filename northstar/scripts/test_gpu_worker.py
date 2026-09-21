#!/usr/bin/env python3
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

SCRIPT = Path(__file__).with_name("gpu-worker.py")
REPLAY = SCRIPT.resolve().parents[1] / "zkvm-replay"
spec = importlib.util.spec_from_file_location("worker", SCRIPT)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

FAKE = r"""
import json, os, signal, sys, time
from pathlib import Path
root = Path(sys.argv[2])
evidence = Path(os.environ['FAKE_EVIDENCE'])
original = json.loads((evidence/'measurements.json').read_text())
(root/'ready.tmp').write_text(json.dumps({'program_vkey_hash': original['program_vkey_hash'], 'pid': os.getpid()}))
(root/'ready.tmp').rename(root/'ready.json')
for line in sys.stdin:
    request = json.loads(line)
    work = root/request['id']
    if request['profile'] in ('timeout', 'cleanup'):
        if request['profile'] == 'cleanup':
            signal.signal(signal.SIGTERM, lambda *_: (work/'terminating').write_text('1'))
        child = os.fork()
        if child == 0:
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            while True: time.sleep(1)
        (work/'pids.json').write_text(json.dumps([os.getpid(), child]))
        while True: time.sleep(1)
    if request['profile'] == 'error':
        (work/'result.tmp').write_text(json.dumps({'id': request['id'], 'ok': False, 'error': 'test failure'}))
        (work/'result.tmp').rename(work/'result.json')
        sys.exit(1)
    if request['profile'] == 'slow': time.sleep(0.3)
    for name in ('northstar-sp1-groth16.bin','northstar-sp1-groth16-onchain.bin','northstar-sp1-public-inputs.bin'):
        (work/name).write_bytes((evidence/name).read_bytes())
    if request['profile'] == 'changed':
        proof = bytearray((work/'northstar-sp1-groth16-onchain.bin').read_bytes())
        proof[0] ^= 1
        (work/'northstar-sp1-groth16-onchain.bin').write_bytes(proof)
    data = dict(original)
    data['worker_request_id'] = request['id']
    (work/'measurements.json').write_text(json.dumps(data))
    (work/'result.tmp').write_text(json.dumps({'id': request['id'], 'ok': True}))
    (work/'result.tmp').rename(work/'result.json')
"""


def stopped(pid):
    path = Path(f"/proc/{pid}/stat")
    return not path.exists() or path.read_text().split()[2] == "Z"


class WorkerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.fake = self.root / "fake-prover"
        self.fake.write_text(f"#!{sys.executable}\n" + FAKE)
        self.fake.chmod(0o700)
        self.state = self.root / "state"
        self.state.mkdir(mode=0o700)
        self.manifest = json.loads((REPLAY / "partial-candidate-v1.json").read_text())
        self.previous = os.environ.get("FAKE_EVIDENCE")
        os.environ["FAKE_EVIDENCE"] = str(REPLAY / "evidence/userspace-v1")
        self.addCleanup(self.restore_env)
        self.worker = module.Worker(
            self.state, self.fake, REPLAY / "fixture-v1.bin", self.manifest, timeout=3
        )
        self.addCleanup(self.worker.stop)

    def restore_env(self):
        if self.previous is None:
            os.environ.pop("FAKE_EVIDENCE", None)
        else:
            os.environ["FAKE_EVIDENCE"] = self.previous

    def request(self, profile="test"):
        return {
            "op": "prove",
            "witness": str(REPLAY / "fixture-v1.bin"),
            "profile": profile,
        }

    def test_reuses_worker_with_distinct_bound_requests(self):
        first = self.worker.handle({"op": "preflight"})
        a = self.worker.handle(self.request())
        b = self.worker.handle(self.request())
        self.assertEqual(first["pid"], self.worker.handle({"op": "preflight"})["pid"])
        self.assertNotEqual(a["directory"], b["directory"])
        self.assertEqual(a["witness_sha256"], self.manifest["baseline_fixture_sha256"])

    def test_changed_envelope_stops_worker(self):
        self.worker.handle({"op": "preflight"})
        with self.assertRaisesRegex(ValueError, "envelope"):
            self.worker.handle(self.request("changed"))
        self.assertIsNone(self.worker.process)

    def test_failed_request_restarts_on_next_call(self):
        first = self.worker.handle({"op": "preflight"})
        with self.assertRaisesRegex(RuntimeError, "request failed"):
            self.worker.handle(self.request("error"))
        second = self.worker.handle({"op": "preflight"})
        self.assertNotEqual(first["pid"], second["pid"])

    def test_timeout_kills_descendants_and_allows_restart(self):
        self.worker.handle({"op": "preflight"})
        self.worker.timeout = 0.4
        with self.assertRaises(TimeoutError):
            self.worker.handle(self.request("timeout"))
        pids = json.loads(next(self.worker.root.glob("*/pids.json")).read_text())
        deadline = time.monotonic() + 3
        while not all(stopped(pid) for pid in pids) and time.monotonic() < deadline:
            time.sleep(0.02)
        self.assertTrue(all(stopped(pid) for pid in pids))
        self.worker.timeout = 3
        self.assertTrue(self.worker.handle({"op": "preflight"})["warmed"])

    def test_busy_request_is_rejected_without_reset(self):
        self.worker.handle({"op": "preflight"})
        self.worker.lock.acquire()
        try:
            with self.assertRaisesRegex(RuntimeError, "busy"):
                self.worker.handle({"op": "preflight"})
            self.assertIsNone(self.worker.process.poll())
        finally:
            self.worker.lock.release()

    def test_termination_during_guardian_cleanup_still_kills_descendants(self):
        import signal

        root = self.state / "guardian-test"
        root.mkdir()
        guardian = subprocess.Popen(
            [sys.executable, str(SCRIPT), "_child", str(self.fake), str(root)],
            stdin=subprocess.PIPE,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        self.addCleanup(lambda: module.stop_group(guardian))
        deadline = time.monotonic() + 3
        while not (root / "ready.json").exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertTrue((root / "ready.json").exists())
        request_id = "a" * 32
        work = root / request_id
        work.mkdir()
        guardian.stdin.write(
            json.dumps({"id": request_id, "profile": "cleanup"}).encode() + b"\n"
        )
        guardian.stdin.flush()
        while not (work / "pids.json").exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        pids = json.loads((work / "pids.json").read_text())

        def cleanup_group():
            try:
                os.killpg(pids[0], signal.SIGKILL)
            except ProcessLookupError:
                pass

        self.addCleanup(cleanup_group)
        guardian.stdin.close()
        while not (work / "terminating").exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertTrue((work / "terminating").exists())
        guardian.send_signal(signal.SIGTERM)
        guardian.wait(timeout=7)
        deadline = time.monotonic() + 1
        while not all(stopped(pid) for pid in pids) and time.monotonic() < deadline:
            time.sleep(0.01)
        self.assertTrue(all(stopped(pid) for pid in pids))

    def test_invalid_deadlines_and_oversized_inputs_are_rejected(self):
        for value in (0, 181, True, "1", float("nan")):
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.worker.handle({"op": "preflight", "timeout_s": value})
        oversized = self.root / "oversized.bin"
        oversized.write_bytes(b"x" * (module.MAX_WITNESS + 1))
        with self.assertRaises(ValueError):
            module.bounded_witness(oversized)

    def test_supervisor_death_cleans_up_worker_and_stale_socket_recovers(self):
        endpoint = self.root / "worker.sock"
        env = {
            **os.environ,
            "NORTHSTAR_GPU_PROVER": str(self.fake),
            "NORTHSTAR_GPU_REPLAY_DIR": str(REPLAY),
        }
        log = (self.root / "daemon.log").open("wb")
        self.addCleanup(log.close)

        def launch():
            process = subprocess.Popen(
                [sys.executable, str(SCRIPT), "serve", str(endpoint), str(self.state)],
                env=env,
                stdout=log,
                stderr=log,
            )

            def cleanup():
                if process.poll() is None:
                    process.terminate()
                    try:
                        process.wait(timeout=3)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()

            self.addCleanup(cleanup)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                if process.poll() is not None:
                    self.fail((self.root / "daemon.log").read_text())
                if endpoint.exists():
                    try:
                        return process, module.call(endpoint, {"op": "preflight"})
                    except OSError:
                        pass
                time.sleep(0.02)
            self.fail("worker readiness timeout")

        process, ready = launch()
        process.kill()
        process.wait(timeout=3)
        deadline = time.monotonic() + 5
        while not stopped(ready["pid"]) and time.monotonic() < deadline:
            time.sleep(0.02)
        self.assertTrue(stopped(ready["pid"]))
        process, second = launch()
        self.assertNotEqual(ready["pid"], second["pid"])
        process.terminate()
        process.wait(timeout=10)


if __name__ == "__main__":
    unittest.main()

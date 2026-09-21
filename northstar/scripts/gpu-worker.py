#!/usr/bin/env python3
"""Opt-in private CUDA worker; one-shot gpu-prover.py remains independent."""

import errno
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import select
import shutil
import signal
import socket
import socketserver
import stat
import subprocess
import sys
import threading
import time
import uuid

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "gpu_prover", Path(__file__).with_name("gpu-prover.py")
)
guards = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guards)
FILES = (
    "northstar-sp1-groth16.bin",
    "northstar-sp1-groth16-onchain.bin",
    "northstar-sp1-public-inputs.bin",
)
MAX_WITNESS = 4 * 1024 * 1024


def stop_group(process, grace=5):
    if process is None:
        return
    if process.stdin:
        try:
            process.stdin.close()
        except OSError:
            pass
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        process.wait(timeout=grace)
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait()


def child_guard(prover, root):
    # This pipe guardian survives supervisor death; EOF kills the entire prover group.
    stopping = False

    def request_stop(_signum, _frame):
        nonlocal stopping
        stopping = True

    # Raising from a signal during finally would skip the remaining descendant cleanup.
    for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
        signal.signal(sig, request_stop)
    process = subprocess.Popen(
        [prover, "worker", root], stdin=subprocess.PIPE, start_new_session=True
    )
    try:
        while not stopping and process.poll() is None:
            if select.select([sys.stdin], [], [], 0.1)[0]:
                line = sys.stdin.buffer.readline(8193)
                if not line:
                    break
                if len(line) > 8192 or not line.endswith(b"\n"):
                    raise ValueError("invalid internal request frame")
                process.stdin.write(line)
                process.stdin.flush()
    finally:
        stop_group(process)


def private_directory(path):
    path.mkdir(parents=True, mode=0o700, exist_ok=True)
    info = path.lstat()
    if (
        not stat.S_ISDIR(info.st_mode)
        or info.st_uid != os.getuid()
        or info.st_mode & 0o077
    ):
        raise ValueError(f"private owned directory required: {path}")


def bounded_witness(path):
    with open(path, "rb") as source:
        data = source.read(MAX_WITNESS + 1)
    if not data or len(data) > MAX_WITNESS:
        raise ValueError("witness size limit exceeded")
    return data


class Worker:
    def __init__(self, state, prover, fixture, manifest, timeout=180):
        self.state, self.prover, self.fixture, self.manifest = (
            state,
            prover,
            fixture,
            manifest,
        )
        self.timeout = timeout
        self.process = self.log = self.root = None
        self.ready = None
        self.lock = threading.Lock()
        if (
            hashlib.sha256(bounded_witness(fixture)).hexdigest()
            != manifest["baseline_fixture_sha256"]
        ):
            raise ValueError("warm-up fixture hash mismatch")

    def stop(self):
        process, self.process = self.process, None
        self.ready = None
        stop_group(process, grace=8)
        if self.log:
            self.log.close()
            self.log = None

    def wait_file(self, path, deadline):
        while not path.exists():
            if time.monotonic() >= deadline:
                raise TimeoutError("worker deadline exceeded")
            if self.process is None or self.process.poll() is not None:
                raise RuntimeError("prover worker exited")
            time.sleep(0.05)
        if time.monotonic() >= deadline:
            raise TimeoutError("worker deadline exceeded")
        return json.loads(path.read_text())

    def start(self, deadline):
        self.stop()
        self.root = self.state / uuid.uuid4().hex
        self.root.mkdir(mode=0o700)
        self.log = (self.root / "worker.log").open("wb")
        self.process = subprocess.Popen(
            [
                sys.executable,
                str(Path(__file__).resolve()),
                "_child",
                str(self.prover),
                str(self.root),
            ],
            stdin=subprocess.PIPE,
            stdout=self.log,
            stderr=subprocess.STDOUT,
            start_new_session=True,
            env={**os.environ, "SP1_PROVER": "cuda"},
        )
        ready = self.wait_file(self.root / "ready.json", deadline)
        if ready["program_vkey_hash"] != self.manifest["program_vkey_hash"]:
            raise ValueError("worker program key mismatch")
        self.prove(bounded_witness(self.fixture), "worker-warmup", deadline)
        self.ready = ready

    def prove(self, data, profile, deadline):
        request_id = uuid.uuid4().hex
        work = self.root / request_id
        work.mkdir(mode=0o700)
        (work / "witness.bin").write_bytes(data)
        self.process.stdin.write(
            json.dumps({"id": request_id, "profile": profile}).encode() + b"\n"
        )
        self.process.stdin.flush()
        result = self.wait_file(work / "result.json", deadline)
        if result.get("id") != request_id or result.get("ok") is not True:
            raise RuntimeError(
                f"proof request failed: {result.get('error', 'invalid response')}"
            )
        measurements = json.loads((work / "measurements.json").read_text())
        if measurements.get("worker_request_id") != request_id:
            raise ValueError("request binding mismatch")
        guards.validate(
            measurements,
            (work / FILES[1]).read_bytes(),
            (work / FILES[2]).read_bytes(),
            self.manifest,
        )
        return {
            "ok": True,
            "directory": str(work),
            "witness_sha256": hashlib.sha256(data).hexdigest(),
        }

    def handle(self, request):
        if not self.lock.acquire(blocking=False):
            raise RuntimeError("worker busy")
        try:
            limit = request.get("timeout_s", 180)
            if type(limit) is not int or not 1 <= limit <= 180:
                raise ValueError("timeout must be an integer from 1 to 180 seconds")
            deadline = time.monotonic() + min(self.timeout, limit)
            op = request.get("op")
            expected = {"op"} if op == "preflight" else {"op", "witness", "profile"}
            if (
                op not in ("preflight", "prove")
                or set(request) - {"timeout_s"} != expected
            ):
                raise ValueError("invalid request")
            if op == "prove" and (
                not isinstance(request["profile"], str)
                or len(request["profile"].encode()) > 128
            ):
                raise ValueError("invalid profile")
            if (
                self.process is None
                or self.process.poll() is not None
                or self.ready is None
            ):
                self.start(deadline)
            if op == "preflight":
                return {"ok": True, "warmed": True, **self.ready}
            return self.prove(
                bounded_witness(request["witness"]), request["profile"], deadline
            )
        except BaseException:
            self.stop()
            raise
        finally:
            self.lock.release()


class Server(socketserver.ThreadingUnixStreamServer):
    daemon_threads = True
    request_queue_size = 1


class Handler(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(5)
        try:
            line = self.rfile.readline(8193)
            if len(line) > 8192 or not line.endswith(b"\n"):
                raise ValueError("request frame limit exceeded")
            response = self.server.worker.handle(json.loads(line))
        except Exception as error:
            response = {"ok": False, "error": str(error)[:1024]}
        try:
            self.wfile.write(json.dumps(response).encode() + b"\n")
        except OSError:
            pass


def serve(endpoint, state, worker):
    private_directory(endpoint.parent)
    private_directory(state)
    lock_path = endpoint.with_name(endpoint.name + ".lock")
    with lock_path.open("a") as lease:
        fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if endpoint.exists() or endpoint.is_symlink():
            if not stat.S_ISSOCK(endpoint.lstat().st_mode):
                raise ValueError("refusing to replace a non-socket endpoint")
            with socket.socket(socket.AF_UNIX) as probe:
                probe.settimeout(1)
                try:
                    probe.connect(str(endpoint))
                except OSError as error:
                    if error.errno != errno.ECONNREFUSED:
                        raise
                else:
                    raise RuntimeError("worker endpoint already active")
            endpoint.unlink()
        try:
            worker.handle({"op": "preflight"})
            with Server(str(endpoint), Handler) as server:
                os.chmod(endpoint, 0o600)
                server.worker = worker
                server.serve_forever(poll_interval=0.1)
        finally:
            worker.stop()
            endpoint.unlink(missing_ok=True)


def call(endpoint, request):
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(200)
        connection.connect(str(endpoint))
        message = json.dumps(request).encode() + b"\n"
        if len(message) > 8192:
            raise ValueError("request frame limit exceeded")
        connection.sendall(message)
        with connection.makefile("rb") as incoming:
            response = incoming.readline(8193)
        if len(response) > 8192 or not response.endswith(b"\n"):
            raise ValueError("invalid worker response frame")
        result = json.loads(response)
        if result.get("ok") is not True:
            raise RuntimeError(result.get("error", "worker request rejected"))
        return result


def main(args):
    if args and args[0] == "_child":
        return child_guard(*args[1:])
    replay = Path(
        os.environ.get(
            "NORTHSTAR_GPU_REPLAY_DIR",
            Path(__file__).resolve().parents[1] / "zkvm-replay",
        )
    ).resolve()
    manifest = json.loads((replay / "partial-candidate-v1.json").read_text())
    if len(args) == 3 and args[0] == "serve":
        endpoint, state = Path(args[1]).absolute(), Path(args[2]).absolute()
        prover = Path(
            os.environ.get(
                "NORTHSTAR_GPU_PROVER",
                replay / "target/release/northstar-zkvm-replay-script",
            )
        ).resolve()
        worker = Worker(state, prover, replay / "fixture-v1.bin", manifest)
        return serve(endpoint, state, worker)
    endpoint = Path(os.environ["NORTHSTAR_GPU_WORKER_SOCKET"])
    if args == ["preflight"]:
        result = call(endpoint, {"op": "preflight"})
        if result["program_vkey_hash"] != manifest["program_vkey_hash"]:
            raise ValueError("worker program key mismatch")
        print(json.dumps(result))
        return
    if len(args) != 4 or args[0] != "groth16":
        raise ValueError(
            "usage: gpu-worker.py serve SOCKET STATE | preflight | groth16 WITNESS MEASUREMENTS PROFILE"
        )
    paths = [Path(name) for name in FILES] + [Path(args[2])]
    if any(path.exists() or path.is_symlink() for path in paths):
        raise ValueError("refusing to overwrite proof artifacts")
    witness = Path(args[1]).resolve()
    expected_hash = hashlib.sha256(bounded_witness(witness)).hexdigest()
    request = {"op": "prove", "witness": str(witness), "profile": args[3]}
    if "NORTHSTAR_GPU_REQUEST_TIMEOUT" in os.environ:
        request["timeout_s"] = int(os.environ["NORTHSTAR_GPU_REQUEST_TIMEOUT"])
    result = call(endpoint, request)
    if result["witness_sha256"] != expected_hash:
        raise ValueError("witness binding mismatch")
    work = Path(result["directory"])
    measurements = json.loads((work / "measurements.json").read_text())
    guards.validate(
        measurements,
        (work / FILES[1]).read_bytes(),
        (work / FILES[2]).read_bytes(),
        manifest,
    )
    for source, destination in zip(
        [work / name for name in FILES] + [work / "measurements.json"], paths
    ):
        with source.open("rb") as incoming, destination.open("xb") as outgoing:
            shutil.copyfileobj(incoming, outgoing)


if __name__ == "__main__":
    os.umask(0o077)
    for sig in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
        # socketserver's selector swallows InterruptedError; shutdown must escape it.
        signal.signal(sig, lambda signum, _frame: sys.exit(128 + signum))
    try:
        main(sys.argv[1:])
    except (Exception, KeyboardInterrupt) as error:
        print(f"GPU worker rejected: {error}", file=sys.stderr)
        sys.exit(1)

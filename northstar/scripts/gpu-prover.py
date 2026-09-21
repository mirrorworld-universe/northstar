#!/usr/bin/env python3
"""Bounded local CUDA prover adapter for NORTHSTAR_LIVE_PROVER."""
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys


def run(command, timeout, *, env=None):
    process = subprocess.Popen(command, env=env, start_new_session=True)
    try:
        result = process.wait(timeout=timeout)
        if result:
            raise RuntimeError(f"command failed with exit status {result}")
    except BaseException:
        # Include descendants (GPU server and native wrapper), not just the parent.
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        process.wait()
        raise


def validate(measurements, proof, public, manifest):
    if measurements["program_vkey_hash"] != manifest["program_vkey_hash"]:
        raise ValueError("program key mismatch")
    if len(proof) != 356 or len(public) != 256 or proof[:4].hex() != "4388a21c":
        raise ValueError("proof envelope mismatch")
    if public.hex() != measurements["public_inputs"]:
        raise ValueError("public input mismatch")
    phase = next(p for p in measurements["phases"] if p["phase"] == "groth16")
    if not 0 < phase["prove_and_wrap_ms"] <= 120_000:
        raise ValueError("prove-and-wrap limit exceeded")


def main(args):
    replay = Path(os.environ.get("NORTHSTAR_GPU_REPLAY_DIR", Path(__file__).resolve().parents[1] / "zkvm-replay")).resolve()
    prover = Path(os.environ.get("NORTHSTAR_GPU_PROVER", replay / "target/release/northstar-zkvm-replay-script")).resolve()
    manifest = json.loads((replay / "partial-candidate-v1.json").read_text())
    if not prover.is_file() or not os.access(prover, os.X_OK):
        raise ValueError("prebuild NORTHSTAR_GPU_PROVER before opening a challenge")
    if args == ["preflight"]:
        import tempfile
        fixture = replay / "fixture-v1.bin"
        if hashlib.sha256(fixture.read_bytes()).hexdigest() != manifest["baseline_fixture_sha256"]:
            raise ValueError("fixture hash mismatch")
        run(["nvidia-smi", "--query-gpu=name,driver_version,memory.total", "--format=csv,noheader"], 10)
        with tempfile.TemporaryDirectory() as work:
            key = Path(work) / "key.json"
            run([str(prover), "key", str(fixture), str(key), "preflight"], 180, env={**os.environ, "SP1_PROVER": "cuda"})
            if json.loads(key.read_text())["program_vkey_hash"] != manifest["program_vkey_hash"]:
                raise ValueError("embedded program key mismatch")
        return
    if len(args) != 4 or args[0] != "groth16":
        raise ValueError("usage: gpu-prover.py preflight | groth16 WITNESS MEASUREMENTS PROFILE")
    measurement = Path(args[2])
    paths = [measurement, Path("northstar-sp1-groth16.bin"), Path("northstar-sp1-groth16-onchain.bin"), Path("northstar-sp1-public-inputs.bin")]
    if any(path.exists() for path in paths):
        raise ValueError("refusing to overwrite prior proof artifacts")
    run([str(prover), *args], 180, env={**os.environ, "SP1_PROVER": "cuda"})
    validate(json.loads(measurement.read_text()), paths[2].read_bytes(), paths[3].read_bytes(), manifest)


def interrupted(signum, _frame):
    raise InterruptedError(f"runner interrupted by signal {signum}")


if __name__ == "__main__":
    for signum in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
        signal.signal(signum, interrupted)
    try:
        main(sys.argv[1:])
    except (OSError, RuntimeError, ValueError, KeyError, StopIteration, subprocess.TimeoutExpired) as error:
        print(f"GPU prover rejected: {error}", file=sys.stderr)
        sys.exit(1)

#!/usr/bin/env python3
"""Exercise an already running local worker; never provision or start services."""
import argparse
import concurrent.futures
import json
import math
import os
from pathlib import Path
import socket
import subprocess
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--requests", type=int, default=30)
    args = parser.parse_args()
    if not 1 <= args.requests <= 100:
        parser.error("requests must be from 1 through 100")
    root = Path(__file__).resolve().parents[1]
    runner = root / "scripts/gpu-worker.py"
    fixture = root / "zkvm-replay/fixture-v1.bin"
    endpoint = os.environ["NORTHSTAR_GPU_WORKER_SOCKET"]
    output = args.output.resolve()
    output.mkdir(mode=0o700)
    allowed = {"PATH", "HOME", "LD_LIBRARY_PATH", "SSL_CERT_FILE", "NIX_SSL_CERT_FILE"}
    environment = {
        key: value for key, value in os.environ.items()
        if key in allowed or key.startswith(("NORTHSTAR_GPU_", "CUDA_"))
    }

    def request(data):
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(200)
            connection.connect(endpoint)
            connection.sendall(json.dumps(data).encode() + b"\n")
            return json.loads(connection.makefile("rb").readline())

    def prove(label, deadline=180):
        directory = output / label
        directory.mkdir(mode=0o700)
        started = time.monotonic()
        with (directory / "request.log").open("w") as log:
            result = subprocess.run(
                [str(runner), "groth16", str(fixture), "measurements.json", label],
                cwd=directory,
                env={**environment, "NORTHSTAR_GPU_REQUEST_TIMEOUT": str(deadline)},
                stdout=log, stderr=subprocess.STDOUT, timeout=deadline + 20,
            )
        return result.returncode, time.monotonic() - started

    started = time.monotonic()
    assert request({"op": "preflight"})["warmed"]
    summary = {
        "schema": "northstar-worker-stress-v1",
        "preflight_s": time.monotonic() - started,
        "serial": [],
    }
    for index in range(args.requests):
        code, wall = prove(f"serial-{index:02}")
        assert code == 0, (index, code)
        measurement = json.loads((output / f"serial-{index:02}/measurements.json").read_text())
        phase = next(phase for phase in measurement["phases"] if phase["phase"] == "groth16")
        assert phase["prove_and_wrap_ms"] <= 120_000
        summary["serial"].append({
            "index": index, "request_wall_s": wall,
            "prove_and_wrap_ms": phase["prove_and_wrap_ms"], "verify_ms": phase["verify_ms"],
        })
        (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")

    with concurrent.futures.ThreadPoolExecutor() as pool:
        pending = pool.submit(prove, "occupied-worker")
        deadline = time.monotonic() + 10
        while True:
            started = time.monotonic()
            reply = request({"op": "preflight"})
            elapsed = time.monotonic() - started
            if not reply["ok"]:
                assert "busy" in reply["error"] and elapsed < 2
                summary["busy_rejection_s"] = elapsed
                break
            assert time.monotonic() < deadline
            time.sleep(0.01)
        code, wall = pending.result()
        assert code == 0
        summary["occupied_request_s"] = wall

    code, wall = prove("cancelled", 10)
    assert code != 0 and wall < 20
    assert "deadline" in (output / "cancelled/request.log").read_text().lower()
    summary["cancellation_s"] = wall
    code, wall = prove("immediate-retry")
    assert code == 0
    summary["immediate_retry_with_restart_s"] = wall
    values = sorted(row["prove_and_wrap_ms"] for row in summary["serial"])
    summary.update({
        "steady_min_ms": values[0],
        "steady_p95_ms": values[math.ceil(0.95 * len(values)) - 1],
        "steady_max_ms": values[-1], "passed": True,
    })
    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")


if __name__ == "__main__":
    main()

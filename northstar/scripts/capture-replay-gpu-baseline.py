#!/usr/bin/env python3
"""Capture or inspect development-only restricted replay GPU measurements."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile


CLASSIFICATION = "legacy-restricted-replay-development-only"


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def integer(value, name, minimum=0):
    if type(value) is not int or value < minimum:
        raise ValueError(f"invalid {name}")
    return value


def artifact(root, name):
    if not isinstance(name, str) or Path(name).is_absolute():
        raise ValueError("artifact must use a relative path")
    path = (root / name).resolve(strict=True)
    if not path.is_relative_to(root) or not path.is_file():
        raise ValueError("artifact is outside measurement directory")
    return path


def require_legacy_candidate(candidate):
    if (candidate["schema"] != "northstar-replay-candidate-v1"
            or type(candidate["proof_kind"]) is not int or candidate["proof_kind"] != 2
            or type(candidate["proof_version"]) is not int or candidate["proof_version"] != 1):
        raise ValueError("only the legacy restricted replay candidate is supported")


def summarize(path, candidate, samples=3):
    """Check measurement structure, not cryptographic validity or acceptance."""
    require_legacy_candidate(candidate)
    path = Path(path).resolve(strict=True)
    data = json.loads(path.read_text())
    if data["schema"] != "northstar-sp1-benchmark-v2":
        raise ValueError("unsupported measurement schema")
    if data["program_vkey_hash"] != candidate["program_vkey_hash"]:
        raise ValueError("program key mismatch")
    public = bytes.fromhex(data["public_inputs"])
    if len(public) != 256 or candidate["public_input_bytes"] != 256:
        raise ValueError("public input length mismatch")
    phases = data["phases"]
    if not isinstance(phases, list):
        raise ValueError("invalid phases")
    setup = [row for row in phases if row["phase"] == "setup"]
    rows = [row for row in phases if row["phase"] == "groth16"]
    if len(setup) != 1 or len(rows) != samples or len(phases) != samples + 1:
        raise ValueError("expected one setup and exact Groth16 sample count")
    result = []
    used_paths = set()
    for index, row in enumerate(rows):
        if type(row["sample"]) is not int or row["sample"] != index:
            raise ValueError("samples must be ordered, unique and consecutive")
        prove = integer(row["prove_and_wrap_ms"], "prove_and_wrap_ms", 1)
        verify = integer(row["verify_ms"], "verify_ms")
        if type(row["onchain_proof_bytes"]) is not int or row["onchain_proof_bytes"] != 356:
            raise ValueError("proof length metadata mismatch")
        proof_path = artifact(path.parent, row["onchain_proof_path"])
        public_path = artifact(path.parent, row["public_inputs_path"])
        if proof_path == public_path or {proof_path, public_path} & used_paths:
            raise ValueError("sample artifact paths must be distinct")
        used_paths.update((proof_path, public_path))
        proof = proof_path.read_bytes()
        if len(proof) != 356 or proof[:4].hex() != "4388a21c" or proof[4:36] != bytes(32):
            raise ValueError("proof envelope mismatch")
        if public_path.read_bytes() != public:
            raise ValueError("public input bytes mismatch")
        result.append({
            "sample": index,
            "process_cold": index == 0,
            "prove_and_wrap_ms": prove,
            "sdk_verify_ms": verify,
            "prove_wrap_and_verify_ms": prove + verify,
            "within_120s_prove_wrap": prove <= 120_000,
            "proof_sha256": digest(proof_path),
            "public_inputs_sha256": digest(public_path),
        })
    return {
        "schema": "northstar-replay-gpu-measurement-summary-v1",
        "classification": CLASSIFICATION,
        "validation_level": "measurement-structure-only",
        "program_vkey_hash": data["program_vkey_hash"],
        "measurements_sha256": digest(path),
        "client_init_ms": integer(data["client_init_ms"], "client_init_ms"),
        "setup_ms": integer(setup[0]["wall_ms"], "setup_ms"),
        "cold_definition": "first sample in one client; circuit cache state not established",
        "samples": result,
        "portal_resolution_compute_units": None,
        "vrf_execution_proof": False,
        "l40s_acceptance": False,
        "production_acceptance": False,
    }


def checked_hash(path, expected, label):
    path = Path(path).resolve(strict=True)
    if digest(path) != expected:
        raise ValueError(f"{label} hash mismatch")
    return path


def run(command, directory, log, timeout):
    with (directory / log).open("xb") as output:
        process = subprocess.Popen(command, cwd=directory, stdout=output,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        try:
            code = process.wait(timeout=timeout)
            if code:
                raise subprocess.CalledProcessError(code, command)
        except BaseException:
            try:
                os.killpg(process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                pass
            # The launcher may exit before one of its descendants.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait(timeout=5)
            raise


def capture(args):
    # The reviewed launcher owns locking, resource limits and descendant cleanup.
    # GNU timeout terminates it through its installed signal/EXIT handlers.
    if not args.execute_reviewed_launcher:
        raise ValueError("capture requires --execute-reviewed-launcher after coordinator review")
    candidate_path = Path(args.candidate).resolve(strict=True)
    retained_path = Path(args.retained_client).resolve(strict=True)
    candidate = json.loads(candidate_path.read_text())
    require_legacy_candidate(candidate)
    retained = json.loads(retained_path.read_text())
    if retained["program_vkey_hash"] != candidate["program_vkey_hash"]:
        raise ValueError("retained client and candidate key mismatch")
    client = checked_hash(args.client, retained["run"]["prover_binary_sha256"], "client")
    server = checked_hash(args.server, retained["run"]["server_binary_sha256"], "server")
    fixture = checked_hash(args.fixture, candidate["baseline_fixture_sha256"], "fixture")
    launcher = checked_hash(args.launcher, args.launcher_sha256, "launcher")
    launcher_env = checked_hash(args.launcher_env, args.launcher_env_sha256, "launcher environment")
    elf_sha256 = candidate["elf_sha256"]
    parent = Path(args.output_parent).resolve(strict=True)
    out = Path(tempfile.mkdtemp(prefix="replay-development-", dir=parent))
    print(out, flush=True)
    metadata = {
        "schema": "northstar-replay-gpu-capture-v1",
        "classification": CLASSIFICATION,
        "started_at": datetime.now(timezone.utc).isoformat(),
        "client_sha256": digest(client),
        "server_sha256": digest(server),
        "launcher_sha256": digest(launcher),
        "launcher_env_sha256": digest(launcher_env),
        "fixture_sha256": digest(fixture),
        "candidate_sha256": digest(candidate_path),
        "retained_client_record_sha256": digest(retained_path),
        "candidate_elf_sha256_not_independently_extracted": elf_sha256,
        "program_vkey_hash": candidate["program_vkey_hash"],
        "launch_contract": "reviewed launcher owns shared device lock and isolated resource scope",
        "production_acceptance": False,
        "status": "started",
    }
    status = 1
    try:
        (out / "CLASSIFICATION.txt").write_text(
            "Legacy restricted replay development baseline. NOT VRF, NOT L40S acceptance, "
            "NOT live Portal resolution.\n")
        run(["nvidia-smi", "--query-gpu=uuid,name,driver_version,memory.total", "--format=csv"],
            out, "gpu.csv", 15)
        prefix = ["timeout", "--signal=TERM", "--kill-after=60s", "15m", str(launcher), str(client)]
        run([*prefix, "key", str(fixture), str(out / "key.json"), "development-key-preflight"],
            out, "key.log", 970)
        if json.loads((out / "key.json").read_text())["program_vkey_hash"] != candidate["program_vkey_hash"]:
            raise ValueError("installed client embedded key mismatch")
        run([*prefix, "groth16", str(fixture), str(out / "measurements.json"),
             "legacy-replay-development", "3"], out, "prove.log", 970)
        summary = summarize(out / "measurements.json", candidate)
        for path, expected, label in (
            (client, metadata["client_sha256"], "client"),
            (server, metadata["server_sha256"], "server"),
            (launcher, metadata["launcher_sha256"], "launcher"),
            (launcher_env, metadata["launcher_env_sha256"], "launcher environment"),
            (fixture, metadata["fixture_sha256"], "fixture"),
        ):
            checked_hash(path, expected, label)
        (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        metadata["status"] = "client-exited-successfully-and-measurement-structure-checked"
        status = 0
    except BaseException as error:
        metadata["error"] = str(error)
        metadata["status"] = "failed; partial artifacts retained"
        raise
    finally:
        metadata["exit_code"] = status
        metadata["finished_at"] = datetime.now(timezone.utc).isoformat()
        (out / "capture.json").write_text(json.dumps(metadata, indent=2) + "\n")
        (out / "exit").write_text(f"{status}\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    inspect = sub.add_parser("summarize", help="inspect existing three-sample measurements; no GPU")
    inspect.add_argument("measurements")
    inspect.add_argument("--candidate", required=True)
    launch = sub.add_parser("capture", help="coordinator-only proposed preinstalled-client capture")
    for name in ("candidate", "retained-client", "client", "server", "fixture", "launcher",
                 "launcher-sha256", "launcher-env", "launcher-env-sha256", "output-parent"):
        launch.add_argument(f"--{name}", required=True)
    launch.add_argument("--execute-reviewed-launcher", action="store_true")
    args = parser.parse_args()
    if args.command == "capture":
        capture(args)
    else:
        print(json.dumps(summarize(args.measurements, json.loads(Path(args.candidate).read_text())),
                         indent=2))


def interrupted(signum, _frame):
    raise InterruptedError(f"capture interrupted by signal {signum}")


if __name__ == "__main__":
    for signum in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
        signal.signal(signum, interrupted)
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f"GPU capture rejected: {error}", file=sys.stderr)
        sys.exit(1)

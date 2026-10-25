#!/usr/bin/env python3
"""Check retained artifact consistency, not proof validity or acceptance."""
import gzip
import hashlib
import json
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]
PACKAGE = REPO / "northstar/zkvm-vrf/evidence/proof-time-v1"
KEY = REPO / "northstar/programs/portal/keys/proof-time-wordwise-v3"


def load(path):
    return json.loads(path.read_text())


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def check():
    inventory = load(PACKAGE / "checksums.json")
    recorded = set()
    for entry in inventory["files"]:
        path = (PACKAGE / entry["file"]).resolve(strict=True)
        assert path.is_relative_to(PACKAGE) and path.is_file()
        assert path.stat().st_size == entry["bytes"]
        assert sha(path) == entry["sha256"]
        assert entry["file"] not in recorded
        recorded.add(entry["file"])
    actual = {str(p.relative_to(PACKAGE)) for p in PACKAGE.rglob("*") if p.is_file()}
    assert actual == recorded | {"checksums.json"}

    summary = load(PACKAGE / "summary.json")
    attempts = load(PACKAGE / "attempts.json")
    controls = load(PACKAGE / "guest-controls.json")
    receipt = load(KEY / "setup-receipt.json")
    assert summary["status"] == "target-not-met"
    assert summary["production_acceptance"] is False
    assert summary["fresh_qualification_cases_completed"] == 0
    assert summary["new_configured_portal_resolutions"] == 0
    assert receipt["production_acceptance"] is False
    assert sha(KEY / "verifying-key.bin") == receipt["verifying_key_sha256"]
    assert sha(KEY / "profile.json") == receipt["profile_sha256"]
    for entry in load(KEY / "source.json")["guest_relevant_source_files"]:
        path = (REPO / entry["file"]).resolve(strict=True)
        assert path.is_relative_to(REPO) and sha(path) == entry["sha256"]

    runs = attempts["runs"]
    assert len(runs) == 16 and len({r["id"] for r in runs}) == 16
    assert all(not r["fresh"] and not r["acceptance_window"] for r in runs)
    assert sum(len(r["proofs"]) for r in runs) == 15
    assert sum(r["attempts_started"] for r in runs) == 17
    best = next(r for r in runs if r["id"] == summary["best_diagnostic_id"])
    proof = best["proofs"][0]
    identity = best["artifact_identities"]
    assert proof["sdk_verified"] and proof["prove_and_wrap_ms"] == 142137
    assert proof["prove_and_wrap_ms"] > summary["target_prove_and_wrap_ms"]
    assert identity["program_vkey_hash"] == receipt["program_vkey_hash"]
    assert identity["elf_sha256"] == receipt["elf_sha256"]
    assert sha(PACKAGE / "best-proof/sdk-proof.bin") == proof["sdk_proof_sha256"]
    public = PACKAGE / "best-proof/public-inputs.bin"
    assert public.stat().st_size == 256 and sha(public) == proof["public_inputs_sha256"]
    witness = (PACKAGE / summary["same_historical_input"]).resolve(strict=True)
    assert witness.is_relative_to(REPO)
    assert hashlib.sha256(gzip.decompress(witness.read_bytes())).hexdigest() == identity["input_sha256"]

    assert len(controls["controls"]) == 17
    assert sum(c["expected_rejection"] for c in controls["controls"]) == 15
    for case in controls["controls"]:
        execution = case["execution"]
        assert case["passed"] and execution["strict_observer_passed"]
        if case["expected_rejection"]:
            assert execution["exit_code"] != 0 and execution["public_output_bytes"] == 0
        else:
            assert execution["exit_code"] == 0 and execution["public_output_bytes"] == 256
    assert len(controls["valid_rollback_executions"]) == 2
    assert len(controls["native_cases"]) == 22
    assert len(controls["native_source_associations"]) == 12
    assert all(c["passed"] for c in controls["native_cases"] + controls["native_source_associations"])
    print("Artifact consistency passed. No new SDK verification or acceptance is claimed.")


if __name__ == "__main__":
    check()

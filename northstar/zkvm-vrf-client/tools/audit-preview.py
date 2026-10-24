#!/usr/bin/env python3
"""Artifact consistency audit for one retained preview proof; not a verifier."""
import argparse
import hashlib
import json
from pathlib import Path

FR = 21888242871839275222246405745257275088548364400416034343698204186575808495617
ROOT = bytes.fromhex("002f850ee998974d6cc00e50cd0814b098c05bfade466d28573240d057f25352")


def digest(path):
    with Path(path).open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def load(path):
    def unique(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError(f"duplicate JSON field: {key}")
            result[key] = value
        return result
    return json.loads(Path(path).read_text(), object_pairs_hook=unique)


def require(ok, message):
    if not ok:
        raise ValueError(message)


def bounded_artifact(root, name):
    require(isinstance(name, str) and not Path(name).is_absolute(), "relative artifact required")
    path = (root / name).resolve(strict=True)
    require(path.is_relative_to(root) and path.is_file(), "artifact outside result directory")
    return path


def public_values(raw):
    require(len(raw) == 256, "public length")
    label = b"northstar-er-step-v1"
    domain = bytearray(32)
    domain[8:8 + len(label)] = label
    domain[30:] = bytes([3, 1])
    fields = [int.from_bytes(raw[i:i + 32], "big") for i in range(0, 256, 32)]
    require(raw[:32] == domain and all(value < FR for value in fields), "domain/canonical fields")
    require(all(fields[index] != 0 for index in (1, 3, 4, 5, 6, 7)), "missing commitment")


def audit(result, elf, native_public, native_input, independent_setup, independent_execute, source_files, client_hash_file):
    result = Path(result).resolve(strict=True)
    m = load(result / "measurements.json")
    require(m["schema"] == "northstar-vrf-sp1-client-v1" and m["status"] == "completed", "incomplete client result")
    require(m["exit_code"] == 0 and m["backend"] == "cuda" and m["command"] == "groth16", "not completed CUDA proving")
    require(m["proof_kind"] == 3 and m["proof_version"] == 1 and m["production_acceptance"] is False, "candidate scope")
    rows = [p for p in m["phases"] if p["phase"] == "groth16"]
    require(len(rows) == 1 and m["proof_attempts_started"] == 1, "one preview attempt required")
    row = rows[0]
    require(row["sdk_verified"] is True and row["acceptance_window"] is False, "preview/SDK report scope")
    require(row["attempt_sequence"] == 0 and row["sample"] == 0 and row["process_cold"] is True, "single first-process preview required")
    require(m["declared_window_proof_target_met"] is None, "preview cannot claim final window")
    for field in ("prove_and_wrap_ms", "verify_ms"):
        require(type(row[field]) is int and row[field] > 0, f"invalid {field}")
    target = row["prove_and_wrap_ms"] <= 120_000
    require(row["within_120s_prove_wrap"] is target and m["proof_target_met"] is target, "target misreported")
    public = bounded_artifact(result, row["public_inputs_path"])
    proof = bounded_artifact(result, row["onchain_proof_path"])
    sdk = bounded_artifact(result, row["sdk_proof_path"])
    raw = public.read_bytes()
    public_values(raw)
    require(raw == Path(native_public).read_bytes() == (result / "execution-public-values.bin").read_bytes(), "native/CPU/public mismatch")
    require(raw.hex() == m["public_inputs"], "metadata public mismatch")
    envelope = proof.read_bytes()
    require(len(envelope) == 356 and row["onchain_proof_bytes"] == 356, "proof length")
    require(envelope[:4].hex() == "4388a21c" and envelope[4:36] == bytes(32) and envelope[36:68] == ROOT, "proof envelope/root/exit")
    require(int.from_bytes(envelope[68:100], "big") < FR, "noncanonical nonce")
    require(digest(public) == row["public_inputs_sha256"] == m["expected_public_inputs_sha256"], "public hash")
    require(digest(proof) == row["proof_sha256"] and digest(sdk) == row["sdk_proof_sha256"], "proof hashes")
    require((result / "sample-0-unverified-sdk-proof.bin").read_bytes() == sdk.read_bytes(), "generated SDK bytes changed")
    require(digest(result / "input.bin") == digest(native_input) == m["input_sha256"], "native input mismatch")
    profile = load(result / "profile.json")
    require(profile["proof_kind"] == 3 and profile["proof_version"] == 1 and profile["checkpoint_steps"] == 1 and profile["production_acceptance"] is False and profile["guest_status"] == "authenticated-envelope", "deployment candidate scope")
    receipt = load(result / "setup-receipt.json")
    require(digest(elf) == profile["elf_sha256"] == receipt["elf_sha256"] == m["elf_sha256"], "ELF identity")
    header = Path(elf).read_bytes()[:64]
    require(len(header) == 64 and header[:7] == bytes([127, 69, 76, 70, 2, 1, 1]) and header[18:20] == bytes([243, 0]) and header[48:52] == bytes(4), "RV64IM ELF header")
    require(digest(result / "profile.json") == m["profile_sha256"] == receipt["profile_sha256"], "deployment profile hash")
    require(profile["policy_profile_sha256"] == m["policy_profile_sha256"] == receipt["policy_profile_sha256"], "typed policy hash")
    key = m["program_vkey_hash"]
    require(key == receipt["program_vkey_hash"] and key.startswith("0x") and len(key) == 66 and all(c in "0123456789abcdef" for c in key[2:]) and 0 < int(key, 16) < FR, "program key")
    require(receipt["schema"] == "northstar-sp1-setup-v1" and receipt["sp1_version"] == m["sp1_version"] == "6.8.0" and receipt["production_acceptance"] is False, "SDK receipt")
    require(digest(result / "verifying-key.bin") == receipt["verifying_key_sha256"] == m["verifying_key_sha256"], "serialized key")
    require(digest(result / "setup-receipt.json") == m["setup_receipt_sha256"] == m["input_setup_receipt_sha256"], "setup receipt hash")
    require((result / "setup-receipt.json").read_bytes() == (result / "input-setup-receipt.json").read_bytes(), "input setup receipt changed")
    independent = {}
    for name, directory in (("setup", independent_setup), ("execute", independent_execute)):
        directory = Path(directory)
        other = load(directory / "measurements.json")
        require(other["status"] == "completed" and other["exit_code"] == 0 and other["backend"] == "cpu", "independent CPU run incomplete")
        require(other["program_vkey_hash"] == key and other["elf_sha256"] == m["elf_sha256"] and other["profile_sha256"] == m["profile_sha256"], "independent setup identity")
        require((directory / "verifying-key.bin").read_bytes() == (result / "verifying-key.bin").read_bytes(), "independent SDK key bytes")
        if name == "execute":
            require((directory / "execution-public-values.bin").read_bytes() == raw, "independent CPU output")
            execution = [p for p in other["phases"] if p["phase"] == "execute"]
            require(len(execution) == 1 and execution[0]["exit_code"] == 0 and execution[0]["public_output_bytes"] == 256, "independent execution report")
            independent["execute_ms"] = execution[0]["wall_ms"]
            independent["cycles"] = execution[0]["cycles"]
    require(m["client_sha256"] == Path(client_hash_file).read_text().split()[0], "retained client build hash")
    source_digest = hashlib.sha256(b"".join(Path(p).read_bytes() for p in source_files)).hexdigest()
    require(source_digest == m["source_sha256"], "frozen client source hash")
    return {
        "schema": "northstar-vrf-preview-component-audit-v1",
        "validation_level": "artifact-consistency-only-not-cryptographic-verification",
        "cryptographic_verification_performed": False,
        "generation_client_reported_sdk_verified": True,
        "classification": "preview-session-request-process-cold-warmup",
        "final_three_case_window": False,
        "sp1_sdk_version": m["sp1_version"], "sp1_circuit_version": m["sp1_circuit_version"],
        "elf_sha256": m["elf_sha256"], "deployment_profile_sha256": m["profile_sha256"],
        "typed_policy_sha256": m["policy_profile_sha256"], "program_vkey_hash": key,
        "client_sha256": m["client_sha256"], "client_source_sha256": source_digest,
        "client_cargo_lock_sha256": m["cargo_lock_sha256"],
        "input_sha256": m["input_sha256"], "public_inputs_sha256": digest(public),
        "onchain_proof_sha256": digest(proof), "sdk_proof_sha256": digest(sdk),
        "verifying_key_sha256": m["verifying_key_sha256"],
        "setup_receipt_sha256": digest(result / "setup-receipt.json"),
        "raw_measurements_sha256": digest(result / "measurements.json"),
        "client_init_ms": m["client_init_ms"], "setup_ms": next(p["wall_ms"] for p in m["phases"] if p["phase"] == "setup"),
        "prove_and_wrap_ms": row["prove_and_wrap_ms"], "sdk_verify_ms": row["verify_ms"],
        "prove_wrap_and_verify_ms": row["prove_and_wrap_ms"] + row["verify_ms"],
        "proof_target_ms": 120000, "proof_target_met": target,
        "independent_cpu": independent, "complete_resolution_compute_units": None,
        "resource_peaks": None, "production_acceptance": False,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("result", "elf", "native-public", "native-input", "independent-setup", "independent-execute", "client-hash-file"):
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--source-file", action="append", required=True)
    args = parser.parse_args()
    print(json.dumps(audit(args.result, args.elf, args.native_public, args.native_input,
                           args.independent_setup, args.independent_execute, args.source_file, args.client_hash_file), indent=2))


if __name__ == "__main__":
    main()

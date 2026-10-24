#!/usr/bin/env python3
"""Stage a separately pinned development key. Never overwrite accepted keys.

The SDK receipt is an artifact-consistency record, not an authenticated setup
attestation. Independently reproduce SDK setup and review the expected hash
before installing output; real proof/full-resolver validation remains required.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re

PORTAL = Path(__file__).resolve().parents[1]
FR = 21888242871839275222246405745257275088548364400416034343698204186575808495617
FQ = 21888242871839275222246405745257275088696311157297823662689037894645226208583
OUTER_SHA256 = "4388a21c687fdd5f218d7e3d13190cac4c5355818d3605fd5fb811df468ee696"
SOURCE_SHA256 = "bb2d2774c3f0a009456b3f4072103da5404018e856cee5eb55355449b98d5056"
LEGACY_HASH = "0063a10e31e90625b76d06bfa22cbf0a4a8fa0850ccf71e063d243bd1a7aa31b"
ROOT = "002f850ee998974d6cc00e50cd0814b098c05bfade466d28573240d057f25352"
FIELDS = ["domain", "session_context", "slot_step", "pre_state_root", "post_state_root",
          "tx_effect_root", "readonly_l1_root", "settlement_effect_root"]


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def load_json(data):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate JSON field: {key}")
            result[key] = value
        return result
    return json.loads(data, object_pairs_hook=unique)


def scalar(value):
    if not isinstance(value, str) or not re.fullmatch(r"0x[0-9a-f]{64}", value):
        raise ValueError("key must be a canonical lowercase 0x-prefixed 32-byte field")
    result = int(value, 16)
    if not 0 < result < FR:
        raise ValueError("key must be nonzero and canonical BN254 Fr")
    return result


def decode_point(data):
    if len(data) != 64:
        raise ValueError("G1 point length")
    if data == bytes(64):
        return None
    x, y = int.from_bytes(data[:32], "big"), int.from_bytes(data[32:], "big")
    if x >= FQ or y >= FQ or (y * y - x * x * x - 3) % FQ:
        raise ValueError("invalid BN254 G1 point")
    return x, y


def encode_point(point):
    if point is None:
        return bytes(64)
    return b"".join(value.to_bytes(32, "big") for value in point)


def add(left, right):
    if left is None:
        return right
    if right is None:
        return left
    x, y = left
    u, v = right
    if x == u and (y + v) % FQ == 0:
        return None
    slope = ((3 * x * x) * pow(2 * y, -1, FQ) if left == right
             else (v - y) * pow(u - x, -1, FQ)) % FQ
    result_x = (slope * slope - x - u) % FQ
    return result_x, (slope * (x - result_x) - y) % FQ


def multiply(point, value):
    result = None
    while value:
        if value & 1:
            result = add(result, point)
        point = add(point, point)
        value >>= 1
    return result


def source_points(source):
    if sha256(source) != SOURCE_SHA256:
        raise ValueError("legacy uncompressed key source hash mismatch")
    text = source.decode().split("const SP1_GROTH16_VK_IC:", 1)[1].split(
        "pub const SP1_GROTH16_VERIFYING_KEY", 1)[0]
    raw = bytes(int(value, 16) for value in re.findall(r"0x([0-9a-f]{2})", text))
    if len(raw) != 6 * 64:
        raise ValueError("expected six original IC points")
    # The pinned legacy source has a noncanonical unused exit-code IC. Exit code
    # is strictly zero in this envelope; never use that point or rewrite legacy.
    return [None if index == 3 * 64 else decode_point(raw[index:index + 64])
            for index in range(0, len(raw), 64)]


def outer_points(outer):
    if sha256(outer) != OUTER_SHA256 or outer[288:292] != (6).to_bytes(4, "big"):
        raise ValueError("outer Groth16 key hash/layout mismatch")
    points = []
    for index in range(6):
        compressed = int.from_bytes(outer[292 + index * 32:324 + index * 32], "big")
        tag, x = compressed >> 254, compressed & ((1 << 254) - 1)
        if tag not in [2, 3] or x >= FQ:
            raise ValueError("invalid Gnark compressed IC")
        y = pow((x * x * x + 3) % FQ, (FQ + 1) // 4, FQ)
        y = min(y, FQ - y) if tag == 2 else max(y, FQ - y)
        points.append(decode_point(x.to_bytes(32, "big") + y.to_bytes(32, "big")))
    return points


def fold(points, program_key):
    result = add(add(points[0], multiply(points[1], program_key)),
                 multiply(points[4], int(ROOT, 16)))
    if result is None:
        raise ValueError("folded IC must not be infinity")
    return encode_point(result)


def rust_array(data):
    return "[\n" + "".join(
        "        " + ", ".join(f"0x{value:02x}" for value in data[index:index + 16]) + ",\n"
        for index in range(0, len(data), 16)) + "    ]"


def generate(elf, receipt_bytes, sdk_key, profile_bytes, reviewed_hash, outer, source):
    receipt, profile = load_json(receipt_bytes), load_json(profile_bytes)
    key = scalar(reviewed_hash)
    if receipt.get("schema") != "northstar-sp1-setup-v1" or receipt.get("sp1_version") != "6.8.0":
        raise ValueError("expected SP1 6.8.0 setup receipt")
    if receipt.get("program_vkey_hash") != reviewed_hash:
        raise ValueError("reviewed program hash differs from SDK setup receipt")
    if reviewed_hash[2:] == LEGACY_HASH:
        raise ValueError("candidate must not reuse legacy guest key")
    if not elf.startswith(b"\x7fELF") or receipt.get("elf_sha256") != sha256(elf):
        raise ValueError("guest ELF hash mismatch")
    if not sdk_key or receipt.get("verifying_key_sha256") != sha256(sdk_key):
        raise ValueError("SDK verifying-key artifact hash mismatch")
    if (type(profile.get("proof_kind")) is not int or profile["proof_kind"] != 3
            or type(profile.get("proof_version")) is not int or profile["proof_version"] != 1
            or type(profile.get("checkpoint_steps")) is not int
            or not 1 <= profile["checkpoint_steps"] <= 16
            or profile.get("public_input_fields") != FIELDS
            or profile.get("production_acceptance") is not False):
        raise ValueError("profile must specify kind3/version1, checkpoint_steps 1..16, eight fields, development only")
    if (profile.get("schema") != "northstar.bounded-vrf-candidate-deployment.v1"
            or profile.get("guest_status") != "authenticated-envelope"
            or profile.get("sp1_version") != receipt["sp1_version"]
            or profile.get("elf_sha256") != sha256(elf)
            or receipt.get("profile_sha256") != sha256(profile_bytes)
            or receipt.get("production_acceptance") is not False):
        raise ValueError("deployment profile/receipt/ELF identities differ")
    policy = profile.get("policy_profile_sha256")
    if (not isinstance(policy, str) or not re.fullmatch(r"[0-9a-f]{64}", policy)
            or policy == "0" * 64 or receipt.get("policy_profile_sha256") != policy):
        raise ValueError("compiled policy identity differs or is missing")
    if sha256(outer) != OUTER_SHA256:
        raise ValueError("outer Groth16 key hash mismatch")
    points, installed = outer_points(outer), source_points(source)
    if any(point != installed[index] for index, point in enumerate(points) if index != 3):
        raise ValueError("compressed and installed variable/fixed IC points differ")
    fixed = fold(points, key)
    manifest = {
        "schema": "northstar-bounded-vrf-portal-key-v1",
        "status": "generated-requires-independent-review",
        "production_acceptance": False,
        "proof_kind": 3, "proof_version": 1,
        "checkpoint_steps": profile["checkpoint_steps"],
        "domain": (bytes(8) + b"northstar-er-step-v1" + bytes(2) + bytes([3, 1])).hex(),
        "public_input_fields": FIELDS,
        "public_input_bytes": 256,
        "program_vkey_hash": reviewed_hash,
        "fixed_inputs_ic_be": fixed.hex(),
        "sp1_version": receipt["sp1_version"],
        "elf_sha256": sha256(elf),
        "sdk_verifying_key_sha256": sha256(sdk_key),
        "setup_receipt_sha256": sha256(receipt_bytes),
        "profile_sha256": sha256(profile_bytes),
        "policy_profile_sha256": policy,
        "outer_groth16_vk_sha256": sha256(outer),
        "outer_key_rust_sha256": sha256(source),
        "sp1_groth16_vk_root": "0x" + ROOT,
        "sp1_groth16_vk_hash_prefix": OUTER_SHA256[:8],
        "proof_envelope_bytes": 356,
        "provenance_boundary": "Receipt hashes check artifact consistency, not authenticated SDK execution. Independently reproduce setup and verify actual candidate proofs before acceptance.",
    }
    rust = (
        "// Generated by tools/bounded_vrf_key.py; review manifest and reproduce SDK setup.\n"
        "use {\n"
        "    super::{verifier_key, CandidateProfile},\n"
        "    groth16_solana::groth16::Groth16Verifyingkey,\n"
        "};\n\n"
        f"pub(super) const CHECKPOINT_STEPS: u32 = {profile['checkpoint_steps']};\n\n"
        "#[rustfmt::skip]\n"
        "pub(super) static PROFILE: Option<CandidateProfile> = Some(CandidateProfile {\n"
        "    checkpoint_steps: CHECKPOINT_STEPS,\n"
        f"    program_vkey_hash: {rust_array(bytes.fromhex(reviewed_hash[2:]))},\n"
        "    fixed_inputs_key: Groth16Verifyingkey {\n"
        "        nr_pubinputs: 2,\n"
        f"        vk_ic: &[{rust_array(fixed)},\n"
        "            verifier_key::SP1_GROTH16_VERIFYING_KEY.vk_ic[2],\n"
        "            verifier_key::SP1_GROTH16_VERIFYING_KEY.vk_ic[5]],\n"
        "        ..verifier_key::SP1_GROTH16_VERIFYING_KEY\n"
        "    },\n"
        "});\n"
    ).encode()
    manifest["generated_rust_sha256"] = sha256(rust)
    return {"candidate_key.rs": rust,
            "bounded-vrf-v1.json": (json.dumps(manifest, indent=2, sort_keys=True) + "\n").encode()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=["generate", "check"])
    for option in ["elf", "setup-receipt", "sdk-verifying-key", "profile", "reviewed-program-vkey-hash", "output-dir"]:
        parser.add_argument("--" + option, required=True)
    parser.add_argument("--outer-vk", type=Path, default=PORTAL / "keys/sp1-v6.1.0-groth16-vk.bin")
    args = parser.parse_args()
    try:
        outputs = generate(
            Path(args.elf).read_bytes(), Path(args.setup_receipt).read_bytes(),
            Path(args.sdk_verifying_key).read_bytes(), Path(args.profile).read_bytes(),
            args.reviewed_program_vkey_hash, args.outer_vk.read_bytes(),
            (PORTAL / "src/instructions/verifier/verifier_key.rs").read_bytes())
        directory = Path(args.output_dir)
        if args.mode == "generate":
            directory.mkdir(parents=True, exist_ok=False)
            for name, data in outputs.items():
                (directory / name).write_bytes(data)
        else:
            for name, expected in outputs.items():
                if (directory / name).read_bytes() != expected:
                    raise ValueError(f"generated output differs: {name}")
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(1, f"error: {error}\n")


if __name__ == "__main__":
    main()

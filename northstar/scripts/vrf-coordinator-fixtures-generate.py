#!/usr/bin/env python3
"""Rebuild offline compatibility bytes from a caller-supplied pinned source tree.

No source code, binaries or package scripts from that tree are executed.
"""

import argparse
import hashlib
import json
from pathlib import Path
import re

COMMIT = "d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd"
REPOSITORY = "https://github.com/mirrorworld-universe/vrf-coordinator-program-library"
COORDINATOR = "FTDEMxTj2RsKBSCGETrFXDiSLu1nJLRfAYDsjJ6GXQ4N"
VERIFIER = "FUBz1nvSSoHnMqFoK2CA3jF5ckpYPdF3ZT4SoLzDNzh1"
CONSUMER = "HFg1j9ZMixGkMo42ZoeqNpqBZpGcVbsRwfFBLcGCn5Kt"
IDL = "sdks/packages/vrf-coordinator/src/idl/vrf_coordinator.json"
SOURCES = [
    IDL, "LICENSE", "Cargo.lock", "sdks/yarn.lock",
    "programs/vrf-coordinator/src/lib.rs",
    "programs/vrf-coordinator/src/ids.rs",
    "programs/vrf-coordinator/src/alpha.rs",
    "programs/vrf-coordinator/src/verify.rs",
    "programs/vrf-coordinator/src/utils.rs",
    "programs/vrf-coordinator/src/states/request_account.rs",
    "programs/vrf-coordinator/src/states/request_mode.rs",
    "programs/vrf-coordinator/src/instructions/request/request_random.rs",
    "programs/vrf-coordinator/src/instructions/fulfillment/fulfill.rs",
    "programs/reference-verify/src/lib.rs",
    "programs/reference-verify/src/ecvrf.rs",
    "programs/test-consumer/src/lib.rs",
    "sdks/packages/vrf-coordinator/src/alpha.ts",
    "sdks/packages/vrf-coordinator/src/vrf-coordinator-lib.ts",
    "sdks/packages/vrf-coordinator/test/alpha.spec.ts",
    "sdks/packages/vrf-coordinator/package.json",
    "sdks/packages/vrf-services/package.json",
    "sdks/packages/vrf-services/src/ecvrf.ts",
]


def encoded(value):
    return (json.dumps(value, indent=2, ensure_ascii=True) + "\n").encode()


def discriminator(name):
    return hashlib.sha256(f"global:{name}".encode()).digest()[:8]


def account(name, signer=False, writable=False, optional=False):
    return dict(name=name, signer=signer, writable=writable, optional=optional)


def base58_decode(value):
    alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
    number = 0
    for char in value:
        number = number * 58 + alphabet.index(char)
    return b"\0" * (len(value) - len(value.lstrip("1"))) + number.to_bytes((number.bit_length() + 7) // 8, "big")


def generate(source, output):
    source = source.resolve()
    idl = json.loads((source / IDL).read_bytes())
    assert idl["address"] == COORDINATOR, "Coordinator ID drift"
    source_records = []
    for name in SOURCES:
        path = (source / name).resolve(strict=True)
        if not path.is_relative_to(source) or not path.is_file():
            raise ValueError("source must be a regular file inside source root")
        raw = path.read_bytes()
        source_records.append(dict(path=name, bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest(),
                                   url=f"{REPOSITORY}/blob/{COMMIT}/{name}"))
    instructions = {}
    for item in idl["instructions"]:
        instructions[item["name"]] = {
            "program": "coordinator", "discriminator_hex": bytes(item["discriminator"]).hex(),
            "accounts": [account(a["name"], a.get("signer", False), a.get("writable", False),
                                 a.get("optional", False)) for a in item["accounts"]],
            "args": item["args"],
            "constraints": [{k: v for k, v in a.items() if k not in ("name", "docs", "signer", "writable", "optional")}
                            for a in item["accounts"]],
        }
    for name, program, accounts, args in [
        ("verify", "reference_verifier", [], ["suite:u8", "vrf_key:[u8;32]", "alpha:[u8;32]", "proof:Vec<u8>"]),
        ("verify_raw_alpha", "reference_verifier", [], ["suite:u8", "vrf_key:[u8;32]", "alpha:Vec<u8>", "proof:Vec<u8>"]),
        ("set_behaviour", "test_consumer", [account("payer", True, True), account("slot", writable=True), account("system_program")],
         ["key:[u8;32]", "behaviour:Behaviour"]),
        ("make_request", "test_consumer", [account("payer", True, True), account("slot", writable=True), account("identity"),
         account("requester"), account("coordinator_config"), account("request", writable=True), account("slot_hashes"),
         account("coordinator_program"), account("system_program")], ["key:[u8;32]", "seed:[u8;32]", "options:RequestOptions"]),
        ("on_random", "test_consumer", [account("callback_signer", True), account("slot", writable=True)],
         ["request:Pubkey", "output:[u8;64]"]),
    ]:
        instructions[name] = dict(program=program, discriminator_hex=discriminator(name).hex(), accounts=accounts, args=args)
    preimage = (b"sonic-vrf/v1" + base58_decode(COORDINATOR) + bytes([7]) * 32 + bytes([1]) * 32
                + (100).to_bytes(8, "little") + (99).to_bytes(8, "little") + bytes([2]) * 32)
    alpha = dict(provenance="offline-derived; pinned Rust and SDK golden vector, not a captured request",
                 program_id=COORDINATOR, request_hex="07" * 32, seed_hex="01" * 32,
                 request_slot=100, hash_slot=99, slot_hash_hex="02" * 32,
                 preimage_hex=preimage.hex(), alpha_hex=hashlib.sha256(preimage).hexdigest())
    assert alpha["alpha_hex"] == "85d739f7f781f84ed02f7f3f15be1c87f985a893a464b5355c94501bb15cdefb"
    ecvrf = (source / "programs/reference-verify/src/ecvrf.rs").read_text()
    vectors_block = ecvrf.split("const VECTORS:", 1)[1].split("\n    ];", 1)[0]
    parts = re.findall(r'"([0-9a-f]*)"', vectors_block)
    assert len(parts) == 12
    vectors = dict(provenance="copied from pinned reference verifier tests; RFC9381 B.3 examples 16-18; not executed here",
                   vectors=[dict(example=16 + i, public_key_hex=parts[i * 4], alpha_hex=parts[i * 4 + 1],
                                 proof_hex=parts[i * 4 + 2], output_hex=parts[i * 4 + 3]) for i in range(3)])
    proof = bytes.fromhex(parts[2])
    # This shape vector uses a real RFC proof with a different alpha. It must not be treated as a valid fulfill input.
    verify_data = discriminator("verify") + b"\x01" + bytes.fromhex(parts[0]) + bytes.fromhex(alpha["alpha_hex"]) + (80).to_bytes(4, "little") + proof
    callback_data = discriminator("on_random") + bytes([7]) * 32 + bytes.fromhex(parts[3]) + bytes.fromhex("aabb")
    callback_spec = (discriminator("on_random") + (15000).to_bytes(4, "little") + (1).to_bytes(4, "little")
                     + bytes([3]) * 32 + b"\x01" + (2).to_bytes(4, "little") + bytes.fromhex("aabb"))
    request_prefix = discriminator("request_random") + bytes([7]) * 32 + bytes([1]) * 32
    encoding = dict(provenance="synthetic encoding-only inputs; NOT valid fulfillment/callback evidence",
                    proof_matches_alpha=False, verify_hex=verify_data.hex(),
                    fulfill_hex=(discriminator("fulfill") + (80).to_bytes(4, "little") + proof).hex(),
                    request_callback_hex=(request_prefix + b"\x00\x01" + (30).to_bytes(2, "little") + b"\x01" + callback_spec).hex(),
                    request_store_hex=(request_prefix + b"\x01\x00\x00").hex(),
                    callback_hex=callback_data.hex(), callback_args_hex="aabb")
    files = {"alpha-golden.json": alpha, "rfc9381-vectors.json": vectors, "encoding-only.json": encoding,
             "UPSTREAM-LICENSE.txt": (source / "LICENSE").read_bytes()}
    artifacts = []
    output.mkdir(parents=True, exist_ok=True)
    for name, value in files.items():
        raw = value if isinstance(value, bytes) else encoded(value)
        (output / name).write_bytes(raw)
        artifacts.append(dict(path=name, sha256=hashlib.sha256(raw).hexdigest(), bytes=len(raw),
                              provenance="synthetic" if name == "encoding-only.json" else "offline_derived"))
    manifest = {
        "schema_version": "northstar-vrf-coordinator-compatibility/v1", "status": "DRAFT_OFFLINE_NOT_APPROVED",
        "source": dict(repository=REPOSITORY, commit=COMMIT, sdk_package="@mirrorworld/sonic.vrf-coordinator", sdk_version="0.1.0"),
        "algorithm": dict(name="ECVRF-EDWARDS25519-SHA512-TAI", standard="RFC9381", suite=1,
                          public_key_bytes=32, alpha_bytes=32, proof_bytes=80, output_bytes=64,
                          proof_parts=[dict(name="Gamma", bytes=32), dict(name="c", bytes=16), dict(name="s", bytes=32)]),
        "programs": {"coordinator": dict(id=COORDINATOR, role="request coordinator", binary_sha256=None),
                     "reference_verifier": dict(id=VERIFIER, role="TEST ONLY; not Portal Production verifier", binary_sha256=None),
                     "test_consumer": dict(id=CONSUMER, role="TEST ONLY consumer", binary_sha256=None)},
        "instructions": instructions,
        "coordinator_accounts": idl["accounts"],
        "coordinator_types": idl["types"],
        "test_consumer_types": {
            "RequestOptions": ["mode:Mode", "with_callback:bool", "compute:u32", "deadline_slots:Option<u16>", "args:Vec<u8>"],
            "Behaviour": ["Ok:0", "Fail:1", "Burn:2:u32", "Forward:3"],
            "Slot": ["key:[u8;32]", "behaviour:Behaviour", "request:Pubkey", "output:[u8;64]", "delivered_slot:u64", "deliveries:u32", "bump:u8"]},
        "pdas": {"config": ["config"], "request": ["request", "requester:32", "request_key:32"],
                 "requester_identity_under_consumer": ["vrf_requester"],
                 "callback_signer_under_coordinator": ["vrf_callback", "consumer_program_id:32"],
                 "test_consumer_slot": ["slot", "request_key:32"]},
        "callback": dict(data=["discriminator:8", "request_address:32", "output:64", "raw_borsh_args:0..64"],
                         accounts=["callback_signer:signer:readonly", "stored_callback_accounts:ordered:non_signers"],
                         max_accounts=16, max_args_bytes=64, remaining_outer_signers=False,
                         writable_rule="outer may promote stored readonly; CPI uses stored writability", atomic_failure=True),
        "lifecycle": dict(model="one request PDA per requester/key; no arena queue or delegation ABI",
                          mode_bytes=dict(Callback=0, Store=1), request_version=1, status_offset=9,
                          request_bytes="401 + 33*callback_accounts + callback_args",
                          fulfillment_slot_rule="request_slot < fulfill_slot <= deadline_slot",
                          callback_success="fee to fulfiller; close request; rent to recorded rent_payer",
                          store_success="output retained until requester close_request",
                          verifier_return="pinned verifier program ID and exactly 64 bytes"),
        "sources": source_records, "artifacts": artifacts,
        "acceptance": dict(runtime="not_run", execution_proof="unsupported_by_current_relation", portal="not_run",
                           baixiong="pending", jonas="pending", stable_fixtures="pending"),
    }
    (output / "compatibility-manifest.json").write_bytes(encoded(manifest))
    print(json.dumps({"manifest_sha256": hashlib.sha256(encoded(manifest)).hexdigest(), "artifacts": len(artifacts)}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-root", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    generate(args.source_root, args.output)

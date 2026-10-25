#!/usr/bin/env python3
"""Check pinned offline Coordinator fixtures and optional runtime observations.

Never fetch or execute artifacts. Never certify runtime, proof or external acceptance.
"""

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import sys

FIXTURES = Path(__file__).resolve().parents[1] / "fixtures/vrf-coordinator-v1"
EXPECTED_MANIFEST_SHA256 = "5891551ac67b003986a300e6b5e92089c886c3584c12c83411cf99374227b95d"
COMMIT = "d1b8c3ffc57a0d6caa50b8d17893f036bf61cabd"
ALGORITHM = "ECVRF-EDWARDS25519-SHA512-TAI"
MAX_JSON = 4 * 1024 * 1024
MAX_ARTIFACT = 64 * 1024 * 1024
MAX_TOTAL = 256 * 1024 * 1024


class Invalid(ValueError):
    pass


def require(condition, message):
    if not condition:
        raise Invalid(message)


def strict_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, f"duplicate JSON key: {key}")
        result[key] = value
    return result


def reject_constant(value):
    raise Invalid(f"non-finite JSON number: {value}")


def load_json(path):
    with Path(path).open("rb") as stream:
        raw = stream.read(MAX_JSON + 1)
    require(len(raw) <= MAX_JSON, "JSON exceeds 4 MiB")
    return json.loads(raw, object_pairs_hook=strict_object, parse_constant=reject_constant)


def encoded(value):
    return (json.dumps(value, indent=2, ensure_ascii=True) + "\n").encode()


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def discriminator(name):
    return hashlib.sha256(f"global:{name}".encode()).digest()[:8]


def base58_decode(value):
    alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
    number = 0
    for char in value:
        require(char in alphabet, "invalid base58")
        number = number * 58 + alphabet.index(char)
    return b"\0" * (len(value) - len(value.lstrip("1"))) + number.to_bytes((number.bit_length() + 7) // 8, "big")


def checked_files(records, root):
    root = Path(root).resolve(strict=True)
    files, identities, total = {}, set(), 0
    for record in records:
        name = record["path"]
        path = PurePosixPath(name)
        require(not path.is_absolute() and ".." not in path.parts and "\\" not in name,
                f"{name}: path must remain relative to artifact root")
        resolved = (root / name).resolve(strict=True)
        require(resolved.is_relative_to(root) and resolved.is_file(), f"{name}: not a regular file within root")
        stat = resolved.stat()
        identity = (stat.st_dev, stat.st_ino)
        require(identity not in identities, f"{name}: file alias")
        identities.add(identity)
        total += stat.st_size
        require(stat.st_size <= MAX_ARTIFACT and total <= MAX_TOTAL, "artifact byte budget exceeded")
        with resolved.open("rb") as stream:
            raw = stream.read(MAX_ARTIFACT + 1)
        require(len(raw) == record["bytes"] and digest(raw) == record["sha256"], f"{name}: size or SHA-256 mismatch")
        files[name] = resolved
    return files


def validate_manifest(manifest, root, source_root=None):
    require(type(manifest) is dict, "manifest must be an object")
    source, algorithm = manifest.get("source"), manifest.get("algorithm")
    require(type(source) is dict and source.get("commit") == COMMIT, "library/SDK pin drift")
    require(type(algorithm) is dict, "algorithm must be an object")
    require(algorithm.get("name") == ALGORITHM and algorithm.get("suite") == 1, "algorithm/suite drift")
    require([algorithm.get(k) for k in ("public_key_bytes", "alpha_bytes", "proof_bytes", "output_bytes")] == [32, 32, 80, 64],
            "algorithm length drift")
    require(digest(encoded(manifest)) == EXPECTED_MANIFEST_SHA256,
            "pinned manifest drift (instruction order/flags, layouts, provenance or unknown fields)")
    files = checked_files(manifest["artifacts"], root)
    if source_root is not None:
        checked_files(manifest["sources"], source_root)
    for name, instruction in manifest["instructions"].items():
        require(instruction["discriminator_hex"] == discriminator(name).hex(), f"{name}: discriminator drift")
    alpha = load_json(files["alpha-golden.json"])
    preimage = (b"sonic-vrf/v1" + base58_decode(alpha["program_id"]) + bytes.fromhex(alpha["request_hex"])
                + bytes.fromhex(alpha["seed_hex"]) + alpha["request_slot"].to_bytes(8, "little")
                + alpha["hash_slot"].to_bytes(8, "little") + bytes.fromhex(alpha["slot_hash_hex"]))
    require(preimage.hex() == alpha["preimage_hex"] and digest(preimage) == alpha["alpha_hex"], "alpha golden mismatch")
    vectors = load_json(files["rfc9381-vectors.json"])["vectors"]
    for vector in vectors:
        require(len(bytes.fromhex(vector["public_key_hex"])) == 32 and len(bytes.fromhex(vector["proof_hex"])) == 80
                and len(bytes.fromhex(vector["output_hex"])) == 64, "RFC vector length mismatch")
    encoding = load_json(files["encoding-only.json"])
    vector = vectors[0]
    proof = bytes.fromhex(vector["proof_hex"])
    verify = (discriminator("verify") + b"\x01" + bytes.fromhex(vector["public_key_hex"])
              + bytes.fromhex(alpha["alpha_hex"]) + (80).to_bytes(4, "little") + proof)
    fulfill = discriminator("fulfill") + (80).to_bytes(4, "little") + proof
    callback = (discriminator("on_random") + bytes.fromhex(alpha["request_hex"])
                + bytes.fromhex(vector["output_hex"]) + bytes.fromhex(encoding["callback_args_hex"]))
    require(encoding["verify_hex"] == verify.hex() and encoding["fulfill_hex"] == fulfill.hex()
            and encoding["callback_hex"] == callback.hex(), "encoding shape mismatch")
    callback_spec = (discriminator("on_random") + (15000).to_bytes(4, "little") + (1).to_bytes(4, "little")
                     + b"\x03" * 32 + b"\x01" + (2).to_bytes(4, "little") + bytes.fromhex("aabb"))
    request_prefix = discriminator("request_random") + b"\x07" * 32 + b"\x01" * 32
    require(encoding["request_callback_hex"] == (request_prefix + b"\x00\x01" + (30).to_bytes(2, "little") + b"\x01" + callback_spec).hex()
            and encoding["request_store_hex"] == (request_prefix + b"\x01\x00\x00").hex(), "request encoding shape mismatch")
    require(encoding["proof_matches_alpha"] is False, "encoding-only vector cannot claim a valid fulfillment")
    return manifest


def shape(value, rule, schema, location="$"):
    if "$ref" in rule:
        return shape(value, schema["$defs"][rule["$ref"].split("/")[-1]], schema, location)
    types = rule.get("type", [])
    types = [types] if isinstance(types, str) else types
    actual = {"object": type(value) is dict, "array": type(value) is list, "string": type(value) is str,
              "integer": type(value) is int, "null": value is None}
    require(not types or any(actual[t] for t in types), f"{location}: wrong type")
    if "const" in rule:
        require(type(value) is type(rule["const"]) and value == rule["const"], f"{location}: wrong constant")
    if "enum" in rule:
        require(value in rule["enum"], f"{location}: unknown value")
    if value is None:
        return
    if type(value) is dict:
        props = rule.get("properties", {})
        require(not (set(rule.get("required", [])) - value.keys()), f"{location}: missing fields")
        require(rule.get("additionalProperties") is not False or not (value.keys() - props.keys()), f"{location}: unknown fields")
        for key, item in value.items():
            if key in props:
                shape(item, props[key], schema, f"{location}.{key}")
    elif type(value) is list:
        require(rule.get("minItems", 0) <= len(value) <= rule.get("maxItems", 10000), f"{location}: array length")
        require(not rule.get("uniqueItems") or len({json.dumps(v, sort_keys=True) for v in value}) == len(value), f"{location}: duplicates")
        for index, item in enumerate(value):
            shape(item, rule["items"], schema, f"{location}[{index}]")
    elif type(value) is str:
        require(rule.get("minLength", 0) <= len(value) <= rule.get("maxLength", 4096), f"{location}: string length")
        require("pattern" not in rule or re.fullmatch(rule["pattern"], value) is not None, f"{location}: string format")
    elif type(value) is int:
        require(rule.get("minimum", 0) <= value <= rule.get("maximum", 2**63 - 1), f"{location}: integer range")


def validate_capture(document, manifest, root, schema=None):
    schema = schema or load_json(FIXTURES / "capture-schema.json")
    shape(document, schema, schema)
    require(document["compatibility_sha256"] == EXPECTED_MANIFEST_SHA256, "capture compatibility pin mismatch")
    checked_files(document["artifacts"], root)
    artifacts = {a["id"]: a for a in document["artifacts"]}
    require(len(artifacts) == len(document["artifacts"]), "duplicate artifact ID")
    blockers = []

    def reference(ref):
        require(ref is None or ref in artifacts, f"unknown artifact: {ref}")
        return ref is not None and artifacts[ref]["provenance"] == "captured"

    window, environment = document["window"], document["environment"]
    require(window["started_ns"] <= window["ended_ns"], "capture window end precedes start")
    if document["evidence_class"] not in ("local_er", "hosted_er"):
        blockers.append("synthetic/native diagnostic is not actual ER handoff")
    if not reference(window["attempts_artifact"]):
        blockers.append("complete captured attempt log missing")
    if not reference(environment["configuration_artifact"]):
        blockers.append("captured environment configuration missing")
    if any(v is None for k, v in environment.items() if k not in ("configuration_artifact", "programs")):
        blockers.append("runtime/genesis/features/binary identity incomplete")
    for name, program in environment["programs"].items():
        require(program["id"] == manifest["programs"][name]["id"], f"{name}: program ID drift")
        if program["binary_sha256"] is None:
            blockers.append(f"{name}: actual program binary identity missing")
        require("mainnet" not in program["features"], "mainnet feature unsupported by pinned library")

    def ordered(rows, time_key, label):
        require([r["sequence"] for r in rows] == list(range(1, len(rows) + 1)), f"{label}: sequence must be consecutive from 1")
        require(len({r["id"] for r in rows}) == len(rows), f"{label}: duplicate ID")
        times = [r[time_key] for r in rows]
        require(times == sorted(times), f"{label}: timestamps not ordered in one monotonic domain")
        require(all(window["started_ns"] <= t <= window["ended_ns"] for t in times), f"{label}: outside capture window")
        for row in rows:
            if not reference(row["artifact"]):
                blockers.append(f"{label} {row['id']}: captured receipt/log missing")
            if row["transaction"] is None:
                blockers.append(f"{label} {row['id']}: transaction identity missing")

    requests, callbacks, attempts = (document[key] for key in ("requests", "callbacks", "attempts"))
    ordered(requests, "submitted_ns", "requests")
    ordered(callbacks, "observed_ns", "callbacks")
    ordered(attempts, "observed_ns", "attempts")
    by_request = {r["id"]: r for r in requests}
    successes = Counter()
    for row in [*callbacks, *attempts]:
        require(row["request_id"] in by_request, "observation references unknown request")
        require(row["observed_ns"] >= by_request[row["request_id"]]["submitted_ns"], "observation precedes request")
    for row in callbacks:
        if row["status"] == "success":
            successes[row["request_id"]] += 1
            require(row["output_hex"] is not None, "successful callback output missing")
        if row["status"] != "success" or row["deliveries"] != 1:
            blockers.append("callback failed/missing or delivery count is not exactly one")
        matching = [a for a in attempts if a["request_id"] == row["request_id"] and a["stage"] == "callback"
                    and a["status"] == row["status"] and a["transaction"] == row["transaction"]
                    and a["observed_ns"] == row["observed_ns"]]
        require(len(matching) == 1, "callback observation must match exactly one retained attempt")
        if row["status"] == "success":
            require(any(a["request_id"] == row["request_id"] and a["stage"] == "fulfill" and a["status"] == "success"
                        and a["transaction"] == row["transaction"] and a["observed_ns"] <= row["observed_ns"] for a in attempts),
                    "successful callback must follow its successful fulfillment transaction")
    callback_attempts = [a for a in attempts if a["stage"] == "callback"]
    require(len(callback_attempts) == len(callbacks), "callback attempt omitted from observations")
    for row in requests:
        require(any(a["request_id"] == row["id"] and a["stage"] == "request" and a["transaction"] == row["transaction"]
                    and a["observed_ns"] == row["submitted_ns"] for a in attempts), "request submission missing from attempts")
        if row["status"] != "fulfilled" or successes[row["id"]] != 1:
            blockers.append("lost/duplicate/unfulfilled callback accounting")
        if not any(a["request_id"] == row["id"] and a["stage"] == "fulfill" and a["status"] == "success" for a in attempts):
            blockers.append("successful fulfillment observation missing")
    if any(row["status"] != "success" for row in attempts):
        blockers.append("failed/missing attempts retained; this window is not a consecutive successful run")
    if len(requests) < 100:
        blockers.append("at least 100 request/callback cycles required")
    if any(a["provenance"] != "captured" for a in artifacts.values()):
        blockers.append("synthetic/retained artifacts are development-only")
    return sorted(set(blockers))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=Path, default=FIXTURES / "compatibility-manifest.json")
    parser.add_argument("--source-root", type=Path)
    parser.add_argument("--capture", type=Path)
    parser.add_argument("--artifact-root", type=Path)
    parser.add_argument("--structure-only", action="store_true")
    args = parser.parse_args()
    report = dict(status="invalid", final_acceptance="not_established", errors=[], blockers=[])
    try:
        manifest = validate_manifest(load_json(args.manifest), args.manifest.parent, args.source_root)
        report["compatibility_sha256"] = EXPECTED_MANIFEST_SHA256
        report["blockers"] = (["runtime capture missing"] if args.capture is None else
                              validate_capture(load_json(args.capture), manifest, args.artifact_root or args.capture.parent))
        report["status"] = "blocked" if report["blockers"] else "ready_for_external_verification"
        report["structural_validation"] = "valid"
        code = 0 if args.structure_only else 1
    except (Invalid, OSError, ValueError, KeyError, TypeError, OverflowError, RuntimeError) as error:
        report["errors"].append(str(error))
        report["structural_validation"] = "invalid"
        code = 2
    report["trust_boundary"] = "Local byte integrity and reported observations only; no cryptographic verification, provenance authentication or external acceptance."
    print(json.dumps(report, indent=2))
    return code


if __name__ == "__main__":
    sys.exit(main())

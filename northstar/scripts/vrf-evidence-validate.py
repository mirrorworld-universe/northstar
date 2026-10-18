#!/usr/bin/env python3
"""Offline VRF evidence checks. This program never certifies live acceptance."""

import argparse
from collections import Counter
from datetime import datetime
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import stat
import sys

SCHEMA = Path(__file__).resolve().parents[1] / "fixtures/vrf-evidence/schema-v1.json"
MAX_DOCUMENT = 4 * 1024 * 1024
MAX_ARTIFACT = 64 * 1024 * 1024
MAX_TOTAL = 256 * 1024 * 1024
PROOF_NS = 120_000_000_000
CALLBACK_NS = 2_000_000_000
PATHS = {"request", "vrf_verification", "consumer_callback"}
TRUST_BOUNDARY = (
    "Offline checks verify shape, local hashes and reported gates only. They do not "
    "verify cryptography, live execution, capture provenance, completeness of attempt "
    "logs, reviewer identity or independent reproduction. Full acceptance is never "
    "established by this tool."
)


class InvalidEvidence(ValueError):
    pass


def strict_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise InvalidEvidence(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def load_json(path):
    with Path(path).open("rb") as stream:
        raw = stream.read(MAX_DOCUMENT + 1)
    if len(raw) > MAX_DOCUMENT:
        raise InvalidEvidence("JSON exceeds 4 MiB limit")
    return json.loads(
        raw,
        object_pairs_hook=strict_object,
        parse_constant=reject_constant,
    )


def reject_constant(value):
    raise InvalidEvidence(f"non-finite JSON number: {value}")


def check_shape(value, rule, schema, location="$"):
    """Evaluate the closed subset used by schema-v1, without third-party packages."""
    if "$ref" in rule:
        name = rule["$ref"].removeprefix("#/$defs/")
        return check_shape(value, schema["$defs"][name], schema, location)
    types = rule.get("type", [])
    types = [types] if isinstance(types, str) else types
    matches = {
        "null": value is None,
        "object": type(value) is dict,
        "array": type(value) is list,
        "string": type(value) is str,
        "integer": type(value) is int,
    }
    if types and not any(matches[t] for t in types):
        raise InvalidEvidence(f"{location}: expected {'/'.join(types)}")
    if "const" in rule and (type(value) is not type(rule["const"]) or value != rule["const"]):
        raise InvalidEvidence(f"{location}: expected {rule['const']!r}")
    if "enum" in rule and value not in rule["enum"]:
        raise InvalidEvidence(f"{location}: unsupported value {value!r}")
    if value is None:
        return
    if type(value) is dict:
        properties = rule.get("properties", {})
        missing = set(rule.get("required", [])) - value.keys()
        unknown = value.keys() - properties.keys()
        if missing:
            raise InvalidEvidence(f"{location}: missing fields {sorted(missing)}")
        if unknown and rule.get("additionalProperties") is False:
            raise InvalidEvidence(f"{location}: unknown fields {sorted(unknown)}")
        for key, item in value.items():
            if key in properties:
                check_shape(item, properties[key], schema, f"{location}.{key}")
    elif type(value) is list:
        if not rule.get("minItems", 0) <= len(value) <= rule.get("maxItems", 10_000):
            raise InvalidEvidence(f"{location}: array length out of bounds")
        if rule.get("uniqueItems") and len({json.dumps(x, sort_keys=True) for x in value}) != len(value):
            raise InvalidEvidence(f"{location}: duplicate array items")
        for index, item in enumerate(value):
            check_shape(item, rule["items"], schema, f"{location}[{index}]")
    elif type(value) is str:
        if not rule.get("minLength", 0) <= len(value) <= rule.get("maxLength", 4096):
            raise InvalidEvidence(f"{location}: string length out of bounds")
        if "pattern" in rule and re.fullmatch(rule["pattern"], value) is None:
            raise InvalidEvidence(f"{location}: invalid string format")
    elif type(value) is int:
        if not rule.get("minimum", 0) <= value <= rule.get("maximum", 2**63 - 1):
            raise InvalidEvidence(f"{location}: integer out of bounds")


def identity_sha256(document):
    identity = {key: document[key] for key in ("revisions", "profile", "environment")}
    return hashlib.sha256(
        json.dumps(identity, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode()
    ).hexdigest()


def timestamp(value):
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    if parsed.utcoffset() is None:
        raise ValueError("timestamp needs timezone")
    return parsed


def validate(document, artifact_root, schema=None):
    schema = schema or load_json(SCHEMA)
    report = {
        "schema_version": "northstar-vrf-evidence-report/v1",
        "structural_validation": "invalid",
        "reported_gates": {},
        "errors": [],
        "blockers": [],
        "measurements": {},
        "final_acceptance": "not_established",
        "trust_boundary": TRUST_BOUNDARY,
    }
    errors, blockers = report["errors"], report["blockers"]
    try:
        check_shape(document, schema, schema)
    except InvalidEvidence as error:
        errors.append(str(error))
        report["status"] = "invalid"
        return report

    def gate(name, conditions):
        reasons = [reason for passed, reason in conditions if not passed]
        report["reported_gates"][name] = "unmet" if reasons else "met_as_reported"
        blockers.extend(f"{name}: {reason}" for reason in reasons)

    def unique(rows, key, label):
        values = [row[key] for row in rows]
        if len(set(values)) != len(values):
            errors.append(f"{label}: duplicate {key}")

    def ordered(rows, label):
        if [row["sequence"] for row in rows] != list(range(1, len(rows) + 1)):
            errors.append(f"{label}: sequence must be ordered and consecutive from 1")

    unique(document["artifacts"], "id", "artifacts")
    artifacts = {row["id"]: row for row in document["artifacts"]}
    root = Path(artifact_root).resolve()
    total = 0
    paths = set()
    verified_paths = {}
    for item in document["artifacts"]:
        label = f"artifact {item['id']}"
        path = PurePosixPath(item["path"])
        if path.is_absolute() or ".." in path.parts or "\\" in item["path"]:
            errors.append(f"{label}: path must remain relative to artifact root")
            continue
        try:
            resolved = (root / item["path"]).resolve(strict=True)
            if not resolved.is_relative_to(root) or not stat.S_ISREG(resolved.stat().st_mode):
                raise ValueError("not a regular file within artifact root")
            if resolved in paths:
                raise ValueError("multiple artifact IDs alias the same file")
            paths.add(resolved)
            size = resolved.stat().st_size
            total += size
            if size > MAX_ARTIFACT or total > MAX_TOTAL:
                raise ValueError("artifact byte budget exceeded")
            with resolved.open("rb") as stream:
                digest = hashlib.sha256(stream.read(MAX_ARTIFACT + 1)).hexdigest()
            if size != item["bytes"] or digest != item["sha256"]:
                raise ValueError("size or SHA-256 mismatch")
            timestamp(item["captured_at"])
            verified_paths[item["id"]] = resolved
        except (OSError, ValueError, RuntimeError) as error:
            errors.append(f"{label}: {error}")

    def reference(ref, kind, label):
        if ref is None:
            return False
        item = artifacts.get(ref)
        if item is None or item["kind"] != kind:
            errors.append(f"{label}: missing artifact reference or wrong kind ({ref})")
            return False
        return item["source"] == "captured" and ref in verified_paths

    identity = identity_sha256(document)
    report["evidence_class"] = document["evidence_class"]
    report["identity_sha256"] = identity
    try:
        if timestamp(document["window"]["started_at"]) > timestamp(document["window"]["ended_at"]):
            raise ValueError("end precedes start")
    except ValueError as error:
        errors.append(f"window: {error}")
    gate("live_environment", [
        (document["evidence_class"] == "live_candidate", "CPU/synthetic evidence is development only"),
        (document["environment"]["kind"] == "northstar_er", "requires the agreed Northstar ER, not devnet-only/CPU"),
        (all(value is not None for value in document["environment"].values()), "environment identity/configuration missing"),
        (all(value is not None for value in document["revisions"].values()), "pinned source/program/SDK/oracle revisions missing"),
        (reference(document["environment"]["configuration_artifact"], "configuration", "environment"), "captured environment configuration missing"),
        (reference(document["window"]["attempts_artifact"], "log", "window"), "complete attempt-window capture missing"),
    ])
    profile = document["profile"]
    profile_checks = [(all(value is not None for key, value in profile.items() if key != "paths"), "profile/guest/key identities missing"),
                      (PATHS <= set(profile["paths"]), "profile lacks request/VRF verification/callback execution coverage")]
    for name, kind in (("profile", "profile"), ("fixtures", "fixtures"), ("guest", "guest"),
                       ("verification_key", "key"), ("rejection_corpus", "corpus")):
        ref = document["bindings"][name]
        present = reference(ref, kind, f"binding {name}")
        profile_checks.append((present, f"captured {name} artifact missing"))
        if ref in artifacts and artifacts[ref]["sha256"] != profile[f"{name}_sha256"]:
            errors.append(f"binding {name}: digest differs from profile")
    gate("pinned_profile", profile_checks)
    fixtures = {}
    fixtures_ref = document["bindings"]["fixtures"]
    fixture_checks = [(reference(fixtures_ref, "fixtures", "fixtures manifest"), "pinned fixtures manifest unavailable or not captured")]
    if fixtures_ref in verified_paths:
        try:
            manifest = load_json(verified_paths[fixtures_ref])
            check_shape(manifest, schema["$defs"]["fixtures_manifest"], schema, "fixtures")
            entries = manifest["fixtures"]
            unique(entries, "id", "fixtures manifest")
            fixtures = {entry["id"]: entry for entry in entries}
            fixture_checks.append((len(fixtures) == len(entries), "duplicate fixture IDs in manifest"))
            for entry in entries:
                label = f"fixture {entry['id']}"
                fixture_checks.extend([
                    (set(entry["paths"]) <= set(profile["paths"]), f"{label}: declared paths outside profile"),
                    (reference(entry["artifact"], "fixture", label), f"{label}: captured fixture bytes missing"),
                ])
                if entry["artifact"] in artifacts and artifacts[entry["artifact"]]["sha256"] != entry["sha256"]:
                    errors.append(f"{label}: digest differs from pinned fixtures manifest")
        except (OSError, ValueError, RecursionError) as error:
            errors.append(f"fixtures manifest: {error}")
            fixture_checks.append((False, "malformed fixtures manifest"))
    gate("pinned_fixtures", fixture_checks)

    def callback_gate(name, series, minimum, latency_gate=False):
        requests, callbacks = series["requests"], series["callbacks"]
        unique(requests, "id", name + " requests")
        unique(callbacks, "id", name + " callbacks")
        ordered(requests, name + " requests")
        ordered(callbacks, name + " callbacks")
        for rows, key in ((requests, "submitted_ns"), (callbacks, "observed_ns")):
            if [row[key] for row in rows] != sorted(row[key] for row in rows):
                errors.append(f"{name}: event timestamps out of order")
        by_id = {row["id"]: row for row in requests}
        counts = Counter(row["request_id"] for row in callbacks)
        latencies = []
        for callback in callbacks:
            request = by_id.get(callback["request_id"])
            if request is None:
                errors.append(f"{name}: callback references unknown request")
            elif callback["observed_ns"] < request["submitted_ns"]:
                errors.append(f"{name}: callback precedes request")
            else:
                latencies.append(callback["observed_ns"] - request["submitted_ns"])
        p95 = sorted(latencies)[(95 * len(latencies) + 99) // 100 - 1] if latencies else None
        report["measurements"][name] = {"requests": len(requests), "callbacks": len(callbacks), "p95_ns": p95}
        checks = [
            (series["identity_sha256"] == identity, "series identity mismatch"),
            (len(requests) >= minimum, f"requires at least {minimum} consecutive requests"),
            (len(requests) == len(callbacks) and all(counts[row["id"]] == 1 for row in requests), "lost/duplicate callbacks"),
            (all(row["status"] == "fulfilled" for row in requests), "missing/pending/failed request"),
            (all(row["status"] == "success" for row in callbacks), "failed callback"),
            (reference(series["capture_artifact"], "log", name), "captured request/callback log missing"),
        ]
        if latency_gate:
            checks.append((p95 is not None and p95 <= CALLBACK_NS, "nearest-rank p95 exceeds 2s or is missing"))
        gate(name, checks)

    callback_gate("er_handoff_100", document["er_handoff"], 100)
    callback_gate("application_1000_p95", document["application"], 1000, True)
    runs = document["resolutions"]
    unique(runs, "id", "resolutions")
    unique(runs, "checkpoint_id", "resolutions")
    ordered(runs, "resolutions")
    checks = [(len(runs) == 3, "requires three consecutive resolution attempts (no filtered successes)")]
    hardware_ids, proof_hashes, transactions = [], [], []
    covered = set()
    previous_end = None
    clocks = {run["timing"]["clock_id"] for run in runs}
    if len(clocks) > 1:
        errors.append("resolutions: consecutive attempts require one monotonic clock domain")
    for run in runs:
        label = f"run {run['id']}"
        fixture = fixtures.get(run["fixture_id"])
        checks.extend([
            (run["identity_sha256"] == identity, f"{label}: identity mismatch"),
            (run["status"] == "completed", f"{label}: missing/rejected/unsupported run"),
            (run["fixture_id"] is not None, f"{label}: pinned fixture case identity missing"),
            (fixture is not None, f"{label}: unknown fixture ID {run['fixture_id']!r}"),
            (run["proof_kind"] == "execution", f"{label}: VRF proof is not an execution proof"),
            (run["proof_origin"] == "generated", f"{label}: retained/synthetic/missing proof"),
            (bool(run["paths"]) and set(run["paths"]) <= set(profile["paths"]), f"{label}: missing or unsupported profile paths"),
            (run["hardware"]["model"] == "L40S" and run["hardware"]["count"] == 1, f"{label}: requires one L40S"),
        ])
        hardware_ids.append(run["hardware"]["device_id"])
        if fixture is not None:
            exact_paths = set(run["paths"]) == set(fixture["paths"])
            checks.append((exact_paths, f"{label}: paths must exactly match pinned fixture paths"))
            if exact_paths and set(fixture["paths"]) <= set(profile["paths"]):
                covered.update(fixture["paths"])
        for field, kind in (("witness_artifact", "witness"), ("history_artifact", "history"),
                            ("checkpoint_artifact", "checkpoint"), ("isolation_artifact", "isolation"),
                            ("proof_artifact", "proof"), ("prover_log_artifact", "log")):
            checks.append((reference(run[field], kind, label), f"{label}: captured {kind} artifact missing"))
        if run["proof_artifact"] in artifacts:
            proof_hashes.append(artifacts[run["proof_artifact"]]["sha256"])
        timing = run["timing"]
        names = ("witness_start_ns", "witness_ready_ns", "submitted_ns", "core_done_ns", "wrapped_ns",
                 "uploaded_ns", "resolve_submitted_ns", "resolved_ns")
        times = [timing[key] for key in names]
        complete = all(value is not None for value in times)
        checks.append((complete, f"{label}: timing boundary missing"))
        if complete:
            if times != sorted(times):
                errors.append(f"{label}: timing boundaries out of order")
            if previous_end is not None and times[0] < previous_end:
                errors.append(f"{label}: consecutive attempts overlap or are out of order")
            previous_end = times[-1]
            duration = timing["wrapped_ns"] - timing["submitted_ns"]
            report["measurements"][run["id"]] = {
                "witness_ns": timing["witness_ready_ns"] - timing["witness_start_ns"],
                "proof_including_wrap_ns": duration,
                "upload_ns": timing["uploaded_ns"] - timing["wrapped_ns"],
                "resolve_ns": timing["resolved_ns"] - timing["resolve_submitted_ns"],
            }
            checks.append((0 <= duration <= PROOF_NS, f"{label}: proof including wrapping exceeds 120s"))
        resolve = run["resolve"]
        checks.extend([
            (resolve["instruction"] == "ResolveChallenge" and resolve["verifier"] == "Production", f"{label}: requires full ResolveChallenge(Production)"),
            (resolve["status"] == "success", f"{label}: resolution did not succeed"),
            (resolve["compute_units"] is not None and resolve["compute_units"] <= 130_000, f"{label}: full instruction CU exceeds 130000 or is missing"),
            (resolve["transaction"] is not None, f"{label}: resolution transaction missing"),
            (reference(resolve["receipt_artifact"], "receipt", label), f"{label}: receipt missing"),
            (reference(resolve["outcome_artifact"], "outcome", label), f"{label}: checkpoint/bond/state outcome missing"),
        ])
        transactions.append(resolve["transaction"])
    checks.extend([
        (PATHS <= covered, "resolution window does not cover request, VRF verification and callback"),
        (len(hardware_ids) == 3 and len(set(hardware_ids)) == 1, "three attempts must use the same single device"),
        (len(proof_hashes) == 3 and len(set(proof_hashes)) == 3, "proof artifacts must be distinct per run"),
        (len(set(transactions)) == len(transactions), "resolution transactions repeated"),
    ])
    gate("three_portal_resolutions", checks)
    lifecycle = document["lifecycle"]
    gate("separate_startup_recovery", [
        (all(lifecycle[key] is not None for key in ("startup_ms", "cold_start_ms", "recovery_ms")), "startup/cold-start/recovery measurements missing"),
        (reference(lifecycle["capture_artifact"], "log", "lifecycle"), "lifecycle capture missing"),
    ])
    cases = document["rejection_cases"]
    unique(cases, "id", "rejection cases")
    corpus_checks = []
    corpus_ref = document["bindings"]["rejection_corpus"]
    if corpus_ref in verified_paths:
        try:
            manifest = load_json(verified_paths[corpus_ref])
            check_shape(manifest, schema["$defs"]["corpus_manifest"], schema, "corpus")
            unique(manifest["cases"], "id", "corpus manifest")
            required = {(case["id"], case["category"]) for case in manifest["cases"]}
            observed = {(case["id"], case["category"]) for case in cases}
            corpus_checks.append((required == observed, "results must match every pinned corpus case exactly"))
        except (OSError, ValueError, RecursionError) as error:
            errors.append(f"corpus manifest: {error}")
    else:
        corpus_checks.append((False, "pinned corpus manifest unavailable"))
    for category, expected in (("incorrect_witness", "rejected"), ("unsupported_path", "rejected"), ("existing_route", "accepted")):
        selected = [case for case in cases if case["category"] == category]
        corpus_checks.append((bool(selected), f"{category} cases missing"))
        for case in selected:
            corpus_checks.extend([
                (case["identity_sha256"] == identity, f"case {case['id']}: identity mismatch"),
                (case["observed"] == expected, f"case {case['id']}: expected {expected}"),
                (reference(case["capture_artifact"], "log", case["id"]), f"case {case['id']}: capture missing"),
            ])
    gate("rejection_corpus", corpus_checks)
    reviews = document["reviews"]
    unique(reviews, "role", "reviews")
    for role, reviewer in (("bilal_profile", "bilal"), ("baixiong_integration", "baixiong"),
                           ("collins_environment", "collins"), ("jonas_proof", "jonas")):
        selected = [review for review in reviews if review["role"] == role]
        review_checks = [(len(selected) == 1, f"{reviewer} sign-off record missing")]
        for review in selected:
            review_checks.extend([
                (review["reviewer"] == reviewer, "wrong named reviewer"),
                (review["decision"] == "accepted", "review pending/rejected"),
                (review["identity_sha256"] == identity, "review identity mismatch"),
                (reference(review["record_artifact"], "review", role), "recorded review artifact missing"),
            ])
            if role in ("jonas_proof", "baixiong_integration"):
                review_checks.append((reference(review["reproduction_artifact"], "log", role), "independent reproduction capture missing"))
            if role == "jonas_proof":
                review_checks.append((len(runs) == 3 and review["run_ids"] == [run["id"] for run in runs], "review does not name all three runs in order"))
        gate(role + "_recorded", review_checks)
    report["structural_validation"] = "invalid" if errors else "valid"
    report["status"] = "invalid" if errors else "blocked" if blockers else "ready_for_external_verification"
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, epilog=TRUST_BOUNDARY)
    parser.add_argument("evidence", type=Path)
    parser.add_argument("--artifact-root", type=Path, help="defaults to evidence file's directory; no network fetches")
    parser.add_argument("--structure-only", action="store_true", help="exit 0 for valid structure, even with unmet gates; never acceptance")
    args = parser.parse_args(argv)
    try:
        report = validate(load_json(args.evidence), args.artifact_root or args.evidence.parent)
    except (OSError, ValueError, RecursionError) as error:
        print(json.dumps({"status": "invalid", "structural_validation": "invalid", "errors": [str(error)],
                          "final_acceptance": "not_established", "trust_boundary": TRUST_BOUNDARY}, indent=2))
        return 2
    print(json.dumps(report, indent=2, sort_keys=True))
    if report["structural_validation"] == "invalid":
        return 2
    return 0 if args.structure_only else 1


if __name__ == "__main__":
    sys.exit(main())

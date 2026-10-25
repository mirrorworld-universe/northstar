#!/usr/bin/env python3
"""Versioned g5/ER-native candidate evidence checks; never an acceptance certificate."""

import argparse
import importlib.util
import json
from pathlib import Path
import re
import sys

SPEC = importlib.util.spec_from_file_location("actual_capture", Path(__file__).with_name("vrf-coordinator-fixtures-capture.py"))
capture = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(capture)
contract = capture.contract
require, closed, uint = capture.require, capture.closed, capture.uint
CONTRACT = contract.FIXTURES / "candidate-development-v2.json"
EXPECTED_CONTRACT_SHA256 = "6fed54799dfeee9428ad32e8a97ae885368f7b914e8df4d63986c8cfb21082f1"
IDENTITY_FIELDS = {"library_commit", "algorithm", "suite", "proof_kind", "proof_version", "domain_hex", "profile_sha256", "execution_profile_sha256",
                   "guest_sha256", "verification_key_sha256", "runtime_revision", "runtime_binary_sha256", "feature_set_sha256", "er_genesis_config_sha256",
                   "capture_journal_sha256", "capture_collector_sha256", "configuration_sha256", "rejection_corpus_sha256", "proof_window_sha256"}
ARTIFACT_FIELDS = {"id", "path", "bytes", "sha256", "provenance"}
RUN_ARTIFACTS = {"witness", "history", "checkpoint", "isolation", "prover_log", "wrapped_proof", "public_inputs", "receipt", "outcome"}
PUBLIC_FIELDS = ("domain", "session_context", "slot_step", "pre_state_root", "post_state_root", "tx_effect_root", "readonly_l1_root", "settlement_effect_root")
TIMES = ("witness_start_ns", "witness_ready_ns", "submitted_ns", "core_done_ns", "wrapped_ns", "uploaded_ns", "resolve_submitted_ns", "resolved_ns")
RUN_FIELDS = {"id", "sequence", "identity_sha256", "checkpoint_id", "wire_sha256", "input_sha256", "output_sha256", "capture_sha256", "paths", "status",
              "proof_origin", "proof_type", "hardware", "timing", "resolve", "artifacts"}
SCALAR_MODULUS = int("30644e72e131a029b85045b68181585d2833e84879b9709143e1f593f0000001", 16)
DOMAIN = capture.CANDIDATE_PROOF_DOMAIN
SP1_PREFIX = bytes.fromhex("4388a21c")
SP1_VERIFIER_ROOT = bytes.fromhex("002f850ee998974d6cc00e50cd0814b098c05bfade466d28573240d057f25352")


def nullable_digest(value, label, length=64):
    require(value is None or type(value) is str and re.fullmatch(f"[0-9a-f]{{{length}}}", value), f"{label}: digest format")


def identifier(value, label):
    require(type(value) is str and re.fullmatch("[A-Za-z0-9][A-Za-z0-9._:-]{0,127}", value), f"{label}: identifier format")


def identity_digest(identity, contract_sha256):
    return capture.sha(capture.canonical(dict(identity=identity, contract_sha256=contract_sha256)))


def validate_packet(packet, audit, artifact_root):
    closed(packet, {"schema_version", "contract_sha256", "identity", "bindings", "artifacts", "proof_window", "retained_failed_windows", "resolutions", "lifecycle", "rejection_cases"}, "candidate packet")
    require(packet["schema_version"] == "northstar-vrf-coordinator-candidate-evidence/v2", "candidate packet version drift")
    development = contract.load_json(CONTRACT)
    expected_contract_sha256 = capture.sha(capture.canonical(development))
    require(expected_contract_sha256 == EXPECTED_CONTRACT_SHA256, "development contract snapshot drift")
    require(packet["contract_sha256"] == expected_contract_sha256, "development contract pin drift")
    identity = packet["identity"]
    closed(identity, IDENTITY_FIELDS, "identity")
    require(identity["library_commit"] == capture.COMMIT and identity["algorithm"] == contract.ALGORITHM
            and type(identity["suite"]) is int and identity["suite"] == 1,
            "actual-library source/algorithm drift")
    require(type(identity["proof_kind"]) is int and identity["proof_kind"] == 3
            and type(identity["proof_version"]) is int and identity["proof_version"] == 1, "candidate kind/version drift")
    require(identity["domain_hex"] == DOMAIN.hex(), "candidate domain drift")
    for key in ("profile_sha256", "execution_profile_sha256", "guest_sha256", "verification_key_sha256", "runtime_binary_sha256", "feature_set_sha256", "er_genesis_config_sha256",
                "capture_journal_sha256", "capture_collector_sha256", "configuration_sha256", "rejection_corpus_sha256", "proof_window_sha256"):
        nullable_digest(identity[key], key)
    nullable_digest(identity["runtime_revision"], "runtime revision", 40)
    blockers = list(audit["blockers"])
    if any(identity[key] is None for key in IDENTITY_FIELDS):
        blockers.append("candidate profile/guest/key/runtime identity missing")
    if identity["capture_journal_sha256"] is not None:
        require(identity["capture_journal_sha256"] == audit["journal"]["sha256"], "packet/journal capture identity drift")
    expected_identity = identity_digest(identity, expected_contract_sha256)
    require(type(packet["artifacts"]) is list and len(packet["artifacts"]) <= 128, "artifact bound")
    for item in packet["artifacts"]:
        closed(item, ARTIFACT_FIELDS, "artifact")
        require(type(item["id"]) is str and re.fullmatch("[A-Za-z0-9][A-Za-z0-9._:-]{0,127}", item["id"]), "artifact ID")
        require(item["provenance"] in ("captured", "synthetic", "retained"), "artifact provenance")
        nullable_digest(item["sha256"], "artifact")
        require(item["sha256"] is not None, "artifact hash missing")
        uint(item["bytes"], "artifact size", 64 * 1024 * 1024)
        require(item["bytes"] > 0, "empty evidence artifact")
    files = contract.checked_files(packet["artifacts"], artifact_root)
    artifacts = {item["id"]: item for item in packet["artifacts"]}
    require(len(artifacts) == len(packet["artifacts"]), "duplicate artifact ID")

    def reference(identifier):
        require(identifier in artifacts, f"unknown artifact {identifier}")
        if artifacts[identifier]["provenance"] != "captured":
            blockers.append("synthetic/retained artifacts are not fresh candidate evidence")
        return files[artifacts[identifier]["path"]]

    bindings = packet["bindings"]
    closed(bindings, {"profile", "execution_profile", "guest", "verification_key", "configuration", "rejection_corpus"}, "bindings")
    for key in ("profile", "guest", "verification_key"):
        ref = bindings[key]
        if ref is None:
            blockers.append(f"candidate {key} bytes missing")
        else:
            reference(ref)
            require(artifacts[ref]["sha256"] == identity[f"{key}_sha256"], f"{key} artifact/identity drift")
            blockers = [b for b in blockers if b != f"{key}_sha256"]
    if bindings["execution_profile"] is None:
        blockers.append("actual Borsh execution-profile bytes missing")
    else:
        raw_profile = reference(bindings["execution_profile"]).read_bytes()
        require(0 < len(raw_profile) <= 1024 * 1024, "execution-profile byte bound")
        policy_digest = capture.sha(b"northstar-vrf-profile-v1" + len(raw_profile).to_bytes(8, "little") + raw_profile)
        require(policy_digest == identity["execution_profile_sha256"], "framed Borsh execution-profile digest drift")
    if bindings["profile"] is not None:
        profile = contract.load_json(reference(bindings["profile"]))
        require(type(profile) is dict and type(profile.get("proof_kind")) is int and profile["proof_kind"] == 3
                and type(profile.get("proof_version")) is int and profile["proof_version"] == 1
                and type(profile.get("checkpoint_steps")) is int and 1 <= profile["checkpoint_steps"] <= 16
                and profile.get("public_input_fields") == list(PUBLIC_FIELDS)
                and profile.get("production_acceptance") is False, "review profile kind/count/ABI/development-only drift")
        require(profile.get("execution_profile_sha256") == identity["execution_profile_sha256"], "reviewed/compiled execution-profile identity drift")
    if bindings["configuration"] is None:
        blockers.append("captured candidate configuration missing")
    else:
        reference(bindings["configuration"])
        require(artifacts[bindings["configuration"]]["sha256"] == identity["configuration_sha256"], "configuration artifact identity drift")
    if bindings["rejection_corpus"] is None:
        blockers.append("pinned complete rejection corpus missing")
    else:
        corpus = contract.load_json(reference(bindings["rejection_corpus"]))
        require(artifacts[bindings["rejection_corpus"]]["sha256"] == identity["rejection_corpus_sha256"], "corpus artifact identity drift")
        closed(corpus, {"schema_version", "cases"}, "corpus")
        require(corpus["schema_version"] == "northstar-vrf-candidate-rejection-corpus/v1", "corpus version")
        require(type(corpus["cases"]) is list and 1 <= len(corpus["cases"]) <= 128, "corpus bound")
        for case in corpus["cases"]:
            closed(case, {"id", "category", "expected"}, "corpus case")
        require(len({c["id"] for c in corpus["cases"]}) == len(corpus["cases"]), "duplicate corpus case")
        require(sorted((c["id"], c["category"], c["expected"]) for c in corpus["cases"])
                == sorted((c["id"], c["category"], c["expected"]) for c in packet["rejection_cases"]), "complete corpus result membership drift")
    atomic_audit = audit.get("atomic_capture", {})
    atomic = atomic_audit.get("captures", [])
    snapshots = {row["wire_sha256"]: row for row in atomic}
    if atomic:
        if identity["capture_collector_sha256"] is not None:
            require(identity["capture_collector_sha256"] == atomic_audit["collector"]["sha256"], "packet/atomic collector identity drift")
        blockers = [b for b in blockers if b != "authoritative_atomic_prestate_and_computed_poststate"]
        observed = atomic_audit.get("identities", {})
        require(observed.get("library_source_revision") == capture.COMMIT, "atomic collector library provenance drift")
        for identity_key, observed_key in (("runtime_revision", "runtime_source_revision"),
                                            ("runtime_binary_sha256", "runtime_binary_sha256"),
                                            ("er_genesis_config_sha256", "genesis_config_sha256")):
            if identity[identity_key] is None or observed.get(observed_key) is None:
                blockers.append(f"{identity_key}: collector identity missing")
            else:
                require(identity[identity_key] == observed[observed_key], "runtime/genesis identity does not match atomic collector")
                remove = "er_genesis_hash" if identity_key == "er_genesis_config_sha256" else identity_key
                blockers = [b for b in blockers if b != remove]
        if identity["feature_set_sha256"] is not None:
            require(all(row.get("feature_set_sha256") == identity["feature_set_sha256"] for row in atomic), "atomic feature identity drift")
        if observed.get("feature_set_sha256") is not None and identity["feature_set_sha256"] is not None:
            require(observed["feature_set_sha256"] == identity["feature_set_sha256"]
                    and observed.get("feature_set_digest_encoding") == "JSON-sorted-base58-active-pairs-v1", "collector feature encoding/identity drift")
        blockers = [b for b in blockers if b not in ("feature_set_sha256", "domain", "relation_version")]
    if not snapshots:
        blockers.append("authoritative atomic input/output capture missing")
    resolutions = packet["resolutions"]
    require(type(resolutions) is list and len(resolutions) <= 3, "resolution count")
    if len(resolutions) != 3:
        blockers.append("three fresh consecutive Production resolutions missing")
    window = packet["proof_window"]
    closed(window, {"id", "clock_id", "started_ns", "ended_ns", "attempts_artifact"}, "proof window")
    identifier(window["id"], "proof window")
    if window["clock_id"] is not None:
        require(type(window["clock_id"]) is str and window["clock_id"], "proof window clock")
    for key in ("started_ns", "ended_ns"):
        if window[key] is not None:
            uint(window[key], key, 2**63 - 1)
    if window["attempts_artifact"] is not None:
        reference(window["attempts_artifact"])
    if any(window[key] is None for key in ("clock_id", "started_ns", "ended_ns", "attempts_artifact")):
        blockers.append("complete monotonic proof attempt window missing")
    else:
        require(type(window["clock_id"]) is str and window["clock_id"], "proof window clock")
        uint(window["started_ns"], "window start", 2**63 - 1)
        uint(window["ended_ns"], "window end", 2**63 - 1)
        require(window["started_ns"] <= window["ended_ns"], "proof window ordering")
        ref = window["attempts_artifact"]
        attempt_log = contract.load_json(reference(ref))
        require(artifacts[ref]["sha256"] == identity["proof_window_sha256"], "proof attempt-window identity drift")
        closed(attempt_log, {"schema_version", "clock_id", "attempts"}, "proof attempt log")
        require(attempt_log["schema_version"] == "northstar-vrf-candidate-proof-attempts/v1"
                and attempt_log["clock_id"] == window["clock_id"], "proof attempt log version/clock drift")
        attempts = attempt_log["attempts"]
        require(type(attempts) is list and len(attempts) <= 128, "proof attempts bound")
        require(all(type(a.get("sequence")) is int for a in attempts)
                and [a.get("sequence") for a in attempts] == list(range(1, len(attempts) + 1)), "proof attempt sequence drift")
        require(len({a.get("id") for a in attempts}) == len(attempts), "duplicate proof attempt")
        require([a.get("id") for a in attempts] == [r.get("id") for r in resolutions], "omitted failure/attempt from proof window")
        for attempt, run in zip(attempts, resolutions):
            closed(attempt, {"id", "sequence", "status", "checkpoint_id", "wire_sha256", "submitted_ns", "wrapped_ns", "resolved_ns"}, "proof attempt")
            for key in ("submitted_ns", "wrapped_ns", "resolved_ns"):
                uint(attempt[key], key, 2**63 - 1)
            require(all(attempt[key] == run[key] for key in ("id", "sequence", "status", "checkpoint_id", "wire_sha256"))
                    and all(attempt[key] == run["timing"][key] for key in ("submitted_ns", "wrapped_ns", "resolved_ns")), "proof attempt/result drift")
    require(type(packet["retained_failed_windows"]) is list and len(packet["retained_failed_windows"]) <= 32, "retained failure window bound")
    for ref in packet["retained_failed_windows"]:
        reference(ref)
    require(all(type(r.get("sequence")) is int for r in resolutions)
            and [r.get("sequence") for r in resolutions] == list(range(1, len(resolutions) + 1)), "resolution sequence drift")
    require(len({r.get("id") for r in resolutions}) == len(resolutions), "duplicate resolution ID")
    require(len({r.get("checkpoint_id") for r in resolutions}) == len(resolutions), "duplicate checkpoint")
    devices, clocks, covered, proofs, measurements = set(), set(), set(), set(), []
    previous_resolved = 0
    for run in resolutions:
        closed(run, RUN_FIELDS, "resolution")
        identifier(run["id"], "resolution")
        identifier(run["checkpoint_id"], "checkpoint")
        for key in ("wire_sha256", "input_sha256", "output_sha256", "capture_sha256"):
            nullable_digest(run[key], key)
            require(run[key] is not None, "resolution fixture identity missing")
        require(run["identity_sha256"] == expected_identity, "resolution candidate identity drift")
        require(run["status"] in ("completed", "failed", "not_run"), "resolution status")
        require(run["proof_origin"] in ("fresh", "retained", "synthetic") and run["proof_type"] in ("execution", "randomness"), "proof provenance/type")
        if run["status"] != "completed" or run["proof_origin"] != "fresh" or run["proof_type"] != "execution":
            blockers.append("all three runs must be fresh completed execution proofs")
        hardware = run["hardware"]
        closed(hardware, {"instance", "model", "count", "device_id"}, "hardware")
        if hardware["instance"] != "g5.xlarge" or hardware["model"] != "A10G" or type(hardware["count"]) is not int or hardware["count"] != 1:
            blockers.append("requires user-selected one g5.xlarge/A10G; no L40S relabeling")
        require(type(hardware["device_id"]) is str and hardware["device_id"], "device identity missing")
        devices.add(hardware["device_id"])
        timing = run["timing"]
        closed(timing, {"clock_id", *TIMES}, "timing")
        require(type(timing["clock_id"]) is str and timing["clock_id"], "proof clock missing")
        clocks.add(timing["clock_id"])
        if window["clock_id"] is not None:
            require(timing["clock_id"] == window["clock_id"], "proof window/run clock drift")
        times = [timing[key] for key in TIMES]
        for value in times:
            uint(value, "timing", 2**63 - 1)
        require(times == sorted(times) and times[0] >= previous_resolved, "proof stage/window clock ordering drift")
        previous_resolved = times[-1]
        if window["started_ns"] is not None and window["ended_ns"] is not None:
            require(window["started_ns"] <= times[0] <= times[-1] <= window["ended_ns"], "resolution outside proof attempt window")
        proof_ns = timing["wrapped_ns"] - timing["submitted_ns"]
        if proof_ns > 120000000000:
            blockers.append(f"{run['id']}: prove+wrap exceeds 120s")
        resolve = run["resolve"]
        closed(resolve, {"instruction", "verifier", "proof_kind", "proof_version", "status", "compute_units", "transaction"}, "resolve")
        uint(resolve["compute_units"], "full resolution CU")
        require(resolve["compute_units"] > 0, "full resolution CU must be measured positive")
        if (type(resolve["proof_kind"]) is not int or type(resolve["proof_version"]) is not int
                or (resolve["instruction"], resolve["verifier"], resolve["proof_kind"], resolve["proof_version"], resolve["status"]) != ("ResolveChallenge", "Production", 3, 1, "success")):
            blockers.append("real candidate ResolveChallenge(Production) required; standalone Verify does not qualify")
        if resolve["compute_units"] > 130000:
            blockers.append(f"{run['id']}: full Production resolution exceeds 130000 CU")
        require(type(resolve["transaction"]) is str and resolve["transaction"], "resolution transaction identity missing")
        snapshot = snapshots.get(run["wire_sha256"])
        require(type(run["paths"]) is list and len(set(run["paths"])) == len(run["paths"])
                and set(run["paths"]) <= {"request", "fulfillment", "consumer_callback"}, "execution path set")
        if snapshot is None:
            blockers.append("resolution wire has no authoritative atomic fixture")
        else:
            require(run["input_sha256"] == snapshot["input_sha256"] and run["output_sha256"] == snapshot["output_sha256"]
                    and run["capture_sha256"] == snapshot["artifact"]["sha256"], "resolution input/output identity drift")
            expected_paths = {"request"} if snapshot["profile"] == "consumer.make_request" else {"fulfillment", "consumer_callback"}
            require(set(run["paths"]) == expected_paths, "resolution fixture path drift")
            require(snapshot["status"] == {"Ok": None}, "failed/control fixture cannot satisfy successful resolution coverage")
            covered.update(expected_paths)
        closed(run["artifacts"], RUN_ARTIFACTS, "resolution artifacts")
        for key, ref in run["artifacts"].items():
            path = reference(ref)
            if key == "wrapped_proof":
                wrapped = path.read_bytes()
                require(len(wrapped) == 356 and wrapped[:4] == SP1_PREFIX and wrapped[4:36] == bytes(32)
                        and wrapped[36:68] == SP1_VERIFIER_ROOT, "wrapped Production proof envelope drift")
                require(int.from_bytes(wrapped[68:100], "big") < SCALAR_MODULUS, "noncanonical proof nonce")
                proofs.add(artifacts[ref]["sha256"])
            if key == "public_inputs":
                public = path.read_bytes()
                require(len(public) == 256 and public[:32] == DOMAIN, "eight-field candidate public input/domain drift")
                require(all(int.from_bytes(public[i:i + 32], "big") < SCALAR_MODULUS for i in range(0, 256, 32)), "noncanonical public scalar")
                require(all(int.from_bytes(public[index * 32:(index + 1) * 32], "big") != 0
                            for index in (1, 3, 4, 5, 6, 7)), "missing public commitment")
        measurements.append(dict(id=run["id"], prove_and_wrap_ns=proof_ns, full_resolution_cu=resolve["compute_units"]))
    if resolutions and (len(devices) != 1 or len(clocks) != 1):
        blockers.append("all three runs must share one device and one monotonic collector domain")
    if len(proofs) != len(resolutions):
        blockers.append("retained/duplicate wrapped proof bytes do not establish three fresh proofs")
    if covered != {"request", "fulfillment", "consumer_callback"}:
        blockers.append("request/fulfillment/callback execution coverage incomplete")
    lifecycle = packet["lifecycle"]
    closed(lifecycle, {"startup_ns", "cold_start_ns", "recovery_ns", "artifact"}, "lifecycle")
    for key in ("startup_ns", "cold_start_ns", "recovery_ns"):
        if lifecycle[key] is None:
            blockers.append("startup/cold/recovery measurements missing; never subtract from proving time")
        else:
            uint(lifecycle[key], key)
    if lifecycle["artifact"] is None:
        blockers.append("lifecycle capture missing")
    else:
        reference(lifecycle["artifact"])
    cases = packet["rejection_cases"]
    require(type(cases) is list and len(cases) <= 128, "rejection corpus bound")
    require(len({case.get("id") for case in cases}) == len(cases), "duplicate rejection case")
    categories = set()
    for case in cases:
        closed(case, {"id", "category", "expected", "observed", "identity_sha256", "artifact"}, "rejection case")
        identifier(case["id"], "rejection case")
        require(case["category"] in ("incorrect_witness", "unsupported_path", "existing_route", "invalid_vrf_proof", "callback_rollback", "application_invariant"), "rejection category")
        require(case["identity_sha256"] == expected_identity, "rejection identity drift")
        expected = case["expected"] if case["category"] == "application_invariant" else "accepted" if case["category"] == "existing_route" else "rejected"
        require(case["expected"] == expected and expected in ("accepted", "rejected"), "rejection expectation drift")
        if case["observed"] != expected:
            blockers.append("incorrect-witness/unsupported/rollback rejection or legacy regression failed")
        categories.add(case["category"])
        reference(case["artifact"])
    if not {"incorrect_witness", "unsupported_path", "existing_route", "invalid_vrf_proof", "callback_rollback"} <= categories:
        blockers.append("complete incorrect-witness/unsupported/VRF/rollback/legacy corpus missing")
    blockers.append("Baixiong/Jonas/fixture-owner/team responses require authenticated external records; not supplied by JSON")
    return dict(schema_version="northstar-vrf-coordinator-candidate-report/v2", status="blocked", final_acceptance="not_established",
                reported_engineering_gates="unmet" if len(blockers) > 1 else "consistent_as_reported_requires_external_verification",
                contract_sha256=expected_contract_sha256, identity_sha256=expected_identity,
                missing_identity=sorted(key for key, value in identity.items() if value is None),
                blockers=sorted(set(blockers)), measurements=measurements,
                trust_boundary="Local hashes, arithmetic and declared identities only; no cryptographic verification, provenance authentication or independent acceptance.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--packet", type=Path, required=True)
    parser.add_argument("--journal", type=Path, required=True)
    parser.add_argument("--sidecars-dir", type=Path)
    parser.add_argument("--artifact-root", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--structure-only", action="store_true")
    args = parser.parse_args()
    try:
        audit = capture.audit_journal(args.journal)
        if args.sidecars_dir is not None:
            capture.attach_atomic_capture(audit, capture.audit_sidecars(args.sidecars_dir, audit))
        report = validate_packet(contract.load_json(args.packet), audit, args.artifact_root or args.packet.parent)
        code = 0 if args.structure_only else 1
    except (capture.Invalid, OSError, ValueError, KeyError, TypeError, IndexError, RuntimeError) as error:
        report = dict(status="invalid", final_acceptance="not_established", errors=[str(error)])
        code = 2
    output = json.dumps(report, indent=2) + "\n"
    if args.output is None:
        print(output, end="")
    else:
        args.output.write_text(output)
        print(json.dumps({k: report[k] for k in ("status", "final_acceptance")}))
    return code


if __name__ == "__main__":
    sys.exit(main())

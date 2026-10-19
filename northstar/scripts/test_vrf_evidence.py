"""All generated records/artifacts are simulated, never live acceptance evidence."""

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("vrf-evidence-validate.py")
FIXTURES = SCRIPT.resolve().parents[1] / "fixtures/vrf-evidence"
spec = importlib.util.spec_from_file_location("vrf_evidence", SCRIPT)
evidence = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evidence)


class EvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.doc = evidence.load_json(FIXTURES / "synthetic-cpu-v1.json")
        self.doc["evidence_class"] = "live_candidate"
        self.doc["limitations"] = ["SIMULATED UNIT TEST. No cryptographic or live evidence."]
        self.doc["artifacts"] = []
        self.doc["revisions"] = {
            "source_commit": "1" * 40, "oracle_revision": "a" * 40,
            "application_revision": "b" * 40, "sdk_revision": "c" * 40,
            "consumer_revision": "d" * 40, "portal_program": "synthetic-portal-not-a-program",
            "portal_binary_sha256": "2" * 64,
        }
        self.doc["environment"] = {
            "id": "synthetic-test-er", "kind": "northstar_er", "er_genesis_hash": "synthetic-er-genesis",
            "l1_genesis_hash": "synthetic-l1-genesis", "runtime_revision": "e" * 40,
            "feature_set_sha256": "3" * 64, "oracle_public_key": "synthetic-public-key-not-a-key",
            "configuration_artifact": self.artifact("configuration", "configuration"),
        }
        self.doc["window"]["attempts_artifact"] = self.artifact("attempts", "log")
        self.doc["profile"].update(id="synthetic-test-profile", version="test-v1", relation_version="synthetic-relation-v1", paths=sorted(evidence.PATHS))
        self.corpus = {"schema_version": "northstar-vrf-rejection-corpus/v1", "cases": [
            {"id": "synthetic-witness-binding", "category": "incorrect_witness"},
            {"id": "synthetic-unsupported-path", "category": "unsupported_path"},
            {"id": "synthetic-old-route", "category": "existing_route"},
        ]}
        fixture_artifact = self.artifact("synthetic-fixture-input", "fixture")
        self.fixtures_manifest = {"schema_version": "northstar-vrf-fixtures/v1", "fixtures": [{
            "id": "synthetic-fixture-not-real", "paths": sorted(evidence.PATHS),
            "artifact": fixture_artifact, "sha256": self.doc["artifacts"][-1]["sha256"],
        }]}
        manifests = {"corpus": self.corpus, "fixtures": self.fixtures_manifest}
        for name, kind in (("profile", "profile"), ("fixtures", "fixtures"), ("guest", "guest"),
                           ("verification_key", "key"), ("rejection_corpus", "corpus")):
            payload = json.dumps(manifests[kind]).encode() if kind in manifests else None
            self.doc["bindings"][name] = self.artifact(name, kind, payload)
            self.doc["profile"][f"{name}_sha256"] = self.doc["artifacts"][-1]["sha256"]
        self.doc["er_handoff"] = self.series(100, "er")
        self.doc["application"] = self.series(1000, "app")
        self.doc["resolutions"] = [self.resolution(index) for index in range(1, 4)]
        self.doc["lifecycle"] = {"startup_ms": 1, "cold_start_ms": 2, "recovery_ms": 3,
                                 "capture_artifact": self.artifact("lifecycle", "log"), "notes": "SIMULATED measurements"}
        self.doc["rejection_cases"] = [{
            **case, "identity_sha256": "0" * 64,
            "observed": "accepted" if case["category"] == "existing_route" else "rejected",
            "capture_artifact": self.artifact(case["id"], "log"),
        } for case in self.corpus["cases"]]
        self.doc["reviews"] = [{
            "role": role, "reviewer": reviewer, "decision": "accepted", "identity_sha256": "0" * 64,
            "run_ids": [run["id"] for run in self.doc["resolutions"]] if reviewer == "jonas" else [],
            "record_artifact": self.artifact(role, "review"),
            "reproduction_artifact": self.artifact(role + "-reproduction", "log") if reviewer in ("jonas", "baixiong") else None,
        } for role, reviewer in (("bilal_profile", "bilal"), ("baixiong_integration", "baixiong"),
                                 ("collins_environment", "collins"), ("jonas_proof", "jonas"))]
        self.rebind()

    def artifact(self, name, kind, payload=None):
        payload = payload if payload is not None else f"SYNTHETIC TEST ONLY, NOT LIVE: {name}\n".encode()
        filename = name + ".synthetic.txt"
        (self.root / filename).write_bytes(payload)
        self.doc["artifacts"].append({
            "id": name, "path": filename, "sha256": hashlib.sha256(payload).hexdigest(), "bytes": len(payload),
            "source": "captured", "kind": kind, "producer": "simulated unit-test collector",
            "command": "unittest, not a live capture", "captured_at": "2000-01-01T00:00:00Z", "origin": "synthetic test bytes",
        })
        return name

    def series(self, count, prefix):
        return {
            "identity_sha256": "0" * 64, "clock_id": "synthetic-clock",
            "capture_artifact": self.artifact(prefix + "-callbacks", "log"),
            "requests": [{"id": f"{prefix}-request-{i}", "sequence": i, "submitted_ns": i * 3_000_000_000,
                          "status": "fulfilled"} for i in range(1, count + 1)],
            "callbacks": [{"id": f"{prefix}-callback-{i}", "request_id": f"{prefix}-request-{i}", "sequence": i,
                           "observed_ns": i * 3_000_000_000 + 2_000_000_000, "status": "success"} for i in range(1, count + 1)],
        }

    def resolution(self, index):
        name = f"synthetic-run-{index}"
        base = index * 200_000_000_000
        result = {
            "id": name, "sequence": index, "identity_sha256": "0" * 64, "checkpoint_id": name + "-checkpoint", "fixture_id": "synthetic-fixture-not-real",
            "status": "completed", "proof_kind": "execution", "proof_origin": "generated", "paths": sorted(evidence.PATHS),
            "hardware": {"model": "L40S", "count": 1, "device_id": "synthetic-gpu-not-real"},
            "timing": dict(zip(("witness_start_ns", "witness_ready_ns", "submitted_ns", "core_done_ns", "wrapped_ns",
                                "uploaded_ns", "resolve_submitted_ns", "resolved_ns"),
                               [base + seconds * 1_000_000_000 for seconds in (0, 1, 2, 121, 122, 123, 124, 125)])),
            "resolve": {"instruction": "ResolveChallenge", "verifier": "Production", "status": "success", "compute_units": 130000,
                        "transaction": name + "-not-a-transaction", "receipt_artifact": self.artifact(name + "-receipt", "receipt"),
                        "outcome_artifact": self.artifact(name + "-outcome", "outcome")},
        }
        result["timing"]["clock_id"] = "synthetic-prover-clock"
        for field, kind in (("witness_artifact", "witness"), ("history_artifact", "history"),
                            ("checkpoint_artifact", "checkpoint"), ("isolation_artifact", "isolation"),
                            ("proof_artifact", "proof"), ("prover_log_artifact", "log")):
            result[field] = self.artifact(name + "-" + kind, kind)
        return result

    def rebind(self):
        identity = evidence.identity_sha256(self.doc)
        for row in [self.doc["er_handoff"], self.doc["application"], *self.doc["resolutions"],
                    *self.doc["rejection_cases"], *self.doc["reviews"]]:
            row["identity_sha256"] = identity

    def pin_fixtures(self, manifest=None, raw=None):
        if raw is None:
            raw = json.dumps(self.fixtures_manifest if manifest is None else manifest).encode()
        item = next(item for item in self.doc["artifacts"] if item["id"] == self.doc["bindings"]["fixtures"])
        (self.root / item["path"]).write_bytes(raw)
        item.update(sha256=hashlib.sha256(raw).hexdigest(), bytes=len(raw))
        self.doc["profile"]["fixtures_sha256"] = item["sha256"]
        self.rebind()

    def report(self):
        return evidence.validate(self.doc, self.root)

    def unmet(self, gate, substring=None):
        report = self.report()
        self.assertEqual(report["reported_gates"][gate], "unmet", report)
        self.assertEqual(report["final_acceptance"], "not_established")
        if substring:
            self.assertTrue(any(substring in reason for reason in report["blockers"]), report["blockers"])
        return report

    def invalid(self, substring):
        report = self.report()
        self.assertEqual(report["structural_validation"], "invalid", report)
        self.assertTrue(any(substring in error for error in report["errors"]), report["errors"])

    def test_inclusive_boundaries_are_reported_not_certified(self):
        report = self.report()
        self.assertEqual(report["errors"], [])
        self.assertEqual(report["blockers"], [])
        self.assertEqual(report["status"], "ready_for_external_verification")
        self.assertEqual(report["final_acceptance"], "not_established")
        self.assertEqual(report["measurements"]["application_1000_p95"]["p95_ns"], 2_000_000_000)
        self.assertEqual(report["measurements"]["synthetic-run-1"]["proof_including_wrap_ns"], 120_000_000_000)

    def test_one_nanosecond_over_proof_limit(self):
        self.doc["resolutions"][1]["timing"]["wrapped_ns"] += 1
        self.unmet("three_portal_resolutions", "exceeds 120s")

    def test_individual_slow_run_not_hidden_by_average(self):
        for run, seconds in zip(self.doc["resolutions"], (10, 121, 10)):
            timing = run["timing"]
            timing["core_done_ns"] = timing["submitted_ns"] + seconds * 1_000_000_000 - 1
            timing["wrapped_ns"] = timing["submitted_ns"] + seconds * 1_000_000_000
        self.assertLess((10 + 121 + 10) / 3, 120)
        self.unmet("three_portal_resolutions", "synthetic-run-2: proof")

    def test_one_cu_over_limit(self):
        self.doc["resolutions"][0]["resolve"]["compute_units"] += 1
        self.unmet("three_portal_resolutions", "CU exceeds")

    def test_wrong_hardware_or_device_count(self):
        for field, value in (("model", "A100"), ("model", "CPU"), ("count", 2)):
            with self.subTest(field=field, value=value):
                original = self.doc["resolutions"][0]["hardware"][field]
                self.doc["resolutions"][0]["hardware"][field] = value
                self.unmet("three_portal_resolutions", "requires one L40S")
                self.doc["resolutions"][0]["hardware"][field] = original

    def test_device_changes_between_attempts(self):
        self.doc["resolutions"][1]["hardware"]["device_id"] = "other-synthetic-device"
        self.unmet("three_portal_resolutions", "same single device")

    def test_missing_callback(self):
        self.doc["er_handoff"]["callbacks"].pop()
        self.unmet("er_handoff_100", "lost/duplicate")

    def test_duplicate_callback_even_when_totals_match(self):
        self.doc["er_handoff"]["callbacks"][-1]["request_id"] = "er-request-99"
        self.unmet("er_handoff_100", "lost/duplicate")

    def test_duplicate_callback_id(self):
        self.doc["er_handoff"]["callbacks"][-1]["id"] = "er-callback-1"
        self.invalid("duplicate id")

    def test_unknown_request_callback(self):
        self.doc["er_handoff"]["callbacks"][-1]["request_id"] = "unsubmitted-request"
        self.invalid("unknown request")

    def test_missing_request_and_failed_callback(self):
        self.doc["er_handoff"]["requests"][-1]["status"] = "missing"
        self.doc["er_handoff"]["callbacks"][-1]["status"] = "failed"
        self.unmet("er_handoff_100", "missing/pending/failed")

    def test_99_requests_not_100(self):
        self.doc["er_handoff"]["requests"].pop()
        self.doc["er_handoff"]["callbacks"].pop()
        self.unmet("er_handoff_100", "at least 100")

    def test_application_gate_is_separate(self):
        self.doc["application"]["requests"].pop()
        self.doc["application"]["callbacks"].pop()
        report = self.unmet("application_1000_p95", "at least 1000")
        self.assertEqual(report["reported_gates"]["er_handoff_100"], "met_as_reported")

    def test_p95_nearest_rank_boundary(self):
        callbacks = self.doc["application"]["callbacks"]
        for callback in callbacks[-50:]:
            callback["observed_ns"] += 1
        self.assertEqual(self.report()["reported_gates"]["application_1000_p95"], "met_as_reported")
        callbacks[-51]["observed_ns"] += 1
        self.unmet("application_1000_p95", "p95 exceeds 2s")

    def test_out_of_order_and_nonconsecutive_runs(self):
        self.doc["resolutions"][0], self.doc["resolutions"][1] = self.doc["resolutions"][1], self.doc["resolutions"][0]
        self.invalid("sequence must be ordered and consecutive")

    def test_gap_in_request_sequence(self):
        self.doc["er_handoff"]["requests"][50]["sequence"] += 1
        self.invalid("sequence must be ordered and consecutive")

    def test_callback_before_request_and_out_of_order(self):
        self.doc["er_handoff"]["callbacks"][3]["observed_ns"] = 0
        self.invalid("callback precedes request")

    def test_missing_independent_review(self):
        self.doc["reviews"] = [review for review in self.doc["reviews"] if review["role"] != "jonas_proof"]
        self.unmet("jonas_proof_recorded", "sign-off record missing")

    def test_review_requires_reproduction_and_exact_runs(self):
        review = self.doc["reviews"][-1]
        review["reproduction_artifact"] = None
        review["run_ids"].pop()
        self.unmet("jonas_proof_recorded", "independent reproduction")

    def test_review_pending_rejected_or_wrong_reviewer(self):
        review = self.doc["reviews"][-1]
        for decision in ("pending", "rejected"):
            with self.subTest(decision=decision):
                review["decision"] = decision
                self.unmet("jonas_proof_recorded", "pending/rejected")
        review["decision"] = "accepted"
        review["reviewer"] = "ivan"
        self.unmet("jonas_proof_recorded", "wrong named reviewer")

    def test_boolean_verification_claim_not_an_interface(self):
        self.doc["resolutions"][0]["cryptographically_verified"] = True
        self.invalid("unknown fields")

    def test_false_final_acceptance_field_rejected(self):
        self.doc["final_acceptance"] = True
        self.invalid("unknown fields")

    def test_wrong_profile_key_guest_environment_identity(self):
        for group, field in (("profile", "guest_sha256"), ("profile", "verification_key_sha256"),
                             ("environment", "feature_set_sha256")):
            with self.subTest(field=field):
                original = self.doc[group][field]
                self.doc[group][field] = "f" * 64
                self.unmet("three_portal_resolutions", "identity mismatch")
                self.doc[group][field] = original

    def test_missing_identity_is_blocked_not_invented(self):
        self.doc["profile"]["guest_sha256"] = None
        self.unmet("pinned_profile", "identities missing")

    def test_missing_or_disordered_timing_and_clock(self):
        self.doc["resolutions"][0]["timing"]["wrapped_ns"] = None
        self.unmet("three_portal_resolutions", "boundary missing")
        self.doc["resolutions"][0]["timing"]["wrapped_ns"] = 0
        self.invalid("timing boundaries out of order")
        self.doc["resolutions"][0]["timing"]["clock_id"] = "other-clock"
        self.invalid("one monotonic clock")

    def test_wrapping_counts_but_startup_is_separate(self):
        self.doc["lifecycle"]["startup_ms"] = 999_000
        self.assertEqual(self.report()["blockers"], [])
        self.doc["resolutions"][0]["timing"]["wrapped_ns"] += 1
        self.unmet("three_portal_resolutions", "including wrapping")

    def test_rejected_unsupported_missing_and_retained_runs(self):
        run = self.doc["resolutions"][0]
        for status in ("rejected", "unsupported", "missing"):
            with self.subTest(status=status):
                run["status"] = status
                self.unmet("three_portal_resolutions", "missing/rejected/unsupported")
        run["status"] = "completed"
        run["proof_origin"] = "retained"
        self.unmet("three_portal_resolutions", "retained/synthetic/missing")
        run["proof_origin"] = "synthetic"
        self.unmet("three_portal_resolutions")

    def test_vrf_proof_is_not_execution_proof(self):
        self.doc["resolutions"][0]["proof_kind"] = "vrf"
        self.unmet("three_portal_resolutions", "VRF proof is not")

    def test_resolve_success_and_full_instruction_required(self):
        resolve = self.doc["resolutions"][0]["resolve"]
        resolve["status"] = "rejected"
        self.unmet("three_portal_resolutions", "did not succeed")
        resolve["status"] = "success"
        resolve["instruction"] = "StandaloneVerifier"
        self.unmet("three_portal_resolutions", "full ResolveChallenge")
        resolve["instruction"] = "ResolveChallenge"
        resolve["verifier"] = "test"
        self.unmet("three_portal_resolutions", "full ResolveChallenge")

    def test_devnet_only_and_cpu_are_blocked(self):
        for kind in ("solana_devnet_only", "cpu"):
            with self.subTest(kind=kind):
                self.doc["environment"]["kind"] = kind
                self.rebind()
                self.unmet("live_environment", "agreed Northstar ER")

    def test_artifact_integrity_missing_and_reference_kind(self):
        item = self.doc["artifacts"][0]
        (self.root / item["path"]).write_bytes(b"changed bytes")
        self.invalid("SHA-256 mismatch")
        (self.root / item["path"]).unlink()
        self.invalid("No such file")
        self.doc["resolutions"][0]["proof_artifact"] = self.doc["environment"]["configuration_artifact"]
        self.invalid("wrong kind")

    def test_artifact_path_and_symlink_escape(self):
        item = self.doc["artifacts"][0]
        original = item["path"]
        item["path"] = "../outside"
        self.invalid("path must remain relative")
        item["path"] = original
        (self.root / original).unlink()
        (self.root / original).symlink_to(SCRIPT)
        self.invalid("within artifact root")

    def test_retained_artifact_and_reused_proof(self):
        item = next(item for item in self.doc["artifacts"] if item["kind"] == "proof")
        item["source"] = "retained"
        self.unmet("three_portal_resolutions", "captured proof artifact missing")
        self.doc["resolutions"][1]["proof_artifact"] = self.doc["resolutions"][0]["proof_artifact"]
        self.unmet("three_portal_resolutions", "distinct per run")

    def test_corpus_missing_case_or_incorrectly_accepted(self):
        case = self.doc["rejection_cases"].pop(0)
        self.unmet("rejection_corpus", "every pinned corpus case")
        self.doc["rejection_cases"].append(case)
        case["observed"] = "accepted"
        self.unmet("rejection_corpus", "expected rejected")

    def test_unsupported_profile_paths_fail_closed(self):
        self.doc["profile"]["paths"].remove("consumer_callback")
        self.rebind()
        self.unmet("pinned_profile", "execution coverage")
        self.unmet("three_portal_resolutions", "unsupported profile paths")

    def test_strict_version_types_and_bounds(self):
        original = copy.deepcopy(self.doc)
        for mutate in (
            lambda: self.doc.update(schema_version="northstar-vrf-evidence/v2"),
            lambda: self.doc["resolutions"][0]["resolve"].update(compute_units=True),
            lambda: self.doc["er_handoff"]["requests"][0].update(submitted_ns=-1),
            lambda: self.doc["er_handoff"].update(requests=self.doc["er_handoff"]["requests"] * 21),
        ):
            with self.subTest(mutate=mutate):
                self.doc = copy.deepcopy(original)
                mutate()
                self.assertEqual(self.report()["structural_validation"], "invalid")

    def test_strict_json_duplicate_nonfinite_and_size(self):
        path = self.root / "bad.json"
        for content in ('{"x":1,"x":2}', '{"x":NaN}', ' ' * (evidence.MAX_DOCUMENT + 1)):
            with self.subTest(content=content[:25]):
                path.write_text(content)
                with self.assertRaises(ValueError):
                    evidence.load_json(path)

    def test_cli_exit_codes_never_certify_even_all_reported_gates_met(self):
        path = self.root / "candidate.json"
        path.write_text(json.dumps(self.doc))
        for args, expected in (([], 1), (["--structure-only"], 0)):
            completed = subprocess.run([sys.executable, str(SCRIPT), str(path), *args], capture_output=True, text=True)
            self.assertEqual(completed.returncode, expected, completed.stderr)
            self.assertEqual(json.loads(completed.stdout)["final_acceptance"], "not_established")
        self.doc["accepted"] = True
        path.write_text(json.dumps(self.doc))
        completed = subprocess.run([sys.executable, str(SCRIPT), str(path)], capture_output=True, text=True)
        self.assertEqual(completed.returncode, 2)

    def test_schema_uses_only_supported_keywords(self):
        schema = evidence.load_json(evidence.SCHEMA)
        supported = {"$schema", "$id", "title", "$defs", "$ref", "type", "const", "enum", "required",
                     "properties", "additionalProperties", "items", "minItems", "maxItems", "uniqueItems",
                     "minLength", "maxLength", "pattern", "minimum", "maximum"}

        def visit(rule):
            self.assertFalse(set(rule) - supported, rule)
            if "$ref" in rule:
                self.assertTrue(rule["$ref"].startswith("#/$defs/"))
                self.assertIn(rule["$ref"].removeprefix("#/$defs/"), schema["$defs"])
            for mapping in ("$defs", "properties"):
                for child in rule.get(mapping, {}).values():
                    visit(child)
            if "items" in rule:
                visit(rule["items"])

        visit(schema)

    def test_unpinned_revision_and_missing_fixture_case(self):
        self.doc["revisions"]["application_revision"] = "master"
        self.invalid("invalid string format")
        self.doc["revisions"]["application_revision"] = "b" * 40
        self.doc["resolutions"][0]["fixture_id"] = None
        self.unmet("three_portal_resolutions", "fixture case identity missing")

    def test_unknown_fixture_id_cannot_be_ready(self):
        for run in self.doc["resolutions"]:
            run["fixture_id"] = "not-in-any-pinned-fixtures"
        report = self.unmet("three_portal_resolutions", "unknown fixture ID")
        self.assertNotEqual(report["status"], "ready_for_external_verification")

    def test_duplicate_fixture_manifest_id_is_invalid(self):
        self.fixtures_manifest["fixtures"].append(copy.deepcopy(self.fixtures_manifest["fixtures"][0]))
        self.pin_fixtures()
        self.invalid("fixtures manifest: duplicate id")

    def test_malformed_fixture_manifest_is_invalid(self):
        malformed = [
            {},
            {"schema_version": "northstar-vrf-fixtures/v2", "fixtures": []},
            {"schema_version": "northstar-vrf-fixtures/v1", "fixtures": []},
            {"schema_version": "northstar-vrf-fixtures/v1", "fixtures": [{"id": "synthetic-no-paths"}]},
            {"schema_version": "northstar-vrf-fixtures/v1", "fixtures": [dict(self.fixtures_manifest["fixtures"][0], paths=[])]},
            {"schema_version": "northstar-vrf-fixtures/v1", "fixtures": [dict(self.fixtures_manifest["fixtures"][0], paths=["unknown-path"])]},
        ]
        for manifest in malformed:
            with self.subTest(manifest=manifest):
                self.pin_fixtures(manifest)
                self.invalid("fixtures manifest:")
        self.pin_fixtures(raw=b"{not JSON; SYNTHETIC TEST ONLY}")
        self.invalid("fixtures manifest:")

    def test_missing_fixture_manifest_blocks_ready(self):
        self.doc["bindings"]["fixtures"] = None
        self.doc["profile"]["fixtures_sha256"] = None
        self.rebind()
        report = self.unmet("pinned_fixtures", "manifest unavailable")
        self.assertEqual(report["status"], "blocked")

    def test_run_paths_must_exactly_match_fixture_declaration(self):
        for paths in (["request"], ["vrf_verification", "consumer_callback"]):
            with self.subTest(paths=paths):
                self.doc["resolutions"][0]["paths"] = paths
                report = self.unmet("three_portal_resolutions", "exactly match pinned fixture paths")
                self.assertEqual(report["status"], "blocked")
        self.doc["resolutions"][0]["paths"] = sorted(evidence.PATHS)
        self.fixtures_manifest["fixtures"][0]["paths"] = ["request"]
        self.pin_fixtures()
        self.unmet("three_portal_resolutions", "exactly match pinned fixture paths")

    def test_valid_multi_fixture_coverage_window(self):
        request = dict(self.fixtures_manifest["fixtures"][0], id="synthetic-request-only", paths=["request"])
        fulfillment_artifact = self.artifact("synthetic-fulfillment-input", "fixture")
        fulfillment = {
            "id": "synthetic-fulfillment-callback", "paths": ["vrf_verification", "consumer_callback"],
            "artifact": fulfillment_artifact, "sha256": self.doc["artifacts"][-1]["sha256"],
        }
        self.fixtures_manifest["fixtures"] = [request, fulfillment]
        for run, fixture in zip(self.doc["resolutions"], (request, fulfillment, fulfillment)):
            run.update(fixture_id=fixture["id"], paths=list(reversed(fixture["paths"])))
        self.pin_fixtures()
        report = self.report()
        self.assertEqual(report["errors"], [])
        self.assertEqual(report["blockers"], [])
        self.assertEqual(report["status"], "ready_for_external_verification")
        self.assertEqual(report["final_acceptance"], "not_established")

    def test_fixture_window_must_cover_all_required_paths(self):
        self.fixtures_manifest["fixtures"][0]["paths"] = ["request"]
        for run in self.doc["resolutions"]:
            run["paths"] = ["request"]
        self.pin_fixtures()
        self.unmet("three_portal_resolutions", "window does not cover")

    def test_fixture_bytes_reference_and_digest_are_checked(self):
        entry = self.fixtures_manifest["fixtures"][0]
        entry["artifact"] = "synthetic-missing-fixture-bytes"
        self.pin_fixtures()
        self.invalid("missing artifact reference")
        entry["artifact"] = "synthetic-fixture-input"
        entry["sha256"] = "f" * 64
        self.pin_fixtures()
        self.invalid("digest differs from pinned fixtures manifest")

    def test_synthetic_fixture_bytes_cannot_satisfy_captured_gate(self):
        item = next(item for item in self.doc["artifacts"] if item["kind"] == "fixture")
        item["source"] = "synthetic"
        self.unmet("pinned_fixtures", "captured fixture bytes missing")

    def test_fixture_manifest_is_bounded(self):
        self.fixtures_manifest["fixtures"] *= 129
        self.pin_fixtures()
        self.invalid("array length out of bounds")

    def test_cpu_development_class_remains_blocked(self):
        self.doc["evidence_class"] = "cpu_development"
        self.unmet("live_environment", "development only")

    def test_incomplete_resolution_window(self):
        self.doc["resolutions"].pop()
        self.unmet("three_portal_resolutions", "three consecutive")

    def test_window_and_artifact_timestamps_require_timezone(self):
        self.doc["window"]["ended_at"] = "1999-01-01T00:00:00Z"
        self.invalid("end precedes start")
        self.doc["window"]["ended_at"] = "2000-01-01T00:00:01Z"
        self.doc["artifacts"][0]["captured_at"] = "2000-01-01T00:00:00"
        self.invalid("timestamp needs timezone")

    def test_artifact_budget_and_file_alias(self):
        original = evidence.MAX_ARTIFACT
        try:
            evidence.MAX_ARTIFACT = 1
            self.invalid("byte budget exceeded")
        finally:
            evidence.MAX_ARTIFACT = original
        alias = copy.deepcopy(self.doc["artifacts"][0])
        alias["id"] = "alias"
        self.doc["artifacts"].append(alias)
        self.invalid("alias the same file")

    def test_shipped_synthetic_example_can_only_be_blocked(self):
        path = FIXTURES / "synthetic-cpu-v1.json"
        report = evidence.validate(evidence.load_json(path), FIXTURES)
        self.assertEqual(report["structural_validation"], "valid", report)
        self.assertEqual(report["status"], "blocked")
        self.assertEqual(report["final_acceptance"], "not_established")
        completed = subprocess.run([sys.executable, str(SCRIPT), str(path)], capture_output=True, text=True)
        self.assertEqual(completed.returncode, 1)


if __name__ == "__main__":
    unittest.main()

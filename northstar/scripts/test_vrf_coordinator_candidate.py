"""Candidate arithmetic tests use fabricated artifacts, never live proof evidence."""

import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).with_name("vrf-coordinator-fixtures-candidate.py")
spec = importlib.util.spec_from_file_location("candidate_evidence", SCRIPT)
candidate = importlib.util.module_from_spec(spec)
spec.loader.exec_module(candidate)


class CandidateEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.packet = candidate.contract.load_json(candidate.contract.FIXTURES / "candidate-not-run-v2.json")
        identity = self.packet["identity"]
        for key in ("profile_sha256", "execution_profile_sha256", "guest_sha256", "verification_key_sha256", "runtime_binary_sha256", "feature_set_sha256", "er_genesis_config_sha256",
                    "capture_journal_sha256", "capture_collector_sha256", "configuration_sha256", "rejection_corpus_sha256", "proof_window_sha256"):
            identity[key] = "1" * 64
        identity["runtime_revision"] = "2" * 40
        raw_execution_profile = b"SIMULATED TEST ONLY; NOT A REAL BORSH EXECUTION PROFILE"
        self.packet["bindings"]["execution_profile"] = self.artifact("simulated-execution-profile", raw_execution_profile)
        identity["execution_profile_sha256"] = hashlib.sha256(b"northstar-vrf-profile-v1" + len(raw_execution_profile).to_bytes(8, "little") + raw_execution_profile).hexdigest()
        profile = dict(proof_kind=3, proof_version=1, checkpoint_steps=1, public_input_fields=list(candidate.PUBLIC_FIELDS),
                       production_acceptance=False, execution_profile_sha256=identity["execution_profile_sha256"])
        for key in ("profile", "guest", "verification_key"):
            ref = self.artifact("simulated-" + key, json.dumps(profile).encode() if key == "profile" else None)
            self.packet["bindings"][key] = ref
            identity[key + "_sha256"] = self.packet["artifacts"][-1]["sha256"]
        self.packet["bindings"]["configuration"] = self.artifact("simulated-config")
        identity["configuration_sha256"] = self.packet["artifacts"][-1]["sha256"]
        self.audit = dict(blockers=[], journal={"sha256": identity["capture_journal_sha256"]},
                          atomic_capture=dict(collector={"sha256": identity["capture_collector_sha256"]}, identities={
            "library_source_revision": candidate.capture.COMMIT, "runtime_source_revision": identity["runtime_revision"],
            "runtime_binary_sha256": identity["runtime_binary_sha256"], "genesis_config_sha256": identity["er_genesis_config_sha256"]},
            captures=[dict(wire_sha256=wire, input_sha256="3" * 64, output_sha256="4" * 64,
                           artifact={"sha256": "5" * 64}, feature_set_sha256=identity["feature_set_sha256"], profile=profile, status={"Ok": None})
                      for wire, profile in (("a" * 64, "consumer.make_request"), ("b" * 64, "coordinator.fulfill"))]))
        self.identity = candidate.identity_digest(identity, self.packet["contract_sha256"])
        for index in range(3):
            base = index * 200000000000
            timing = dict(clock_id="synthetic-unit-test-clock", **dict(zip(candidate.TIMES,
                [base + t * 1000000000 for t in (0, 1, 2, 121, 122, 123, 124, 125)])))
            self.packet["resolutions"].append(dict(
                id=f"simulated-run-{index}", sequence=index + 1, identity_sha256=self.identity,
                checkpoint_id=f"simulated-checkpoint-{index}", wire_sha256=("a" if index == 0 else "b") * 64,
                input_sha256="3" * 64, output_sha256="4" * 64, capture_sha256="5" * 64,
                paths=["request"] if index == 0 else ["fulfillment", "consumer_callback"], status="completed",
                proof_origin="fresh", proof_type="execution", hardware=dict(instance="g5.xlarge", model="A10G", count=1, device_id="synthetic-not-a-gpu"),
                timing=timing, resolve=dict(instruction="ResolveChallenge", verifier="Production", proof_kind=3, proof_version=1,
                    status="success", compute_units=130000, transaction=f"synthetic-not-a-chain-tx-{index}"),
                artifacts={key: self.artifact(f"simulated-{index}-{key}",
                    candidate.DOMAIN + (1).to_bytes(32, "big") * 7 if key == "public_inputs" else
                    candidate.SP1_PREFIX + bytes(32) + candidate.SP1_VERIFIER_ROOT + index.to_bytes(32, "big") + bytes(256)
                    if key == "wrapped_proof" else None) for key in sorted(candidate.RUN_ARTIFACTS)}))
        self.packet["lifecycle"] = dict(startup_ns=0, cold_start_ns=0, recovery_ns=0, artifact=self.artifact("simulated-lifecycle"))
        categories = ["incorrect_witness", "unsupported_path", "existing_route", "invalid_vrf_proof", "callback_rollback"]
        self.packet["rejection_cases"] = [dict(id="simulated-" + category, category=category,
            expected="accepted" if category == "existing_route" else "rejected",
            observed="accepted" if category == "existing_route" else "rejected", identity_sha256=self.identity,
            artifact=self.artifact("simulated-case-" + category)) for category in categories]
        corpus = dict(schema_version="northstar-vrf-candidate-rejection-corpus/v1",
                      cases=[{k: c[k] for k in ("id", "category", "expected")} for c in self.packet["rejection_cases"]])
        self.packet["bindings"]["rejection_corpus"] = self.artifact("simulated-corpus", json.dumps(corpus).encode())
        identity["rejection_corpus_sha256"] = self.packet["artifacts"][-1]["sha256"]
        self.packet["proof_window"].update(id="simulated-window", clock_id="synthetic-unit-test-clock", started_ns=0, ended_ns=600000000000)
        self.pin_attempts()

    def pin_attempts(self):
        log = dict(schema_version="northstar-vrf-candidate-proof-attempts/v1", clock_id=self.packet["proof_window"]["clock_id"],
                   attempts=[dict(**{key: run[key] for key in ("id", "sequence", "status", "checkpoint_id", "wire_sha256")},
                                  **{key: run["timing"][key] for key in ("submitted_ns", "wrapped_ns", "resolved_ns")})
                             for run in self.packet["resolutions"]])
        raw = json.dumps(log).encode()
        ref = self.packet["proof_window"]["attempts_artifact"]
        if ref is None:
            ref = self.artifact("simulated-proof-attempts", raw)
            self.packet["proof_window"]["attempts_artifact"] = ref
        item = next(a for a in self.packet["artifacts"] if a["id"] == ref)
        (self.root / item["path"]).write_bytes(raw)
        item.update(bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest())
        self.packet["identity"]["proof_window_sha256"] = item["sha256"]
        self.identity = candidate.identity_digest(self.packet["identity"], self.packet["contract_sha256"])
        for row in [*self.packet["resolutions"], *self.packet["rejection_cases"]]:
            row["identity_sha256"] = self.identity

    def artifact(self, identifier, raw=None):
        raw = raw if raw is not None else f"SYNTHETIC UNIT TEST ONLY; NOT LIVE: {identifier}\n".encode()
        path = identifier + ".test"
        (self.root / path).write_bytes(raw)
        self.packet["artifacts"].append(dict(id=identifier, path=path, bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest(), provenance="captured"))
        return identifier

    def report(self):
        return candidate.validate_packet(self.packet, self.audit, self.root)

    def test_review_profile_abi_count_and_policy_hash_reject_drift(self):
        ref = self.packet["bindings"]["profile"]
        item = next(a for a in self.packet["artifacts"] if a["id"] == ref)
        original = json.loads((self.root / item["path"]).read_bytes())
        for field, value in (("checkpoint_steps", 0), ("checkpoint_steps", 17), ("production_acceptance", True),
                             ("execution_profile_sha256", "0" * 64), ("public_input_fields", list(reversed(candidate.PUBLIC_FIELDS)))):
            document = dict(original, **{field: value})
            raw = json.dumps(document).encode()
            item.update(bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest())
            (self.root / item["path"]).write_bytes(raw)
            self.packet["identity"]["profile_sha256"] = item["sha256"]
            self.pin_attempts()
            with self.assertRaises(candidate.capture.Invalid):
                self.report()

    def test_contract_snapshot_cannot_silently_change_target(self):
        document = candidate.contract.load_json(candidate.CONTRACT)
        document["hardware"]["gpu_model"] = "L40S"
        path = self.root / "changed-contract.json"
        path.write_text(json.dumps(document))
        with patch.object(candidate, "CONTRACT", path):
            with self.assertRaisesRegex(candidate.capture.Invalid, "contract snapshot drift"):
                self.report()

    def test_g5_boundary_is_consistent_as_reported_but_never_acceptance(self):
        report = self.report()
        self.assertEqual(report["reported_engineering_gates"], "consistent_as_reported_requires_external_verification")
        self.assertEqual(report["final_acceptance"], "not_established")
        self.assertIn("authenticated external", report["blockers"][0])
        self.assertEqual(report["measurements"][0]["prove_and_wrap_ns"], 120000000000)

    def test_over_threshold_individual_run_not_hidden_by_average(self):
        self.packet["resolutions"][1]["timing"]["wrapped_ns"] += 1
        self.pin_attempts()
        self.assertTrue(any("exceeds 120s" in b for b in self.report()["blockers"]))
        self.packet["resolutions"][1]["resolve"]["compute_units"] += 1
        self.assertTrue(any("exceeds 130000" in b for b in self.report()["blockers"]))

    def test_l40s_and_other_device_do_not_satisfy_selected_target(self):
        self.packet["resolutions"][0]["hardware"]["model"] = "L40S"
        self.assertTrue(any("g5.xlarge/A10G" in b for b in self.report()["blockers"]))
        self.packet["resolutions"][1]["hardware"]["device_id"] = "other"
        self.assertTrue(any("share one device" in b for b in self.report()["blockers"]))

    def test_old_kind_wrong_domain_algorithm_and_approval_claim_reject(self):
        changes = [lambda p: p["identity"].update(proof_kind=2), lambda p: p["identity"].update(proof_version=True),
                   lambda p: p["identity"].update(domain_hex="00" * 32), lambda p: p["identity"].update(algorithm="C2SP-ristretto255"), lambda p: p["identity"].update(suite=True),
                   lambda p: p.update(independent_acceptance="accepted")]
        for change in changes:
            with self.subTest(change=change):
                saved = copy.deepcopy(self.packet)
                change(self.packet)
                with self.assertRaises(candidate.capture.Invalid):
                    self.report()
                self.packet = saved

    def test_stage_order_clock_domain_and_failed_window(self):
        self.packet["resolutions"][0]["timing"]["core_done_ns"] = 0
        with self.assertRaisesRegex(candidate.capture.Invalid, "clock ordering"):
            self.report()
        self.packet["resolutions"][0]["timing"]["core_done_ns"] = 121000000000
        self.packet["resolutions"][0]["timing"]["clock_id"] = "different"
        with self.assertRaisesRegex(candidate.capture.Invalid, "window/run clock"):
            self.report()
        self.packet["resolutions"][0]["timing"]["clock_id"] = "synthetic-unit-test-clock"
        self.packet["resolutions"][1]["status"] = "failed"
        self.pin_attempts()
        self.assertTrue(any("fresh completed" in b for b in self.report()["blockers"]))

    def test_standalone_or_randomness_proof_cannot_qualify(self):
        self.packet["resolutions"][0]["proof_type"] = "randomness"
        self.packet["resolutions"][0]["resolve"]["instruction"] = "Verify"
        report = self.report()
        self.assertTrue(any("fresh completed execution" in b for b in report["blockers"]))
        self.assertTrue(any("standalone Verify" in b for b in report["blockers"]))

    def test_output_fixture_and_path_drift_reject(self):
        self.packet["resolutions"][0]["output_sha256"] = "0" * 64
        with self.assertRaisesRegex(candidate.capture.Invalid, "input/output identity"):
            self.report()
        self.packet["resolutions"][0]["output_sha256"] = "4" * 64
        self.packet["resolutions"][0]["paths"].append("consumer_callback")
        with self.assertRaisesRegex(candidate.capture.Invalid, "path drift"):
            self.report()

    def test_omitted_failed_proof_attempt_rejects(self):
        ref = self.packet["proof_window"]["attempts_artifact"]
        item = next(a for a in self.packet["artifacts"] if a["id"] == ref)
        log = json.loads((self.root / item["path"]).read_bytes())
        log["attempts"].append(dict(log["attempts"][-1], id="simulated-failed-attempt", sequence=4, status="failed"))
        raw = json.dumps(log).encode()
        (self.root / item["path"]).write_bytes(raw)
        item.update(bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest())
        self.packet["identity"]["proof_window_sha256"] = item["sha256"]
        identity = candidate.identity_digest(self.packet["identity"], self.packet["contract_sha256"])
        for row in [*self.packet["resolutions"], *self.packet["rejection_cases"]]:
            row["identity_sha256"] = identity
        with self.assertRaisesRegex(candidate.capture.Invalid, "omitted failure/attempt"):
            self.report()

    def test_whole_capture_and_sidecar_identity_drift_reject(self):
        self.audit["journal"]["sha256"] = "9" * 64
        with self.assertRaisesRegex(candidate.capture.Invalid, "journal capture identity"):
            self.report()
        self.audit["journal"]["sha256"] = self.packet["identity"]["capture_journal_sha256"]
        self.packet["resolutions"][0]["capture_sha256"] = "9" * 64
        with self.assertRaisesRegex(candidate.capture.Invalid, "input/output identity"):
            self.report()

    def test_omitted_corpus_result_rejects(self):
        self.packet["rejection_cases"].pop()
        with self.assertRaisesRegex(candidate.capture.Invalid, "membership drift"):
            self.report()

    def test_guest_bytes_identity_drift_rejects(self):
        ref = self.packet["bindings"]["guest"]
        item = next(a for a in self.packet["artifacts"] if a["id"] == ref)
        (self.root / item["path"]).write_text("changed")
        with self.assertRaisesRegex(candidate.capture.Invalid, "SHA-256 mismatch"):
            self.report()

    def test_public_input_old_domain_rejects(self):
        ref = self.packet["resolutions"][0]["artifacts"]["public_inputs"]
        item = next(a for a in self.packet["artifacts"] if a["id"] == ref)
        old_domain = bytearray(candidate.DOMAIN)
        old_domain[30] = 2
        raw = bytes(old_domain) + bytes(224)
        item.update(bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest())
        (self.root / item["path"]).write_bytes(raw)
        with self.assertRaisesRegex(candidate.capture.Invalid, "public input/domain drift"):
            self.report()

    def test_wrapped_envelope_exit_root_and_nonce_reject(self):
        ref = self.packet["resolutions"][0]["artifacts"]["wrapped_proof"]
        item = next(a for a in self.packet["artifacts"] if a["id"] == ref)
        original = (self.root / item["path"]).read_bytes()
        mutations = [original[:-1], b"bad!" + original[4:], original[:4] + b"\x01" + original[5:],
                     original[:36] + bytes(32) + original[68:],
                     original[:68] + candidate.SCALAR_MODULUS.to_bytes(32, "big") + original[100:]]
        for raw in mutations:
            item.update(bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest())
            (self.root / item["path"]).write_bytes(raw)
            with self.assertRaises(candidate.capture.Invalid):
                self.report()

    def test_slot_step_zero_is_not_misclassified_as_missing_commitment(self):
        ref = self.packet["resolutions"][0]["artifacts"]["public_inputs"]
        item = next(a for a in self.packet["artifacts"] if a["id"] == ref)
        raw = candidate.DOMAIN + (1).to_bytes(32, "big") + bytes(32) + (1).to_bytes(32, "big") * 5
        item.update(bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest())
        (self.root / item["path"]).write_bytes(raw)
        self.assertEqual(self.report()["final_acceptance"], "not_established")

    def test_zero_public_commitment_and_noncanonical_scalar_reject(self):
        ref = self.packet["resolutions"][0]["artifacts"]["public_inputs"]
        item = next(a for a in self.packet["artifacts"] if a["id"] == ref)
        for field in (0, candidate.SCALAR_MODULUS):
            raw = candidate.DOMAIN + field.to_bytes(32, "big") + (1).to_bytes(32, "big") * 6
            item.update(bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest())
            (self.root / item["path"]).write_bytes(raw)
            with self.assertRaises(candidate.capture.Invalid):
                self.report()

    def test_retained_provenance_stays_blocked(self):
        self.packet["artifacts"][0]["provenance"] = "retained"
        self.assertTrue(any("not fresh candidate" in b for b in self.report()["blockers"]))

    def test_missing_actual_sidecars_remain_missing(self):
        self.audit.pop("atomic_capture")
        report = self.report()
        self.assertTrue(any("atomic input/output" in b for b in report["blockers"]))
        self.assertTrue(any("coverage incomplete" in b for b in report["blockers"]))

    def test_legacy_acceptance_contract_not_reinterpreted(self):
        legacy = candidate.contract.load_json(candidate.contract.FIXTURES.parent / "vrf-evidence/schema-v1.json")
        self.assertEqual(legacy["properties"]["schema_version"]["const"], "northstar-vrf-evidence/v1")
        source = SCRIPT.with_name("vrf-evidence-validate.py").read_text()
        self.assertIn('model"] == "L40S"', source)
        self.assertNotIn('"g5.xlarge"', source)


if __name__ == "__main__":
    unittest.main()

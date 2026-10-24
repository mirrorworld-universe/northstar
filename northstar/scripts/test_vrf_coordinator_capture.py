"""Real retained-capture tests are opt-in; mutated records are synthetic tests only."""

import copy
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("vrf-coordinator-fixtures-capture.py")
spec = importlib.util.spec_from_file_location("capture", SCRIPT)
capture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(capture)
ARTIFACTS = os.environ.get("NORTHSTAR_VRF_RUNTIME_ARTIFACTS")
SIDECARS = os.environ.get("NORTHSTAR_VRF_CANDIDATE_CAPTURE")


class ParserTests(unittest.TestCase):
    def test_legacy_wire_requires_canonical_lengths_and_complete_bytes(self):
        for raw in (b"", b"\x80\x00", b"\xff\xff\xff", b"\x01" + b"\0" * 64 + b"\x81\0\0"):
            with self.assertRaises(capture.Invalid):
                capture.parse_wire(raw)

    def test_byte_lengths_do_not_accept_booleans_or_out_of_range(self):
        for value in ([True], [256], [-1], "00"):
            with self.assertRaises(capture.Invalid):
                capture.byte_array(value)

    def test_atomic_sidecar_unknown_fields_and_versions_reject(self):
        sidecar = {key: None for key in capture.SIDECAR_FIELDS}
        sidecar.update(approved=True)
        with self.assertRaisesRegex(capture.Invalid, "unknown/missing"):
            capture.validate_atomic_sidecar(sidecar)
        sidecar.pop("approved")
        sidecar.update(domain=capture.CAPTURE_DOMAIN, version=2)
        with self.assertRaisesRegex(capture.Invalid, "domain/version"):
            capture.validate_atomic_sidecar(sidecar)

    def test_account_absence_remains_explicit(self):
        key = "11111111111111111111111111111111"
        self.assertEqual(capture.account_map([dict(pubkey=key, account=None)], "test"), {key: None})
        with self.assertRaisesRegex(capture.Invalid, "unknown/missing"):
            capture.account_map([dict(pubkey=key)], "test")
        with self.assertRaisesRegex(capture.Invalid, "duplicate"):
            capture.account_map([dict(pubkey=key, account=None)] * 2, "test")

    def test_compact_receipts_select_exact_known_run(self):
        path = capture.contract.FIXTURES / "first-wave-capture-receipts.json"
        receipt = capture.load_receipt(path, "cycles-100-final")
        self.assertEqual(receipt["journal"]["bytes"], 17650190)
        with self.assertRaisesRegex(capture.Invalid, "unknown retained"):
            capture.load_receipt(path, "unknown")
        with self.assertRaisesRegex(capture.Invalid, "requires"):
            capture.load_receipt(None, "cycles-100-final")

    def test_partial_jsonl_duplicate_key_and_nonfinite_reject(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "test.jsonl"
            for raw in (b'{"kind":"setup"}', b'{"kind":"setup","kind":"summary"}\n', b'{"kind":"setup","x":NaN}\n'):
                path.write_bytes(raw)
                with self.assertRaises(capture.Invalid):
                    capture.read_journal(path)


@unittest.skipUnless(ARTIFACTS, "set NORTHSTAR_VRF_RUNTIME_ARTIFACTS to retained runtime directory")
class ActualCaptureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = Path(ARTIFACTS)
        cls.rows = capture.read_journal(cls.root / "smoke-2.jsonl")
        cls.receipts = capture.contract.load_json(capture.contract.FIXTURES / "first-wave-capture-receipts.json")["runs"]
        cls.manifest = capture.contract.validate_manifest(capture.contract.load_json(capture.contract.FIXTURES / "compatibility-manifest.json"), capture.contract.FIXTURES)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.path = Path(self.temp.name) / "synthetic-mutated-journal.jsonl"
        self.rows = copy.deepcopy(type(self).rows)

    def audit_mutation(self, receipt=None):
        self.path.write_text("".join(json.dumps(row, separators=(",", ":")) + "\n" for row in self.rows))
        return capture.audit_journal(self.path, self.root / "smoke-2.transactions", self.manifest, receipt)

    def row(self, kind):
        return next(r for r in self.rows if r["kind"] == kind)

    def test_actual_final_100_cycle_capture_and_receipt(self):
        result = capture.audit_journal(self.root / "cycles-100-final.jsonl", manifest=self.manifest,
                                       receipt=self.receipts["cycles-100-final"])
        self.assertEqual(result["signed_transactions"], 305)
        self.assertEqual(result["successful_callbacks"], 100)
        self.assertEqual(result["expected_rejections"][0]["code"], 6003)
        self.assertIsNone(result["clock_id"])
        self.assertIn("collector_wide_monotonic_clock", result["missing"])
        self.assertIn("authoritative_atomic_prestate_and_computed_poststate", result["missing"])
        self.assertIn("negative_controls_embedded_in_success_window", result["blockers"])
        self.assertFalse(result["signature_cryptography_verified"])
        self.assertEqual(result["final_acceptance"], "not_established")

    def test_actual_final_callback_rollback_capture_and_receipt(self):
        result = capture.audit_journal(self.root / "callback-failure-final.jsonl", manifest=self.manifest,
                                       receipt=self.receipts["callback-failure-final"])
        self.assertEqual(result["successful_callbacks"], 0)
        self.assertEqual([r["code"] for r in result["expected_rejections"]], [6003, 6001])
        self.assertIn("negative_control_only", result["blockers"])

    def test_source_and_provenance_drift_reject(self):
        for field, value in (("source_revision", "0" * 40), ("level", "hosted-er"), ("delegated_handoff", True), ("execution_proof", True)):
            with self.subTest(field=field):
                self.rows = copy.deepcopy(type(self).rows)
                self.rows[0][field] = value
                with self.assertRaises(capture.Invalid):
                    self.audit_mutation()

    def test_algorithm_and_proof_length_drift_reject(self):
        self.row("request")["account"]["data"][227] = 2
        with self.assertRaisesRegex(capture.Invalid, "algorithm drift"):
            self.audit_mutation()
        self.rows = copy.deepcopy(type(self).rows)
        self.row("request")["proof"]["proof"].pop()
        with self.assertRaisesRegex(capture.Invalid, "length drift"):
            self.audit_mutation()

    def test_alpha_key_output_drift_reject(self):
        for kind, field, index in (("request", "data", 163), ("callback", "data", 73)):
            with self.subTest(kind=kind):
                self.rows = copy.deepcopy(type(self).rows)
                self.row(kind)["account"][field][index] ^= 1
                with self.assertRaises(capture.Invalid):
                    self.audit_mutation()

    def test_wire_hash_drift_reject(self):
        self.row("wire_transaction")["sha256"] = "0" * 64
        with self.assertRaisesRegex(capture.Invalid, "SHA-256 mismatch"):
            self.audit_mutation()

    def test_omitted_failure_rejects_even_if_all_its_rows_removed(self):
        label = "invariant_invalid_proof_preserves_pending_request"
        self.rows = [r for r in self.rows if r.get("label") != label and r["kind"] != "invariant"]
        with self.assertRaisesRegex(capture.Invalid, "omitted failure"):
            self.audit_mutation()

    def test_negative_preservation_observation_is_required(self):
        self.rows = [r for r in self.rows if r["kind"] != "invariant"]
        with self.assertRaisesRegex(capture.Invalid, "preservation observation omitted"):
            self.audit_mutation()

    def test_missing_duplicate_callback_reject(self):
        self.rows = [r for r in self.rows if r["kind"] != "callback"]
        with self.assertRaisesRegex(capture.Invalid, "callback observations"):
            self.audit_mutation()
        self.rows = copy.deepcopy(type(self).rows)
        self.rows.insert(-1, copy.deepcopy(self.row("callback")))
        with self.assertRaisesRegex(capture.Invalid, "duplicate callback"):
            self.audit_mutation()

    def test_sealed_receipt_detects_otherwise_consistent_metadata_change(self):
        receipt = dict(journal=capture.file_identity(self.root / "smoke-2.jsonl"))
        self.rows[0]["synthetic_setup"].append("SYNTHETIC mutation")
        with self.assertRaisesRegex(capture.Invalid, "receipt mismatch"):
            self.audit_mutation(receipt)

    def test_synthetic_single_clock_is_consistent_but_never_execution_verified(self):
        for sequence, row in enumerate(self.rows, 1):
            row.update(clock_domain="synthetic-unit-clock", observed_ns=sequence * 100, capture_sequence=sequence)
            if row["kind"] == "submission":
                row["submitted_ns"] = sequence * 100 - 1
        result = self.audit_mutation()
        self.assertNotIn("collector_wide_monotonic_clock", result["missing"])
        self.assertFalse(result["complete_execution_witness"])
        self.assertEqual(result["final_acceptance"], "not_established")
        self.rows[5]["observed_ns"] = 1
        with self.assertRaisesRegex(capture.Invalid, "clock drift"):
            self.audit_mutation()

    def test_mixed_clock_is_rejected_instead_of_inventing_times(self):
        for sequence, row in enumerate(self.rows, 1):
            row.update(clock_domain="synthetic-a", observed_ns=sequence)
        self.rows[3]["clock_domain"] = "synthetic-b"
        with self.assertRaisesRegex(capture.Invalid, "mixed collector clock"):
            self.audit_mutation()


@unittest.skipUnless(SIDECARS, "set NORTHSTAR_VRF_CANDIDATE_CAPTURE to real atomic sidecars")
class AtomicCaptureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = Path(SIDECARS)
        cls.sidecar = capture.load_sidecar(sorted(cls.root.glob("[0-9][0-9][0-9][0-9][0-9].json"))[0])

    def setUp(self):
        self.doc = copy.deepcopy(type(self).sidecar)

    def rebind(self):
        self.doc["input_sha256"] = capture.sha(capture.canonical_capture(self.doc["input"]))
        self.doc["output_sha256"] = capture.sha(capture.canonical_capture({"execution": self.doc["execution"],
                                                                      "committed_accounts": self.doc["committed_accounts"]}))

    def test_actual_complete_atomic_sidecar(self):
        result = capture.validate_atomic_sidecar(self.doc)
        self.assertTrue(result["atomic_source_shape_validated"])
        self.assertFalse(result["execution_verified"])
        self.assertEqual(len(result["input_sha256"]), 64)

    def actual_journal_audit(self):
        path = Path(os.environ.get("NORTHSTAR_VRF_CANDIDATE_JOURNAL", str(self.root.with_suffix(".jsonl"))))
        if not path.is_file():
            self.skipTest("set NORTHSTAR_VRF_CANDIDATE_JOURNAL for the paired raw journal")
        return capture.audit_journal(path)

    def test_actual_complete_directory_is_correlated_to_real_wires(self):
        audit = self.actual_journal_audit()
        atomic = capture.audit_sidecars(self.root, audit)
        capture.attach_atomic_capture(audit, atomic)
        self.assertEqual(audit["clock_id"], atomic["clock_domain"])
        self.assertEqual(len(atomic["captures"]), sum(w["instruction"] in ("make_request", "fulfill") for w in audit["wires"]))
        self.assertFalse(audit["complete_execution_witness"])
        self.assertEqual(audit["final_acceptance"], "not_established")
        if self.root.name == "atomic-100.capture":
            receipt = capture.contract.load_json(capture.contract.FIXTURES / "atomic-capture-receipts-v2.json")["runs"]["atomic-100"]
            self.assertEqual(audit["journal"], receipt["journal"])
            self.assertEqual(capture.sha(capture.canonical(atomic["captures"])), receipt["sidecar_catalog_sha256"])
            self.assertEqual(audit["successful_callbacks"], 100)
            self.assertEqual(audit["signed_transactions"], 304)
            self.assertEqual(atomic["source_provenance_issues"], [])
            self.assertEqual(audit["reported_runtime_window"], "consistent_as_reported_requires_external_verification")

    def test_compiled_source_receipt_mismatch_remains_a_blocker(self):
        collector = capture.load_sidecar(self.root / "collector.json")
        identities = copy.deepcopy(collector["identities"])
        identities["runtime_untracked_sources"] = [dict(path="northstar/tests/vrf_coordinator_runtime.rs", sha256="0" * 64)]
        self.assertIn("compiled harness hash differs from captured source receipt", capture.source_provenance_issues(identities))
        with self.assertRaisesRegex(capture.Invalid, "collector hash binding"):
            capture.validate_atomic_sidecar(self.doc, collector_sha256="0" * 64)

    def test_omitted_sidecars_reject_even_when_end_count_is_changed(self):
        audit = self.actual_journal_audit()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "collector.json").write_bytes((self.root / "collector.json").read_bytes())
            end = capture.load_sidecar(self.root / "collector-end.json")
            (root / "collector-end.json").write_text(json.dumps(end))
            with self.assertRaisesRegex(capture.Invalid, "omission/count"):
                capture.audit_sidecars(root, audit)
            end["records"] = 0
            (root / "collector-end.json").write_text(json.dumps(end))
            with self.assertRaisesRegex(capture.Invalid, "sidecars omitted"):
                capture.audit_sidecars(root, audit)

    def test_atomic_field_clock_wire_and_pin_mutations_reject(self):
        mutations = [lambda d: d.update(start_ns=d["end_ns"] + 1),
                     lambda d: d.update(candidate_proof_kind=2), lambda d: d.update(observation_id="wrong"),
                     lambda d: d.update(prestate_rechecked_before_commit=False),
                     lambda d: d.update(execution_verified=True),
                     lambda d: d["input"].update(domain="unknown"),
                     lambda d: d["input"]["programs"][0].update(elf_sha256="0" * 64),
                     lambda d: d["execution"].update(fee=1),
                     lambda d: d["committed_accounts"].pop()]
        for mutation in mutations:
            with self.subTest(mutation=mutation):
                self.doc = copy.deepcopy(type(self).sidecar)
                mutation(self.doc)
                self.rebind()
                with self.assertRaises(capture.Invalid):
                    capture.validate_atomic_sidecar(self.doc)

    def test_sysvar_bytes_and_explicit_absence_mutations_reject(self):
        required = next(r for r in self.doc["input"]["sysvars"] if r["cached_data"])
        required["cached_data"][0] ^= 1
        self.rebind()
        with self.assertRaisesRegex(capture.Invalid, "cache/account drift"):
            capture.validate_atomic_sidecar(self.doc)
        self.doc = copy.deepcopy(type(self).sidecar)
        absent = next(r for r in self.doc["input"]["accounts"] if r["account"] is None)
        self.doc["input"]["accounts"].remove(absent)
        self.rebind()
        with self.assertRaises(capture.Invalid):
            capture.validate_atomic_sidecar(self.doc)


if __name__ == "__main__":
    unittest.main()

"""Offline contract tests. Every generated runtime record is simulated, not live."""

import copy
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("vrf-coordinator-fixtures.py")
spec = importlib.util.spec_from_file_location("coordinator_fixtures", SCRIPT)
fixtures = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixtures)
ROOT = fixtures.FIXTURES


class FixtureTests(unittest.TestCase):
    def setUp(self):
        self.manifest = fixtures.load_json(ROOT / "compatibility-manifest.json")

    def test_pinned_manifest_and_bytes(self):
        fixtures.validate_manifest(self.manifest, ROOT)

    def test_pin_algorithm_and_lengths_reject_drift(self):
        for path, value, reason in [
            (("source", "commit"), "0" * 40, "pin drift"),
            (("algorithm", "name"), "C2SP-ristretto255", "algorithm/suite drift"),
            (("algorithm", "suite"), 2, "algorithm/suite drift"),
            (("algorithm", "proof_bytes"), 64, "length drift"),
            (("algorithm", "output_bytes"), 32, "length drift"),
        ]:
            with self.subTest(path=path):
                manifest = copy.deepcopy(self.manifest)
                manifest[path[0]][path[1]] = value
                with self.assertRaisesRegex(fixtures.Invalid, reason):
                    fixtures.validate_manifest(manifest, ROOT)

    def test_order_flags_discriminator_layout_and_claim_drift(self):
        changes = [
            lambda m: m["instructions"]["request_random"]["accounts"].reverse(),
            lambda m: m["instructions"]["fulfill"]["accounts"][4].update(signer=True),
            lambda m: m["instructions"]["fulfill"].update(discriminator_hex="00" * 8),
            lambda m: m["callback"].update(accounts=["unordered"]),
            lambda m: m["lifecycle"].update(model="arena queue"),
            lambda m: m["programs"]["reference_verifier"].update(id="another-program"),
            lambda m: m["acceptance"].update(jonas="accepted"),
            lambda m: m.update(accepted=True),
        ]
        for change in changes:
            with self.subTest(change=change):
                manifest = copy.deepcopy(self.manifest)
                change(manifest)
                with self.assertRaisesRegex(fixtures.Invalid, "pinned manifest drift"):
                    fixtures.validate_manifest(manifest, ROOT)

    def test_fixture_tampering(self):
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            for row in self.manifest["artifacts"]:
                (root / row["path"]).write_bytes((ROOT / row["path"]).read_bytes())
            (root / "alpha-golden.json").write_text("{}")
            with self.assertRaisesRegex(fixtures.Invalid, "SHA-256 mismatch"):
                fixtures.validate_manifest(self.manifest, root)

    def test_alpha_is_exact_golden_and_not_rfc_empty_alpha(self):
        alpha = fixtures.load_json(ROOT / "alpha-golden.json")
        self.assertEqual(alpha["alpha_hex"], "85d739f7f781f84ed02f7f3f15be1c87f985a893a464b5355c94501bb15cdefb")
        self.assertEqual(len(bytes.fromhex(alpha["preimage_hex"])), 156)
        vector = fixtures.load_json(ROOT / "rfc9381-vectors.json")["vectors"][0]
        self.assertEqual(vector["alpha_hex"], "")
        self.assertNotEqual(alpha["alpha_hex"], vector["alpha_hex"])

    def test_callback_wire_has_no_vector_length_prefix(self):
        wire = fixtures.load_json(ROOT / "encoding-only.json")
        data = bytes.fromhex(wire["callback_hex"])
        self.assertEqual(len(data), 106)
        self.assertEqual(data[:8], fixtures.discriminator("on_random"))
        self.assertEqual(data[8:40], b"\x07" * 32)
        self.assertEqual(data[104:], b"\xaa\xbb")
        self.assertFalse(wire["proof_matches_alpha"])
        self.assertEqual(len(bytes.fromhex(wire["verify_hex"])), 157)
        self.assertEqual(len(bytes.fromhex(wire["fulfill_hex"])), 92)

    def test_request_borsh_tags_and_lengths(self):
        wire = fixtures.load_json(ROOT / "encoding-only.json")
        callback = bytes.fromhex(wire["request_callback_hex"])
        store = bytes.fromhex(wire["request_store_hex"])
        self.assertEqual(len(callback), 132)
        self.assertEqual(len(store), 75)
        self.assertEqual(callback[:8], fixtures.discriminator("request_random"))
        self.assertEqual(callback[72:77], b"\x00\x01\x1e\x00\x01")
        self.assertEqual(store[72:], b"\x01\x00\x00")
        self.assertEqual(callback[89:93], (1).to_bytes(4, "little"))
        self.assertEqual(callback[125], 1)
        self.assertEqual(callback[126:130], (2).to_bytes(4, "little"))
        self.assertEqual(callback[130:], b"\xaa\xbb")

    def test_manifest_null_source_rejects_cleanly(self):
        self.manifest["source"] = None
        with self.assertRaisesRegex(fixtures.Invalid, "pin drift"):
            fixtures.validate_manifest(self.manifest, ROOT)

    def test_outer_and_inner_callback_signer_differ(self):
        fulfill_signer = self.manifest["instructions"]["fulfill"]["accounts"][4]
        self.assertEqual(fulfill_signer["name"], "callback_signer")
        self.assertFalse(fulfill_signer["signer"])
        self.assertTrue(fulfill_signer["optional"])
        self.assertTrue(self.manifest["instructions"]["on_random"]["accounts"][0]["signer"])

    def test_json_parser_rejects_duplicate_nonfinite_and_oversize(self):
        with tempfile.TemporaryDirectory() as name:
            path = Path(name) / "bad.json"
            for payload in (b'{"x":1,"x":2}', b'{"x":NaN}', b" " * (fixtures.MAX_JSON + 1)):
                path.write_bytes(payload)
                with self.assertRaises(fixtures.Invalid):
                    fixtures.load_json(path)

    def test_source_verification_requires_actual_files(self):
        with tempfile.TemporaryDirectory() as name:
            with self.assertRaises(FileNotFoundError):
                fixtures.validate_manifest(self.manifest, ROOT, name)


class CaptureTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.manifest = fixtures.load_json(ROOT / "compatibility-manifest.json")
        self.doc = fixtures.load_json(ROOT / "capture-not-run.json")
        self.doc["evidence_class"] = "local_er"
        self.doc["clock_id"] = "simulated-test-clock-not-live"
        self.doc["window"].update(started_ns=0, ended_ns=10_000)
        self.doc["environment"].update(runtime_revision="1" * 40, runtime_binary_sha256="2" * 64,
                                       er_genesis_hash="simulated-er-genesis", l1_genesis_hash="simulated-l1-genesis",
                                       feature_set_sha256="3" * 64)
        for program in self.doc["environment"]["programs"].values():
            program["binary_sha256"] = "4" * 64
        artifact = self.artifact("simulated-log", b"SIMULATED TEST; NOT LIVE RUNTIME EVIDENCE\n")
        self.doc["window"]["attempts_artifact"] = artifact
        self.doc["environment"]["configuration_artifact"] = artifact
        for i in range(1, 101):
            req = f"simulated-request-{i}"
            tx = f"simulated-request-tx-{i}"
            fulfill_tx = f"simulated-fulfill-tx-{i}"
            self.doc["requests"].append(dict(id=req, request_address=f"simulated-pda-{i}", sequence=i,
                                            submitted_ns=i * 10, status="fulfilled", transaction=tx, artifact=artifact))
            self.doc["callbacks"].append(dict(id=f"simulated-callback-{i}", request_id=req, sequence=i,
                                             observed_ns=i * 10 + 2, status="success", transaction=fulfill_tx,
                                             output_hex="00" * 64, deliveries=1, artifact=artifact))
            for stage, time, transaction in (("request", i * 10, tx), ("fulfill", i * 10 + 1, fulfill_tx),
                                              ("callback", i * 10 + 2, fulfill_tx)):
                self.doc["attempts"].append(dict(id=f"simulated-{stage}-{i}", request_id=req,
                                                sequence=len(self.doc["attempts"]) + 1, stage=stage, status="success",
                                                observed_ns=time, transaction=transaction, artifact=artifact, reason=None))

    def artifact(self, identifier, raw):
        path = identifier + ".txt"
        (self.root / path).write_bytes(raw)
        self.doc["artifacts"].append(dict(id=identifier, path=path, sha256=hashlib.sha256(raw).hexdigest(),
                                          bytes=len(raw), provenance="captured"))
        return identifier

    def validate(self):
        return fixtures.validate_capture(self.doc, self.manifest, self.root)

    def test_consistent_simulated_capture_reports_no_arithmetic_blockers_only(self):
        self.assertEqual(self.validate(), [])

    def test_empty_shipped_capture_blocks(self):
        self.doc = fixtures.load_json(ROOT / "capture-not-run.json")
        blockers = self.validate()
        self.assertIn("at least 100 request/callback cycles required", blockers)
        self.assertTrue(any("not actual ER" in b for b in blockers))

    def test_pin_drift(self):
        self.doc["compatibility_sha256"] = "f" * 64
        with self.assertRaisesRegex(fixtures.Invalid, "pin mismatch"):
            self.validate()

    def test_shape_type_version_unknown_length_rejection(self):
        changes = [lambda d: d.update(approved=True), lambda d: d.update(schema_version="v2"),
                   lambda d: d["requests"][0].update(sequence=True),
                   lambda d: d["callbacks"][0].update(output_hex="00" * 32),
                   lambda d: d["callbacks"][0].update(output_hex="AA" * 64),
                   lambda d: d["environment"].update(runtime_revision="short")]
        for change in changes:
            with self.subTest(change=change):
                saved = copy.deepcopy(self.doc)
                change(self.doc)
                with self.assertRaises(fixtures.Invalid):
                    self.validate()
                self.doc = saved

    def test_native_diagnostic_cannot_be_er(self):
        self.doc["evidence_class"] = "native_diagnostic"
        self.assertTrue(any("not actual ER" in b for b in self.validate()))

    def test_99_cycles_block(self):
        self.doc["requests"].pop()
        self.doc["callbacks"].pop()
        self.doc["attempts"] = self.doc["attempts"][:-3]
        self.assertIn("at least 100 request/callback cycles required", self.validate())

    def test_missing_and_duplicate_callback_block(self):
        self.doc["callbacks"][-1]["request_id"] = "simulated-request-99"
        self.doc["callbacks"][-1]["transaction"] = "simulated-fulfill-tx-99"
        self.doc["attempts"][-1].update(request_id="simulated-request-99", transaction="simulated-fulfill-tx-99")
        self.assertIn("lost/duplicate/unfulfilled callback accounting", self.validate())

    def test_failed_attempts_retained_and_block_consecutive_window(self):
        self.doc["attempts"][-2].update(status="failed", reason="SIMULATED failure retained")
        self.doc["callbacks"][-1].update(status="failed", output_hex=None, deliveries=0)
        self.doc["attempts"][-1]["status"] = "failed"
        self.doc["requests"][-1]["status"] = "pending"
        self.assertTrue(any("failed/missing attempts retained" in b for b in self.validate()))

    def test_unknown_request_precedes_submission_and_sequence_rejection(self):
        changes = [lambda d: d["callbacks"][0].update(request_id="unknown"),
                   lambda d: d["callbacks"][0].update(observed_ns=0),
                   lambda d: d["requests"][1].update(sequence=3),
                   lambda d: d["attempts"][1].update(sequence=3),
                   lambda d: d["callbacks"][1].update(observed_ns=1),
                   lambda d: d["window"].update(ended_ns=11)]
        for change in changes:
            with self.subTest(change=change):
                saved = copy.deepcopy(self.doc)
                change(self.doc)
                with self.assertRaises(fixtures.Invalid):
                    self.validate()
                self.doc = saved

    def test_callback_attempt_omission_rejected(self):
        self.doc["callbacks"].pop()
        with self.assertRaisesRegex(fixtures.Invalid, "omitted from observations"):
            self.validate()

    def test_fulfill_transaction_binding(self):
        self.doc["attempts"][-2]["transaction"] = "different-simulated-transaction"
        with self.assertRaisesRegex(fixtures.Invalid, "fulfillment transaction"):
            self.validate()

    def test_out_of_order_request_delivery_allowed(self):
        first, second = self.doc["callbacks"][:2]
        first["request_id"], second["request_id"] = second["request_id"], first["request_id"]
        # Both submissions must precede both observations.
        self.doc["requests"][1]["submitted_ns"] = 10
        self.doc["attempts"][3]["observed_ns"] = 10
        self.doc["callbacks"][0]["observed_ns"] = 22
        self.doc["attempts"][1:6] = [
            dict(self.doc["attempts"][3], sequence=2),
            dict(self.doc["attempts"][4], sequence=3),
            dict(self.doc["attempts"][5], sequence=4, observed_ns=22),
            dict(self.doc["attempts"][1], sequence=5, observed_ns=22),
            dict(self.doc["attempts"][2], sequence=6, observed_ns=22),
        ]
        first["transaction"], second["transaction"] = second["transaction"], first["transaction"]
        self.assertEqual(self.validate(), [])

    def test_program_id_and_feature_rejection(self):
        self.doc["environment"]["programs"]["coordinator"]["id"] = "different"
        with self.assertRaisesRegex(fixtures.Invalid, "program ID drift"):
            self.validate()

    def test_mainnet_feature_rejected(self):
        self.doc["environment"]["programs"]["coordinator"]["features"] = ["mainnet"]
        with self.assertRaisesRegex(fixtures.Invalid, "mainnet feature unsupported"):
            self.validate()

    def test_identity_missing_blocks(self):
        self.doc["environment"]["feature_set_sha256"] = None
        self.doc["environment"]["programs"]["coordinator"]["binary_sha256"] = None
        self.assertTrue(any("identity incomplete" in b for b in self.validate()))
        self.assertTrue(any("binary identity missing" in b for b in self.validate()))

    def test_artifact_digest_escape_alias_and_unknown_reject(self):
        row = self.doc["artifacts"][0]
        saved = copy.deepcopy(row)
        for path in ("../outside", "/absolute", "dir\\escape"):
            row["path"] = path
            with self.assertRaisesRegex(fixtures.Invalid, "remain relative"):
                self.validate()
        row.update(saved)
        row["sha256"] = "f" * 64
        with self.assertRaisesRegex(fixtures.Invalid, "SHA-256 mismatch"):
            self.validate()
        row.update(saved)
        self.doc["artifacts"].append(dict(row, id="alias"))
        with self.assertRaisesRegex(fixtures.Invalid, "file alias"):
            self.validate()
        self.doc["artifacts"].pop()
        self.doc["callbacks"][0]["artifact"] = "unknown"
        with self.assertRaisesRegex(fixtures.Invalid, "unknown artifact"):
            self.validate()

    def test_symlink_escape_and_hardlink_alias_reject(self):
        outside = self.root.parent / (self.root.name + "-outside.txt")
        outside.write_bytes(b"outside")
        self.addCleanup(outside.unlink)
        (self.root / "escape").symlink_to(outside)
        self.doc["artifacts"][0]["path"] = "escape"
        with self.assertRaisesRegex(fixtures.Invalid, "within root"):
            self.validate()
        self.doc["artifacts"][0]["path"] = "simulated-log.txt"
        os.link(self.root / "simulated-log.txt", self.root / "hardlink.txt")
        self.doc["artifacts"].append(dict(self.doc["artifacts"][0], id="hardlink", path="hardlink.txt"))
        with self.assertRaisesRegex(fixtures.Invalid, "file alias"):
            self.validate()

    def test_synthetic_and_retained_artifacts_block(self):
        for provenance in ("synthetic", "retained"):
            self.doc["artifacts"][0]["provenance"] = provenance
            self.assertTrue(any("development-only" in b for b in self.validate()))

    def test_cli_never_certifies_even_consistent_fiction(self):
        path = self.root / "simulated.json"
        path.write_bytes(fixtures.encoded(self.doc))
        for flags, code in (([], 1), (["--structure-only"], 0)):
            result = subprocess.run([sys.executable, str(SCRIPT), "--capture", str(path), *flags], capture_output=True, text=True)
            self.assertEqual(result.returncode, code, result.stderr + result.stdout)
            report = json.loads(result.stdout)
            self.assertEqual(report["status"], "ready_for_external_verification")
            self.assertEqual(report["final_acceptance"], "not_established")
        self.doc["compatibility_sha256"] = "f" * 64
        path.write_bytes(fixtures.encoded(self.doc))
        result = subprocess.run([sys.executable, str(SCRIPT), "--capture", str(path), "--structure-only"], capture_output=True)
        self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main()

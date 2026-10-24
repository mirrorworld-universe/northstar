#!/usr/bin/env python3
"""Offline math/consistency tests; synthetic SDK inputs are not setup evidence."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import bounded_vrf_key as tool


class KeyTests(unittest.TestCase):
    def setUp(self):
        self.source = (tool.PORTAL / "src/instructions/verifier/verifier_key.rs").read_bytes()
        self.outer = (tool.PORTAL / "keys/sp1-v6.1.0-groth16-vk.bin").read_bytes()
        self.elf = b"\x7fELFsynthetic-unit-test-only"
        self.sdk_key = b"synthetic-unit-test-only-key-artifact"
        self.key = "0x" + (17).to_bytes(32, "big").hex()
        self.policy = "12" * 32
        self.profile = {"proof_kind": 3, "proof_version": 1, "checkpoint_steps": 1,
                        "public_input_fields": tool.FIELDS, "production_acceptance": False,
                        "schema": "northstar.bounded-vrf-candidate-deployment.v1",
                        "guest_status": "authenticated-envelope", "sp1_version": "6.8.0",
                        "elf_sha256": tool.sha256(self.elf), "policy_profile_sha256": self.policy}
        self.receipt = {
            "schema": "northstar-sp1-setup-v1", "sp1_version": "6.8.0",
            "elf_sha256": tool.sha256(self.elf),
            "program_vkey_hash": self.key,
            "verifying_key_sha256": tool.sha256(self.sdk_key),
            "profile_sha256": tool.sha256(json.dumps(self.profile).encode()),
            "policy_profile_sha256": self.policy, "production_acceptance": False,
        }

    def generate(self, **changes):
        args = dict(elf=self.elf, receipt_bytes=json.dumps(self.receipt).encode(),
                    sdk_key=self.sdk_key, profile_bytes=json.dumps(self.profile).encode(),
                    reviewed_hash=self.key, outer=self.outer, source=self.source)
        args.update(changes)
        return tool.generate(**args)

    def test_legacy_fixed_point_reproduced_without_changing_legacy(self):
        points = tool.source_points(self.source)
        expected = bytes.fromhex(
            "2390e300ae7e4623e609ad8e790ca8ffb2fd25f1fb5abb77948b5f6f18e50a0e"
            "0e056ebd8118f6a72b342bc7cfe7fc327cb2b8f9b7e38f0edb226b5bbde4070e")
        self.assertEqual(tool.fold(points, int(tool.LEGACY_HASH, 16)), expected)

    def test_compressed_outer_ic_provenance(self):
        points = tool.outer_points(self.outer)
        installed = tool.source_points(self.source)
        self.assertEqual(len(points), 6)
        for index in [0, 1, 2, 4, 5]:
            self.assertEqual(points[index], installed[index])
        self.assertEqual(tool.encode_point(points[3]).hex(),
                         "097c875a6ebd0999b06e7267ff3d8a6bf859bb9635abae07cb6b3534ba409a83"
                         "2d84b73fb0f397e356b524f2d0e9ddc9d0f6ac75e566041147ee8554879183ec")
        for point in points:
            self.assertIsNone(tool.multiply(point, tool.FR))
        with self.assertRaises(ValueError):
            tool.outer_points(self.outer + b"x")

    def test_math_group_and_encoding(self):
        point = (1, 2)
        self.assertIsNone(tool.multiply(point, tool.FR))
        self.assertEqual(tool.multiply(point, 2), tool.add(point, point))
        self.assertEqual(tool.decode_point(tool.encode_point(point)), point)
        self.assertIsNone(tool.decode_point(bytes(64)))
        for raw in [b"", bytes(63), bytes(63) + b"\x01", tool.FQ.to_bytes(32, "big") + bytes(32)]:
            with self.assertRaises(ValueError):
                tool.decode_point(raw)

    def test_generated_candidate_is_separate_and_deterministic(self):
        outputs = self.generate()
        self.assertEqual(outputs, self.generate())
        manifest = json.loads(outputs["bounded-vrf-v1.json"])
        self.assertEqual(manifest["program_vkey_hash"], self.key)
        self.assertEqual(manifest["checkpoint_steps"], 1)
        self.assertIn(b"checkpoint_steps: CHECKPOINT_STEPS", outputs["candidate_key.rs"])
        self.assertEqual(manifest["generated_rust_sha256"], tool.sha256(outputs["candidate_key.rs"]))
        self.assertEqual(manifest["domain"], (bytes(8) + b"northstar-er-step-v1" + bytes(2) + b"\x03\x01").hex())
        self.assertEqual(len(bytes.fromhex(manifest["domain"])), 32)
        self.assertNotEqual(tool.fold(tool.source_points(self.source), 17),
                            tool.fold(tool.source_points(self.source), int(tool.LEGACY_HASH, 16)))
        self.assertFalse(manifest["production_acceptance"])
        self.assertIn("not authenticated SDK execution", manifest["provenance_boundary"])
        self.assertNotIn(str(tool.PORTAL), outputs["bounded-vrf-v1.json"].decode())

    def test_full_and_folded_equivalence_for_synthetic_candidate(self):
        points = tool.outer_points(self.outer)
        fixed = tool.decode_point(tool.fold(points, 17))
        for digest, nonce in [(0, 0), (1, 9), (tool.FR - 1, tool.FR - 1)]:
            full = points[0]
            for point, value in zip(points[1:], [17, digest, 0, int(tool.ROOT, 16), nonce]):
                full = tool.add(full, tool.multiply(point, value))
            reduced = tool.add(tool.add(fixed, tool.multiply(points[2], digest)),
                               tool.multiply(points[5], nonce))
            self.assertEqual(full, reduced)

    def test_rejects_unreviewed_or_invalid_hash(self):
        for value in ["0x" + tool.LEGACY_HASH, "0x" + bytes(32).hex(),
                      "0x" + tool.FR.to_bytes(32, "big").hex(), "0x01", self.key.upper(),
                      "0x" + (18).to_bytes(32, "big").hex()]:
            receipt = copy.deepcopy(self.receipt)
            if value.endswith(tool.LEGACY_HASH):
                receipt["program_vkey_hash"] = value
            with self.assertRaises(ValueError):
                self.generate(reviewed_hash=value, receipt_bytes=json.dumps(receipt).encode())

    def test_rejects_artifact_and_receipt_mutations(self):
        for changes in [dict(elf=self.elf + b"x"), dict(sdk_key=self.sdk_key + b"x"),
                        dict(outer=self.outer + b"x"), dict(source=self.source + b"x")]:
            with self.assertRaises(ValueError):
                self.generate(**changes)
        for field, value in [("schema", "unknown"), ("sp1_version", "6.9.0"),
                             ("elf_sha256", "00"), ("verifying_key_sha256", "00"),
                             ("program_vkey_hash", "0x" + tool.LEGACY_HASH)]:
            receipt = dict(self.receipt, **{field: value})
            with self.assertRaises(ValueError):
                self.generate(receipt_bytes=json.dumps(receipt).encode())

    def test_profile_domain_and_order_reject(self):
        for field, value in [("proof_kind", 2), ("proof_kind", 3.0), ("proof_version", 2),
                             ("proof_version", True), ("checkpoint_steps", 0),
                             ("checkpoint_steps", 17), ("checkpoint_steps", True),
                             ("checkpoint_steps", 1.0),
                             ("production_acceptance", True),
                             ("public_input_fields", list(reversed(tool.FIELDS)))]:
            profile = dict(self.profile, **{field: value})
            with self.assertRaises(ValueError):
                self.generate(profile_bytes=json.dumps(profile).encode())

    def test_reviewed_cardinality_is_emitted_with_the_key(self):
        for count in [1, 2, 3, 16]:
            profile = dict(self.profile, checkpoint_steps=count)
            profile_bytes = json.dumps(profile).encode()
            receipt = dict(self.receipt, profile_sha256=tool.sha256(profile_bytes))
            outputs = self.generate(profile_bytes=profile_bytes, receipt_bytes=json.dumps(receipt).encode())
            self.assertEqual(json.loads(outputs["bounded-vrf-v1.json"])["checkpoint_steps"], count)
            self.assertIn(f"const CHECKPOINT_STEPS: u32 = {count};".encode(), outputs["candidate_key.rs"])

    def test_deployment_policy_and_receipt_binding(self):
        for field, value in [("profile_sha256", "00"), ("policy_profile_sha256", "34" * 32),
                             ("production_acceptance", True)]:
            with self.assertRaises(ValueError):
                self.generate(receipt_bytes=json.dumps(dict(self.receipt, **{field: value})).encode())
        for field, value in [("schema", "unknown"), ("guest_status", "qualification-only"),
                             ("sp1_version", "6.9.0"), ("elf_sha256", "00"),
                             ("policy_profile_sha256", "00" * 32),
                             ("policy_profile_sha256", "invalid")]:
            profile_bytes = json.dumps(dict(self.profile, **{field: value})).encode()
            receipt = dict(self.receipt, profile_sha256=tool.sha256(profile_bytes))
            with self.assertRaises(ValueError):
                self.generate(profile_bytes=profile_bytes, receipt_bytes=json.dumps(receipt).encode())

    def test_duplicate_json_fields_reject(self):
        with self.assertRaises(ValueError):
            self.generate(receipt_bytes=b'{"schema":"one", "schema":"two"}')

    def test_cli_check_detects_output_changes_and_never_overwrites(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            inputs = {"elf": self.elf, "setup-receipt": json.dumps(self.receipt).encode(),
                      "sdk-verifying-key": self.sdk_key, "profile": json.dumps(self.profile).encode()}
            args = []
            for name, data in inputs.items():
                path = root / name
                path.write_bytes(data)
                args += ["--" + name, str(path)]
            args += ["--reviewed-program-vkey-hash", self.key, "--output-dir", str(root / "out")]
            script = str(Path(tool.__file__))
            def run(mode):
                return subprocess.run([sys.executable, "-B", script, mode, *args], capture_output=True)
            self.assertEqual(run("generate").returncode, 0)
            self.assertEqual(run("check").returncode, 0)
            self.assertNotEqual(run("generate").returncode, 0)
            generated = root / "out/candidate_key.rs"
            generated.write_bytes(generated.read_bytes() + b"\n")
            self.assertNotEqual(run("check").returncode, 0)


if __name__ == "__main__":
    unittest.main()

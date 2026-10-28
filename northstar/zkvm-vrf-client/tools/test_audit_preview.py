"""Synthetic artifact tests. Dummy proof bytes deliberately are not a valid proof."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

SPEC = importlib.util.spec_from_file_location("preview_audit", Path(__file__).with_name("audit-preview.py"))
AUDIT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(AUDIT)


class PreviewTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.result = self.root / "result"
        self.result.mkdir()
        sha = lambda b: __import__("hashlib").sha256(b).hexdigest()
        def put(name, data):
            path = self.result / name
            path.write_bytes(data)
            return sha(data)
        def encode(value):
            return json.dumps(value).encode()
        self.elf = self.root / "synthetic.elf"
        elf = bytearray(64)
        elf[:7] = bytes([127, 69, 76, 70, 2, 1, 1])
        elf[18:20] = bytes([243, 0])
        self.elf.write_bytes(elf)
        public = bytearray(256)
        label = b"northstar-er-step-v1"
        public[8:8 + len(label)] = label
        public[30:32] = bytes([3, 1])
        for index in range(1, 8):
            public[index * 32 + 31] = index
        self.native_public = self.root / "native-public.bin"
        self.native_public.write_bytes(public)
        self.native_input = self.root / "native-input.bin"
        self.native_input.write_bytes(b"synthetic input")
        input_hash = put("input.bin", self.native_input.read_bytes())
        put("execution-public-values.bin", public)
        public_hash = put("public.bin", public)
        proof_hash = put("proof.bin", bytes.fromhex("4388a21c") + bytes(32) + AUDIT.ROOT + bytes(288))
        sdk_hash = put("sdk.bin", b"synthetic serialized proof; not cryptographic")
        put("sample-0-unverified-sdk-proof.bin", (self.result / "sdk.bin").read_bytes())
        profile = {"proof_kind": 3, "proof_version": 1, "checkpoint_steps": 1,
                   "production_acceptance": False, "guest_status": "authenticated-envelope",
                   "elf_sha256": sha(elf), "policy_profile_sha256": sha(b"synthetic policy")}
        profile_hash = put("profile.json", encode(profile))
        key = "0x" + "0" * 63 + "1"
        vk_hash = put("verifying-key.bin", b"synthetic key artifact")
        receipt = {"schema": "northstar-sp1-setup-v1", "sp1_version": "6.8.0", "elf_sha256": sha(elf),
                   "profile_sha256": profile_hash, "policy_profile_sha256": profile["policy_profile_sha256"],
                   "program_vkey_hash": key, "verifying_key_sha256": vk_hash, "production_acceptance": False}
        receipt_hash = put("setup-receipt.json", encode(receipt))
        put("input-setup-receipt.json", encode(receipt))
        self.source = self.root / "synthetic-source.rs"
        self.source.write_bytes(b"synthetic source")
        self.client_hash = self.root / "client.sha256"
        self.client_hash.write_text("a" * 64 + "  synthetic-client\n")
        self.measurement = {
            "schema": "northstar-vrf-sp1-client-v1", "status": "completed", "exit_code": 0,
            "backend": "cuda", "command": "groth16", "proof_kind": 3, "proof_version": 1,
            "production_acceptance": False, "proof_attempts_started": 1,
            "declared_window_proof_target_met": None, "proof_target_met": False,
            "public_inputs": public.hex(), "expected_public_inputs_sha256": public_hash,
            "input_sha256": input_hash, "elf_sha256": sha(elf), "profile_sha256": profile_hash,
            "policy_profile_sha256": profile["policy_profile_sha256"], "program_vkey_hash": key,
            "verifying_key_sha256": vk_hash, "setup_receipt_sha256": receipt_hash,
            "input_setup_receipt_sha256": receipt_hash, "client_sha256": "a" * 64,
            "source_sha256": sha(self.source.read_bytes()), "cargo_lock_sha256": "b" * 64,
            "sp1_version": "6.8.0", "sp1_circuit_version": "v6.1.0", "client_init_ms": 1,
            "phases": [{"phase": "setup", "wall_ms": 2}, {"phase": "groth16-generated-unverified"},
                       {"phase": "groth16", "sdk_verified": True, "acceptance_window": False,
                        "attempt_sequence": 0, "sample": 0, "process_cold": True,
                        "prove_and_wrap_ms": 309924, "verify_ms": 499,
                        "within_120s_prove_wrap": False, "public_inputs_path": "public.bin",
                        "onchain_proof_path": "proof.bin", "sdk_proof_path": "sdk.bin",
                        "onchain_proof_bytes": 356, "proof_sha256": proof_hash,
                        "sdk_proof_sha256": sdk_hash, "public_inputs_sha256": public_hash}],
        }
        self.setup = self.root / "independent-setup"
        self.execute = self.root / "independent-execute"
        for directory in (self.setup, self.execute):
            directory.mkdir()
            other = copy.deepcopy(self.measurement)
            other["backend"] = "cpu"
            other["phases"] = [{"phase": "execute", "exit_code": 0, "public_output_bytes": 256,
                                "wall_ms": 6154, "cycles": 122290302}]
            (directory / "measurements.json").write_bytes(encode(other))
            (directory / "verifying-key.bin").write_bytes(b"synthetic key artifact")
            (directory / "execution-public-values.bin").write_bytes(public)

    def audit(self):
        (self.result / "measurements.json").write_text(json.dumps(self.measurement))
        return AUDIT.audit(self.result, self.elf, self.native_public, self.native_input,
                           self.setup, self.execute, [self.source], self.client_hash)

    def test_consistent_synthetic_packet_still_never_means_cryptographic_verification(self):
        result = self.audit()
        self.assertFalse(result["cryptographic_verification_performed"])
        self.assertFalse(result["production_acceptance"])
        self.assertFalse(result["final_three_case_window"])
        self.assertFalse(result["proof_target_met"])
        self.assertIsNone(result["complete_resolution_compute_units"])
        self.assertEqual(result["prove_wrap_and_verify_ms"], 310423)

    def test_slow_attempt_or_window_cannot_be_relabelled(self):
        self.measurement["proof_target_met"] = True
        with self.assertRaisesRegex(ValueError, "target misreported"):
            self.audit()
        self.measurement["proof_target_met"] = False
        self.measurement["phases"][-1]["acceptance_window"] = True
        with self.assertRaisesRegex(ValueError, "scope"):
            self.audit()

    def test_public_or_source_or_build_drift_rejects(self):
        self.native_public.write_bytes(bytes(256))
        with self.assertRaisesRegex(ValueError, "native/CPU/public"):
            self.audit()
        self.native_public.write_bytes((self.result / "public.bin").read_bytes())
        self.source.write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "source hash"):
            self.audit()
        self.source.write_bytes(b"synthetic source")
        self.client_hash.write_text("c" * 64)
        with self.assertRaisesRegex(ValueError, "build hash"):
            self.audit()

    def test_absolute_and_escaping_artifacts_reject(self):
        self.measurement["phases"][-1]["onchain_proof_path"] = str(self.elf)
        with self.assertRaisesRegex(ValueError, "relative artifact"):
            self.audit()
        (self.result / "escape").symlink_to(self.elf)
        self.measurement["phases"][-1]["onchain_proof_path"] = "escape"
        with self.assertRaisesRegex(ValueError, "outside result"):
            self.audit()

    def test_duplicate_json_and_noncanonical_public_reject(self):
        duplicate = self.root / "duplicate.json"
        duplicate.write_text('{"a":1,"a":2}')
        with self.assertRaisesRegex(ValueError, "duplicate JSON"):
            AUDIT.load(duplicate)
        with self.assertRaises(ValueError):
            AUDIT.public_values(bytes(256))


if __name__ == "__main__":
    unittest.main()

import importlib.util
from pathlib import Path
import struct
import unittest

spec = importlib.util.spec_from_file_location("network_gateway", Path(__file__).with_name("network-test-gateway.py"))
gateway = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gateway)


class NetworkGatewayTests(unittest.TestCase):
    def test_network_key_is_converted_to_onchain_field_packing(self):
        words = list(range(1, 9))
        value = sum(word << (31 * (7 - index)) for index, word in enumerate(words))
        self.assertEqual(gateway.onchain_key(struct.pack(">8I", *words)), "0x" + value.to_bytes(32, "big").hex())
        for invalid in [bytes(31), struct.pack(">8I", 0x7f000001, *([0] * 7))]:
            with self.assertRaises(ValueError):
                gateway.onchain_key(invalid)

    def test_request_response_hashes_have_the_sdk_required_width(self):
        import tempfile
        import threading
        import time
        from types import SimpleNamespace
        from unittest.mock import Mock
        with tempfile.TemporaryDirectory() as directory:
            instance = gateway.Gateway.__new__(gateway.Gateway)
            instance.root = Path(directory)
            instance.lock = threading.Lock()
            instance.program = SimpleNamespace(vk_hash=bytes(32))
            instance.proto = SimpleNamespace(RequestProofResponse=SimpleNamespace,
                                            RequestProofResponseBody=SimpleNamespace,
                                            GetProofRequestStatusResponse=SimpleNamespace)
            instance.jobs = {}
            instance.count = Mock()
            instance.pool = Mock()
            instance.content = Mock(return_value=struct.pack("<QQ", 1, 1) + b"x" + bytes(16))
            body = SimpleNamespace(vk_hash=bytes(32), mode=4, strategy=2, stdin_private=True,
                                   deadline=int(time.time()) + 110, stdin_uri="stdin", version="sp1-v6",
                                   cycle_limit=100, gas_limit=100)
            response = instance.request_proof(SimpleNamespace(body=body), Mock())
            self.assertEqual(len(response.tx_hash), 32)
            self.assertEqual(len(response.body.request_id), 32)
            status = instance.get_status(SimpleNamespace(request_id=response.body.request_id), Mock())
            self.assertEqual(len(status.request_tx_hash), 32)
            self.assertEqual(status.fulfillment_status, 2)
            instance.pool.submit.assert_called_once()

    def test_stdin_decodes_exactly_one_buffer(self):
        witness = b"witness"
        encoded = struct.pack("<QQ", 1, len(witness)) + witness + struct.pack("<QQ", 0, 0)
        self.assertEqual(gateway.decode_stdin(encoded), witness)
        for changed in [encoded[:-1], encoded + b"extra", struct.pack("<QQ", 2, 0) + bytes(16),
                        struct.pack("<QQ", 1, gateway.MAX_WITNESS + 1) + bytes(16)]:
            with self.assertRaises(ValueError):
                gateway.decode_stdin(changed)
        for cursor, proofs in [(1, 0), (0, 1)]:
            with self.assertRaises(ValueError):
                gateway.decode_stdin(encoded[:-16] + struct.pack("<QQ", cursor, proofs))

    def test_retained_network_outputs_keep_existing_adapter_contract(self):
        import json
        path = Path(__file__).with_name("gpu-prover.py")
        adapter_spec = importlib.util.spec_from_file_location("gpu_adapter", path)
        adapter = importlib.util.module_from_spec(adapter_spec)
        adapter_spec.loader.exec_module(adapter)
        replay = Path(__file__).resolve().parents[1] / "zkvm-replay"
        evidence = replay / "evidence/succinct-network-local-v1"
        measurements = json.loads((evidence / "measurements.json").read_text())
        manifest = json.loads((replay / "partial-candidate-v1.json").read_text())
        adapter.validate(measurements, (evidence / "northstar-sp1-groth16-onchain.bin").read_bytes(),
                         (evidence / "northstar-sp1-public-inputs.bin").read_bytes(), manifest)
        cached = json.loads((evidence / "cache-reuse-measurements.json").read_text())
        self.assertEqual(cached["network_request_id"], measurements["network_request_id"])
        self.assertEqual(cached["phases"][0]["prove_and_wrap_ms"], measurements["phases"][0]["prove_and_wrap_ms"])

    def test_program_artifact_requires_exact_bincode_length(self):
        encoded = struct.pack("<Q", 3) + b"ELF"
        self.assertEqual(gateway.decode_vec(encoded), b"ELF")
        for changed in [b"", encoded[:-1], encoded + b"extra"]:
            with self.assertRaises(ValueError):
                gateway.decode_vec(changed)

    def test_artifact_uris_are_owned_and_identifiers_are_bound(self):
        instance = gateway.Gateway.__new__(gateway.Gateway)
        instance.base = "http://127.0.0.1:50052"
        instance.root = Path("/state")
        instance.lock = __import__("threading").Lock()
        name = "a" * 32
        instance.artifacts = {name: 7}
        self.assertEqual(instance.artifact_path(instance.base + "/artifacts/" + name), Path("/state") / (name + ".bin"))
        for uri in ["http://example.com/artifacts/" + name,
                    instance.base + "/artifacts/../file",
                    instance.base + "/artifacts/" + "b" * 32,
                    instance.base + "/artifacts/" + name + "?query"]:
            with self.assertRaises(ValueError):
                instance.artifact_path(uri)


if __name__ == "__main__":
    unittest.main()

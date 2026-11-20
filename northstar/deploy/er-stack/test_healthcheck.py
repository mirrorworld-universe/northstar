import contextlib
import io
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import healthcheck


class ReadinessTest(unittest.TestCase):
    def setUp(self):
        root = Path(self.enterContext(tempfile.TemporaryDirectory()))
        self.token = "test-credential-free-of-network-effects"
        token_file = root / "bearer"
        token_file.write_bytes(self.token.encode() + b"\n")
        self.info = {
            "version": 1,
            "policy_sha256": "reviewed-policy",
            "public_key": "expected-key",
            "commitment": "finalized_portal_checkpoint_membership",
            "arbitrary_alpha": False,
        }
        configs = {
            "/etc/northstar/relay.json": {
                "signer": {"bearer_token_file": str(token_file), "policy_sha256": "reviewed-policy"},
                "worker": {"vrf_public_key": "expected-key"},
            },
            "/etc/northstar/signer.json": {
                "bearer_token_file": str(token_file),
                "policy": {"vrf_public_key": "expected-key"},
            },
        }
        read_bytes = Path.read_bytes

        def read(path):
            if str(path) in configs:
                return json.dumps(configs[str(path)]).encode()
            return read_bytes(path)

        self.enterContext(patch.object(Path, "read_bytes", read))
        self.request = self.enterContext(patch.object(healthcheck, "request", return_value=self.info))

    def test_signer_uses_authenticated_info(self):
        self.assertEqual(healthcheck.main("signer"), 0)
        self.request.assert_called_once_with(
            9030, "GET", "/v1/info", headers={"Authorization": f"Bearer {self.token}"}
        )

    def test_validator_checks_pinned_signer_and_both_rpc_endpoints(self):
        with patch.object(healthcheck, "rpc", side_effect=["ok", 123]) as rpc:
            self.assertEqual(healthcheck.main("validator"), 0)
        self.assertEqual(
            [call.args for call in rpc.call_args_list],
            [(8899, "getHealth", []), (8910, "getSlot", [{"commitment": "confirmed"}])],
        )

    def test_validator_rejects_different_signer_policy(self):
        self.info["policy_sha256"] = "different-policy"
        with contextlib.redirect_stderr(io.StringIO()), patch.object(healthcheck, "rpc") as rpc:
            self.assertEqual(healthcheck.main("validator"), 1)
        rpc.assert_not_called()

    def test_failed_probe_hides_credential(self):
        self.request.side_effect = OSError(self.token)
        with contextlib.redirect_stderr(io.StringIO()) as log:
            self.assertEqual(healthcheck.main("signer"), 1)
        self.assertNotIn(self.token, log.getvalue())


if __name__ == "__main__":
    unittest.main()

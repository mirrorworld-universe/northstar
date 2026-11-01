#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
import urllib.error

spec = importlib.util.spec_from_file_location("native_recheck", Path(__file__).with_name("recheck-native-qualification.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class RecoveryGuards(unittest.TestCase):
    def test_read_retry_never_submits_transactions(self):
        rpc = module.ReadRetryRpc("https://api.devnet.solana.com")
        with self.assertRaises(ValueError):
            rpc.call("sendTransaction", [])

    def test_read_retry_is_bounded_and_preserves_response(self):
        rpc = module.ReadRetryRpc("https://api.devnet.solana.com")
        limited = urllib.error.HTTPError(rpc.url, 429, "limited", {}, None)
        with patch.object(module.Rpc, "call", side_effect=[limited, {"value": None}]), patch.object(module.time, "sleep"):
            self.assertEqual(rpc.call("getAccountInfo", []), {"value": None})
        with patch.object(module.Rpc, "call", side_effect=limited) as calls, patch.object(module.time, "sleep"):
            with self.assertRaises(urllib.error.HTTPError):
                rpc.call("getSlot", [])
            self.assertEqual(calls.call_count, 9)

    def test_completed_restart_is_not_repeated(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            current = {"pid": 10, "start_time": "a"}
            class Deployment:
                stops = 0
                def execute(self, arguments):
                    return "active"
                def stop(self):
                    self.stops += 1
                def start(self):
                    current.update(pid=11, start_time="b")
            deployment = Deployment()
            with patch.object(module, "check_running", side_effect=lambda _: dict(current)):
                first = module.restart_case(deployment, root, "resume-sol")
                self.assertNotEqual(first["before"], first["after"])
                again = module.restart_case(deployment, root, "resume-sol")
                self.assertEqual(first, again)
                self.assertEqual(deployment.stops, 1)

    def test_restart_cannot_be_claimed_for_unchanged_process(self):
        with tempfile.TemporaryDirectory() as directory:
            class Deployment:
                def execute(self, arguments):
                    return "active"
                def stop(self):
                    pass
                def start(self):
                    pass
            with patch.object(module, "check_running", return_value={"pid": 10, "start_time": "a"}):
                with self.assertRaisesRegex(ValueError, "did not replace"):
                    module.restart_case(Deployment(), Path(directory), "resume-sol")


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
import base64
import copy
import importlib.util
import json
import os
from pathlib import Path
from types import SimpleNamespace
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

spec = importlib.util.spec_from_file_location(
    "handoff", Path(__file__).with_name("vrf-handoff.py")
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
FIXTURE = json.loads(
    Path(__file__).with_name("fixtures").joinpath("vrf-handoff.json").read_text()
)


class HandoffTests(unittest.TestCase):
    def test_writer_matches_shared_contract(self):
        native = FIXTURE["native"]
        checked = module.context(
            native["revision"],
            native["run_id"],
            native["run_attempt"],
            native["runner_name"],
            native["enabled"],
        )
        stop = Mock()
        pending = module.transition(
            {"version": 1, "state": "idle"}, "begin", checked, stop
        )
        stop.assert_called_once_with()
        self.assertEqual(pending["state"], "deploying")
        ready = module.transition(
            pending, "complete", checked, inspect=lambda: FIXTURE["observed"]
        )
        self.assertEqual(ready, FIXTURE)

    def test_failed_stop_preserves_previous_generation(self):
        before = copy.deepcopy(FIXTURE)
        native = before["native"] | {"run_id": "43"}
        with self.assertRaisesRegex(RuntimeError, "unresolved"):
            module.transition(
                before, "begin", native, Mock(side_effect=RuntimeError("unresolved"))
            )
        self.assertEqual(before, FIXTURE)

    def test_incomplete_generation_blocks_another_begin(self):
        pending = FIXTURE | {"state": "deploying"}
        stop = Mock()
        with self.assertRaisesRegex(RuntimeError, "unfinished"):
            module.transition(
                pending, "begin", FIXTURE["native"] | {"run_id": "43"}, stop
            )
        stop.assert_not_called()

    def test_stale_completion_and_failure_do_not_replace_current_generation(self):
        pending = FIXTURE | {"state": "deploying"}
        stale = FIXTURE["native"] | {"run_attempt": "1"}
        inspect = Mock()
        with self.assertRaisesRegex(RuntimeError, "stale"):
            module.transition(pending, "complete", stale, inspect=inspect)
        inspect.assert_not_called()
        self.assertEqual(module.transition(pending, "fail", stale), pending)
        self.assertEqual(
            module.transition(pending, "fail", FIXTURE["native"])["state"], "failed"
        )

    def test_completed_generation_cannot_be_restarted_in_place(self):
        with self.assertRaisesRegex(RuntimeError, "reused"):
            module.transition(FIXTURE, "begin", FIXTURE["native"], Mock())

    def test_host_lock_is_nonblocking_and_missing_state_is_not_reinitialized(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            with patch.object(
                module.os,
                "fstat",
                return_value=SimpleNamespace(st_uid=0, st_mode=0o755),
            ):
                with self.assertRaisesRegex(RuntimeError, "missing"):
                    with module.locked(root):
                        self.fail("missing established state accepted")
                (root / "current.json").write_text(json.dumps(FIXTURE))
                with module.locked(root):
                    with self.assertRaises(BlockingIOError):
                        with module.locked(root):
                            self.fail("concurrent lock accepted")

    def test_observation_binds_incarnation_but_not_settlement_progress(self):
        observed = FIXTURE["observed"]
        raw = bytearray(226)
        raw[:8] = module.SESSION_TAG
        raw[88:120] = module.public_key(observed["validator"])

        def rpc(port, method, _params=None):
            if method == "getGenesisHash":
                return module.DEVNET if port == 8899 else observed["er_genesis"]
            if method == "getIdentity":
                return {"identity": observed["validator"]}
            if method == "getSessionPda":
                return observed["session"]
            if method == "getAccountInfo":
                return {
                    "value": {
                        "owner": observed["portal"],
                        "executable": False,
                        "data": [base64.b64encode(raw).decode(), "base64"],
                    }
                }
            self.fail(method)

        with (
            patch.object(module, "runtime", return_value=observed["process"]),
            patch.object(
                module,
                "command",
                return_value="argv[]=/validator --portal " + observed["portal"] + " ;",
            ),
            patch.object(module, "rpc", side_effect=rpc),
        ):
            initial = module.observe()
            raw[120:225] = bytes([1]) * 105
            self.assertEqual(module.observe(), initial)
            raw[40] = 1
            self.assertNotEqual(module.observe(), initial)
            raw[0] ^= 1
            with self.assertRaisesRegex(RuntimeError, "session identity"):
                module.observe()
            raw[:8] = module.SESSION_TAG
            with patch.object(
                module,
                "runtime",
                side_effect=[observed["process"], observed["process"] | {"pid": 124}],
            ):
                with self.assertRaisesRegex(RuntimeError, "changed during observation"):
                    module.observe()

    def test_guard_import_does_not_create_checkout_bytecode(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            helper = root / "vrf-handoff.py"
            helper.write_bytes(Path(module.__file__).read_bytes())
            (root / "stop-vrf-service.py").write_text("def stop():\n    pass\n")
            environment = os.environ.copy()
            environment.pop("PYTHONDONTWRITEBYTECODE", None)
            subprocess.run(
                [
                    sys.executable,
                    "-c",
                    "import runpy,sys; runpy.run_path(sys.argv[1])['quiesce']()",
                    str(helper),
                ],
                env=environment,
                check=True,
            )
            self.assertFalse((root / "__pycache__").exists())

    def test_missing_portal_is_rejected_before_rpc(self):
        with (
            patch.object(
                module, "runtime", side_effect=[FIXTURE["observed"]["process"]]
            ),
            patch.object(module, "command", return_value="no Portal"),
        ):
            with self.assertRaisesRegex(RuntimeError, "Portal"):
                module.observe()


if __name__ == "__main__":
    unittest.main()

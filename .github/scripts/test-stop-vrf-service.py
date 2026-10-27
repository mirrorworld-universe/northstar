#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "stop_vrf", Path(__file__).with_name("stop-vrf-service.py")
)
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class StopTests(unittest.TestCase):
    def runner(self, loaded="loaded"):
        calls = []

        def run(command, **_):
            calls.append(command)
            return SimpleNamespace(stdout=loaded + "\n", returncode=0)

        return calls, run

    def test_absent_unit_is_noop(self):
        calls, run = self.runner("not-found")
        module.stop(run, lambda: self.fail("no journal exists"))
        self.assertEqual(len(calls), 1)

    def test_clean_stop_disables_boot_start(self):
        calls, run = self.runner()
        module.stop(run, lambda: None)
        self.assertEqual(
            [c[1] for c in calls], ["show", "is-active", "stop", "disable"]
        )

    def test_unknown_outcomes_restore_worker_and_block_cd(self):
        calls, run = self.runner()

        def blocked():
            raise RuntimeError("unresolved")

        with self.assertRaisesRegex(RuntimeError, "unresolved"):
            module.stop(run, blocked)
        self.assertEqual(calls[-1][1], "start")
        self.assertFalse(any(c[1] == "disable" for c in calls))
        self.assertFalse(any("northstar-validator" in " ".join(c) for c in calls))

    def test_snapshot_binding_and_status_checks(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            state = root / "journals"
            binding = "a" * 64
            directory = state / binding
            directory.mkdir(parents=True)
            config = root / "config.json"
            config.write_text(json.dumps({"state_dir": str(directory)}))
            for status in (
                "pending",
                "retryable",
                "unresolved",
                "unknown",
                "confirmed",
            ):
                checked = {"version": 1, "binding": binding, "statuses": [status]}
                with patch.object(
                    module, "validate_state", return_value=checked
                ) as validate:
                    if status == "confirmed":
                        module.inspect_journal(config, state)
                    else:
                        with self.assertRaises(RuntimeError):
                            module.inspect_journal(config, state)
                    validate.assert_called_once_with(config)
            with patch.object(
                module, "validate_state", side_effect=RuntimeError("missing snapshot")
            ):
                with self.assertRaises(RuntimeError):
                    module.inspect_journal(config, state)

    def test_absent_unit_still_guards_established_state(self):
        with tempfile.TemporaryDirectory() as temporary:
            config = Path(temporary) / "config.json"
            config.write_text("{}")
            calls, run = self.runner("not-found")

            def unresolved():
                raise RuntimeError("unresolved established state")

            with (
                patch.object(module, "CONFIG", config),
                self.assertRaisesRegex(RuntimeError, "unresolved"),
            ):
                module.stop(run, unresolved)
            self.assertFalse(any(c[1] == "disable" for c in calls))

    def test_control_only_history_blocks_node_changes_without_worker_records(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "control-state.json").write_text(
                json.dumps({"version": 1, "records": {"deposit": {"receipt": None}}})
            )
            for loaded in ("loaded", "not-found"):
                with (
                    self.subTest(loaded=loaded),
                    patch.object(module, "CONFIG", root / "absent"),
                    patch.object(module, "STATE", root / "absent-state"),
                    patch.object(module, "DEPLOYMENT", root, create=True),
                    patch.object(
                        module,
                        "audit_controls",
                        side_effect=RuntimeError("unresolved control intent"),
                        create=True,
                    ) as audit,
                ):
                    calls, run = self.runner(loaded)
                    with self.assertRaisesRegex(RuntimeError, "control"):
                        module.stop(run, lambda: None)
                    audit.assert_called_once()
                    self.assertFalse(any(c[1] == "disable" for c in calls))

    def test_terminal_status_is_not_snapshot_validation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            state = root / "journals"
            directory = state / ("a" * 64)
            directory.mkdir(parents=True)
            config = root / "config.json"
            config.write_text(json.dumps({"state_dir": str(directory)}))
            for value in [
                {"version": 99, "binding": directory.name, "records": {}},
                {
                    "version": 1,
                    "binding": directory.name,
                    "records": {"one": {"status": "confirmed"}},
                },
            ]:
                with self.subTest(value=value):
                    (directory / "public-state.json").write_text(json.dumps(value))
                    with patch.object(
                        module,
                        "validate_state",
                        side_effect=RuntimeError("invalid snapshot"),
                    ) as validate:
                        with self.assertRaises(RuntimeError):
                            module.inspect_journal(config, state)
                        validate.assert_called_once_with(config)

    def test_receipt_cache_updates_preserve_owner_and_private_mode(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            index = root / "control-state.json"
            index.write_text(
                json.dumps({"version": 1, "pending_programs": [], "records": {}})
            )
            index.chmod(0o600)
            before = index.stat()
            checked = {"version": 1, "pending_programs": [], "records": {}}
            result = SimpleNamespace(stdout=json.dumps(checked), returncode=0)
            with (
                patch.object(module, "DEPLOYMENT", root),
                patch.object(module.subprocess, "run", return_value=result),
            ):
                module.audit_controls()
            after = index.stat()
            self.assertEqual(
                (after.st_uid, after.st_gid), (before.st_uid, before.st_gid)
            )
            self.assertEqual(after.st_mode & 0o777, 0o600)
            self.assertEqual(json.loads(index.read_text()), checked)

    def test_deploy_order_and_trust_boundary(self):
        source = Path(__file__).parents[1].joinpath("workflows/ci.yml").read_text()
        stop = source.index("Stop managed VRF before deployment")
        self.assertLess(stop, source.index("Generate new portal program address"))
        self.assertLess(
            source.index(
                "Run SPL deposit, resume, withdrawal, and settlement smoke test"
            ),
            source.index("Reconcile CI-owned VRF stack and enable service"),
        )
        deploy = source[source.index("  deploy:") :]
        self.assertIn("runs-on: [devnet]", deploy)
        self.assertIn("github.ref == 'refs/heads/release'", deploy)
        self.assertIn("persist-credentials: false", deploy)
        self.assertIn("NORTHSTAR_VRF_ENABLED == 'true'", deploy)


if __name__ == "__main__":
    unittest.main()

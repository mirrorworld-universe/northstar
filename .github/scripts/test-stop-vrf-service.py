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
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        for name, path in (
            ("CONFIG", root / "config.json"),
            ("STATE", root / "service-state" / "journals"),
            ("DEPLOYMENT", root / "deployment"),
        ):
            setting = patch.object(module, name, path)
            setting.start()
            self.addCleanup(setting.stop)

    def runner(self, loaded="loaded", active=None, stops=True):
        calls = []
        running = loaded == "loaded" if active is None else active

        def run(command, **_):
            nonlocal running
            calls.append(command)
            if "--property=LoadState" in command:
                return SimpleNamespace(stdout=loaded + "\n", returncode=0)
            if "--property=ActiveState" in command:
                return SimpleNamespace(
                    stdout="active\n" if running else "inactive\n", returncode=0
                )
            if command[1] == "is-active":
                return SimpleNamespace(stdout="", returncode=0 if running else 3)
            if command[1] == "stop" and stops:
                running = False
            return SimpleNamespace(stdout="", returncode=0)

        return calls, run

    def test_absent_unit_is_noop(self):
        calls, run = self.runner("not-found")
        module.stop(run, lambda: self.fail("no journal exists"))
        self.assertEqual(len(calls), 2)
        self.assertTrue(any("--property=ActiveState" in command for command in calls))

    def test_absent_load_state_does_not_skip_a_running_worker(self):
        calls, run = self.runner("not-found", active=True)
        module.stop(run, lambda: None)
        self.assertTrue(any(command[1] == "stop" for command in calls))
        self.assertTrue(any("--property=ActiveState" in command for command in calls))

    def test_unconfirmed_shutdown_blocks_native_mutation(self):
        calls, run = self.runner("not-found", active=True, stops=False)
        with self.assertRaisesRegex(RuntimeError, "inactive|shutdown"):
            module.stop(run, lambda: self.fail("inspection before shutdown"))

    def test_parent_only_orphan_state_blocks_native_restart(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            parent = root / "service-state"
            parent.mkdir()
            with (
                patch.object(module, "DEPLOYMENT", root / "deployment"),
                patch.object(module, "STATE", parent / "journals"),
                patch.object(module, "CONFIG", root / "missing-config"),
            ):
                calls, run = self.runner("not-found")
                with self.assertRaisesRegex(RuntimeError, "orphan state"):
                    module.stop(
                        run, lambda: (_ for _ in ()).throw(RuntimeError("orphan state"))
                    )
                with self.assertRaisesRegex(RuntimeError, "control index"):
                    module.audit_controls()

    def test_clean_stop_disables_boot_start(self):
        calls, run = self.runner()
        module.stop(run, lambda: None)
        self.assertEqual(
            [c[1] for c in calls], ["show", "show", "stop", "show", "disable"]
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

    def test_absent_unit_cannot_ignore_a_worker_reservation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "worker-state.json").write_text("{}")
            calls, run = self.runner("not-found")
            with (
                patch.object(module, "DEPLOYMENT", root),
                patch.object(module, "CONFIG", root / "missing-config"),
                patch.object(module, "STATE", root / "missing-state"),
                patch.object(module, "audit_controls"),
            ):
                with self.assertRaisesRegex(RuntimeError, "missing worker"):
                    module.stop(
                        run,
                        lambda: (_ for _ in ()).throw(
                            RuntimeError("missing worker configuration")
                        ),
                    )

    def test_worker_reservation_must_match_installed_configuration(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            state = root / "journals"
            config = root / "config.json"
            value = {"state_dir": str(state / ("a" * 64))}
            config.write_text(json.dumps(value))
            (root / "worker-state.json").write_text(
                json.dumps({"version": 1, "binding": "b" * 64, "config": value})
            )
            checked = {"version": 1, "binding": "a" * 64, "statuses": []}
            with (
                patch.object(module, "DEPLOYMENT", root),
                patch.object(module, "validate_state", return_value=checked),
            ):
                with self.assertRaisesRegex(RuntimeError, "reservation|configuration"):
                    module.inspect_journal(config, state)

    def test_deploy_order_and_trust_boundary(self):
        source = Path(__file__).parents[1].joinpath("workflows/ci.yml").read_text()
        stop = source.index("Stop managed VRF before deployment")
        self.assertLess(stop, source.index("Generate new portal program address"))
        self.assertIn("sudo -n python3 -B .github/scripts/stop-vrf-service.py", source)
        deploy = source[source.index("  deploy:") :]
        self.assertIn("runs-on: [self-hosted, linux, devnet]", deploy)
        self.assertIn("github.ref == 'refs/heads/release'", deploy)
        self.assertIn("persist-credentials: false", deploy)
        self.assertNotIn("NORTHSTAR_VRF_", deploy)
        self.assertNotIn("Reconcile CI-owned VRF stack", deploy)
        self.assertNotIn("vrf-coordinator-program-library", deploy)
        self.assertNotIn("NORTHSTAR_VRF_FUNDER_KEYPAIR", deploy)
        self.assertFalse(
            Path(__file__).parents[1].joinpath("workflows/vrf-dispatch.yml").exists()
        )
        self.assertNotIn("vrf-handoff", deploy)


if __name__ == "__main__":
    unittest.main()

#!/usr/bin/env python3
# Sonic: Validate staging without starting services or using deployment credentials.
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / ".github/scripts/build-continuity-v2.sh"
WORKFLOW = ROOT / ".github/workflows/ci.yml"
FAKE_CARGO = r'''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys
args = sys.argv[1:]
with open(os.environ["FAKE_CARGO_CALLS"], "a") as output:
    output.write(json.dumps({"args": args, "auto_install": os.environ.get("RUSTUP_AUTO_INSTALL")}) + "\n")
if args == ["build-sbf", "--version"]:
    if os.environ.get("FAKE_FAIL") == "backend":
        sys.exit(1)
    print("cargo-build-sbf 4.5.0\nplatform-tools v1.57")
elif args[0] == "build-sbf":
    manifest = args[args.index("--manifest-path") + 1]
    name = "northstar_token_bridge.so" if "token-bridge" in manifest else "northstar_portal.so"
    if os.environ.get("FAKE_FAIL") == name:
        sys.exit(1)
    if os.environ.get("FAKE_FAIL") != "missing-elf":
        (Path(args[args.index("--sbf-out-dir") + 1]) / name).write_bytes(b"\x7fELF" + name.encode())
elif args[0] == "build":
    name = args[args.index("--bin") + 1] if "--bin" in args else "libnorthstar_geyser_events.so"
    if os.environ.get("FAKE_FAIL") == name:
        sys.exit(1)
    directory = Path(args[args.index("--target-dir") + 1]) / "release"
    directory.mkdir(parents=True, exist_ok=True)
    (directory / name).write_bytes(b"\x7fELF" + name.encode())
else:
    raise AssertionError(args)
'''


class BuildTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        tools = self.root / "tools"
        tools.mkdir()
        (tools / "cargo").write_text(FAKE_CARGO)
        (tools / "cargo").chmod(0o755)
        self.stage = self.root / "staged"
        self.calls = self.root / "calls.jsonl"
        self.env = {
            **os.environ,
            "PATH": str(tools) + os.pathsep + os.environ["PATH"],
            "NORTHSTAR_CONTINUITY_BUILD_TARGET_DIR": str(self.root / "build-cache"),
            "FAKE_CARGO_CALLS": str(self.calls),
        }

    def run_build(self, path=None, failure=None):
        env = self.env | ({"FAKE_FAIL": failure} if failure else {})
        return subprocess.run(["bash", str(SCRIPT), str(path or self.stage)],
                              env=env, capture_output=True, text=True, timeout=30)

    def test_explicit_feature_builds_are_frozen_with_hashed_completion_manifest(self):
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = json.loads((self.stage / "manifest.json").read_text())
        self.assertEqual((manifest["protocol"], manifest["platform_tools"], manifest["arch"]),
                         ("continuity-v2", "v1.56", "v0"))
        self.assertFalse(manifest["activation_performed"])
        self.assertFalse(manifest["hosted_qualified"])
        self.assertFalse(manifest["proof_acceptance_qualified"])
        for role, filename in {
            "validator": "bin/agave-validator", "cli": "bin/solana", "geyser_events": "lib/libnorthstar_geyser_events.so",
            "portal": "programs/northstar_portal.so", "token_bridge": "programs/northstar_token_bridge.so",
        }.items():
            self.assertEqual(manifest["sha256"][role], hashlib.sha256((self.stage / filename).read_bytes()).hexdigest())
        calls = [json.loads(line) for line in self.calls.read_text().splitlines()]
        self.assertEqual(len(calls), 6)
        for call in calls:
            self.assertEqual(call["auto_install"], "0")
        for call in calls[1:]:
            arguments = call["args"]
            self.assertIn("--locked", arguments)
            if "northstar-geyser-events" in arguments:
                self.assertIn("--lib", arguments)
                self.assertNotIn("--features", arguments)
                continue
            feature = arguments[arguments.index("--features") + 1]
            features = feature.split(",")
            self.assertTrue(any(value.endswith("checkpoint-continuity-v2") for value in features))
            if "agave-validator" in arguments:
                self.assertEqual(features, ["northstar/checkpoint-continuity-v2", "solana-core/vrf-relay"])
            if arguments[0] == "build-sbf":
                self.assertEqual(arguments[arguments.index("--arch") + 1], "v0")
                self.assertEqual(arguments[arguments.index("--tools-version") + 1], "v1.56")
            elif arguments[arguments.index("--bin") + 1] == "solana":
                self.assertIn("--no-default-features", arguments)

    def test_event_plugin_is_built_and_bound_to_the_staged_manifest(self):
        result = self.run_build()
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = json.loads((self.stage / "manifest.json").read_text())
        self.assertIn("geyser_events", manifest["sha256"])
        plugin = self.stage / "lib/libnorthstar_geyser_events.so"
        self.assertEqual(manifest["sha256"]["geyser_events"], hashlib.sha256(plugin.read_bytes()).hexdigest())

    def test_existing_directory_and_artifacts_are_not_replaced(self):
        self.stage.mkdir()
        original = self.stage / "manifest.json"
        original.write_text("retained")
        self.assertNotEqual(self.run_build().returncode, 0)
        self.assertEqual(original.read_text(), "retained")
        self.assertFalse(self.calls.exists())

    def test_existing_symlink_is_not_followed(self):
        target = self.root / "retained"
        target.mkdir()
        self.stage.symlink_to(target, target_is_directory=True)
        self.assertNotEqual(self.run_build().returncode, 0)
        self.assertEqual(list(target.iterdir()), [])
        self.assertFalse(self.calls.exists())

    def test_relative_directory_is_rejected_before_build(self):
        self.assertNotEqual(self.run_build("relative-stage").returncode, 0)
        self.assertFalse(self.calls.exists())

    def test_failed_build_never_publishes_completion(self):
        for failure in ("northstar_portal.so", "northstar_token_bridge.so", "agave-validator", "solana", "libnorthstar_geyser_events.so", "missing-elf"):
            with self.subTest(failure=failure):
                path = self.root / failure
                self.assertNotEqual(self.run_build(path, failure).returncode, 0)
                self.assertFalse((path / "manifest.json").exists())

    def test_unavailable_backend_does_not_install_or_create_stage(self):
        self.assertNotEqual(self.run_build(failure="backend").returncode, 0)
        self.assertFalse(self.stage.exists())
        self.assertEqual(len(self.calls.read_text().splitlines()), 1)

    def test_workflow_uses_only_the_isolated_versioned_activation_controller(self):
        deployment = WORKFLOW.read_text().split("  deploy:\n", 1)[1]
        stage = deployment.index("bash .github/scripts/build-continuity-v2.sh")
        sdk = deployment.index("bash .github/scripts/prepare-continuity-sdk.sh")
        activation = deployment.index("python3 -B .github/scripts/continuity_devnet.py run")
        self.assertLess(stage, activation)
        self.assertLess(sdk, activation)
        self.assertIn('--stage "$NATIVE_STAGE" --revision "$GITHUB_SHA"', deployment)
        self.assertIn("steps.continuity_activate.outputs.report_path", deployment)
        self.assertEqual(deployment.count("continuity_devnet.py run"), 1)
        for legacy in ("Hold before native activation", 'rm -f "$KEYPAIR"', "--force --no-passphrase",
                       "sudo sed -i", "sudo systemctl restart", "portal open-session", "devnet-e2e-smoke.sh", "devnet-spl-e2e-smoke.sh"):
            self.assertNotIn(legacy, deployment)
        for command in ("test-continuity-devnet-model.py", "continuity-control-journal.test.mjs", "devnet-continuity-smoke.test.mjs"):
            self.assertIn(command, WORKFLOW.read_text())


class SbfWrapperTests(unittest.TestCase):
    def run_wrapper(self, backend=True, install_failure=False):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        calls = root / "calls.jsonl"
        cargo = root / "cargo"
        cargo.write_text(f"#!{sys.executable}\n" + r'''
import json
import os
import sys
with open(os.environ["FAKE_CARGO_CALLS"], "a") as output:
    output.write(json.dumps(sys.argv[1:]) + "\n")
if sys.argv[1:] == ["--list"]:
    print("\x1b[32m    build-sbf\x1b[0m")
if sys.argv[1] == "install" and os.environ.get("FAKE_INSTALL_FAILURE") == "1":
    sys.exit(23)
''')
        cargo.chmod(0o755)
        (root / "grep").symlink_to(shutil.which("grep"))
        if backend:
            executable = root / "cargo-build-sbf"
            executable.write_text("#!/bin/sh\nexit 0\n")
            executable.chmod(0o755)
        env = {**os.environ, "PATH": str(root), "FAKE_CARGO_CALLS": str(calls),
               "FAKE_INSTALL_FAILURE": "1" if install_failure else "0"}
        result = subprocess.run([shutil.which("bash"), str(ROOT / "cargo-build-sbf"), "--version"],
                                env=env, capture_output=True, text=True, timeout=10)
        return result, [json.loads(line) for line in calls.read_text().splitlines()]

    def test_existing_backend_does_not_depend_on_colored_cargo_listing(self):
        result, calls = self.run_wrapper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, [["build-sbf", "--arch", "v0", "--tools-version", "v1.56", "--version"]])

    def test_missing_backend_installation_is_version_and_lock_pinned(self):
        result, calls = self.run_wrapper(backend=False)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls[0], ["install", "cargo-build-sbf", "--version", "4.5.0", "--locked"])
        self.assertEqual(calls[1], ["build-sbf", "--arch", "v0", "--tools-version", "v1.56", "--version"])

    def test_failed_backend_installation_never_runs_a_build(self):
        result, calls = self.run_wrapper(backend=False, install_failure=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(any(call[0] == "build-sbf" for call in calls))


if __name__ == "__main__":
    unittest.main()

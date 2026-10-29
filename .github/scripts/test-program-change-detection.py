#!/usr/bin/env python3
import json
import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest

WORKFLOW = Path(__file__).parents[1] / "workflows/ci.yml"


class ProgramChangesTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        self.git("init", "--quiet")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        self.success = self.commit(
            {
                "Cargo.lock": "initial",
                "northstar/programs/portal/src/lib.rs": "initial",
                "northstar/programs/token-bridge/src/lib.rs": "initial",
            }
        )
        self.tools = self.root / "bin"
        self.tools.mkdir()
        curl = self.tools / "curl"
        curl.write_text(
            '#!/bin/sh\nif [ "${API_FAILURE:-0}" = 1 ]; then exit 22; fi\nprintf \'%s\\n\' "$API_RESPONSE"\n'
        )
        curl.chmod(0o755)

    def git(self, *arguments):
        return subprocess.check_output(
            ["git", *arguments], cwd=self.repo, text=True, stderr=subprocess.PIPE
        ).strip()

    def commit(self, files):
        for name, value in files.items():
            path = self.repo / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(value)
        self.git("add", ".")
        self.git("commit", "--quiet", "-m", "fixture")
        return self.git("rev-parse", "HEAD")

    def detect(self, before, baseline=None, failure=False, response=None):
        block = (
            WORKFLOW.read_text()
            .split("      - name: Check for program changes\n", 1)[1]
            .split("\n      - ", 1)[0]
        )
        script = textwrap.dedent(block.split("        run: |\n", 1)[1])
        script = script.replace("${{ github.event.before }}", before)
        output = self.root / "outputs"
        output.unlink(missing_ok=True)
        value = (
            {"workflow_runs": [{"head_sha": baseline or self.success}]}
            if response is None
            else response
        )
        env = os.environ | {
            "PATH": str(self.tools) + os.pathsep + os.environ["PATH"],
            "GITHUB_OUTPUT": str(output),
            "GITHUB_TOKEN": "fixture-token",
            "GITHUB_REPOSITORY": "mirrorworld-universe/northstar",
            "GITHUB_API_URL": "https://github.invalid",
            "API_RESPONSE": json.dumps(value),
            "API_FAILURE": "1" if failure else "0",
        }
        result = subprocess.run(
            ["bash", "-c", script],
            cwd=self.repo,
            env=env,
            capture_output=True,
            text=True,
            timeout=15,
        )
        outputs = (
            dict(line.split("=", 1) for line in output.read_text().splitlines())
            if output.exists()
            else {}
        )
        return result, outputs

    def test_cancelled_release_cannot_hide_portal_changes_from_next_push(self):
        cancelled = self.commit(
            {"northstar/programs/portal/src/lib.rs": "not deployed"}
        )
        self.commit({".github/workflows/ci.yml": "runner label only"})
        result, outputs = self.detect(cancelled)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            outputs, {"portal_changed": "true", "token_bridge_changed": "true"}
        )

    def test_large_change_list_does_not_hide_cargo_changes(self):
        files = {f"docs/{i:04d}-{'x' * 48}.md": "updated" for i in range(2000)}
        files["Cargo.lock"] = "updated"
        self.commit(files)
        result, outputs = self.detect(self.success)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            outputs, {"portal_changed": "true", "token_bridge_changed": "true"}
        )

    def test_successful_baseline_skips_unchanged_programs(self):
        self.commit({".github/workflows/ci.yml": "runner label only"})
        result, outputs = self.detect(self.success)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            outputs, {"portal_changed": "false", "token_bridge_changed": "false"}
        )

    def test_bridge_only_change_does_not_redeploy_portal(self):
        self.commit({"northstar/programs/token-bridge/src/lib.rs": "updated"})
        result, outputs = self.detect(self.success)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            outputs, {"portal_changed": "false", "token_bridge_changed": "true"}
        )

    def test_no_successful_release_requires_both_programs(self):
        self.commit({".github/workflows/ci.yml": "runner label only"})
        result, outputs = self.detect(self.success, response={"workflow_runs": []})
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            outputs, {"portal_changed": "true", "token_bridge_changed": "true"}
        )

    def test_missing_successful_commit_requires_both_programs(self):
        self.commit({".github/workflows/ci.yml": "runner label only"})
        result, outputs = self.detect(self.success, baseline="f" * 40)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            outputs, {"portal_changed": "true", "token_bridge_changed": "true"}
        )

    def test_api_failure_cannot_report_no_program_changes(self):
        self.commit({".github/workflows/ci.yml": "runner label only"})
        result, outputs = self.detect(self.success, failure=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(outputs, {})

    def test_malformed_api_response_cannot_report_no_program_changes(self):
        self.commit({".github/workflows/ci.yml": "runner label only"})
        result, outputs = self.detect(self.success, response={})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(outputs, {})


if __name__ == "__main__":
    unittest.main()

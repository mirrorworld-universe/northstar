#!/usr/bin/env python3
"""Sonic: Reject incompatible npm before checkout; preserve the frozen install contract."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("prepare-continuity-sdk.sh")
FAKE_TOOL = r'''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
tool = Path(sys.argv[0]).name
args = sys.argv[1:]
with open(os.environ['CALLS'], 'a') as output:
    output.write(json.dumps({'tool': tool, 'args': args}) + '\n')
if tool == 'npm':
    if args == ['--version']:
        print(os.environ['NPM_VERSION'])
    elif args == ['run', 'build']:
        Path('dist').mkdir()
        Path('dist/index.js').write_text('module.exports = {}')
elif tool == 'git':
    if args[:2] == ['init', '--quiet']:
        directory = Path(args[-1])
        directory.mkdir()
        (directory / 'package-lock.json').write_text('{}')
    elif 'rev-parse' in args:
        print('ea732d51f71f62e6fe46c77eaf781b9474b00508')
elif tool == 'sha256sum':
    assert args == ['--check', '--status']
    assert sys.stdin.read().startswith('78697b65487a7c5932151165ec887b37246e25a3571af752b8c151cc636cd00c  ')
elif tool == 'node':
    print('v24.21.0')
else:
    raise AssertionError(tool)
'''


class BootstrapTests(unittest.TestCase):
    def run_bootstrap(self, version):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        tools = root / "bin"
        tools.mkdir()
        calls = root / "calls.jsonl"
        for name in ("git", "node", "npm", "sha256sum"):
            tool = tools / name
            tool.write_text(FAKE_TOOL)
            tool.chmod(0o755)
        checkout = root / "sdk"
        environment = os.environ | {"PATH": str(tools) + os.pathsep + os.environ["PATH"],
                                    "CALLS": str(calls), "NPM_VERSION": version}
        result = subprocess.run(["bash", str(SCRIPT), str(checkout)], env=environment,
                                capture_output=True, text=True, timeout=10)
        invoked = [json.loads(line) for line in calls.read_text().splitlines()]
        return result, checkout, invoked

    def test_npm10_is_rejected_before_checkout_or_install(self):
        result, checkout, calls = self.run_bootstrap("10.9.9")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("requires npm 11", result.stderr)
        self.assertFalse(checkout.exists())
        self.assertTrue(all(call["tool"] == "npm" and call["args"] == ["--version"] for call in calls))

    def test_npm11_keeps_frozen_dependencies_and_disabled_lifecycle_scripts(self):
        result, checkout, calls = self.run_bootstrap("11.19.0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue((checkout / "dist/index.js").is_file())
        installs = [call["args"] for call in calls if call["tool"] == "npm" and call["args"][0] == "ci"]
        self.assertEqual(installs, [["ci", "--include=dev", "--ignore-scripts", "--no-audit", "--no-fund"]])


if __name__ == "__main__":
    unittest.main()

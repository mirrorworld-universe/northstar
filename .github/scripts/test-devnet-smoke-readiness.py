#!/usr/bin/env python3
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(
    os.environ.get("SMOKE_SCRIPT", Path(__file__).with_name("devnet-e2e-smoke.sh"))
)


class SmokeReadinessTests(unittest.TestCase):
    def run_smoke(self, account, er_session="session"):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            tools = root / "bin"
            tools.mkdir()
            calls = root / "calls"
            calls.touch()
            curl = tools / "curl"
            curl.write_text(
                f"#!{sys.executable}\n"
                + """import json,os,sys
from pathlib import Path
arguments=sys.argv
payload=json.loads(arguments[arguments.index('--data')+1])
method=payload['method']
with open(os.environ['CALLS'],'a') as log: print(method,file=log)
values={'getIdentity':{'identity':'validator'},'getSessionPda':os.environ['ER_SESSION'] or None,'getAccountInfo':json.loads(os.environ['ACCOUNT']), 'getSlot':100,'getBalance':{'value':1000000000},'getSignaturesForAddress':[]}
print(json.dumps({'jsonrpc':'2.0','id':1,'result':values[method]}))
"""
            )
            cli = tools / "solana"
            cli.write_text("""#!/usr/bin/env bash
if [[ " $* " == *" find-program-derived-address "* ]]; then
  printf '%s\\n' '{"address":"session"}'
else
  printf '%s\\n' CLI_TRANSACTION >> "$CALLS"
  exit 37
fi
""")
            (tools / "solana-keygen").write_text("#!/bin/sh\nprintf '%s\\n' payer\n")
            (tools / "sleep").write_text("#!/bin/sh\nexit 77\n")
            (tools / "seq").write_text("#!/bin/sh\nprintf '%s\\n' 1\n")
            for tool in tools.iterdir():
                tool.chmod(0o755)
            key = root / "key.json"
            key.write_text("[]")
            log = root / "validator.log"
            log.touch()
            env = os.environ | {
                "HOME": str(root),
                "PATH": str(tools) + os.pathsep + os.environ["PATH"],
                "SOLANA_CLI": str(cli),
                "DEPLOYER_KEYPAIR": str(key),
                "REPORT_PATH": str(root / "report"),
                "VALIDATOR_LOG": str(log),
                "PORTAL_ADDRESS": "portal",
                "RESTART_COMMAND": "true",
                "DEVNET_RPC": "http://devnet.invalid",
                "L1_LOCAL_RPC": "http://local.invalid",
                "ER_RPC": "http://er.invalid",
                "ACCOUNT": json.dumps(account),
                "ER_SESSION": er_session,
                "CALLS": str(calls),
            }
            result = subprocess.run(
                ["bash", str(SCRIPT)],
                env=env,
                capture_output=True,
                text=True,
                timeout=10,
            )
            return result, calls.read_text().splitlines()

    def test_spl_default_er_rpc_matches_the_colocated_sol_smoke(self):
        spl = SCRIPT.with_name("devnet-spl-e2e-smoke.sh").read_text()
        default = next(line for line in spl.splitlines() if line.startswith("ER_RPC="))
        self.assertEqual(default, "ER_RPC=${ER_RPC:-http://localhost:8910}")

    def test_missing_l1_session_fails_before_waiting_or_depositing(self):
        result, calls = self.run_smoke({"value": None}, er_session="")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Missing L1 Portal session", result.stderr)
        self.assertIn("getAccountInfo", calls)
        self.assertNotIn("getSessionPda", calls)
        self.assertNotIn("CLI_TRANSACTION", calls)

    def test_wrong_session_owner_is_not_accepted(self):
        result, calls = self.run_smoke({"value": {"owner": "other-program"}})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("L1 session owner mismatch", result.stderr)
        self.assertNotIn("CLI_TRANSACTION", calls)

    def test_wrong_er_session_is_not_used_for_deposit(self):
        result, calls = self.run_smoke(
            {"value": {"owner": "portal"}}, er_session="other-session"
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("CLI_TRANSACTION", calls)
        self.assertIn("Waiting for ER session session", result.stderr)

    def test_valid_session_proceeds_to_the_stubbed_deposit(self):
        result, calls = self.run_smoke({"value": {"owner": "portal"}})
        self.assertEqual(result.returncode, 37, result.stderr)
        self.assertLess(calls.index("getAccountInfo"), calls.index("getSessionPda"))
        self.assertEqual(calls.count("CLI_TRANSACTION"), 1)
        self.assertIn("phase=deposit_l1", result.stderr)

    def test_malformed_account_response_is_not_session_absence(self):
        result, calls = self.run_smoke({})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Malformed L1 session response", result.stderr)
        self.assertNotIn("CLI_TRANSACTION", calls)


if __name__ == "__main__":
    unittest.main()

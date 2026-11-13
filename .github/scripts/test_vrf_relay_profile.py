import tempfile
import unittest
from pathlib import Path
import hashlib
import json
import vrf_relay_profile as relay
import copy
from continuity_devnet import public_text
from continuity_rollout_state import service_dropin


class RelayProfileTests(unittest.TestCase):
    def fixture(self):
        keys=[public_text(bytes([value])*32) for value in range(1,7)]
        identity={"session":keys[0],"context":"11"*32}
        profile={"version":1,"context":identity["context"],"configuration":{"version":1,"worker":{
            "network":"devnet_er","rpc_url":"http://127.0.0.1:8910","l1_rpc_url":"http://127.0.0.1:8899",
            "expected_genesis":"11111111111111111111111111111111","er_session":identity["session"],"er_session_context":identity["context"],
            "coordinator":keys[1],"verifier":keys[2],"vrf_public_key":keys[3],"fulfiller":keys[4],
            "coordinator_programdata_sha256":"22"*32,"verifier_programdata_sha256":"33"*32,
            "signer_file":relay.CREDENTIALS+"vrf-relay-fulfiller","state_dir":str(relay.JOURNAL),
            "commitment":"confirmed","allowed_consumers":[keys[5]],"allow_store":False,"poll_ms":2000,"max_attempts":2,"max_records":128},
            "signer":{"url":"http://127.0.0.1:9030","bearer_token_file":relay.CREDENTIALS+"vrf-relay-bearer","policy_sha256":"44"*32}}}
        return profile,identity

    def validate(self,profile,identity):
        return relay.validate(profile,identity,"11111111111111111111111111111111","http://127.0.0.1:8899","http://127.0.0.1:8910")

    def test_complete_remote_only_profile_is_accepted(self):
        profile,identity=self.fixture()
        self.assertEqual(self.validate(profile,identity),profile["configuration"])

    def test_incomplete_local_key_standalone_and_shared_l1_profiles_are_rejected(self):
        profile,identity=self.fixture()
        for changes in ({"vrf_key_file":"/secret"},{"network":"devnet_l1"},{"coordinator":"9XHn4tU6H6gibio4omC9Xs2ZCXyqrhTzY3FRidsHuGgm"},
                        {"er_session_context":"55"*32},{"allow_store":True},{"allowed_consumers":[]},{"rpc_url":"https://leader.example"},
                        {"signer_file":"/etc/northstar-vrf-request-signer/vrf.json"},
                        {"signer_file":"/run/credentials/northstar-validator.service/vrf-relay-fulfiller"}):
            altered=copy.deepcopy(profile); altered["configuration"]["worker"].update(changes)
            with self.subTest(changes=changes), self.assertRaises(ValueError):self.validate(altered,identity)
        del profile["configuration"]["worker"]["coordinator"]
        with self.assertRaises(ValueError):self.validate(profile,identity)

    def test_callback_or_fulfiller_alias_is_rejected(self):
        profile,identity=self.fixture()
        worker=profile["configuration"]["worker"]
        worker["fulfiller"]=worker["vrf_public_key"]
        with self.assertRaises(ValueError):self.validate(profile,identity)

    def test_managed_argument_replacement_is_exact_and_retains_unrelated_flags(self):
        old=["/old", "--portal", "portal", "--account-index", "program-id"]
        self.assertEqual(relay.arguments(old,"/new.json"),old+["--vrf-relay-config","/new.json"])
        configured=relay.arguments(old,"/old.json")
        self.assertEqual(relay.arguments(configured,"/new.json","/old.json"),old+["--vrf-relay-config","/new.json"])
        with self.assertRaises(ValueError): relay.arguments(configured,"/new.json")
        with self.assertRaises(ValueError): relay.arguments(configured+["--vrf-relay-config=/old.json"],"/new.json","/old.json")

    def test_remote_module_dropin_binds_only_delivery_credentials_and_hardening(self):
        text=service_dropin(["/validator","--portal","portal","--vrf-relay-config","/runtime/vrf.json"],"/state/checkpoints","/state/journal")
        self.assertIn("NoNewPrivileges=yes",text)
        self.assertIn("LoadCredential=vrf-relay-fulfiller:",text)
        self.assertIn("LoadCredential=vrf-relay-bearer:",text)
        self.assertIn("RuntimeDirectory=northstar-vrf-relay",text)
        self.assertIn("RuntimeDirectoryMode=0700",text)
        self.assertIn("ExecStartPre=/usr/bin/install -m 0400 %d/vrf-relay-fulfiller %t/northstar-vrf-relay/vrf-relay-fulfiller",text)
        self.assertIn("ExecStartPre=/usr/bin/install -m 0400 %d/vrf-relay-bearer %t/northstar-vrf-relay/vrf-relay-bearer",text)
        self.assertIn("-/run/northstar-vrf-request-signer",text)
        self.assertNotIn("LoadCredential=vrf.json",text)
        self.assertNotIn("NoNewPrivileges",service_dropin(["/validator","--portal","portal"],"/state/checkpoints","/state/journal"))

    def test_profile_reads_require_exact_digest_and_private_regular_file(self):
        with tempfile.TemporaryDirectory() as root:
            path=Path(root)/"profile.json"
            path.write_text(json.dumps({"version":1,"context":"11"*32,"configuration":{}}))
            path.chmod(0o600)
            sha=hashlib.sha256(path.read_bytes()).hexdigest()
            self.assertEqual(relay.read_profile(path,sha)["context"],"11"*32)
            with self.assertRaises(ValueError):relay.read_profile(path,"00"*32)
            path.chmod(0o644)
            with self.assertRaises(ValueError):relay.read_profile(path,sha)


if __name__=="__main__": unittest.main()

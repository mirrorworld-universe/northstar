#!/usr/bin/env python3
import copy
import hashlib
import importlib.util
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

spec = importlib.util.spec_from_file_location("resume_upload", Path(__file__).with_name("resume-native-upload.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
FUNDER = "8FsGRi7LHwnemwvwEjeSFe2tShVqqfPkGPatJ3rgYmSF"


def buffer_account(data=b"retained fixture", authority=FUNDER):
    return {"owner": module.LOADER, "executable": False,
            "raw": (1).to_bytes(4, "little") + b"\1" + module.public_bytes(authority) + data}


class RpcFixture:
    def __init__(self, accounts, slots=100, genesis=module.DEVNET):
        self.accounts, self.slots, self.genesis = accounts, slots, genesis

    def call(self, method, params=None):
        return {"getGenesisHash": self.genesis, "getHealth": "ok", "getSlot": self.slots,
                "getIdentity": {"identity": "validator"}, "getMinimumBalanceForRentExemption": 1000,
                "getBalance": {"value": 2_000_000_000}}[method]

    def account(self, address):
        return self.accounts.get(address)


class Guards(unittest.TestCase):
    def test_buffer_binding_and_payload_guards(self):
        data = b"retained fixture"
        account = buffer_account(data)
        digest = hashlib.sha256(data).hexdigest()
        module.validate_buffer(account, len(data), FUNDER, digest)
        for changed in (None, account | {"owner": "other"}, account | {"executable": True},
                        account | {"raw": account["raw"] + b"\0"}, buffer_account(data, "11111111111111111111111111111111"),
                        buffer_account(b"changed payload")):
            with self.subTest(changed=changed is None):
                with self.assertRaises(ValueError):
                    module.validate_buffer(changed, len(data), FUNDER, digest)

    def test_l1_network_and_freshness_guards(self):
        module.validate_local(RpcFixture({}, slots=100), RpcFixture({}, slots=132))
        for local, upstream in ((RpcFixture({}, slots=100), RpcFixture({}, slots=133)),
                                (RpcFixture({}, genesis="other"), RpcFixture({})),
                                (RpcFixture({}, slots=True), RpcFixture({}))):
            with self.assertRaises(ValueError):
                module.validate_local(local, upstream)

    def exercise_recovery(self, changed_buffer=False):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        state_root = root / "state"
        control = state_root / module.REVISION / "control"
        control.mkdir(parents=True)
        stage = root / "stage"
        for role in ("portal", "token_bridge"):
            image = stage / module.ARTIFACTS[role]
            image.parent.mkdir(parents=True, exist_ok=True)
            image.write_bytes(b"verified fixture")
        plan = {"source_revision": module.REVISION, "programs": module.PROGRAMS, "validator": "validator",
                "funder": FUNDER, "max_estimated_cost_lamports": 10_000_000_000,
                "artifacts": {"portal": "portal-hash", "token_bridge": "bridge-hash"}}
        reservation = {"plan_sha256": hashlib.sha256(module.canonical(plan)).hexdigest(), "stage": str(stage),
                       "devnet_rpc": "https://api.devnet.solana.com", "l1_rpc": "http://127.0.0.1:8899", "er_rpc": "http://127.0.0.1:8910"}
        records = {"reservation.json": reservation, "plan.json": plan,
                   "programs.json": {"version": 2, "roles": {"portal": {"elf_sha256": "portal-hash", "complete": None}}}}
        retained = b"retained fixture" if not changed_buffer else b"modified fixture"
        accounts = {module.PROGRAMS["portal-buffer"]: buffer_account(retained)}
        sends = []
        class FakeDeployment:
            def __init__(self, args):
                self.args, self.control, self.stage, self.plan = args, control, stage, plan
                self.original = {"service": "unchanged"}
                self.l1 = self.upstream = RpcFixture(accounts)
                self.manifest = {"sha256": plan["artifacts"]}
            def prepare(self):
                return None
            def inventory(self):
                return self.original
            def execute(self, arguments, timeout=120):
                if arguments[:4] == ["sudo", "-n", "python3", "-B"]:
                    assert str(arguments[-1]).endswith(".github/scripts/stop-vrf-service.py")
                    self.quiesced = True
                    return
                assert self.quiesced, "worker must be quiesced before native uploads"
                self.assertions(arguments)
                sends.append(arguments)
                role = next(role for role in ("portal", "token_bridge") if str(arguments[7]) == str(stage / module.ARTIFACTS[role]))
                accounts[module.PROGRAMS[role]] = {"deployed": True}
                accounts.pop(module.PROGRAMS[role + "-buffer"], None)
            def assertions(self, arguments):
                assert arguments[2] == "http://127.0.0.1:8899"
                assert arguments[5:7] == ["program", "deploy"]
                assert arguments[-2:] == ["--max-sign-attempts", "10"]
                assert "--program-id" in arguments and "--buffer" in arguments
            def program_view(self, role):
                return {"slot": 100, "hash": plan["artifacts"][role]} if module.PROGRAMS[role] in accounts else None
        original_path = Path
        def path(value):
            return state_root if str(value) == "/solana/northstar/continuity-v2" else original_path(value)
        def save(location, value):
            records[location.name] = copy.deepcopy(value)
        with patch.object(module, "Path", side_effect=path), patch.object(module, "DROPIN", root / "absent"), \
             patch.object(module, "Deployment", FakeDeployment), patch.object(module, "checked_directory"), \
             patch.object(module, "BUFFER_HASH", hashlib.sha256(b"retained fixture").hexdigest()), \
             patch.object(module, "private_json", side_effect=lambda location: copy.deepcopy(records[location.name])), \
             patch.object(module, "atomic_json", side_effect=save), patch.object(module, "immutable_json", side_effect=save):
            if changed_buffer:
                with self.assertRaises(ValueError):
                    module.recover(root / "sdk", 123)
                self.assertEqual(sends, [])
                self.assertNotIn("upload-recovery.json", records)
            else:
                module.recover(root / "sdk", 123)
                self.assertEqual(len(sends), 2)
                self.assertEqual(records["upload-recovery-complete.json"]["plan_sha256"], reservation["plan_sha256"])
                for role in ("portal", "token_bridge"):
                    self.assertIsNotNone(records["programs.json"]["roles"][role]["complete"])

    def test_same_ids_and_buffers_complete_without_native_service_mutation(self):
        self.exercise_recovery()

    def test_changed_buffer_stops_before_any_send(self):
        self.exercise_recovery(changed_buffer=True)


if __name__ == "__main__":
    unittest.main()

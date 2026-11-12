#!/usr/bin/env python3
"""Sonic: Exercise irreversible-action guards with fake RPC and service operations."""
import hashlib
import json
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

import continuity_devnet as rollout
from continuity_rollout_state import ARTIFACTS, DEVNET, atomic_json, private_json


class MutationBoundary(Exception):
    pass


class DeploymentTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        args = SimpleNamespace(stage=self.root / "stage", revision="a" * 40, state_root=self.root / "state",
                               devnet_rpc="https://api.devnet.solana.com", l1_rpc="http://localhost:8899",
                               er_rpc="http://localhost:8910", max_estimated_cost_lamports=10_000_000_000,
                               funder_keypair=self.root / "funder.json")
        value = self.deployment = rollout.Deployment(args)
        value.control.mkdir(mode=0o700, parents=True)
        value.stage = args.stage
        checksums = {}
        for role, relative in ARTIFACTS.items():
            filename = value.stage / relative
            filename.parent.mkdir(parents=True, exist_ok=True)
            filename.write_bytes(b"\x7fELF" + role.encode())
            checksums[role] = hashlib.sha256(filename.read_bytes()).hexdigest()
        value.manifest = {"sha256": checksums}
        value.plan = {"funder": rollout.public_text(bytes([1]) * 32), "validator": rollout.public_text(bytes([2]) * 32),
                      "programs": {name: rollout.public_text(bytes([index]) * 32) for index, name in enumerate(
                          ("portal", "token_bridge", "portal-buffer", "token_bridge-buffer"), 3)}, "artifacts": checksums}
        value.original = {"arguments": ["/retained/agave-validator", "--portal", "old", "--ledger", "/retained/ledger"],
                          "binary_sha256": "original", "uid": os.getuid(), "gid": os.getgid(), "retained_paths": []}
        value.upstream = SimpleNamespace(
            call=lambda method, *_: 1000 if method == "getMinimumBalanceForRentExemption" else {"value": 20_000_000_000},
            account=Mock(return_value=None),
        )

    def test_reconciled_smoke_cannot_skip_restart_without_a_ready_barrier(self):
        value = self.deployment
        value.fresh = SimpleNamespace(phase=Mock(return_value="running"), verify_desired=Mock())
        value.execute = Mock()
        with self.assertRaises((ValueError, OSError)):
            value.smoke(resumed_phase="resume-sol")
        value.execute.assert_not_called()

    def test_reconciled_smoke_consumes_only_the_remaining_spl_restart(self):
        value = self.deployment
        value.args.sdk_checkout = self.root / "sdk"
        value.plan.update(source_revision=value.args.revision, profile="e" * 64)
        value.er = SimpleNamespace(call=Mock(return_value=rollout.public_text(bytes([9]) * 32)))
        value.retention = Mock(return_value={})
        atomic_json(value.control / "retained.json", {})
        atomic_json(value.control / "activation.json", {"event_source": "l1"})
        record = {"version": 1, "kind": "fresh-poststart-reconciliation", "source_revision": value.args.revision,
            "controller_revision": "d" * 40, "plan_sha256": hashlib.sha256(rollout.canonical(value.plan)).hexdigest(),
            "review_sha256": "f" * 64, "context": "e" * 64, "extra_native_starts": 1, "program_submissions": 0, "phase": "running"}
        atomic_json(value.control / "fresh-poststart-reconciliation.json", record)
        directory = value.control / "fresh-reconciliation"
        directory.mkdir(mode=0o700)
        ready = {"version": 1, "plan_sha256": record["plan_sha256"], "review_sha256": record["review_sha256"],
            "context": record["context"], "l1_floor": 100, "phase": "resume-sol"}
        atomic_json(directory / "start-attempted.json", ready)
        atomic_json(directory / "ready.json", ready)
        value.fresh = SimpleNamespace(phase=Mock(return_value="running"), verify_desired=Mock(), report=lambda report: report)
        value.stop, value.start = Mock(), Mock()
        phases = []
        def execute(command, **_):
            phases.append(command[-1])
            return json.dumps({"phase": command[-1], "context": "e" * 64})
        value.execute = Mock(side_effect=execute)
        value.smoke(resumed_phase="resume-sol")
        self.assertEqual(phases, ["bootstrap", "prepare-sol", "resume-sol", "prepare-spl", "resume-spl"])
        value.stop.assert_called_once()
        value.start.assert_called_once()
        value.fresh.verify_desired.assert_called_with(running=True)

    def test_unresolved_program_upload_is_not_replanned_or_resubmitted(self):
        value = self.deployment
        atomic_json(value.control / "programs.json", {"version": 2, "roles": {"portal": {
            "complete": None, "elf_sha256": value.manifest["sha256"]["portal"]}}})
        value.program_view = Mock(return_value=None)
        value.execute = Mock(return_value="")
        with self.assertRaisesRegex(ValueError, "unresolved"):
            value.deploy_programs()
        self.assertFalse(any("deploy" in call.args[0] for call in value.execute.call_args_list))
        self.assertIsNone(private_json(value.control / "programs.json")["roles"]["portal"]["complete"])

    def test_partial_upload_scope_cannot_use_an_unrecorded_rpc_or_worker_stop(self):
        value = self.deployment
        value.execute = Mock()
        for rpc, stop in (("https://unrecorded.invalid", False), (value.args.l1_rpc, True)):
            with self.assertRaisesRegex(ValueError, "submission scope"):
                value.deploy_programs(resume_roles={"portal"}, submission_rpc=rpc, stop_worker=stop)
        value.execute.assert_not_called()

    def test_completed_programs_do_not_require_rent_or_worker_or_submission_actions(self):
        from urllib.error import HTTPError
        value = self.deployment
        views = {role: {"programdata": role, "deployment_slot": 50, "sha256": value.manifest["sha256"][role]}
                 for role in ("portal", "token_bridge")}
        atomic_json(value.control / "programs.json", {"version": 2, "roles": {
            role: {"elf_sha256": value.manifest["sha256"][role], "complete": view} for role, view in views.items()}})
        value.program_view = Mock(side_effect=lambda role: views[role])
        value.upstream.call = Mock(side_effect=HTTPError("https://api.devnet.solana.com", 429, "rate limited", {}, None))
        value.execute = Mock(side_effect=MutationBoundary)
        value.deploy_programs()
        value.upstream.call.assert_not_called()
        value.execute.assert_not_called()
        self.assertEqual(private_json(value.control / "programs.json")["roles"], {
            role: {"elf_sha256": value.manifest["sha256"][role], "complete": view} for role, view in views.items()})

    def test_insufficient_balance_stops_before_service_or_chain_mutation(self):
        value = self.deployment
        value.upstream.call = lambda method, *_: 1000 if method == "getMinimumBalanceForRentExemption" else {"value": 1}
        value.execute = Mock(side_effect=MutationBoundary)
        with self.assertRaisesRegex(ValueError, "funding"):
            value.deploy_programs()
        value.execute.assert_not_called()
        self.assertFalse((value.control / "programs.json").exists())

    def test_malformed_rent_response_is_not_a_funding_estimate(self):
        value = self.deployment
        value.upstream.call = lambda method, *_: True if method == "getMinimumBalanceForRentExemption" else {"value": 20_000_000_000}
        value.execute = Mock(side_effect=MutationBoundary)
        with self.assertRaisesRegex(ValueError, "rent"):
            value.deploy_programs()
        value.execute.assert_not_called()

    def test_unknown_upload_schema_is_not_reinitialized(self):
        value = self.deployment
        atomic_json(value.control / "programs.json", {"version": 1, "roles": {}})
        value.execute = Mock(side_effect=MutationBoundary)
        with self.assertRaisesRegex(ValueError, "upload.*schema"):
            value.deploy_programs()
        value.execute.assert_not_called()

    def test_completed_upload_observation_is_immutable(self):
        value = self.deployment
        original = {"programdata": "original", "sha256": "a" * 64}
        atomic_json(value.control / "programs.json", {"version": 2, "roles": {"portal": {
            "elf_sha256": value.manifest["sha256"]["portal"], "complete": original}}})
        value.program_view = Mock(return_value={"programdata": "original", "sha256": "b" * 64})
        value.execute = Mock(side_effect=MutationBoundary)
        with self.assertRaisesRegex(ValueError, "completed.*changed"):
            value.deploy_programs()
        self.assertEqual(private_json(value.control / "programs.json")["roles"]["portal"]["complete"], original)

    def test_failed_service_with_live_main_pid_is_not_inactive(self):
        value = self.deployment
        value.execute = Mock(side_effect=["", "failed", "77"])
        with self.assertRaisesRegex(ValueError, "inactivity"):
            value.stop()

    def test_child_operations_keep_the_shared_deployment_lock(self):
        value = self.deployment
        value.args.lock_descriptor = 42
        with patch.object(rollout, "command", return_value=SimpleNamespace(returncode=0, stdout=b"", stderr=b"")) as run:
            value.execute(["unprivileged-operation"])
        self.assertEqual(run.call_args.args[2], (42,))

    def test_event_listener_probe_requires_the_health_response(self):
        value = self.deployment
        response = Mock(status=200)
        response.read.return_value = b"ok"
        opener = Mock()
        opener.open.return_value.__enter__ = Mock(return_value=response)
        opener.open.return_value.__exit__ = Mock(return_value=False)
        with patch.object(rollout.urllib.request, "build_opener", return_value=opener):
            self.assertTrue(value.event_listener_ready())
            opener.open.assert_called_with("http://127.0.0.1:9101/health", timeout=5)
            response.read.assert_called_with(16)
            response.read.return_value = b"not ready"
            self.assertFalse(value.event_listener_ready())
            response.status = 503
            response.read.return_value = b"ok"
            self.assertFalse(value.event_listener_ready())

    def test_rpc_health_alone_does_not_satisfy_event_listener_readiness(self):
        value = self.deployment
        atomic_json(value.control / "programs.json", {"version": 2, "roles": {
            role: {"complete": {"deployment_slot": 100}} for role in ("portal", "token_bridge")}})
        responses = {"getSlot": 100, "getHealth": "ok", "getGenesisHash": DEVNET,
                     "getIdentity": {"identity": value.plan["validator"]}}
        value.l1 = SimpleNamespace(call=lambda method, *_: responses[method])
        value.upstream = SimpleNamespace(call=lambda *_: 100)
        value.er = Mock()
        for failure in (False, OSError("listener unavailable")):
            with self.subTest(failure=failure):
                value.event_listener_ready = Mock(side_effect=failure if isinstance(failure, Exception) else None, return_value=False)
                value.execute = Mock(side_effect=["", "active"])
                with patch.object(rollout.time, "monotonic", side_effect=[0, 0, 1201]), patch.object(rollout.time, "sleep"):
                    with self.assertRaisesRegex(RuntimeError, "startup/catchup timed out"):
                        value.start()
                value.event_listener_ready.assert_called_once()

    def test_changed_retained_binary_cannot_be_started(self):
        value = self.deployment
        value.runtime.mkdir(mode=0o700, parents=True)
        (value.runtime / "bin").mkdir(mode=0o700)
        binary = value.runtime / "bin/agave-validator"
        binary.write_bytes(b"changed")
        value.inventory = Mock(return_value=value.original)
        value.stop = Mock()
        value.retention = Mock(return_value={"retained": True})
        value.start = Mock(side_effect=MutationBoundary)
        def execute(arguments, **_):
            if "inspect" in arguments:
                return json.dumps(rollout.inspect_paths(private_json(arguments[-1])["paths"]))
            if "--property=ExecStart" in arguments:
                return "{ path=/usr/bin/env ; argv[]=/usr/bin/env " + str(binary) + " --portal " + value.plan["programs"]["portal"] + "; }"
            return ""
        value.execute = execute
        with patch.object(rollout, "DROPIN", self.root / "unit/managed.conf"):
            with self.assertRaisesRegex(ValueError, "binary.*differ"):
                value.activate()
        value.start.assert_not_called()


if __name__ == "__main__":
    unittest.main()

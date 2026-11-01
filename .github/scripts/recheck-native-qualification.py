#!/usr/bin/env python3
"""Qualify an independent case on the unchanged deployed session; never replace prior signed work."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import time
from types import SimpleNamespace
import urllib.error

from continuity_devnet import Deployment, DROPIN, ROOT, Rpc
from continuity_rollout_state import atomic_json, canonical, checked_directory, file_hash, immutable_json, private_json, require

REVISION = "b545c054c35861b6ed01e3e4baf9939cffedec47"


class ReadRetryRpc(Rpc):
    def call(self, method, params=None):
        require(method.startswith("get") or method == "isBlockhashValid", "retry wrapper is read-only")
        for attempt in range(9):
            try:
                return super().call(method, params)
            except urllib.error.HTTPError as error:
                if error.code not in (429, 502, 503, 504) or attempt == 8:
                    raise
                time.sleep(min(15, 1 << attempt))


def check_running(deployment):
    current = deployment.inventory()
    spec = private_json(deployment.control / "activation.json")
    require(current["arguments"] == spec["arguments"] and current["state_environment"] == spec["state_environment"]
            and current["binary_sha256"] == deployment.manifest["sha256"]["validator"], "running native specification differs")
    marker = private_json(deployment.control / "activation-installed.json")
    require(deployment.inspect([DROPIN])[str(DROPIN)].get("sha256") == marker["sha256"], "native drop-in changed")
    return {"pid": current["pid"], "start_time": current["start_time"]}


def restart_case(deployment, directory, label):
    intent = directory / (label + "-restart-intent.json")
    completed = directory / (label + "-restart-complete.json")
    if completed.exists():
        return private_json(completed)
    if not intent.exists():
        immutable_json(intent, {"version": 1, "before": check_running(deployment)})
    before = private_json(intent)["before"]
    state = deployment.execute(["systemctl", "show", "--property=ActiveState", "--value", "northstar-validator.service"]).strip()
    if state == "active" and check_running(deployment) == before:
        deployment.stop()
    deployment.start()
    after = check_running(deployment)
    require(after != before, "restart did not replace the native process")
    value = {"version": 1, "before": before, "after": after}
    immutable_json(completed, value)
    return value


def run(sdk, descriptor):
    root = Path("/solana/northstar/continuity-v2")
    control = root / REVISION / "control"
    checked_directory(control, private=True)
    saved = private_json(control / "plan.json")
    reservation = private_json(control / "reservation.json")
    require(reservation["plan_sha256"] == hashlib.sha256(canonical(saved)).hexdigest(), "native plan differs")
    require(saved["source_revision"] == REVISION and saved["sdk_revision"] == "24030ce9fc3b80aa21738db2bb9f5a5fbea030d8", "native source binding differs")
    original_config = private_json(control / "sdk-config.json")
    require(original_config["source_revision"] == REVISION and original_config["profile"] == saved["profile"]
            and original_config["portal"] == saved["programs"]["portal"] and original_config["token_bridge"] == saved["programs"]["token_bridge"], "native smoke identity differs")
    args = SimpleNamespace(stage=Path(reservation["stage"]), revision=REVISION, sdk_checkout=sdk, state_root=root,
                           funder_keypair=Path(original_config["funder_keypair"]), devnet_rpc=reservation["devnet_rpc"],
                           l1_rpc=reservation["l1_rpc"], er_rpc=reservation["er_rpc"],
                           max_estimated_cost_lamports=saved["max_estimated_cost_lamports"], lock_descriptor=descriptor)
    deployment = Deployment(args)
    deployment.upstream = ReadRetryRpc(args.devnet_rpc)
    deployment.prepare()
    require(deployment.plan == saved, "original native plan changed")
    check_running(deployment)
    deployment.deploy_programs()
    old_root = Path(original_config["smoke_directory"])
    original_intent = old_root / "controls/sol-withdraw/intent.json"
    intent_hash = file_hash(original_intent)
    old_config = original_config | {"sdk_checkout": str(sdk)}
    old_config_path = control / "absence-inspection-config.json"
    atomic_json(old_config_path, old_config)
    absence_path = control / "original-sol-absence-public.json"
    deployment.execute(["node", ROOT / ".github/scripts/native-smoke-absence.mjs", old_config_path, absence_path], timeout=300)
    absence = private_json(absence_path)
    require(absence["intent_file_sha256"] == intent_hash and absence["disposition"] == "not_included", "original intent absence differs")
    independent = control / "sdk-smoke-independent-v1"
    config = old_config | {"smoke_directory": str(independent)}
    config_path = control / "sdk-independent-config.json"
    atomic_json(config_path, config)
    if independent.exists():
        checked_directory(independent, private=True)
        index = private_json(independent / "keys/index.json")
        require(index.get("sol-payer") != absence["original_payer"] and index.get("sol-recipient") != absence["original_recipient"], "independent case reused original test keys")
    immutable_json(control / "independent-smoke-case.json", {
        "version": 1, "native_plan_sha256": reservation["plan_sha256"], "original_intent_sha256": intent_hash,
        "absence_sha256": absence["disposition_sha256"], "case": "independent-v1", "context": absence["context"],
    })
    phases, restarts = [], []
    for phase in ("prepare-sol", "resume-sol", "prepare-spl", "resume-spl"):
        if phase.startswith("resume-"):
            if (independent / (phase + ".json")).exists():
                restarts.append(private_json(independent / (phase + "-restart-complete.json")))
            else:
                restarts.append(restart_case(deployment, independent, phase))
        print(json.dumps({"phase": phase, "case": "independent-v1", "status": "starting"}), flush=True)
        raw = deployment.execute(["node", ROOT / ".github/scripts/devnet-continuity-smoke.mjs", config_path, phase], timeout=1200)
        phases.append(json.loads(raw.splitlines()[-1]))
        require(file_hash(original_intent) == intent_hash, "original signed intent changed")
        if phase == "prepare-sol":
            require(phases[-1]["payer"] != absence["original_payer"], "new case used original SOL source")
            index = private_json(independent / "keys/index.json")
            require(index["sol-recipient"] != absence["original_recipient"], "new case used original recipient")
    require(private_json(control / "retained.json") == deployment.retention(), "legacy retained state changed")
    check_running(deployment)
    report = {"version": 3, "result": "passed", "scope": "same-session independent SOL/SPL payout and two restarts",
              "source_revision": REVISION, "sdk_revision": saved["sdk_revision"], "genesis": saved["genesis"],
              "portal": config["portal"], "token_bridge": config["token_bridge"], "validator": saved["validator"],
              "profile": saved["profile"], "context": absence["context"], "original_absence": absence,
              "original_signed_intent_unchanged": True, "legacy_state_unchanged": True,
              "phases": phases, "restarts": restarts, "qualification_source": os.environ["GITHUB_SHA"],
              "proof_acceptance_qualified": False, "oracle_callback_qualified": False}
    report_path = control / "independent-qualification-report.json"
    atomic_json(report_path, report)
    print(json.dumps(report), flush=True)
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a") as stream:
            stream.write("report_path=" + str(report_path) + "\nportal=" + config["portal"] + "\n")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--sdk-checkout", type=Path, required=True)
    parser.add_argument("--confirmation", choices=("run-independent-case",), required=True)
    args = parser.parse_args()
    descriptor = os.open("/solana/northstar/continuity-v2-deploy.lock", os.O_RDWR | os.O_NOFOLLOW)
    try:
        info = os.fstat(descriptor)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077, "invalid private deployment lock")
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        run(args.sdk_checkout, descriptor)
    finally:
        os.close(descriptor)


if __name__ == "__main__":
    main()

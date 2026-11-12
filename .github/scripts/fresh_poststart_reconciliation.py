#!/usr/bin/env python3
"""Sonic: One reviewed settlement-pending resume; preserve every prior native and signed barrier."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import stat
import time
from types import SimpleNamespace
from urllib.parse import urlsplit

from continuity_devnet import Deployment, DROPIN, SERVICE
from continuity_rollout_state import atomic_json, checked_directory, immutable_json, private_json, require, service_dropin
from continuity_upload_resume import LOCK, STATE_ROOT, failure_summary
from fresh_devnet import FreshStack, digest
from native_runtime_diagnostics import effective_arguments_match, fresh_resume_review


def stopped_binding(ops, plan, saved):
    activation = private_json(ops.control / "activation.json")
    specification = {"arguments": activation["arguments"], "state_environment": activation["state_environment"],
                     "binary_sha256": plan["artifacts"]["validator"], "uid": saved["original"]["uid"], "gid": saved["original"]["gid"]}
    raw = ops.execute(["systemctl", "show", "--property=ActiveState,MainPID,Restart,InactiveEnterTimestamp,ExecStart", SERVICE])
    values = dict(line.split("=", 1) for line in raw.splitlines() if "=" in line)
    text = service_dropin(specification["arguments"], *[specification["state_environment"][key] for key in
        ("NORTHSTAR_CHECKPOINT_PLAN_DIR", "NORTHSTAR_UNSETTLED_STATE_DIR")])
    observed = ops.inspect([ops.fresh.dropin])[str(ops.fresh.dropin)]
    require(observed.get("sha256") == hashlib.sha256(text.encode()).hexdigest() and observed.get("mode") == 0o600
            and effective_arguments_match(values.get("ExecStart", ""), text), "stopped fresh service binding differs")
    return specification, values


def reconcile_fresh_resume(ops, controller_revision, review_sha256, context):
    require(re.fullmatch("[0-9a-f]{40}", controller_revision) and re.fullmatch("[0-9a-f]{64}", review_sha256)
            and re.fullmatch("[0-9a-f]{64}", context), "invalid poststart reconciliation provenance")
    marker, directory = ops.control / "fresh-poststart-reconciliation.json", ops.control / "fresh-reconciliation"
    require(not os.path.lexists(marker) and not os.path.lexists(directory), "poststart reconciliation already recorded")
    plan, saved = private_json(ops.control / "plan.json"), private_json(ops.control / "reservation.json")
    require(plan.get("version") == 2 and plan.get("mode") == "fresh-stack" and plan.get("source_revision") == ops.args.revision
            and saved.get("version") == 2 and saved.get("source_revision") == ops.args.revision
            and saved.get("plan_sha256") == digest(plan) and plan.get("original_sha256") == digest(saved["original"]),
            "poststart reconciliation retained plan differs")
    specification, stopped = stopped_binding(ops, plan, saved)
    reviewed = fresh_resume_review(ops.args.state_root, ops.args.revision, specification, stopped)
    require(reviewed.get("status") == "reviewable" and reviewed.get("review_sha256") == review_sha256
            and reviewed.get("context") == context, "poststart reconciliation review differs")
    config = private_json(ops.control / "sdk-config.json")
    programs = private_json(ops.control / "programs.json")
    require(programs.get("version") == 2 and set(programs.get("roles", {})) == {"portal", "token_bridge"}
            and all(programs["roles"][role].get("elf_sha256") == plan["artifacts"][role]
                    and programs["roles"][role].get("complete") for role in ("portal", "token_bridge")), "retained uploads differ")
    ops.fresh.authorize()
    ops.prepare()
    require(ops.plan == plan and ops.fresh.verify_desired() == specification, "prepared poststart runtime differs")
    ops.fresh.archive_unchanged()
    for role in ("portal", "token_bridge"):
        require(ops.program_view(role) == programs["roles"][role]["complete"], "poststart program observation differs")
    again_spec, again_stopped = stopped_binding(ops, plan, saved)
    again = fresh_resume_review(ops.args.state_root, ops.args.revision, again_spec, again_stopped)
    require(again.get("review_sha256") == review_sha256 and again_stopped == stopped, "reviewed stopped state changed during preflight")
    floor = ops.upstream.call("getSlot", [{"commitment": "processed"}])
    require(type(floor) is int and 0 <= floor < 1 << 64, "invalid poststart independent L1 floor")
    floor = max(floor, private_json(ops.control / "fresh-starts/resume-sol.json")["l1_floor"])
    record = {"version": 1, "kind": "fresh-poststart-reconciliation", "source_revision": ops.args.revision,
              "controller_revision": controller_revision, "plan_sha256": digest(plan), "review_sha256": review_sha256,
              "context": context, "extra_native_starts": 1, "program_submissions": 0, "phase": "prepared"}
    immutable_json(marker, record)
    directory.mkdir(mode=0o700)
    intent = {"version": 1, "plan_sha256": digest(plan), "review_sha256": review_sha256,
              "context": context, "l1_floor": floor, "phase": "resume-sol"}
    try:
        immutable_json(directory / "start-attempted.json", intent)
        record["phase"] = "start-attempted"
        atomic_json(marker, record)
        ops.flush([marker, directory / "start-attempted.json", directory, ops.control])
        ops.start_runtime()
        require(ops.fresh.verify_desired(running=True) == specification, "started poststart service binding differs")
        end = time.monotonic() + 1200
        while time.monotonic() < end:
            if min(ops.l1.call("getSlot", [{"commitment": level}]) for level in ("confirmed", "finalized")) >= floor:
                break
            time.sleep(1)
        else:
            raise RuntimeError("poststart independent L1 floor timed out")
        require(ops.er.call("getGenesisHash") == config["er_genesis"], "retained ER genesis changed")
        snapshot = ops.fresh.snapshot(plan)
        require(snapshot["identity"]["context"] == context and snapshot["er_genesis"] == config["er_genesis"],
                "retained poststart session identity differs")
        ops.fresh.archive_unchanged()
        immutable_json(directory / "ready.json", intent)
        record["phase"] = "running"
        atomic_json(marker, record)
        ops.fresh.advance("running")
        ops.flush([directory / "ready.json", marker, ops.control / "fresh-progress.json", directory, ops.control])
        ops.smoke(resumed_phase="resume-sol")
        report = private_json(ops.control / "report.json")
        require(report.get("result") == "passed" and report.get("qualification_kind") == "fresh-stack"
                and report.get("fresh_plan_sha256") == digest(plan) and report.get("source_revision") == ops.args.revision
                and report.get("context") == context and report.get("profile") == plan["profile"]
                and all(report.get(role) == plan["programs"][role] for role in ("portal", "token_bridge")),
                "poststart qualification report differs")
        report["poststart_reconciliation"] = {"controller_revision": controller_revision, "review_sha256": review_sha256,
            "extra_native_starts": 1, "program_submissions": 0}
        atomic_json(ops.control / "report.json", report)
        ops.fresh.advance("qualified")
        record["phase"] = "qualified"
        atomic_json(marker, record)
        ops.flush([marker, ops.control / "report.json", ops.control / "fresh-progress.json", ops.control])
        return report
    except BaseException:
        ops.fresh.fail(force=True)
        record.update(phase="failed", operator_reconciliation_required=True)
        atomic_json(marker, record)
        ops.flush([marker, ops.control])
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--context", required=True)
    parser.add_argument("--review-sha256", required=True)
    parser.add_argument("--sdk-checkout", required=True, type=Path)
    args = parser.parse_args()
    require(re.fullmatch("[0-9a-f]{40}", args.revision) and re.fullmatch("[0-9a-f]{64}", args.context)
            and re.fullmatch("[0-9a-f]{64}", args.review_sha256) and os.environ.get("GITHUB_REF") == "refs/heads/master"
            and os.environ.get("GITHUB_REPOSITORY") == "mirrorworld-universe/northstar"
            and re.fullmatch("[0-9a-f]{40}", os.environ.get("GITHUB_SHA", "")), "poststart reconciliation requires trusted master")
    descriptor = os.open(LOCK, os.O_RDWR | os.O_NOFOLLOW)
    try:
        info = os.fstat(descriptor)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077, "invalid private deployment lock")
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        control = STATE_ROOT / args.revision / "control"
        for directory in (STATE_ROOT, control.parent, control):
            checked_directory(directory, private=directory == control)
        plan, saved = private_json(control / "plan.json"), private_json(control / "reservation.json")
        require(saved.get("version") == 2 and saved.get("source_revision") == args.revision
                and saved.get("plan_sha256") == digest(plan) and plan.get("mode") == "fresh-stack", "missing retained fresh plan")
        for endpoint in (saved["l1_rpc"], saved["er_rpc"]):
            url = urlsplit(endpoint)
            require(url.scheme in ("http", "https") and url.hostname in ("localhost", "127.0.0.1", "::1")
                    and not url.username and not url.password and not url.query and not url.fragment and url.path in ("", "/"),
                    "poststart control RPC must be loopback")
        upstream = urlsplit(saved["devnet_rpc"])
        require(upstream.scheme == "https" and upstream.hostname and not upstream.username and not upstream.password
                and not upstream.query and not upstream.fragment, "poststart upstream RPC must be unauthenticated HTTPS")
        configured = SimpleNamespace(**vars(args), replace_context=saved["replacement"]["snapshot"]["identity"]["context"],
            stage=Path(saved["stage"]), state_root=STATE_ROOT, funder_keypair=Path("/home/ubuntu/.config/solana/id.json"),
            devnet_rpc=saved["devnet_rpc"], l1_rpc=saved["l1_rpc"], er_rpc=saved["er_rpc"], max_estimated_cost_lamports=10_000_000_000,
            abandon_previous_state=True, event_source="l1", upgrade_existing=False, recover_failed_l1_slot=None,
            reconcile_failed_l1=False, l1_rpc_profile=None, lock_descriptor=descriptor)
        ops = Deployment(configured)
        ops.fresh = FreshStack(ops, SERVICE, DROPIN)
        def interrupted(_signum, _frame):
            raise InterruptedError("fresh poststart reconciliation interrupted")
        previous_handler = signal.signal(signal.SIGTERM, interrupted)
        try:
            report = reconcile_fresh_resume(ops, os.environ["GITHUB_SHA"], args.review_sha256, args.context)
        finally:
            signal.signal(signal.SIGTERM, previous_handler)
        print(json.dumps(report), flush=True)
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a") as output:
                output.write("report_path=" + str(ops.control / "report.json") + "\n")
    finally:
        os.close(descriptor)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(json.dumps(failure_summary(error, "invocation")), flush=True)
        raise SystemExit("fresh poststart reconciliation failed; retained state requires operator review") from None

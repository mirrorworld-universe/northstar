#!/usr/bin/env python3
"""Continue a fully uploaded, never-started fresh plan with its frozen artifacts."""
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import signal
import stat
from types import SimpleNamespace
from urllib.parse import urlsplit

from continuity_devnet import Deployment, DROPIN, SERVICE
from continuity_rollout_state import DEVNET, atomic_json, checked_directory, immutable_json, private_json, require
from continuity_upload_resume import LOCK, STATE_ROOT, failure_summary, prepared, unchanged_service
from fresh_devnet import FreshStack, digest
from native_runtime_diagnostics import fresh_continuation_status

REVIEWED_PRESTART_CONTROLLER = "c768a521eb968fdef2dbc84eb3bc7c78495ac760"


def continuation_attempt(ops, plan, reservation, programs, acknowledgement):
    primary = ops.control / "fresh-controller-continuation.json"
    retry = ops.control / "fresh-controller-continuation-retry.json"
    require(not os.path.lexists(retry), "additional fresh continuation already recorded; operator reconciliation required")
    if acknowledgement is None:
        require(not os.path.lexists(primary), "fresh controller continuation already recorded; operator reconciliation required")
        return primary, {}
    require(re.fullmatch("[0-9a-f]{64}", acknowledgement), "invalid failed continuation acknowledgement")
    previous = private_json(primary)
    summary = fresh_continuation_status(ops.control, plan, reservation, reservation["replacement"]["service"], programs)
    require(summary.get("status") == "recorded" and summary.get("phase") == "failed"
            and summary.get("controller_revision") == REVIEWED_PRESTART_CONTROLLER
            and summary.get("quarantine_requested") is False and digest(previous) == acknowledgement
            and not any(flag in previous for flag in ("quarantine_requested", "quarantine_verified")),
            "reviewed prestart continuation record differs")
    return retry, {"previous_failed_continuation_sha256": acknowledgement}


def continue_prepared_fresh(ops, controller_revision, plan_sha256, *, failed_continuation_sha256=None):
    require(re.fullmatch("[0-9a-f]{40}", controller_revision) and re.fullmatch("[0-9a-f]{64}", plan_sha256),
            "invalid prepared continuation provenance")
    plan = private_json(ops.control / "plan.json")
    reservation = private_json(ops.control / "reservation.json")
    state = private_json(ops.control / "programs.json")
    require(plan.get("version") == 2 and plan.get("mode") == "fresh-stack" and plan.get("genesis") == DEVNET
            and plan.get("source_revision") == ops.args.revision and digest(plan) == plan_sha256
            and reservation.get("version") == 2 and reservation.get("source_revision") == ops.args.revision
            and reservation.get("plan_sha256") == plan_sha256
            and plan.get("original_sha256") == digest(reservation["original"])
            and plan.get("replacement_sha256") == digest(reservation["replacement"]), "prepared continuation plan differs")
    require(private_json(ops.control / "fresh-progress.json") == {
        "version": 1, "plan_sha256": plan_sha256, "phase": "prepared"}
        and not any(os.path.lexists(ops.control / name) for name in ("fresh-starts", "activation.json", "activation-installed.json"))
        and not os.path.lexists(ops.runtime), "fresh continuation requires no prior native activation")
    require(isinstance(state, dict) and set(state) == {"version", "roles"} and state["version"] == 2
            and isinstance(state["roles"], dict) and set(state["roles"]) == {"portal", "token_bridge"}
            and all(isinstance(entry, dict) and set(entry) == {"elf_sha256", "complete"}
                    and entry["elf_sha256"] == plan["artifacts"][role] and isinstance(entry["complete"], dict) and entry["complete"]
                    for role, entry in state["roles"].items()), "prepared continuation requires completed uploads")
    marker, acknowledgement = continuation_attempt(ops, plan, reservation, state, failed_continuation_sha256)
    require(ops.l1.call("getHealth") == "ok" and ops.l1.call("getGenesisHash") == DEVNET
            and ops.l1.call("getIdentity")["identity"] == plan["validator"], "prepared continuation L1 identity differs")
    original_upstream = ops.upstream
    try:
        ops.upstream = ops.l1
        ops.fresh.authorize()
        ops.prepare()
        require(ops.plan == plan, "prepared continuation plan changed during verification")
        prepared(ops)
        ops.deploy_programs()
        pid = unchanged_service(ops)
        before = ops.inventory()
        require(str(before["pid"]) == pid == str(reservation["original"]["pid"])
                and before["start_time"] == reservation["original"]["start_time"], "prepared continuation previous process changed")
    finally:
        ops.upstream = original_upstream
    require(private_json(ops.control / "programs.json") == state, "prepared continuation upload observations changed")
    require(ops.upstream.call("getGenesisHash") == DEVNET, "prepared continuation independent genesis differs")
    reference_slot = ops.upstream.call("getSlot", [{"commitment": "confirmed"}])
    require(type(reference_slot) is int and 0 <= reference_slot < 1 << 64
            and reference_slot >= max(entry["complete"]["deployment_slot"] for entry in state["roles"].values()),
            "prepared continuation independent slot precedes deployment")
    record = {"version": 2 if acknowledgement else 1, "kind": "prepared-fresh-controller", "source_revision": ops.args.revision,
              "controller_revision": controller_revision, "plan_sha256": plan_sha256,
              "programs_sha256": digest(state), "previous_service_sha256": digest(ops.fresh.previous["service"]),
              "previous_pid": int(pid), "previous_start_time": before["start_time"],
              "preflight_rpc": ops.args.l1_rpc, "qualification_rpc": ops.args.devnet_rpc, "phase": "attempted", **acknowledgement}
    immutable_json(marker, record)
    ops.flush([marker, ops.control])
    try:
        report = ops.run()
        require(private_json(ops.control / "fresh-progress.json") == {
            "version": 1, "plan_sha256": plan_sha256, "phase": "qualified"}
            and report == private_json(ops.control / "report.json") and report.get("result") == "passed"
            and report.get("qualification_kind") == "fresh-stack" and report.get("fresh_plan_sha256") == plan_sha256
            and all(report.get(role) == plan["programs"][role] for role in ("portal", "token_bridge"))
            and report.get("profile") == plan["profile"] and isinstance(report.get("context"), str)
            and re.fullmatch("[0-9a-f]{64}", report["context"]) and report["context"] != ops.args.replace_context,
            "prepared continuation lacks qualified fresh evidence")
        record["phase"] = "qualified"
        atomic_json(marker, record)
        ops.flush([marker, ops.control / "report.json", ops.control / "fresh-progress.json", ops.control])
        return report
    except BaseException:
        record.update(phase="failed", operator_reconciliation_required=True)
        try:
            progress = private_json(ops.control / "fresh-progress.json")
            prestart = progress.get("phase") == "prepared" and not any(os.path.lexists(path) for path in (
                ops.control / "fresh-starts", ops.control / "activation.json", ops.runtime))
        except Exception:
            prestart = False
        if not prestart:
            record["quarantine_requested"] = True
            try:
                ops.fresh.fail(force=True)
                record["quarantine_verified"] = private_json(ops.control / "fresh-progress.json").get("quarantined") is True
            except Exception:
                record["quarantine_verified"] = False
        atomic_json(marker, record)
        ops.flush([marker, ops.control])
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--replace-context", required=True)
    parser.add_argument("--plan-sha256", required=True)
    parser.add_argument("--sdk-checkout", required=True, type=Path)
    parser.add_argument("--failed-continuation-sha256")
    args = parser.parse_args()
    require(re.fullmatch("[0-9a-f]{40}", args.revision) and re.fullmatch("[0-9a-f]{64}", args.replace_context)
            and re.fullmatch("[0-9a-f]{64}", args.plan_sha256) and os.environ.get("GITHUB_REF") == "refs/heads/master"
            and os.environ.get("GITHUB_REPOSITORY") == "mirrorworld-universe/northstar"
            and re.fullmatch("[0-9a-f]{40}", os.environ.get("GITHUB_SHA", "")), "prepared continuation requires the trusted master workflow")
    descriptor = os.open(LOCK, os.O_RDWR | os.O_NOFOLLOW)
    try:
        info = os.fstat(descriptor)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077,
                "deployment lock is not private regular state")
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        control = STATE_ROOT / args.revision / "control"
        for directory in (STATE_ROOT, control.parent, control):
            checked_directory(directory, private=directory == control)
        saved = private_json(control / "reservation.json")
        require(saved.get("version") == 2 and saved.get("source_revision") == args.revision
                and saved.get("plan_sha256") == args.plan_sha256
                and saved.get("replacement", {}).get("snapshot", {}).get("identity", {}).get("context") == args.replace_context,
                "prepared continuation reservation differs")
        for endpoint in (saved["l1_rpc"], saved["er_rpc"]):
            url = urlsplit(endpoint)
            require(url.scheme in ("http", "https") and url.hostname in ("localhost", "127.0.0.1", "::1")
                    and not url.username and not url.password and not url.query and not url.fragment and url.path in ("", "/"),
                    "prepared continuation control RPC must be a loopback root")
        upstream = urlsplit(saved["devnet_rpc"])
        require(upstream.scheme == "https" and upstream.hostname and not upstream.username and not upstream.password
                and not upstream.query and not upstream.fragment, "prepared continuation upstream RPC requires unauthenticated HTTPS")
        configured = SimpleNamespace(**vars(args), stage=Path(saved["stage"]), state_root=STATE_ROOT,
            funder_keypair=Path("/home/ubuntu/.config/solana/id.json"), devnet_rpc=saved["devnet_rpc"],
            l1_rpc=saved["l1_rpc"], er_rpc=saved["er_rpc"], max_estimated_cost_lamports=10_000_000_000,
            abandon_previous_state=True, event_source="l1", upgrade_existing=False, recover_failed_l1_slot=None,
            reconcile_failed_l1=False, l1_rpc_profile=None, lock_descriptor=descriptor)
        ops = Deployment(configured)
        ops.fresh = FreshStack(ops, SERVICE, DROPIN)
        def interrupted(_signum, _frame):
            raise InterruptedError("prepared continuation interrupted")
        previous_handler = signal.signal(signal.SIGTERM, interrupted)
        try:
            report = continue_prepared_fresh(ops, os.environ["GITHUB_SHA"], args.plan_sha256,
                failed_continuation_sha256=args.failed_continuation_sha256)
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
        raise SystemExit("prepared continuation failed; retained state requires operator review") from None

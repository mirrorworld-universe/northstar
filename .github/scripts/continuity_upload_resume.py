#!/usr/bin/env python3
"""Reconcile one retained fresh-program upload pass without native service actions."""
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

from continuity_devnet import Deployment, DROPIN, LOADER, SERVICE, public_bytes
from continuity_rollout_state import (ARTIFACTS, DEVNET, atomic_json, checked_directory,
                                     immutable_json, private_json, require)
from fresh_devnet import FreshStack, digest
from native_runtime_diagnostics import upload_operation_errors
from native_upgrade import service_spec

STATE_ROOT = Path("/solana/northstar/continuity-v2")
LOCK = Path("/solana/northstar/continuity-v2-deploy.lock")
ROLES = {"portal", "token_bridge"}
CURRENT_PHASE = "invocation"
PHASES = {"invocation", "trusted-source", "deployment-lock", "retained-directories", "retained-reservation",
          "recorded-rpc-bindings", "construct-controller", "retained-records", "retry-evidence", "source-prepare",
          "previous-service", "buffer-binding", "attempt-record", "submission", "finalized-programs",
          "previous-service-validation", "completion"}


def mark_phase(value):
    global CURRENT_PHASE
    require(value in PHASES, "unknown reconciliation phase")
    CURRENT_PHASE = value


def failure_summary(error, phase):
    allowed = {"FileNotFoundError", "PermissionError", "BlockingIOError", "ValueError", "TypeError", "KeyError",
               "RuntimeError", "AttributeError", "TimeoutExpired", "InterruptedError", "TimeoutError", "HTTPError", "URLError"}
    name = type(error).__name__
    codes = {"SDK revision differs": "sdk-revision", "SDK dependency lock differs": "sdk-lock",
             "deployment directory ownership or permissions differ": "directory-binding",
             "deployment lock is not private regular state": "lock-binding",
             "upload retry pass already recorded; operator reconciliation required": "attempt-budget",
             "retained upload retry exhaustion is not established": "retry-evidence"}
    report = {"status": "reconciliation-failed", "phase": phase if phase in PHASES else "unavailable",
              "failure_type": name if name in allowed else "unclassified", "failure_code": codes.get(str(error), "unclassified"),
              "raw_error_included": False}
    if isinstance(error, OSError) and type(error.errno) is int and 0 < error.errno < 65536:
        report["errno"] = error.errno
    return report


def prepared(ops):
    require(private_json(ops.control / "plan.json") == ops.plan
            and ops.plan.get("version") == 2 and ops.plan.get("mode") == "fresh-stack"
            and ops.plan.get("genesis") == DEVNET and ops.plan.get("source_revision") == ops.args.revision
            and ops.plan.get("artifacts") == ops.manifest["sha256"], "retained upload plan differs")
    reservation = private_json(ops.control / "reservation.json")
    require(reservation.get("version") == 2 and reservation.get("source_revision") == ops.args.revision
            and reservation.get("plan_sha256") == digest(ops.plan)
            and reservation.get("l1_rpc") == ops.args.l1_rpc
            and reservation.get("replacement") == ops.fresh.previous, "retained upload reservation differs")
    require(private_json(ops.control / "fresh-progress.json") == {
        "version": 1, "plan_sha256": digest(ops.plan), "phase": "prepared"}
        and not any(os.path.lexists(ops.control / name) for name in
                    ("fresh-starts", "activation.json", "activation-installed.json"))
        and not os.path.lexists(ops.runtime), "upload reconciliation requires prepared progress and no native start")


def unchanged_service(ops):
    previous = ops.fresh.previous
    require(ops.fresh.state() == "active" and service_spec(ops.inventory()) == previous["service"],
            "previous service changed during upload reconciliation")
    ops.fresh.effective(previous["service"])
    verifier, _binding = ops.fresh.archived_binding(previous["service"])
    snapshot = ops.fresh.snapshot(verifier.base_plan)
    require(snapshot["identity"] == previous["snapshot"]["identity"]
            and snapshot["er_genesis"] == previous["snapshot"]["er_genesis"], "previous context changed during upload reconciliation")
    pid = ops.execute(["systemctl", "show", "--property=MainPID", "--value", ops.fresh.service]).strip()
    require(pid.isdigit() and int(pid) > 0, "previous service has no stable process")
    return pid


def validate_buffers(ops):
    state = private_json(ops.control / "programs.json")
    require(state.get("version") == 2 and isinstance(state.get("roles"), dict)
            and set(state["roles"]) <= ROLES, "retained upload state differs")
    pending = set()
    for role in sorted(ROLES):
        entry = state["roles"].get(role)
        if entry is None:
            require(ops.upstream.account(ops.plan["programs"][role]) is None
                    and ops.upstream.account(ops.plan["programs"][role + "-buffer"]) is None,
                    "unrecorded program or buffer cannot be overwritten")
            continue
        require(isinstance(entry, dict) and set(entry) == {"elf_sha256", "complete"}
                and entry["elf_sha256"] == ops.manifest["sha256"][role], "retained upload artifact differs")
        observed = ops.program_view(role)
        if entry["complete"] is not None:
            require(observed == entry["complete"], "completed upload observation changed")
            continue
        pending.add(role)
        if observed is not None:
            continue
        buffer = ops.upstream.account(ops.plan["programs"][role + "-buffer"])
        if buffer is None:
            continue
        elf = (ops.stage / ARTIFACTS[role]).read_bytes()
        raw = buffer["raw"]
        require(0 < len(elf) <= 10 * 1024 * 1024 and buffer["owner"] == LOADER and not buffer["executable"]
                and len(raw) == len(elf) + 37 and raw[:5] == (1).to_bytes(4, "little") + b"\x01"
                and raw[5:37] == public_bytes(ops.plan["funder"]), "retained buffer authority, owner or size differs")
        require(all(actual in (0, expected) for actual, expected in zip(raw[37:], elf)),
                "retained buffer contents differ from the frozen artifact")
    require(pending, "no unresolved upload intent requires reconciliation")
    return pending


def resume_uploads(ops, controller_revision):
    mark_phase("retained-records")
    require(re.fullmatch("[0-9a-f]{40}", controller_revision), "invalid upload controller revision")
    require(not any(os.path.lexists(ops.control / name) for name in
                    ("program-upload-reconcile.json", "program-upload-reconcile-report.json")),
            "upload retry pass already recorded; operator reconciliation required")
    original_plan = private_json(ops.control / "plan.json")
    original_reservation = private_json(ops.control / "reservation.json")
    private_json(ops.control / "programs.json")
    require(original_plan.get("version") == 2 and original_plan.get("mode") == "fresh-stack"
            and original_plan.get("source_revision") == ops.args.revision
            and original_reservation.get("plan_sha256") == digest(original_plan)
            and private_json(ops.control / "fresh-progress.json") == {
                "version": 1, "plan_sha256": digest(original_plan), "phase": "prepared"}
            and not any(os.path.lexists(ops.control / name) for name in
                        ("fresh-starts", "activation.json", "activation-installed.json"))
            and not os.path.lexists(ops.runtime), "retained upload requires an existing prepared plan")
    mark_phase("retry-evidence")
    errors = upload_operation_errors(ops.control / "operations.log", ops.control.stat().st_uid)
    markers = errors.get("markers", {})
    require(errors.get("status") == "classified" and markers.get("upload_write_failure", 0) > 0
            and markers.get("retry_budget_exhausted", 0) > 0
            and not any(markers.get(name) for name in ("insufficient_funds", "elf_verification", "program_size", "transaction_rejected")),
            "retained upload retry exhaustion is not established")
    mark_phase("source-prepare")
    ops.fresh.authorize()
    ops.prepare()
    require(ops.plan == original_plan, "retained plan changed during preparation")
    prepared(ops)
    mark_phase("previous-service")
    pid = unchanged_service(ops)
    require(ops.l1.call("getHealth") == "ok" and ops.l1.call("getGenesisHash") == DEVNET
            and ops.l1.call("getIdentity")["identity"] == ops.plan["validator"], "recorded L1 RPC identity differs")
    mark_phase("buffer-binding")
    pending = validate_buffers(ops)
    mark_phase("attempt-record")
    marker = ops.control / "program-upload-reconcile.json"
    record = {"version": 1, "kind": "retained-program-upload", "plan_sha256": digest(ops.plan),
              "controller_revision": controller_revision, "source_revision": ops.args.revision,
              "submission_rpc": ops.args.l1_rpc, "previous_service_sha256": digest(ops.fresh.previous["service"]),
              "pending_roles": sorted(pending), "attempt_budget": 1, "phase": "attempted"}
    immutable_json(marker, record)
    ops.flush([marker, ops.control])
    try:
        mark_phase("submission")
        ops.deploy_programs(resume_roles=pending, submission_rpc=ops.args.l1_rpc, stop_worker=False)
        mark_phase("finalized-programs")
        state = private_json(ops.control / "programs.json")
        require(set(state["roles"]) == ROLES and all(state["roles"][role]["complete"] is not None
                and ops.program_view(role) == state["roles"][role]["complete"] for role in ROLES),
                "program uploads are not finalized against the frozen artifacts")
        mark_phase("previous-service-validation")
        require(unchanged_service(ops) == pid, "previous service process changed during upload reconciliation")
        prepared(ops)
        mark_phase("completion")
        record["phase"] = "completed"
        atomic_json(marker, record)
        report = {"version": 1, "result": "uploads-completed", "source_revision": ops.args.revision,
                  "controller_revision": controller_revision, "plan_sha256": digest(ops.plan),
                  "programs": {role: ops.plan["programs"][role] for role in sorted(ROLES)},
                  "native_cutover_performed": False, "previous_service_unchanged": True,
                  "original_keys_retained": True, "controller_upload_passes": 1}
        atomic_json(ops.control / "program-upload-reconcile-report.json", report)
        ops.flush([marker, ops.control / "programs.json", ops.control / "program-upload-reconcile-report.json", ops.control])
        return report
    except BaseException:
        record.update(phase="failed", operator_reconciliation_required=True)
        atomic_json(marker, record)
        atomic_json(ops.control / "program-upload-reconcile-report.json", {
            "version": 1, "result": "failed", "source_revision": ops.args.revision,
            "controller_revision": controller_revision, "native_cutover_performed": False,
            "operator_reconciliation_required": True})
        ops.flush([marker, ops.control / "program-upload-reconcile-report.json", ops.control])
        raise


def main():
    mark_phase("trusted-source")
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--revision", required=True)
    parser.add_argument("--replace-context", required=True)
    parser.add_argument("--sdk-checkout", required=True, type=Path)
    args = parser.parse_args()
    require(re.fullmatch("[0-9a-f]{40}", args.revision) and re.fullmatch("[0-9a-f]{64}", args.replace_context)
            and os.environ.get("GITHUB_REF") == "refs/heads/master"
            and os.environ.get("GITHUB_REPOSITORY") == "mirrorworld-universe/northstar"
            and re.fullmatch("[0-9a-f]{40}", os.environ.get("GITHUB_SHA", "")), "upload reconciliation requires the trusted master workflow")
    mark_phase("deployment-lock")
    descriptor = os.open(LOCK, os.O_RDWR | os.O_NOFOLLOW)
    try:
        info = os.fstat(descriptor)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077,
                "deployment lock is not private regular state")
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        mark_phase("retained-directories")
        control = STATE_ROOT / args.revision / "control"
        for directory in (STATE_ROOT, control.parent, control):
            checked_directory(directory, private=directory == control)
        mark_phase("retained-reservation")
        saved = private_json(control / "reservation.json")
        require(saved.get("version") == 2 and saved.get("source_revision") == args.revision
                and saved.get("replacement", {}).get("snapshot", {}).get("identity", {}).get("context") == args.replace_context,
                "retained upload reservation identity differs")
        mark_phase("recorded-rpc-bindings")
        for endpoint in (saved["l1_rpc"], saved["er_rpc"]):
            url = urlsplit(endpoint)
            require(url.scheme in ("http", "https") and url.hostname in ("localhost", "127.0.0.1", "::1")
                    and not url.username and not url.password and not url.query and not url.fragment and url.path in ("", "/"),
                    "retained control RPC must be a loopback root")
        upstream = urlsplit(saved["devnet_rpc"])
        require(upstream.scheme == "https" and upstream.hostname and not upstream.username and not upstream.password
                and not upstream.query and not upstream.fragment, "retained upstream RPC requires unauthenticated HTTPS")
        mark_phase("construct-controller")
        configured = SimpleNamespace(**vars(args), stage=Path(saved["stage"]), state_root=STATE_ROOT,
            funder_keypair=Path("/home/ubuntu/.config/solana/id.json"), devnet_rpc=saved["devnet_rpc"],
            l1_rpc=saved["l1_rpc"], er_rpc=saved["er_rpc"], max_estimated_cost_lamports=10_000_000_000,
            abandon_previous_state=True, event_source="l1", upgrade_existing=False, recover_failed_l1_slot=None,
            reconcile_failed_l1=False, l1_rpc_profile=None, lock_descriptor=descriptor)
        ops = Deployment(configured)
        ops.fresh = FreshStack(ops, SERVICE, DROPIN)
        def interrupted(_signum, _frame):
            raise InterruptedError("upload reconciliation interrupted")
        previous_handler = signal.signal(signal.SIGTERM, interrupted)
        try:
            report = resume_uploads(ops, os.environ["GITHUB_SHA"])
        finally:
            signal.signal(signal.SIGTERM, previous_handler)
        print(json.dumps(report), flush=True)
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a") as output:
                output.write("report_path=" + str(ops.control / "program-upload-reconcile-report.json") + "\n")
    finally:
        os.close(descriptor)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        print(json.dumps(failure_summary(error, CURRENT_PHASE)), flush=True)
        raise SystemExit("upload reconciliation failed; retained state requires operator review") from None

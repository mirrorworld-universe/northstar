"""Sonic: Reconcile one stopped L1 recovery without clearing its original start barrier."""
import hashlib
import json
import os
from pathlib import Path
import re
import shlex

import native_l1_recovery as recovery
from continuity_rollout_state import checked_directory, immutable_json, private_json, require, service_dropin
from native_upgrade import ROOT, STATE_KEYS, digest, start_floor

STOP_PROPERTIES = ("ActiveState", "SubState", "MainPID", "Result", "ExecMainCode", "ExecMainStatus",
                   "ExecMainExitTimestamp", "Restart", "NRestarts", "NeedDaemonReload")


def stopped_service(upgrade):
    values = {name: upgrade.ops.execute(["systemctl", "show", "--property=" + name, "--value", upgrade.service]).strip()
              for name in STOP_PROPERTIES}
    expected = {"ActiveState": "inactive", "SubState": "dead", "MainPID": "0", "Result": "success",
                "ExecMainCode": "2", "ExecMainStatus": "15", "Restart": "no", "NRestarts": "0", "NeedDaemonReload": "no"}
    require(all(values[name] == value for name, value in expected.items())
            and values["ExecMainExitTimestamp"] == upgrade.args.reconcile_stopped_at,
            "reconciliation requires the explicitly reviewed stopped service")
    return values


def verify_installation(upgrade):
    upgrade.native_files()
    upgrade.effective_start()
    recovery.verify_service_files(upgrade.ops, upgrade.base, upgrade.service, upgrade.dropin)
    desired = upgrade.plan["desired"]
    text = service_dropin(desired["arguments"], *[desired["state_environment"][name] for name in STATE_KEYS])
    require(upgrade.inspect([upgrade.dropin])[str(upgrade.dropin)].get("sha256") == hashlib.sha256(text.encode()).hexdigest(),
            "reconciled service drop-in differs")
    effective = upgrade.ops.execute(["systemctl", "show", "--property=ExecStart", "--value", upgrade.service])
    arguments = re.findall(r"(?:^|[ ;])argv\[\]=(.*?)(?= ; |;| \})", effective)
    require(len(arguments) == 1 and shlex.split(arguments[0]) == shlex.split(text.rsplit("ExecStart=", 1)[1].strip()),
            "reconciled effective arguments differ")


def retained_state(upgrade):
    retained = upgrade.fingerprint(upgrade.plan["retained_paths"])
    controls = {name: value for name, value in retained.items()
                if Path(name) == upgrade.base_control or upgrade.base_control in Path(name).parents}
    require(controls == private_json(upgrade.frozen)["protocol_controls"], "retained protocol control records changed")
    directories = recovery.paths(upgrade.runtime)
    observed = upgrade.inspect(directories)
    accounts = upgrade.runtime / "l1/accounts"
    require(all(observed[str(directory)].get("directory") is True
                and observed[str(directory)].get("uid") == upgrade.plan["desired"]["uid"]
                and observed[str(directory)].get("gid") == upgrade.plan["desired"]["gid"]
                and observed[str(directory)].get("mode") in ({0o700, 0o755} if directory == accounts else {0o700})
                for directory in directories),
            "retained recovery directories differ")
    return retained, observed


def prepare(upgrade):
    require(upgrade.recovery_slot is not None and upgrade.phase() == "start-attempted" and upgrade.current is None,
            "reconciliation requires an unqualified stopped L1 recovery")
    require(isinstance(getattr(upgrade.args, "reconcile_stopped_at", None), str)
            and 0 < len(upgrade.args.reconcile_stopped_at) <= 160
            and not any(ord(character) < 32 for character in upgrade.args.reconcile_stopped_at),
            "an explicit reviewed stop timestamp is required")
    require(not os.path.lexists(upgrade.control / "report.json"), "a qualified recovery cannot be reconciled again")
    directory = upgrade.control / "l1-retry"
    require(not any(os.path.lexists(directory / name) for name in ("start-attempted.json", "outcome.json", "failure.json")),
            "the additional recovery start was already attempted; operator reconciliation is required")
    stopped = stopped_service(upgrade)
    verify_installation(upgrade)
    retained, directories = retained_state(upgrade)
    require(upgrade.snapshot("l1")["identity"] == upgrade.plan["identity"], "retained session changed before reconciliation")
    revision = upgrade.ops.execute(["git", "-C", ROOT, "rev-parse", "HEAD"]).strip()
    require(re.fullmatch("[0-9a-f]{40}", revision), "full reconciliation controller revision required")
    authorization = {"version": 1, "additional_starts": 1, "reason": "reviewed_readiness_timeout", "readiness_seconds": 3600,
                     "plan_sha256": digest(upgrade.plan), "first_start_sha256": digest(private_json(upgrade.started)),
                     "controller_revision": revision, "stopped_service": stopped, "retained": retained, "l1_directories": directories}
    if not os.path.lexists(directory):
        directory.mkdir(mode=0o700)
    checked_directory(directory, private=True)
    immutable_json(directory / "authorization.json", authorization)
    upgrade.ops.flush([directory / "authorization.json", directory, upgrade.control])
    upgrade.retry_authorization = authorization
    upgrade.retrying = True


def retry_floor(upgrade):
    directory = upgrade.control / "l1-retry"
    authorization = private_json(directory / "authorization.json")
    started = private_json(directory / "start-attempted.json")
    require(authorization.get("version") == 1 and authorization.get("plan_sha256") == digest(upgrade.plan)
            and authorization.get("first_start_sha256") == digest(private_json(upgrade.started))
            and started.get("version") == 1 and started.get("authorization_sha256") == digest(authorization)
            and set(started) == {"version", "authorization_sha256", "l1_floor"}
            and type(started.get("l1_floor")) is int
            and start_floor(upgrade.started, upgrade.plan) <= started["l1_floor"] <= 2 ** 64 - 1,
            "reconciled start barrier differs")
    return started["l1_floor"]


def run(upgrade):
    directory = upgrade.control / "l1-retry"
    authorization = private_json(directory / "authorization.json")
    require(authorization == upgrade.retry_authorization, "reconciliation authorization changed before start")
    require(not os.path.lexists(directory / "start-attempted.json"), "additional recovery start already attempted")
    require(authorization["stopped_service"] == stopped_service(upgrade), "reviewed stopped service changed")
    verify_installation(upgrade)
    retained, directories = retained_state(upgrade)
    require(retained == authorization["retained"] and directories == authorization["l1_directories"],
            "retained recovery state changed after reconciliation")
    require(upgrade.snapshot("l1")["identity"] == upgrade.plan["identity"], "retained session changed while stopped")
    floor = upgrade.ops.upstream.call("getSlot", [{"commitment": "processed"}])
    require(type(floor) is int and 0 <= floor <= 2 ** 64 - 1, "invalid reconciled L1 slot floor")
    started = {"version": 1, "authorization_sha256": digest(authorization),
               "l1_floor": max(floor, start_floor(upgrade.started, upgrade.plan))}
    immutable_json(directory / "start-attempted.json", started)
    upgrade.ops.flush([directory / "start-attempted.json", directory / "authorization.json", directory, upgrade.control])
    try:
        upgrade.ops.execute(["sudo", "-n", "systemctl", "start", upgrade.service], timeout=180)
        report = upgrade.qualify()
        report = {**report, "qualification_kind": "native-l1-recovery-retry", "recovery_retry": 1,
                  "controller_revision": authorization["controller_revision"], "readiness_seconds": 3600,
                  "retry_authorization_sha256": digest(authorization)}
        immutable_json(directory / "report.json", report)
        immutable_json(directory / "outcome.json", {"version": 1, "authorization_sha256": digest(authorization),
                                                   "result": "qualified", "report_sha256": digest(report)})
        upgrade.ops.flush([directory / "report.json", directory / "outcome.json", directory])
    except BaseException:
        stopped = upgrade.quarantine()
        immutable_json(directory / "failure.json", {"version": 1, "authorization_sha256": digest(authorization),
                                                   "result": "qualification_failed", "stop_verified": stopped})
        upgrade.ops.flush([directory / "failure.json", directory])
        raise
    if os.environ.get("GITHUB_OUTPUT"):
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write("qualification_kind=native-l1-recovery-retry\nreport_path=" + str(directory / "report.json") + "\n")
    print(json.dumps(report), flush=True)
    return report

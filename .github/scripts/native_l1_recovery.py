"""Sonic: Explicit failed-L1 recovery preserves the old ledger and protocol namespace."""
import grp
from pathlib import Path
import pwd
import re

from continuity_rollout_state import DEVNET, SDK_REVISION, checked_directory, flag_value, private_json, require

FEATURE = "8whkDenELnfwYLirBc4Wb4H2Z1z5K7tiykpdxJS7cWg1"
FEATURE_OWNER = "Feature111111111111111111111111111111111111"
REVIEWED_NATIVE_FIX = "c25f5f80b7f94a5948ce3e116fd98fd739ee2310"
STATE_PATHS = {
    "--ledger": "ledger",
    "--accounts": "accounts",
    "--accounts-index-path": "accounts-index",
    "--snapshots": "snapshots",
    "--full-snapshot-archive-path": "snapshots/full",
    "--incremental-snapshot-archive-path": "snapshots/incremental",
    "--tower": "tower",
}


def paths(runtime):
    parent = Path(runtime) / "l1"
    return [parent, *dict.fromkeys(parent / name for name in STATE_PATHS.values())]


def arguments(original, runtime):
    require(isinstance(original, list) and original, "invalid recovery arguments")
    flag_value(original, "--ledger")
    blocked = {"--expected-bank-hash", "--wait-for-supermajority", "--halt-at-slot", "--hard-fork"}
    require(not any(argument.split("=", 1)[0] in blocked for argument in original),
            "L1 recovery cannot discard explicit replay constraints")
    if any(argument == "--expected-genesis-hash" or argument.startswith("--expected-genesis-hash=") for argument in original):
        require(flag_value(original, "--expected-genesis-hash") == DEVNET, "retained genesis differs")
    remove_values = set(STATE_PATHS) | {"--use-snapshot-archives-at-startup", "--expected-genesis-hash"}
    remove_flags = {"--no-genesis-fetch", "--no-snapshot-fetch", "--no-incremental-snapshots"}
    result, position = [original[0]], 1
    while position < len(original):
        argument = original[position]
        name = argument.split("=", 1)[0]
        if name in remove_values:
            if "=" not in argument:
                require(position + 1 < len(original) and not original[position + 1].startswith("--"),
                        "recovery state flag value is missing")
                position += 1
                if name in ("--accounts", "--accounts-index-path"):
                    while position + 1 < len(original) and not original[position + 1].startswith("-"):
                        position += 1
        elif name not in remove_flags:
            result.append(argument)
        position += 1
    parent = Path(runtime) / "l1"
    require(parent.is_absolute(), "absolute recovery namespace required")
    for name, suffix in STATE_PATHS.items():
        result.extend([name, str(parent / suffix)])
    result.extend(["--expected-genesis-hash", DEVNET, "--use-snapshot-archives-at-startup", "when-newest"])
    if "--no-voting" not in result:
        result.append("--no-voting")
    return result


def feature(operations, failed_slot, revision):
    require(type(failed_slot) is int and 0 < failed_slot <= 2 ** 64 - 1, "invalid failed L1 slot")
    operations.execute(["git", "-C", Path(__file__).resolve().parents[2], "merge-base", "--is-ancestor", REVIEWED_NATIVE_FIX, revision])
    observed = operations.upstream.account(FEATURE)
    require(observed is not None and observed.get("owner") == FEATURE_OWNER and observed.get("executable") is False
            and observed.get("raw") == b"\x01" + failed_slot.to_bytes(8, "little"),
            "failed slot does not match the supported ProgramData feature activation")


def verify_service_files(operations, base, service, dropin):
    def property_value(name):
        return operations.execute(["systemctl", "show", "--property=" + name, "--value", service]).strip()
    original = private_json(base / "control/reservation.json")["original"]
    require(property_value("NeedDaemonReload") == "no" and property_value("DynamicUser") == "no"
            and property_value("Restart") == "no", "recovery service properties differ")
    username = property_value("User") or "root"
    account = pwd.getpwuid(int(username)) if username.isdigit() else pwd.getpwnam(username)
    group = property_value("Group")
    gid = int(group) if group.isdigit() else grp.getgrnam(group).gr_gid if group else account.pw_gid
    require(account.pw_uid == original["uid"] and gid == original["gid"], "recovery service owner differs")
    fragment = property_value("FragmentPath")
    overrides = property_value("DropInPaths").split()
    require(str(dropin) in overrides and set(overrides) <= set(original["retained_paths"]) | {str(dropin)},
            "unknown recovery service override")
    require(fragment in original["retained_paths"], "recovery service fragment differs")
    retained = private_json(base / "control/retained.json")
    service_files = [fragment, *[name for name in overrides if name != str(dropin)]]
    observed = operations.inspect(service_files)
    for name in service_files:
        require(name in retained and isinstance(observed[name].get("sha256"), str)
                and all(observed[name].get(field) == retained[name].get(field)
                        for field in ("sha256", "mode", "uid", "gid")), "retained service configuration changed")
    return original


def offline_inventory(operations, state_root, service, dropin):
    def property_value(name):
        return operations.execute(["systemctl", "show", "--property=" + name, "--value", service]).strip()
    require(property_value("ActiveState") in ("inactive", "failed") and property_value("MainPID") == "0",
            "L1 recovery requires a stopped validator")
    require(property_value("NeedDaemonReload") == "no" and property_value("DynamicUser") == "no"
            and property_value("Restart") == "no", "recovery service properties differ")
    effective = property_value("ExecStart")
    matches = set(re.findall(re.escape(str(state_root)) + r"/([0-9a-f]{40})/runtime/bin/agave-validator", effective))
    require(len(matches) == 1, "recovery requires an unambiguous original managed service")
    base = Path(state_root) / matches.pop()
    checked_directory(base)
    checked_directory(base / "control", private=True)
    activation = private_json(base / "control/activation.json")
    original = verify_service_files(operations, base, service, dropin)
    expected = activation["arguments"]
    require(expected[0] == str(base / "runtime/bin/agave-validator") and "path=/usr/bin/env" in effective
            and all(value in effective for value in [*expected, *activation["state_environment"].values()]),
            "effective original recovery service differs")
    return {"arguments": expected, "state_environment": activation["state_environment"],
            "binary_sha256": activation["artifacts"]["validator"], "uid": original["uid"], "gid": original["gid"],
            "retained_paths": original["retained_paths"]}


def retained_snapshot(base_control, base_plan, descriptor, snapshot):
    config = private_json(base_control / "sdk-config.json")
    binding = private_json(base_control / "sdk-smoke/binding.json")
    identity = snapshot["identity"]
    require(config.get("version") == binding.get("version") == 2
            and config.get("source_revision") == binding.get("source_revision") == base_plan["source_revision"]
            and config.get("sdk_revision") == binding.get("sdk_revision") == SDK_REVISION
            and config.get("profile") == base_plan["profile"] and config.get("genesis") == binding.get("genesis") == DEVNET
            and config.get("smoke_directory") == str(base_control / "sdk-smoke"), "retained recovery SDK binding differs")
    for name in ("portal", "token_bridge", "validator"):
        require(config.get(name) == binding.get(name) == identity[name], "retained recovery program identity differs")
    for name in ("l1_rpc", "er_rpc"):
        require(config.get(name) == binding.get(name) == getattr(descriptor, name), "retained recovery RPC binding differs")
    require(identity.get("nonce") == "1" and identity.get("authority") == binding.get("authority") == base_plan["funder"]
            and all(identity.get(name) == binding.get(name) for name in
                    ("context", "grid_id", "ttl_slots", "fee_cap", "settlement_interval_slots")),
            "retained recovery session identity differs")
    require(isinstance(config.get("er_genesis"), str) and config["er_genesis"] == binding.get("er_genesis"),
            "retained recovery ER genesis differs")
    return {**snapshot, "er_genesis": config["er_genesis"]}


def verify_empty_paths(runtime):
    known = paths(runtime)
    for directory in known:
        require(directory.is_dir() and not directory.is_symlink(), "recovery L1 directory differs")
        require(all(entry in known for entry in directory.iterdir()), "recovery L1 directory contains unrecognized pre-start data")


def verify_snapshot(operations, failed_slot):
    value = operations.l1.call("getHighestSnapshotSlot")
    require(isinstance(value, dict), "recovery snapshot response differs")
    slots = [value.get("full"), value.get("incremental")]
    require(type(slots[0]) is int and slots[0] >= 0 and all(slot is None or type(slot) is int and slot >= 0 for slot in slots)
            and max(slot for slot in slots if slot is not None) > failed_slot,
            "recovery snapshot must be newer than the failed L1 slot")

"""Sonic: Durable native deployment metadata; no transaction or service mutations."""

import hashlib
import json
import os
from pathlib import Path
import re
import stat
import tempfile

DEVNET = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG"
SDK_REVISION = "beb895f5178bdb21fbfe13786981b19241d3d8c4"
ARTIFACTS = {
    "validator": "bin/agave-validator",
    "cli": "bin/solana",
    "geyser_events": "lib/libnorthstar_geyser_events.so",
    "portal": "programs/northstar_portal.so",
    "token_bridge": "programs/northstar_token_bridge.so",
}


def require(condition, message):
    if not condition:
        raise ValueError(message)


L1_RPC_INDEXES = ("program-id", "spl-token-owner", "spl-token-mint")


def l1_rpc_arguments(arguments, profile=None):
    require(profile in (None, "full"), "unknown L1 RPC profile")
    if profile is None:
        return list(arguments)
    result, index = [], 0
    while index < len(arguments):
        argument = arguments[index]
        flag, separator, value = argument.partition("=")
        require(flag not in ("--account-index-include-key", "--account-index-exclude-key"),
                "full L1 RPC requires unrestricted index keys")
        if flag != "--account-index":
            result.append(argument)
            index += 1
            continue
        selected = [value] if separator else []
        index += 1
        while index < len(arguments) and not arguments[index].startswith("-"):
            selected.append(arguments[index])
            index += 1
        selected = [part for value in selected for part in value.split(",")]
        require(selected and all(value in L1_RPC_INDEXES for value in selected), "unknown or empty native account index")
    return result + ["--account-index", *L1_RPC_INDEXES]


def release_mode_arguments(recovery_slot="", replace_context="", l1_rpc_profile=""):
    require(all(isinstance(value, str) for value in (recovery_slot, replace_context, l1_rpc_profile)), "native mode selectors must be strings")
    require(not (recovery_slot and replace_context), "fresh activation and L1 recovery selectors are mutually exclusive")
    require(l1_rpc_profile in ("", "full"), "unknown L1 RPC profile selector")
    require(not l1_rpc_profile or not (recovery_slot or replace_context), "L1 RPC profile requires an ordinary native upgrade")
    if replace_context:
        require(re.fullmatch("[0-9a-f]{64}", replace_context), "invalid fresh context selector")
        return ["--replace-context", replace_context, "--abandon-previous-state"]
    result = ["--upgrade-existing"]
    if recovery_slot:
        require(re.fullmatch("[1-9][0-9]{0,19}", recovery_slot) and int(recovery_slot) < 1 << 64,
                "invalid L1 recovery slot selector")
        result.extend(["--recover-failed-l1-slot", recovery_slot])
    if l1_rpc_profile:
        result.extend(["--l1-rpc-profile", l1_rpc_profile])
    return result


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()


def private_json(filename):
    descriptor = os.open(filename, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(descriptor)
        allowed = {os.getuid()}
        if os.getuid() == 0 and os.environ.get("SUDO_UID", "").isdigit():
            allowed.add(int(os.environ["SUDO_UID"]))
        require(stat.S_ISREG(info.st_mode) and not info.st_mode & 0o077 and info.st_uid in allowed
                and info.st_size <= 16 * 1024 * 1024, "control input is not a private regular file")
        with os.fdopen(os.dup(descriptor), "rb") as stream:
            return json.load(stream)
    finally:
        os.close(descriptor)


def atomic_json(filename, value):
    filename = Path(filename)
    if os.path.lexists(filename):
        private_json(filename)
    descriptor, temporary = tempfile.mkstemp(prefix=filename.name + ".", suffix=".tmp", dir=filename.parent)
    try:
        with os.fdopen(descriptor, "wb") as output:
            output.write(canonical(value) + b"\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, filename)
        directory = os.open(filename.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    except BaseException:
        raise


def sync_paths(paths):
    for configured in paths:
        filename = Path(configured)
        info = filename.lstat()
        require(stat.S_ISDIR(info.st_mode) or stat.S_ISREG(info.st_mode), "cannot flush a nonregular deployment path")
        flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK
        if stat.S_ISDIR(info.st_mode):
            flags |= os.O_DIRECTORY
        descriptor = os.open(filename, flags)
        try:
            os.fsync(descriptor)
        finally:
            os.close(descriptor)


def append_private(filename, data):
    descriptor = os.open(filename, os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
    try:
        info = os.fstat(descriptor)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077,
                "diagnostic log is not a private regular file")
        with os.fdopen(os.dup(descriptor), "ab") as output:
            output.write(data)
            output.flush()
            os.fsync(output.fileno())
    finally:
        os.close(descriptor)


def immutable_json(filename, value):
    if os.path.lexists(filename):
        require(private_json(filename) == value, "established deployment metadata changed")
    else:
        atomic_json(filename, value)


def file_hash(filename):
    descriptor = os.open(filename, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(descriptor)
        require(stat.S_ISREG(info.st_mode) and info.st_size <= 2 * 1024**3, "retained input is not a bounded regular file")
        result = hashlib.sha256()
        while block := os.read(descriptor, 1024 * 1024):
            result.update(block)
        return result.hexdigest()
    finally:
        os.close(descriptor)


def verify_artifacts(directory, revision, *, retained=False):
    directory = Path(directory)
    manifest = private_json(directory / "manifest.json")
    require(manifest.get("version") == 2 and manifest.get("protocol") == "continuity-v2"
            and manifest.get("source_revision") == revision and re.fullmatch("[0-9a-f]{40}", revision)
            and manifest.get("platform_tools") == "v1.56" and manifest.get("arch") == "v0"
            and manifest.get("activation_performed") is False and manifest.get("hosted_qualified") is False
            and manifest.get("proof_acceptance_qualified") is False,
            "artifact manifest identity differs")
    roles = dict(ARTIFACTS)
    require(isinstance(manifest.get("sha256"), dict), "incorrect native artifact roles")
    if retained and set(manifest["sha256"]) == set(roles) - {"geyser_events"}:
        del roles["geyser_events"]
    require(set(manifest["sha256"]) == set(roles), "incorrect native artifact roles")
    for role, relative in roles.items():
        require(file_hash(directory / relative) == manifest["sha256"][role], "native artifact bytes changed")
        with (directory / relative).open("rb") as source:
            require(source.read(4) == b"\x7fELF", "native artifact is not ELF")
    return manifest


def checked_directory(directory, private=False):
    directory = Path(directory)
    info = directory.lstat()
    require(directory.is_absolute() and stat.S_ISDIR(info.st_mode) and not stat.S_ISLNK(info.st_mode)
            and info.st_uid == os.getuid() and not info.st_mode & (0o077 if private else 0o022),
            "deployment directory ownership or permissions differ")


def flag_value(arguments, name):
    matches = []
    for position, argument in enumerate(arguments):
        if argument == name:
            require(position + 1 < len(arguments), "service flag value is missing")
            matches.append(arguments[position + 1])
        elif argument.startswith(name + "="):
            matches.append(argument[len(name) + 1:])
    require(len(matches) == 1 and matches[0], "service flag is missing or duplicated: " + name)
    return matches[0]


def geyser_plugin_flag(event_source):
    require(event_source in ("l1", "er"), "unknown event source")
    return "--geyser-plugin-config" if event_source == "l1" else "--er-geyser-plugin-config"


def geyser_event_source(specification):
    # Unmarked retained records predate L1 delivery and must keep their original ER hashes and arguments.
    source = specification.get("event_source", "er")
    geyser_plugin_flag(source)
    return source


def geyser_events_config(library, event_source="er"):
    require(Path(library).is_absolute(), "event plugin path must be absolute")
    geyser_plugin_flag(event_source)
    return {"libpath": str(library), "name": event_source + "-events", "listen_addr": "0.0.0.0:9101", "commitment": "confirmed"}


def service_arguments(original, executable, portal, geyser_config=None, event_source="er"):
    require(isinstance(original, list) and original and all(isinstance(value, str) and value for value in original), "invalid service argument vector")
    flag_value(original, "--portal")
    result = list(original)
    result[0] = str(executable)
    for position, argument in enumerate(result):
        if argument == "--portal":
            result[position + 1] = portal
        elif argument.startswith("--portal="):
            result[position] = "--portal=" + portal
    if geyser_config is not None:
        require(Path(geyser_config).is_absolute(), "event plugin config path must be absolute")
        require(not any(argument == "--er-geyser-plugin-config" or argument.startswith("--er-geyser-plugin-config=")
                        for argument in original), "existing ER Geyser configuration requires explicit deployment review")
        result.extend([geyser_plugin_flag(event_source), str(geyser_config)])
    return result


def upgrade_arguments(original, executable, geyser_config, previous_config=None, *, previous_source="er", event_source="er"):
    require(isinstance(original, list) and original and all(isinstance(value, str) and value for value in original), "invalid upgrade argument vector")
    flag_value(original, "--portal")
    source_flag = geyser_plugin_flag(previous_source)
    source_flags = (source_flag, "--accountsdb-plugin-config") if previous_source == "l1" else (source_flag,)
    target_flag = geyser_plugin_flag(event_source)
    result = [str(executable)]
    found = []
    position = 1
    while position < len(original):
        argument = original[position]
        matched = next((flag for flag in source_flags if argument == flag or argument.startswith(flag + "=")), None)
        if matched:
            width = 2 if argument == matched else 1
            require(width == 1 or position + 1 < len(original), "event plugin flag value is missing")
            value = original[position + 1] if width == 2 else argument.split("=", 1)[1]
            require(value and not value.startswith("--"), "event plugin flag value is missing")
            if previous_source == "er" or value == str(previous_config):
                found.append(value)
            else:
                result.extend(original[position:position + width])
            position += width
        else:
            result.append(argument)
            position += 1
    require(found == ([str(previous_config)] if previous_config else []), "unknown or duplicated retained " + previous_source.upper() + " plugin configuration")
    require(Path(executable).is_absolute() and Path(geyser_config).is_absolute(), "upgrade native paths must be absolute")
    return [*result, target_flag, str(geyser_config)]


def systemd_argument(value):
    require(isinstance(value, str) and value and not any(ord(character) < 32 for character in value), "service argument contains a control character")
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%").replace("$", "$$") + '"'


def service_dropin(arguments, checkpoint_directory, unsettled_directory):
    for directory in (checkpoint_directory, unsettled_directory):
        require(Path(directory).is_absolute() and not any(character.isspace() for character in str(directory)), "native state directory must be an absolute unambiguous path")
    command = ["/usr/bin/env", "NORTHSTAR_CHECKPOINT_PLAN_DIR=" + str(checkpoint_directory),
               "NORTHSTAR_UNSETTLED_STATE_DIR=" + str(unsettled_directory), *arguments]
    return "[Service]\nExecStart=\nExecStart=" + " ".join(systemd_argument(value) for value in command) + "\n"


def inspect_paths(paths):
    result = {}
    for configured in paths:
        filename = Path(configured)
        require(filename.is_absolute(), "inspection path must be absolute")
        if not os.path.lexists(filename):
            result[str(filename)] = {"absent": True}
            continue
        info = filename.lstat()
        require(stat.S_ISDIR(info.st_mode) or stat.S_ISREG(info.st_mode), "inspection input is not a regular file/directory")
        value = {"mode": stat.S_IMODE(info.st_mode), "uid": info.st_uid, "gid": info.st_gid}
        if stat.S_ISREG(info.st_mode):
            value["sha256"] = file_hash(filename)
        else:
            value["directory"] = True
        result[str(filename)] = value
    return result


def retained_fingerprint(paths):
    result = {}
    count = 0
    for configured in paths:
        root = Path(configured)
        require(root.is_absolute(), "retained path must be absolute")
        if not os.path.lexists(root):
            result[str(root)] = {"absent": True}
            continue
        pending = [root]
        while pending:
            current = pending.pop()
            info = current.lstat()
            require(not stat.S_ISLNK(info.st_mode), "retained state contains a symlink")
            require(stat.S_ISREG(info.st_mode) or stat.S_ISDIR(info.st_mode), "retained state is not a regular file/directory")
            count += 1
            require(count <= 20_000, "retained state exceeds inventory bound")
            metadata = {"mode": stat.S_IMODE(info.st_mode), "uid": info.st_uid, "gid": info.st_gid}
            if stat.S_ISDIR(info.st_mode):
                metadata["directory"] = True
                pending.extend(sorted(current.iterdir()))
            else:
                metadata.update(size=info.st_size, sha256=file_hash(current))
            result[str(current)] = metadata
    return result

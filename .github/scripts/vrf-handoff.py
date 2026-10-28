#!/usr/bin/env python3
"""Sonic: Fence native deployment and publish a host-bound oracle handoff."""

import argparse
import base64
from contextlib import contextmanager
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import urllib.request

ROOT = Path("/var/lib/northstar-deployment")
LIB = Path("/usr/local/lib/northstar-deployment")
DEVNET = "EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG"
REPOSITORY = "mirrorworld-universe/northstar"
SESSION_TAG = bytes([243, 81, 72, 115, 214, 188, 72, 144])
BASE58 = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def public_key(value):
    require(isinstance(value, str) and 32 <= len(value) <= 44, "invalid public key")
    number = 0
    for character in value:
        require(character in BASE58, "invalid public key")
        number = number * 58 + BASE58.index(character)
    raw = bytes(len(value) - len(value.lstrip("1"))) + number.to_bytes(
        (number.bit_length() + 7) // 8, "big"
    )
    require(len(raw) == 32, "invalid public key width")
    return raw


def context(revision, run_id, attempt, runner, enabled):
    require(re.fullmatch(r"[a-f0-9]{40}", revision) is not None, "invalid revision")
    require(
        all(re.fullmatch(r"[1-9][0-9]{0,19}", value) for value in (run_id, attempt)),
        "invalid workflow identity",
    )
    require(0 < len(runner) <= 128 and runner.isprintable(), "invalid runner identity")
    require(type(enabled) is bool, "invalid enablement")
    return {
        "repository": REPOSITORY,
        "revision": revision,
        "run_id": run_id,
        "run_attempt": attempt,
        "runner_name": runner,
        "enabled": enabled,
    }


def load(path):
    descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    with os.fdopen(descriptor, "rb") as stream:
        info = os.fstat(stream.fileno())
        require(
            stat.S_ISREG(info.st_mode)
            and info.st_uid == 0
            and info.st_mode & 0o022 == 0,
            "handoff must be a root-owned regular file",
        )
        raw = stream.read(65537)
    require(len(raw) <= 65536, "handoff is oversized")
    value = json.loads(raw)
    require(
        type(value["version"]) is int and value["version"] == 1, "unsupported handoff"
    )
    require(
        value["state"] in {"idle", "deploying", "ready", "failed"},
        "invalid handoff state",
    )
    fields = {"version", "state"}
    if value["state"] != "idle":
        fields.add("native")
    if value["state"] == "ready":
        fields.add("observed")
        require(isinstance(value.get("observed"), dict), "invalid host observation")
    require(set(value) == fields, "invalid handoff fields")
    if value["state"] != "idle":
        native = value["native"]
        require(
            native
            == context(
                native["revision"],
                native["run_id"],
                native["run_attempt"],
                native["runner_name"],
                native["enabled"],
            ),
            "invalid native identity",
        )
    return value


def save(root, value):
    with tempfile.NamedTemporaryFile(mode="w", dir=root, delete=False) as stream:
        os.fchmod(stream.fileno(), 0o644)
        json.dump(value, stream, sort_keys=True)
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())
    os.replace(stream.name, root / "current.json")
    directory = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


@contextmanager
def locked(root=ROOT):
    created = False
    try:
        root.mkdir(mode=0o755)
        created = True
    except FileExistsError:
        pass
    descriptor = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        info = os.fstat(descriptor)
        require(
            info.st_uid == 0 and info.st_mode & 0o022 == 0, "unsafe handoff directory"
        )
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if created:
            save(root, {"version": 1, "state": "idle"})
            parent = os.open(root.parent, os.O_RDONLY | os.O_DIRECTORY)
            try:
                os.fsync(parent)
            finally:
                os.close(parent)
        else:
            require((root / "current.json").exists(), "established handoff is missing")
        yield
    finally:
        os.close(descriptor)


def command(*arguments):
    return subprocess.check_output(arguments, text=True, timeout=30).strip()


def runtime():
    pid = int(
        command(
            "systemctl",
            "show",
            "--property=MainPID",
            "--value",
            "northstar-validator.service",
        )
    )
    require(pid > 0, "validator is not running")
    process = Path("/proc") / str(pid)
    start = process.joinpath("stat").read_text().rpartition(")")[2].split()[19]
    digest = hashlib.sha256()
    with process.joinpath("exe").open("rb") as executable:
        for block in iter(lambda: executable.read(1024 * 1024), b""):
            digest.update(block)
    return {
        "pid": pid,
        "start_ticks": start,
        "boot_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip(),
        "sha256": digest.hexdigest(),
    }


def rpc(port, method, params=None):
    request = urllib.request.Request(
        f"http://127.0.0.1:{port}/",
        data=json.dumps(
            {"jsonrpc": "2.0", "id": 1, "method": method, "params": params or []}
        ).encode(),
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(request, timeout=20) as response:
        raw = response.read(4 * 1024 * 1024 + 1)
    require(len(raw) <= 4 * 1024 * 1024, "RPC result is oversized")
    value = json.loads(raw)
    require(
        value.get("jsonrpc") == "2.0"
        and type(value.get("id")) is int
        and value["id"] == 1
        and "error" not in value
        and "result" in value,
        "invalid RPC result",
    )
    return value["result"]


def observe():
    before = runtime()
    start = command(
        "systemctl",
        "show",
        "--property=ExecStart",
        "--value",
        "northstar-validator.service",
    )
    portals = re.findall(
        r"(?:^|\s)--portal(?:=|\s+)([1-9A-HJ-NP-Za-km-z]{32,44})(?=\s|;|$)", start
    )
    require(len(portals) == 1, "cannot resolve exactly one Portal")
    portal = portals[0]
    public_key(portal)
    require(rpc(8899, "getGenesisHash") == DEVNET, "L1 is not Solana devnet")
    session = rpc(8910, "getSessionPda")
    validator = rpc(8899, "getIdentity")["identity"]
    public_key(session)
    account = rpc(
        8899,
        "getAccountInfo",
        [session, {"encoding": "base64", "commitment": "confirmed"}],
    )["value"]
    require(
        account is not None
        and account["owner"] == portal
        and account["executable"] is False,
        "invalid session owner",
    )
    require(account["data"][1] == "base64", "invalid session encoding")
    raw = base64.b64decode(account["data"][0], validate=True)
    require(
        len(raw) == 226
        and raw[:8] == SESSION_TAG
        and raw[88:120] == public_key(validator),
        "invalid session identity",
    )
    er_genesis = rpc(8910, "getGenesisHash")
    require(
        isinstance(er_genesis, str) and 0 < len(er_genesis) <= 128, "invalid ER genesis"
    )
    require(
        runtime() == before and rpc(8910, "getSessionPda") == session,
        "validator changed during observation",
    )
    # Settlement progress starts at byte 120; it is not an incarnation identifier.
    return {
        "portal": portal,
        "session": session,
        "validator": validator,
        "l1_genesis": DEVNET,
        "er_genesis": er_genesis,
        "session_identity_sha256": hashlib.sha256(raw[8:120]).hexdigest(),
        "process": before,
    }


def install_observer():
    LIB.mkdir(mode=0o755, parents=True, exist_ok=True)
    info = LIB.lstat()
    require(
        stat.S_ISDIR(info.st_mode) and info.st_uid == 0 and info.st_mode & 0o022 == 0,
        "unsafe observer directory",
    )
    for name in ("vrf-handoff.py", "stop-vrf-service.py"):
        raw = Path(__file__).with_name(name).read_bytes()
        with tempfile.NamedTemporaryFile(dir=LIB, delete=False) as stream:
            os.fchmod(stream.fileno(), 0o755)
            stream.write(raw)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(stream.name, LIB / name)
    directory = os.open(LIB, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def quiesce():
    spec = importlib.util.spec_from_file_location(
        "stop_vrf", Path(__file__).with_name("stop-vrf-service.py")
    )
    module = importlib.util.module_from_spec(spec)
    previous = sys.dont_write_bytecode
    sys.dont_write_bytecode = True
    try:
        spec.loader.exec_module(module)
    finally:
        sys.dont_write_bytecode = previous
    module.stop()


def transition(previous, operation, native, stop=quiesce, inspect=observe):
    if operation == "begin":
        require(
            previous["state"] in {"idle", "ready", "failed"},
            "unfinished native deployment requires reconciliation",
        )
        require(previous.get("native") != native, "native generation cannot be reused")
        stop()
        return {"version": 1, "state": "deploying", "native": native}
    if operation == "fail":
        if previous.get("native") != native or previous["state"] != "deploying":
            return previous
        return {"version": 1, "state": "failed", "native": native}
    require(
        operation == "complete"
        and previous["state"] == "deploying"
        and previous["native"] == native,
        "stale native completion",
    )
    return {"version": 1, "state": "ready", "native": native, "observed": inspect()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("operation", choices=["begin", "complete", "fail", "observe"])
    parser.add_argument("--revision")
    parser.add_argument("--run-id")
    parser.add_argument("--run-attempt")
    parser.add_argument("--runner")
    parser.add_argument("--enabled", choices=["true", "false"])
    args = parser.parse_args()
    if args.operation == "observe":
        print(json.dumps(observe()))
        return
    native = context(
        args.revision or "",
        args.run_id or "",
        args.run_attempt or "",
        args.runner or "",
        args.enabled == "true",
    )
    require(args.enabled is not None, "enablement is required")
    if args.operation == "fail" and not ROOT.exists():
        return
    with locked():
        path = ROOT / "current.json"
        previous = load(path)
        updated = transition(previous, args.operation, native)
        if args.operation == "begin":
            install_observer()
        if updated != previous:
            save(ROOT, updated)


if __name__ == "__main__":
    main()

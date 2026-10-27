#!/usr/bin/env python3
"""Stop the managed oracle before CD mutates programs or restarts the ER."""

import fcntl
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile

UNIT = "northstar-vrf-oracle.service"
CONFIG = Path("/etc/northstar-vrf-oracle/config.json")
STATE = Path("/var/lib/northstar-vrf-oracle/journals")
DEPLOYMENT = Path("/solana/northstar/vrf-service")


def load(path, limit):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise RuntimeError("managed VRF input is not a regular file")
        raw = stream.read(limit + 1)
    if len(raw) > limit:
        raise RuntimeError("managed VRF input exceeds its size bound")
    return json.loads(raw)


def validate_state(config_path):
    try:
        result = subprocess.run(
            [
                "/usr/local/lib/northstar-vrf-oracle/vrf-oracle",
                "validate-state",
                "--config",
                str(config_path),
            ],
            check=True,
            capture_output=True,
            text=True,
            timeout=60,
        )
        return json.loads(result.stdout)
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        raise RuntimeError("managed VRF snapshot validation failed") from error


def inspect_journal(config_path=CONFIG, state_root=STATE):
    config = load(config_path, 65_536)
    directory = Path(config["state_dir"])
    if (
        not re.fullmatch("[a-f0-9]{64}", directory.name)
        or directory.parent != state_root
    ):
        raise RuntimeError("managed VRF journal path is not deployment-bound")
    snapshot = validate_state(config_path)
    if (
        snapshot["version"] != 1
        or snapshot["binding"] != directory.name
        or not isinstance(snapshot["statuses"], list)
    ):
        raise RuntimeError("managed VRF journal identity changed")
    allowed = {
        "pending",
        "retryable",
        "unresolved",
        "confirmed",
        "expired",
        "exhausted",
    }
    statuses = snapshot["statuses"]
    if any(status not in allowed for status in statuses):
        raise RuntimeError("unknown managed VRF lifecycle state")
    if any(status in {"pending", "retryable", "unresolved"} for status in statuses):
        raise RuntimeError(
            "VRF has pending or unresolved intents; reconcile before restarting the ER"
        )


def audit_controls():
    index = DEPLOYMENT / "control-state.json"
    markers = [
        CONFIG,
        STATE,
        DEPLOYMENT / "keys",
        DEPLOYMENT / "keys-established.json",
        DEPLOYMENT / "deployments",
    ]
    if not index.exists():
        if any(os.path.lexists(path) for path in markers):
            raise RuntimeError("established VRF control index is missing")
        return
    try:
        result = subprocess.run(
            [
                "/usr/local/lib/northstar-vrf-oracle/devnet-control",
                "--allow-development-keys",
                "inspect-controls",
                "--index",
                str(index),
            ],
            check=True,
            capture_output=True,
            text=True,
            timeout=120,
        )
        checked = json.loads(result.stdout)
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        raise RuntimeError("VRF control history validation failed") from error
    if (
        checked["version"] != 1
        or checked["pending_programs"] != []
        or not isinstance(checked["records"], dict)
    ):
        raise RuntimeError("invalid VRF control index")
    owner = index.stat(follow_symlinks=False)
    with tempfile.NamedTemporaryFile(
        mode="w", dir=DEPLOYMENT, delete=False
    ) as temporary:
        os.fchown(temporary.fileno(), owner.st_uid, owner.st_gid)
        json.dump(checked, temporary, indent=2)
        temporary.write("\n")
        temporary.flush()
        os.fsync(temporary.fileno())
    os.replace(temporary.name, index)
    directory = os.open(DEPLOYMENT, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)
    if any(record["receipt"] is None for record in checked["records"].values()):
        raise RuntimeError(
            "VRF has unresolved control intents; reconcile before restarting the ER"
        )


def _stop(run, inspect):
    loaded = run(
        ["systemctl", "show", "--property=LoadState", "--value", UNIT],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if loaded == "not-found":
        if os.path.lexists(CONFIG) or os.path.lexists(STATE):
            inspect()
        audit_controls()
        return
    if loaded != "loaded":
        raise RuntimeError("managed VRF unit cannot be inspected")
    active = (
        run(["systemctl", "is-active", "--quiet", UNIT], check=False).returncode == 0
    )
    run(["systemctl", "stop", UNIT], check=True)
    try:
        inspect()
        audit_controls()
    except Exception:
        if active:
            run(["systemctl", "start", UNIT], check=True)
        raise
    run(["systemctl", "disable", UNIT], check=True)


def stop(run=subprocess.run, inspect=inspect_journal):
    if not DEPLOYMENT.exists():
        return _stop(run, inspect)
    directory = os.open(DEPLOYMENT, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        fcntl.flock(directory, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return _stop(run, inspect)
    finally:
        os.close(directory)


if __name__ == "__main__":
    stop()

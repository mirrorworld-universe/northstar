#!/usr/bin/env python3
"""Stop the managed oracle before CD mutates programs or restarts the ER."""

import json
import os
from pathlib import Path
import re
import stat
import subprocess

UNIT = "northstar-vrf-oracle.service"
CONFIG = Path("/etc/northstar-vrf-oracle/config.json")
STATE = Path("/var/lib/northstar-vrf-oracle/journals")


def load(path, limit):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, "rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise RuntimeError("managed VRF input is not a regular file")
        raw = stream.read(limit + 1)
    if len(raw) > limit:
        raise RuntimeError("managed VRF input exceeds its size bound")
    return json.loads(raw)


def inspect_journal(config_path=CONFIG, state_root=STATE):
    config = load(config_path, 65_536)
    directory = Path(config["state_dir"])
    if (
        not re.fullmatch("[a-f0-9]{64}", directory.name)
        or directory.parent != state_root
    ):
        raise RuntimeError("managed VRF journal path is not deployment-bound")
    snapshot = load(directory / "public-state.json", 16 * 1024 * 1024)
    if snapshot["binding"] != directory.name or not isinstance(
        snapshot["records"], dict
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
    statuses = [record["status"] for record in snapshot["records"].values()]
    if any(status not in allowed for status in statuses):
        raise RuntimeError("unknown managed VRF lifecycle state")
    if any(status in {"pending", "retryable", "unresolved"} for status in statuses):
        raise RuntimeError(
            "VRF has pending or unresolved intents; reconcile before restarting the ER"
        )


def stop(run=subprocess.run, inspect=inspect_journal):
    loaded = run(
        ["systemctl", "show", "--property=LoadState", "--value", UNIT],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    if loaded == "not-found":
        return
    if loaded != "loaded":
        raise RuntimeError("managed VRF unit cannot be inspected")
    active = (
        run(["systemctl", "is-active", "--quiet", UNIT], check=False).returncode == 0
    )
    run(["systemctl", "stop", UNIT], check=True)
    try:
        inspect()
    except Exception:
        if active:
            run(["systemctl", "start", UNIT], check=True)
        raise
    run(["systemctl", "disable", UNIT], check=True)


if __name__ == "__main__":
    stop()

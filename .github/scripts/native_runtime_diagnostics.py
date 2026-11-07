"""Sonic: Public, read-only diagnostics; never expose command lines or journal text."""
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess

from continuity_devnet import DROPIN, SERVICE, public_bytes
from continuity_rollout_state import ARTIFACTS, DEVNET, SDK_REVISION, canonical, file_hash, flag_value, private_json, service_dropin

STATE_ROOT = Path("/solana/northstar/continuity-v2")
LOCK = Path("/solana/northstar/continuity-v2-deploy.lock")
PUBLIC_PROPERTIES = (
    "LoadState", "ActiveState", "SubState", "MainPID", "Result", "ExecMainCode", "ExecMainStatus",
    "NRestarts", "UnitFileState", "ExecMainStartTimestamp", "ExecMainExitTimestamp", "InactiveEnterTimestamp",
    "User", "Group", "DynamicUser", "Restart", "MemoryMax", "OOMPolicy",
)
MARKERS = {
    "checkpoint_lineage_recovery_failed": "checkpoint_lineage_recovery_failed",
    "checkpoint_lineage_mismatch": "checkpoint_lineage_mismatch",
    "quarantin": "checkpoint_quarantine",
    "panicked at": "runtime_panic",
    "permission denied": "permission_denied",
    "address already in use": "listener_in_use",
    "no space left on device": "disk_full",
    "insufficient disk space": "disk_full",
    "os error 28": "disk_full",
    "too many open files": "file_descriptor_limit",
    "failed to open ledger": "ledger_open_failure",
    "bank hash mismatch": "bank_hash_mismatch",
    "snapshot hash mismatch": "snapshot_hash_mismatch",
    "out of memory": "memory_limit",
    "oom-kill": "memory_limit",
    "failed to load er geyser plugins": "event_plugin_load_failure",
    "error while loading shared libraries": "native_library_load_failure",
}


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def execute(arguments):
    return subprocess.run(arguments, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30, check=False)


def marker_counts(messages):
    counts = {}
    for message in messages:
        if not isinstance(message, str) or "program log:" in message.lower():
            continue
        for marker in {value for text, value in MARKERS.items() if text in message.lower()}:
            counts[marker] = counts.get(marker, 0) + 1
    return counts


def disk_summary(directory):
    try:
        info = os.statvfs(directory)
        return {"status": "observed", "total_bytes": info.f_blocks * info.f_frsize,
                "available_bytes": info.f_bavail * info.f_frsize, "free_bytes": info.f_bfree * info.f_frsize,
                "available_inodes": info.f_favail}
    except OSError:
        return {"status": "unavailable"}


def file_log_summary(path):
    if (not path.is_absolute() or any(part in ("..", "control", "keys", ".ssh", ".config", "checkpoints", "unsettled", "rocksdb", "accounts", "accounts_index", "snapshots") for part in path.parts)
            or not any(suffix in path.name for suffix in (".log", ".out", ".err"))
            or not any(name in path.name.lower() for name in ("validator", "agave", "solana", "northstar", "native", "stderr", "stdout"))):
        return {"status": "not_an_approved_log_sink"}
    descriptor = None
    try:
        flags = os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | getattr(os, "O_NOATIME", 0)
        rotated = False
        try:
            descriptor = os.open(path, flags)
        except OSError as error:
            if error.errno != errno.ELOOP:
                raise
            target = Path(os.readlink(path))
            target = target if target.is_absolute() else path.parent / target
            if (target.parent != path.parent or not target.name.startswith(path.stem)
                    or not any(suffix in target.name for suffix in (".log", ".out", ".err"))):
                return {"status": "unapproved_log_alias"}
            descriptor = os.open(target, flags)
            rotated = True
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode):
            return {"status": "not_a_regular_log"}
        os.lseek(descriptor, max(0, info.st_size - 64 * 1024), os.SEEK_SET)
        tail = os.read(descriptor, 64 * 1024)
        return {"status": "classified", "bytes_examined": len(tail), "modified_at_unix": int(info.st_mtime), "rotated_sibling": rotated,
                "marker_counts": marker_counts(tail.decode(errors="replace").splitlines()),
                "raw_messages_included": False, "markers_are_not_a_root_cause_diagnosis": True}
    except OSError:
        return {"status": "unavailable"}
    finally:
        if descriptor is not None:
            os.close(descriptor)


def operational_details(values, root, records):
    report = {"filesystems": {"state_root": disk_summary(root)}, "file_logs": []}
    if not (records.get("recorded_plan_identity_matches") and records.get("managed_dropin_matches_original")
            and records.get("original_binary_matches") and len(records.get("original_revisions", [])) == 1):
        return report
    try:
        activation = private_json(root / records["original_revisions"][0] / "control/activation.json")
        arguments = activation["arguments"]
        working = Path(values.get("WorkingDirectory", ""))
        try:
            ledger = Path(flag_value(arguments, "--ledger"))
            if not ledger.is_absolute() and working.is_absolute():
                ledger = working / ledger
            if ledger.is_absolute():
                report["filesystems"]["ledger"] = disk_summary(ledger)
        except ValueError:
            pass
        candidates = []
        flags = [flag for flag in ("--log", "-o") if any(argument == flag or argument.startswith(flag + "=") for argument in arguments)]
        modes = {"inherit", "null", "tty", "journal", "journal-or-kmsg", "kmsg", "socket", "file", "append", "truncate", "fd"}
        report["logger_configuration"] = {"argument_flags": flags, "working_directory_set": working.is_absolute(),
            **{name: values.get(name, "").split(":", 1)[0] if values.get(name, "").split(":", 1)[0] in modes else "unknown"
               for name in ("StandardOutput", "StandardError")}}
        try:
            if len(flags) > 1:
                report["logger_configuration"]["argument_mode"] = "ambiguous"
                raise ValueError("ambiguous logger aliases")
            configured = flag_value(arguments, flags[0] if flags else "--log")
            report["logger_configuration"]["argument_mode"] = "stderr" if configured == "-" else "file"
            if configured != "-":
                configured = Path(configured)
                if not configured.is_absolute() and working.is_absolute():
                    configured = working / configured
                candidates.append(("validator_flag", configured))
        except ValueError:
            if not flags:
                report["logger_configuration"]["argument_mode"] = "default"
                if working.is_absolute():
                    candidates.append(("validator_default", working / ("agave-validator-" + records["validator"] + ".log")))
        for name in ("StandardOutput", "StandardError"):
            if not records.get("effective_start_mentions_original_binary"):
                continue
            value = values.get(name, "")
            if ":" in value and value.split(":", 1)[0] in ("append", "file", "truncate"):
                candidates.append((name, Path(value.split(":", 1)[1])))
            elif value in ("append", "file", "truncate") and values.get(name + "File"):
                candidates.append((name, Path(values[name + "File"])))
        seen = set()
        for source, path in candidates:
            if path not in seen:
                seen.add(path)
                report["file_logs"].append({"source": source, **file_log_summary(path)})
    except (OSError, ValueError, TypeError, KeyError):
        report["log_lookup_status"] = "unavailable"
    return report


def journal_summary(run, namespace=""):
    selected_namespace = namespace if re.fullmatch(r"[A-Za-z0-9_.-]{1,64}", namespace) else ""
    try:
        arguments = ["journalctl", "--unit", SERVICE, "--no-pager", "--lines=120", "--output=json",
                     "--output-fields=MESSAGE,PRIORITY,__REALTIME_TIMESTAMP"]
        if selected_namespace:
            arguments.append("--namespace=" + selected_namespace)
        result = run(arguments)
    except (OSError, subprocess.TimeoutExpired):
        return {"status": "unavailable"}
    if result.returncode or len(result.stdout) > 2 * 1024 * 1024:
        return {"status": "unavailable"}
    messages, entries = [], 0
    for line in result.stdout.splitlines():
        try:
            entry = json.loads(line)
        except (ValueError, TypeError):
            continue
        entries += 1
        message = entry.get("MESSAGE") if isinstance(entry, dict) else None
        messages.append(message)
    return {"status": "classified", "entries_examined": entries, "marker_counts": marker_counts(messages), "namespaced": bool(selected_namespace),
            "raw_messages_included": False, "markers_are_not_a_root_cause_diagnosis": True}


def read_dropin(dropin):
    descriptor = os.open(dropin, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    try:
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode) or info.st_size > 64 * 1024:
            raise ValueError("unbounded or nonregular managed drop-in")
        return os.read(descriptor, 64 * 1024 + 1).decode()
    finally:
        os.close(descriptor)


def managed_records(start, root, dropin, retained_start=""):
    references = start + "\n" + retained_start
    original = sorted(set(re.findall(re.escape(str(root)) + r"/([0-9a-f]{40})/runtime/(?:bin/agave-validator|checkpoints|unsettled)", references)))
    upgraded = sorted(set(re.findall(re.escape(str(root)) + r"/upgrades/([0-9a-f]{40})/runtime/", references)))
    report = {"original_revisions": original, "upgrade_revisions": upgraded, "onchain_state_checked": False}
    if len(original) != 1 or len(upgraded) > 1:
        return {**report, "status": "no_unambiguous_managed_binding"}
    base = root / original[0]
    control = base / "control"
    try:
        for directory in (root, base, control):
            info = directory.lstat()
            if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode):
                return {**report, "status": "invalid_directory"}
        plan = private_json(control / "plan.json")
        reservation = private_json(control / "reservation.json")
        activation = private_json(control / "activation.json")
        installed = private_json(control / "activation-installed.json")
        artifacts = plan["artifacts"]
        roles = set(artifacts)
        if roles not in (set(ARTIFACTS), set(ARTIFACTS) - {"geyser_events"}):
            return {**report, "status": "unknown_artifact_roles"}
        for value in (plan["programs"]["portal"], plan["programs"]["token_bridge"], plan["validator"]):
            public_bytes(value)
        profile = digest({"version": 2, "source": original[0], "artifacts": artifacts,
                          "sdk": SDK_REVISION, "proof_acceptance_qualified": False})
        expected_environment = {"NORTHSTAR_CHECKPOINT_PLAN_DIR": str(base / "runtime/checkpoints"),
                                "NORTHSTAR_UNSETTLED_STATE_DIR": str(base / "runtime/unsettled")}
        identity_ok = (plan.get("version") == reservation.get("version") == activation.get("version") == 2
                       and plan.get("source_revision") == reservation.get("source_revision") == original[0]
                       and plan.get("sdk_revision") == SDK_REVISION and plan.get("genesis") == DEVNET
                       and reservation.get("plan_sha256") == digest(plan)
                       and plan.get("original_sha256") == digest(reservation["original"])
                       and plan.get("profile") == profile and activation.get("artifacts") == artifacts
                       and activation.get("state_environment") == expected_environment
                       and activation["arguments"][0] == str(base / "runtime" / ARTIFACTS["validator"])
                       and flag_value(activation["arguments"], "--portal") == plan["programs"]["portal"])
        text = service_dropin(activation["arguments"], *expected_environment.values())
        expected_dropin_hash = hashlib.sha256(text.encode()).hexdigest()
        report.update(status="inspected", recorded_plan_identity_matches=identity_ok,
                      original_dropin_record_matches=installed == {"sha256": expected_dropin_hash},
                      managed_dropin_matches_original=file_hash(dropin) == expected_dropin_hash,
                      effective_start_mentions_original_binary=activation["arguments"][0] in start,
                      original_binary_matches=file_hash(base / "runtime" / ARTIFACTS["validator"]) == artifacts["validator"],
                      original_artifact_roles=sorted(roles))
        if identity_ok:
            report.update(portal=plan["programs"]["portal"], token_bridge=plan["programs"]["token_bridge"],
                          validator=plan["validator"], profile=plan["profile"])
        report["journal_directories"] = {}
        for role in ("checkpoints", "unsettled"):
            directory = base / "runtime" / role
            info = directory.lstat()
            report["journal_directories"][role] = {
                "directory": stat.S_ISDIR(info.st_mode) and not stat.S_ISLNK(info.st_mode),
                "mode_is_private": stat.S_IMODE(info.st_mode) == 0o700,
                "owner_matches_record": info.st_uid == reservation["original"]["uid"],
            }
        if upgraded:
            upgrade_control = root / "upgrades" / upgraded[0] / "control"
            upgrade_plan = private_json(upgrade_control / "plan.json")
            progress = private_json(upgrade_control / "progress.json")
            report["upgrade_record"] = {
                "version": 1 if upgrade_plan.get("version") == 1 else "unknown",
                "identity_matches": upgrade_plan.get("version") == 1 and upgrade_plan.get("kind") == "native-upgrade"
                    and upgrade_plan.get("source_revision") == upgraded[0]
                    and upgrade_plan.get("base_revision") == original[0]
                    and upgrade_plan.get("base_plan_sha256") == digest(plan),
                "phase": progress.get("phase") if progress.get("phase") in
                    ("prepared", "frozen", "installed", "start-attempted", "qualified") else "unknown",
                "progress_matches": progress.get("version") == 1 and progress.get("plan_sha256") == digest(upgrade_plan),
                "start_barrier_present": os.path.lexists(upgrade_control / "start-attempted.json"),
            }
    except FileNotFoundError:
        report["status"] = "missing_record_or_path"
    except (OSError, ValueError, TypeError, KeyError):
        report["status"] = "invalid_or_unreadable_record"
    return report


def collect_report(run=execute, root=STATE_ROOT, dropin=DROPIN, lock=LOCK):
    report = {"version": 1, "read_only": True, "service": SERVICE,
              "scope": "systemd and retained metadata only; no RPC qualification or recovery"}
    private_properties = ("ExecStart", "StandardOutput", "StandardOutputFile", "StandardError", "StandardErrorFile", "WorkingDirectory", "LogNamespace")
    result = run(["systemctl", "show", SERVICE, *["--property=" + name for name in (*PUBLIC_PROPERTIES, *private_properties)]])
    if len(result.stdout) > 64 * 1024:
        return {**report, "status": "service_query_unavailable"}
    values = dict(line.split("=", 1) for line in result.stdout.decode(errors="replace").splitlines() if "=" in line)
    if not any(name in values for name in PUBLIC_PROPERTIES):
        return {**report, "status": "service_query_unavailable"}
    report["systemd_query_exit_code"] = result.returncode
    report["systemd"] = {key: values[key][:160] for key in PUBLIC_PROPERTIES if key in values}
    report["journal"] = journal_summary(run, values.get("LogNamespace", ""))
    descriptor = None
    try:
        descriptor = os.open(lock, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode) or info.st_mode & 0o077:
            return {**report, "status": "deployment_lock_invalid"}
        fcntl.flock(descriptor, fcntl.LOCK_SH | fcntl.LOCK_NB)
        try:
            retained_start = read_dropin(dropin)
            report["managed_dropin_readable"] = True
        except (OSError, ValueError):
            retained_start = ""
            report["managed_dropin_readable"] = False
        report["managed_records"] = managed_records(values.get("ExecStart", ""), root, dropin, retained_start)
        report["operational"] = operational_details(values, root, report["managed_records"])
        report["status"] = "snapshot_collected"
    except FileNotFoundError:
        report["status"] = "deployment_lock_missing"
    except BlockingIOError:
        report["status"] = "deployment_busy"
    except OSError as error:
        report["status"] = "deployment_busy" if error.errno in (errno.EAGAIN, errno.EWOULDBLOCK) else "deployment_lock_unavailable"
    finally:
        if descriptor is not None:
            os.close(descriptor)
    return report


def main():
    try:
        report = collect_report()
    except Exception:
        report = {"version": 1, "read_only": True, "status": "diagnostics_unavailable"}
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()

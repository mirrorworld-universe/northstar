"""Sonic: Public, read-only diagnostics; never expose command lines or journal text."""
import errno
import fcntl
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import stat
import subprocess
import time
import zlib

from continuity_devnet import DROPIN, SERVICE, public_bytes
from native_l1_recovery import STATE_PATHS
from continuity_rollout_state import (
    ARTIFACTS, DEVNET, SDK_REVISION, canonical, file_hash, flag_value, geyser_event_source,
    geyser_events_config, l1_rpc_arguments, private_json, service_dropin, upgrade_arguments,
)

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
    "duplicate running instances of the same validator node": "duplicate_validator_instance",
    "no space left on device": "disk_full",
    "insufficient disk space": "disk_full",
    "os error 28": "disk_full",
    "too many open files": "file_descriptor_limit",
    "failed to open ledger": "ledger_open_failure",
    "bank hash mismatch": "bank_hash_mismatch",
    "bank_hash_mismatch": "bank_hash_mismatch",
    "we have tried to repair duplicate slot:": "duplicate_slot_repair_exhausted",
    "we are attempting to dump a block that we produced": "local_block_hash_conflict",
    "attempting to enable alpenglow but we have the wrong version": "migration_genesis_mismatch",
    "snapshot hash mismatch": "snapshot_hash_mismatch",
    "out of memory": "memory_limit",
    "oom-kill": "memory_limit",
    "failed to load er geyser plugins": "event_plugin_load_failure",
    "error while loading shared libraries": "native_library_load_failure",
    "searching for an rpc service with shred version": "rpc_peer_search",
    "no snapshots available": "snapshot_unavailable",
    "unable to find any rpc peers": "rpc_peer_unavailable",
    "failed to download a snapshot archive for slot": "snapshot_download_failed",
    "loading bank from full snapshot archive:": "snapshot_load_started",
    "cannot resume continuity session:": "continuity_resume_failed",
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


def panic_sites(messages):
    repository = Path(__file__).resolve().parents[2]
    sites, context = set(), 0
    for message in messages:
        if not isinstance(message, str) or "program log:" in message.lower():
            context = 0
            continue
        if "panicked at" in message.lower() or "datapoint: panic " in message.lower():
            context = 3
        elif context:
            context -= 1
        else:
            continue
        # Checkout paths can themselves contain northstar; match overlapping suffixes.
        for filename, line in re.findall(
                r"(?=(?:^|[/\s'\"])((?:accounts-db|core|gossip|ledger|northstar|runtime|validator)/(?:[A-Za-z0-9_-]+/)*[A-Za-z0-9_-]+\.rs):([1-9][0-9]{0,6})(?::[0-9]+)?)", message):
            if (repository / filename).is_file():
                sites.add((filename, int(line)))
        for suffix, line in re.findall(
                r"(?=(?:^|[/\s'\"])(src/(?:[A-Za-z0-9_-]+/)*[A-Za-z0-9_-]+\.rs):([1-9][0-9]{0,6})(?::[0-9]+)?)", message):
            candidates = [candidate for candidate in repository.glob("*/" + suffix) if candidate.is_file()]
            if len(candidates) == 1:
                sites.add((candidates[0].relative_to(repository).as_posix(), int(line)))
    return [{"file": filename, "line": line} for filename, line in sorted(sites)[:8]]


def startup_evidence(messages):
    report = {"stage_counts": {}}
    stages = (
        ("solana_runtime::snapshot_bank_utils", "Loading bank from full snapshot archive:", "snapshot_load_started"),
        ("agave_snapshots::unarchive", "Extracting ", "snapshot_extract_started"),
        ("agave_snapshots::unarchive", "Extracted ", "snapshot_extract_completed"),
        ("solana_runtime::serde_snapshot", "Building accounts index... Done in ", "accounts_index_completed"),
        ("solana_runtime::serde_snapshot", "Building accounts index...", "accounts_index_started"),
        ("solana_accounts_db::accounts_db", "generating index: processed all slots", "accounts_index_slots_completed"),
        ("solana_runtime::snapshot_bank_utils", "Rebuilding status cache from ", "snapshot_status_cache"),
        ("solana_rpc::rpc_service", "rpc bound to ", "rpc_listener_started"),
        ("northstar", "Ephemeral rollup started on ", "er_runtime_started"),
    )
    for message in messages:
        if not isinstance(message, str) or "program log:" in message.lower():
            continue
        matched = re.fullmatch(r"\[([0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,9})?Z)\s+"
                               r"(?:ERROR|WARN|INFO|DEBUG|TRACE)\s+([a-z0-9_:]+)\]\s*(.*)", message)
        if not matched:
            continue
        timestamp, module, body = matched.groups()
        stage = next((name for expected_module, prefix, name in stages if module == expected_module and body.startswith(prefix)), None)
        progress = re.match(r"generating index: processed ([0-9]{1,20})/([0-9]{1,20}) slots\.\.\.(?:\s|$)", body)
        if module == "solana_accounts_db::accounts_db" and progress:
            processed, total = map(int, progress.groups())
            if 0 <= processed <= total <= 2 ** 64 - 1 and total > 0:
                report.setdefault("first_index_progress", {"processed": processed, "total": total, "at": timestamp})
                report["index_progress"] = {"processed": processed, "total": total}
                stage = "accounts_index_progress"
        if stage:
            report["stage_counts"][stage] = report["stage_counts"].get(stage, 0) + 1
            report.update(last_stage=stage, last_stage_at=timestamp)
    return report


def tail_evidence(tail):
    messages = [message for message in tail.decode(errors="replace").splitlines() if "program log:" not in message.lower()]
    context, remaining = [], 0
    for message in messages:
        if "panicked at" in message.lower() or "datapoint: panic " in message.lower():
            remaining = 4
        if remaining:
            context.append(message)
            remaining -= 1
    text = "\n".join(context)
    kinds = set()
    if "panicked at" in text.lower():
        for kind, expression in {
            "bank_hash_mismatch": r"bank(?: |_)hash(?: |_)mismatch",
            "result_unwrap": r"called\s+`?Result::(?:unwrap|expect)",
            "option_unwrap": r"called\s+`?Option::(?:unwrap|expect)",
            "assertion_failed": r"assertion[^\n]*failed",
        }.items():
            if re.search(expression, text, re.IGNORECASE):
                kinds.add(kind)
    slots = {int(slot) for message in messages if "bank hash" in message.lower() or "bank_hash" in message.lower()
             for slot in re.findall(r"(?:at |For )?slot\s+([0-9]{1,20})(?=[:\s,])", message)
             if int(slot) <= 2 ** 64 - 1}
    return {"marker_counts": marker_counts(messages), "panic_sites": panic_sites(messages),
            "panic_kinds": sorted(kinds), "bank_hash_slots": sorted(slots)[:8], "startup": startup_evidence(messages)}


def disk_summary(directory):
    try:
        info = os.statvfs(directory)
        return {"status": "observed", "total_bytes": info.f_blocks * info.f_frsize,
                "available_bytes": info.f_bavail * info.f_frsize, "free_bytes": info.f_bfree * info.f_frsize,
                "available_inodes": info.f_favail}
    except OSError:
        return {"status": "unavailable"}


def approved_log_path(path):
    return (path.is_absolute() and len(path.parts) > 1 and path.parts[1] not in ("proc", "sys", "dev", "etc")
            and not any(part in ("..", "control", "keys", ".ssh", ".config", "checkpoints", "unsettled", "rocksdb", "accounts", "accounts_index", "snapshots") for part in path.parts)
            and path.suffix.lower() not in (".json", ".key", ".pem", ".pub", ".bin", ".db", ".sqlite", ".so")
            and not path.name.startswith(("id_", "MANIFEST-")) and path.name not in ("CURRENT", "IDENTITY", "LOCK")
            and re.fullmatch(r"[0-9]+\.log", path.name) is None)


def unit_log_sinks(run, values):
    try:
        result = run(["systemctl", "cat", "--no-pager", SERVICE])
        if result.returncode or len(result.stdout) > 256 * 1024:
            return {}
        section, configured = "", {}
        for raw in result.stdout.decode(errors="replace").splitlines():
            line = raw.strip()
            if line.startswith("[") and line.endswith("]"):
                section = line
            elif section == "[Service]" and "=" in line and not line.startswith(("#", ";")):
                name, value = line.split("=", 1)
                if name.strip() in ("StandardOutput", "StandardError"):
                    tokens = shlex.split(value, comments=True)
                    configured[name.strip()] = tokens[0] if len(tokens) == 1 else ""
        return {name: value for name, value in configured.items()
                if value.split(":", 1)[0] == values.get(name, "").split(":", 1)[0]}
    except (OSError, ValueError, subprocess.TimeoutExpired):
        return {}


def file_log_summary(path):
    if not approved_log_path(path):
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
            if target.parent != path.parent or not target.name.startswith(path.stem) or not approved_log_path(target):
                return {"status": "unapproved_log_alias"}
            descriptor = os.open(target, flags)
            rotated = True
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode):
            return {"status": "not_a_regular_log"}
        os.lseek(descriptor, max(0, info.st_size - 64 * 1024), os.SEEK_SET)
        tail = os.read(descriptor, 64 * 1024)
        return {"status": "classified", "bytes_examined": len(tail), "modified_at_unix": int(info.st_mtime), "rotated_sibling": rotated,
                **tail_evidence(tail),
                "raw_messages_included": False, "markers_are_not_a_root_cause_diagnosis": True}
    except OSError:
        return {"status": "unavailable"}
    finally:
        if descriptor is not None:
            os.close(descriptor)


def previous_log_rotations(path):
    if not approved_log_path(path):
        return []
    found = []
    try:
        with os.scandir(path.parent) as entries:
            for index, entry in enumerate(entries):
                if index >= 128:
                    break
                if not entry.name.startswith(path.name) or not entry.is_file(follow_symlinks=False):
                    continue
                suffix = entry.name[len(path.name):]
                if re.fullmatch(r"(?:\.[0-9]+|[-.][0-9]{8}|[-.][0-9]{4}-[0-9]{2}-[0-9]{2})(?:\.gz)?", suffix):
                    candidate = Path(entry.path)
                    if approved_log_path(candidate):
                        found.append((entry.stat(follow_symlinks=False).st_mtime_ns, candidate))
    except OSError:
        return []
    return [candidate for _, candidate in sorted(found, reverse=True)[:2]]


def gzip_log_summary(path, *, max_compressed_bytes=4 * 1024 ** 3, max_decoded_bytes=64 * 1024 ** 3, max_seconds=90):
    if not approved_log_path(path):
        return {"status": "not_an_approved_log_sink"}
    descriptor = None
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | getattr(os, "O_NOATIME", 0))
        info = os.fstat(descriptor)
        if not stat.S_ISREG(info.st_mode):
            return {"status": "not_a_regular_log"}
        metadata = {"compressed_bytes": info.st_size, "modified_at_unix": int(info.st_mtime), "compressed": True,
                    "limits": {"compressed_bytes": max_compressed_bytes, "decoded_bytes": max_decoded_bytes, "seconds": max_seconds}}
        if info.st_size > max_compressed_bytes:
            return {"status": "rotation_over_limit", "limit_kind": "compressed_bytes", **metadata}
        source = os.fdopen(descriptor, "rb")
        descriptor = None
        deadline, scanned, tail = time.monotonic() + max_seconds, 0, b""
        with source, gzip.GzipFile(fileobj=source, mode="rb") as archive:
            while True:
                if time.monotonic() > deadline:
                    return {"status": "rotation_over_limit", "limit_kind": "elapsed_seconds", "decoded_bytes_scanned": scanned, **metadata}
                chunk = archive.read(min(64 * 1024, max_decoded_bytes - scanned + 1))
                if not chunk:
                    break
                scanned += len(chunk)
                if scanned > max_decoded_bytes:
                    return {"status": "rotation_over_limit", "limit_kind": "decoded_bytes", "decoded_bytes_scanned": scanned, **metadata}
                tail = (tail + chunk)[-64 * 1024:]
        return {"status": "classified", "bytes_examined": len(tail), "decoded_bytes_scanned": scanned, **metadata,
                **tail_evidence(tail),
                "raw_messages_included": False, "markers_are_not_a_root_cause_diagnosis": True}
    except (OSError, EOFError, zlib.error):
        return {"status": "unavailable"}
    finally:
        if descriptor is not None:
            os.close(descriptor)


def rpc_index_configuration(arguments):
    allowed = {"program-id", "spl-token-owner", "spl-token-mint"}
    configured, filters = set(), set()
    if not isinstance(arguments, list) or len(arguments) > 4096 or not all(isinstance(value, str) for value in arguments):
        return {"status": "ambiguous"}
    try:
        for index, argument in enumerate(arguments):
            flag, separator, inline = argument.partition("=")
            if flag not in ("--account-index", "--account-index-include-key", "--account-index-exclude-key"):
                continue
            selected = [inline] if separator else []
            for value in arguments[index + 1:]:
                if value.startswith("-"):
                    break
                selected.append(value)
            selected = [part for value in selected for part in value.split(",")]
            if not selected or any(not value for value in selected):
                return {"status": "ambiguous"}
            if flag == "--account-index":
                if not set(selected) <= allowed:
                    return {"status": "ambiguous"}
                configured.update(selected)
            else:
                for value in selected:
                    public_bytes(value)
                filters.add("include-only" if flag == "--account-index-include-key" else "exclude-list")
        if len(filters) > 1:
            return {"status": "ambiguous"}
        return {"status": "observed", "configured": sorted(configured), "key_filter": next(iter(filters), "unrestricted")}
    except (TypeError, ValueError):
        return {"status": "ambiguous"}


def operational_details(values, root, records, bound_arguments=None):
    report = {"filesystems": {"state_root": disk_summary(root)}, "file_logs": []}
    if bound_arguments is not None:
        if not bound_arguments:
            return report
    elif not (records.get("recorded_plan_identity_matches") and records.get("managed_dropin_matches_original")
            and records.get("original_binary_matches") and len(records.get("original_revisions", [])) == 1):
        return report
    try:
        arguments = bound_arguments if bound_arguments is not None else private_json(
            root / records["original_revisions"][0] / "control/activation.json")["arguments"]
        report["rpc_account_indexes"] = rpc_index_configuration(arguments)
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
            if not (bound_arguments or records.get("effective_start_mentions_original_binary")):
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
                current = file_log_summary(path)
                report["file_logs"].append({"source": source, **current})
                if (values.get("ActiveState") in ("failed", "inactive") and not current.get("rotated_sibling")
                        and not current.get("marker_counts")):
                    for previous in previous_log_rotations(path):
                        summary = gzip_log_summary(previous) if previous.suffix == ".gz" else file_log_summary(previous)
                        report["file_logs"].append({"source": source + "_rotation", **summary})
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
    return {"status": "classified", "entries_examined": entries, "marker_counts": marker_counts(messages), "panic_sites": panic_sites(messages), "namespaced": bool(selected_namespace),
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


def effective_arguments_match(start, text):
    paths = re.findall(r"(?:^|[ {])path=([^;]+);", start)
    arguments = re.findall(r"(?:^|[ ;])argv\[\]=(.*?)(?= ; |;| \})", start)
    try:
        return (len(paths) == len(arguments) == 1 and paths[0].strip() == "/usr/bin/env"
                and shlex.split(arguments[0]) == shlex.split(text.rsplit("ExecStart=", 1)[1].strip()))
    except (ValueError, IndexError):
        return False


def upgrade_runtime_arguments(start, root, dropin, revision, plan, base_plan, reservation, environment, binding):
    namespace = root / "upgrades" / revision
    control, runtime = namespace / "control", namespace / "runtime"
    try:
        binding["stage"] = "directories"
        for directory in (root / "upgrades", namespace, control, runtime, runtime / "bin", runtime / "lib"):
            if not stat.S_ISDIR(directory.lstat().st_mode):
                return []
        binding["stage"] = "service_plan"
        desired, artifacts = plan["desired"], plan["artifacts"]
        arguments = desired["arguments"]
        event_source = geyser_event_source(plan)
        if (not isinstance(arguments, list) or not arguments or len(arguments) > 4096
                or not all(isinstance(value, str) and not any(character in value for character in "\n\r;") for value in arguments)
                or set(artifacts) != set(ARTIFACTS)
                or any(not isinstance(value, str) or not re.fullmatch(r"[0-9a-f]{64}", value) for value in artifacts.values())
                or arguments[0] != str(runtime / ARTIFACTS["validator"])
                or l1_rpc_arguments(arguments, plan.get("l1_rpc_profile")) != arguments
                or desired["state_environment"] != environment or plan["before"]["state_environment"] != environment
                or any(desired[key] != reservation["original"][key] for key in ("uid", "gid"))
                or flag_value(arguments, "--portal") != base_plan["programs"]["portal"]
                or any(artifacts[role] != base_plan["artifacts"][role] for role in ("portal", "token_bridge"))):
            return []
        upgrade_arguments(arguments, runtime / ARTIFACTS["validator"], runtime / "geyser-events.json", runtime / "geyser-events.json",
                          previous_source=event_source, event_source=event_source)
        binding["stage"] = "barriers_and_effective_start"
        text = service_dropin(arguments, *environment.values())
        installed = {"sha256": hashlib.sha256(text.encode()).hexdigest()}
        progress = private_json(control / "progress.json")
        started = private_json(control / "start-attempted.json")
        if (progress != {"version": 1, "plan_sha256": digest(plan), "phase": progress.get("phase")}
                or progress.get("phase") not in ("start-attempted", "qualified")
                or private_json(control / "frozen.json").get("plan_sha256") != digest(plan)
                or private_json(control / "installed.json") != installed
                or set(started) != {"plan_sha256", "l1_floor"} or started["plan_sha256"] != digest(plan)
                or type(plan.get("l1_floor")) is not int or type(started["l1_floor"]) is not int
                or not 0 <= plan["l1_floor"] <= started["l1_floor"] <= 2 ** 64 - 1
                or file_hash(dropin) != installed["sha256"] or stat.S_IMODE(dropin.lstat().st_mode) != 0o600
                or not effective_arguments_match(start, text) or desired["binary_sha256"] != artifacts["validator"]):
            return []
        binding["stage"] = "native_artifacts"
        for role in ("validator", "geyser_events"):
            target = runtime / ARTIFACTS[role]
            info = target.lstat()
            if (not stat.S_ISREG(info.st_mode) or info.st_uid != desired["uid"]
                    or stat.S_IMODE(info.st_mode) != 0o500 or file_hash(target) != artifacts[role]):
                return []
        binding["stage"] = "plugin_configuration"
        configuration = runtime / "geyser-events.json"
        info = configuration.lstat()
        if (info.st_uid != desired["uid"] or stat.S_IMODE(info.st_mode) != 0o400
                or private_json(configuration) != geyser_events_config(runtime / ARTIFACTS["geyser_events"], event_source)):
            return []
        binding["stage"] = "matched"
        return arguments
    except (OSError, ValueError, TypeError, KeyError):
        return []


def recovery_directory_metadata(namespace, owner):
    directories = {}
    roles = [("l1_root", ""), *((flag.removeprefix("--").replace("-", "_"), suffix) for flag, suffix in STATE_PATHS.items())]
    for role, suffix in roles:
        try:
            info = (namespace / "runtime/l1" / suffix).lstat()
            directories[role] = {"directory": stat.S_ISDIR(info.st_mode), "symlink": stat.S_ISLNK(info.st_mode),
                                 "mode": f"{stat.S_IMODE(info.st_mode):04o}", "owner_matches_record": info.st_uid == owner["uid"],
                                 "group_matches_record": info.st_gid == owner["gid"]}
        except FileNotFoundError:
            directories[role] = {"absent": True}
        except OSError:
            directories[role] = {"status": "unavailable"}
    retry = namespace / "control/l1-retry"
    records = {"authorization": "authorization.json", "start_attempted": "start-attempted.json",
               "outcome": "outcome.json", "failure": "failure.json"}
    if os.path.lexists(retry) and not stat.S_ISDIR(retry.lstat().st_mode):
        return {"l1_directories": directories, "retry_directory": "invalid"}
    return {"l1_directories": directories, "retry_records": {name: os.path.lexists(retry / filename) for name, filename in records.items()}}


def managed_records(start, root, dropin, retained_start="", bound_arguments=None, bound_specification=None):
    references = start + "\n" + retained_start
    original = sorted(set(re.findall(re.escape(str(root)) + r"/([0-9a-f]{40})/runtime/(?:bin/agave-validator|checkpoints|unsettled)", references)))
    # Retained L1 directories may belong to an older upgrade; only executables select a runtime.
    upgraded = sorted(set(re.findall(re.escape(str(root)) + r"/upgrades/([0-9a-f]{40})/runtime/bin/agave-validator\b", references)))
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
        recorded_sdk = plan.get("sdk_revision")
        profile = digest({"version": 2, "source": original[0], "artifacts": artifacts,
                          "sdk": recorded_sdk, "proof_acceptance_qualified": False})
        expected_environment = {"NORTHSTAR_CHECKPOINT_PLAN_DIR": str(base / "runtime/checkpoints"),
                                "NORTHSTAR_UNSETTLED_STATE_DIR": str(base / "runtime/unsettled")}
        identity_ok = (plan.get("version") == reservation.get("version") == activation.get("version") == 2
                       and plan.get("source_revision") == reservation.get("source_revision") == original[0]
                       and isinstance(recorded_sdk, str) and re.fullmatch("[0-9a-f]{40}", recorded_sdk)
                       and reservation.get("sdk_revision") == recorded_sdk and plan.get("genesis") == DEVNET
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
        if (identity_ok and report["managed_dropin_matches_original"] and report["original_binary_matches"]
                and effective_arguments_match(start, text) and bound_arguments is not None):
            bound_arguments.extend(activation["arguments"])
            if bound_specification is not None:
                bound_specification.update(arguments=activation["arguments"], state_environment=expected_environment,
                                           binary_sha256=artifacts["validator"], uid=reservation["original"]["uid"],
                                           gid=reservation["original"]["gid"])
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
            arguments, binding = [], {"stage": "identity"}
            if identity_ok and report["upgrade_record"]["identity_matches"]:
                arguments = upgrade_runtime_arguments(start, root, dropin, upgraded[0], upgrade_plan, plan, reservation, expected_environment, binding)
            report["upgrade_record"].update(runtime_binding_matches=bool(arguments), runtime_binding_stage=binding["stage"])
            if arguments and type(upgrade_plan.get("recovery_failed_slot")) is int:
                report["upgrade_record"].update(recovery_directory_metadata(root / "upgrades" / upgraded[0], upgrade_plan["desired"]))
            if arguments and bound_arguments is not None:
                bound_arguments[:] = arguments
                if bound_specification is not None:
                    bound_specification.clear()
                    bound_specification.update(upgrade_plan["desired"])
    except FileNotFoundError:
        report["status"] = "missing_record_or_path"
    except (OSError, ValueError, TypeError, KeyError):
        report["status"] = "invalid_or_unreadable_record"
    return report


def upload_operation_errors(path, owner):
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | getattr(os, "O_NOATIME", 0))
        try:
            info = os.fstat(descriptor)
            if not stat.S_ISREG(info.st_mode) or info.st_uid != owner or info.st_mode & 0o077:
                return {"status": "unavailable", "raw_messages_included": False}
            if info.st_size > 64 * 1024 * 1024:
                return {"status": "file_limit_exceeded", "raw_messages_included": False}
            os.lseek(descriptor, max(0, info.st_size - 128 * 1024), os.SEEK_SET)
            data = os.read(descriptor, 128 * 1024)
        finally:
            os.close(descriptor)
        text = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", data.decode(errors="replace"))
        patterns = {"http_rate_limit": r"\b429\b", "rpc_transport": r"connection reset|connection closed|timed? out|connection refused|error sending request|transport error",
                    "upload_write_failure": r"Data writes to account failed", "retry_budget_exhausted": r"Max retries exceeded",
                    "blockhash_expired": r"BlockhashNotFound|blockhash.*expired|block height exceeded",
                    "insufficient_funds": r"insufficient funds|InsufficientFunds",
                    "transaction_rejected": r"Transaction simulation failed|TransactionError",
                    "program_size": r"program.*too large|program.*larger than|account data too small",
                    "elf_verification": r"ELF error|ELF verification|failed to verify.*program"}
        counts = {name: len(re.findall(pattern, text, re.I)) for name, pattern in patterns.items()}
        writes = sorted(set(int(value) for value in re.findall(r"\b([0-9]{1,6}) write transactions failed\b", text, re.I)))
        codes = sorted(set(int(value) for value in re.findall(r"\bRPC (?:response )?error[: =]+(-[0-9]{4,5})\b", text, re.I)))
        return {"status": "classified", "bytes_examined": len(data), "tail_only": info.st_size > len(data),
                "markers": {name: count for name, count in counts.items() if count}, "write_transactions_failed": writes,
                "rpc_error_codes": codes, "raw_messages_included": False, "markers_are_not_a_root_cause_diagnosis": True}
    except (OSError, ValueError):
        return {"status": "unavailable", "raw_messages_included": False}


def upload_attempt_status(control, plan, reservation, specification, prefix, previous_sha256=None):
    unavailable = {"status": "unavailable"}
    marker = control / (prefix + ".json")
    if not os.path.lexists(marker):
        return unavailable if os.path.lexists(control / (prefix + "-report.json")) else {
            "status": "not-recorded", "attempt_barrier_present": False}
    try:
        record = private_json(marker)
        if not isinstance(record, dict):
            return unavailable
        version, phase = record.get("version"), record.get("phase")
        keys = {"version", "kind", "plan_sha256", "controller_revision", "source_revision", "submission_rpc",
                "previous_service_sha256", "pending_roles", "attempt_budget", "phase"}
        if phase == "failed":
            keys.add("operator_reconciliation_required")
        if version == 2:
            keys.update(("read_rpc", "submission_roles"))
            submitted = record.get("submission_roles")
            if (record.get("read_rpc") != reservation.get("l1_rpc") or not isinstance(submitted, list)
                    or len(submitted) != len(set(submitted)) or not set(submitted) <= {"portal", "token_bridge"}):
                return unavailable
        if previous_sha256 is not None:
            keys.add("previous_failed_record_sha256")
            if version != 2 or record.get("previous_failed_record_sha256") != previous_sha256:
                return unavailable
        roles = record.get("pending_roles")
        if (set(record) != keys or type(version) is not int or version not in (1, 2)
                or record.get("kind") != "retained-program-upload"
                or record.get("plan_sha256") != digest(plan) or record.get("source_revision") != plan["source_revision"]
                or not isinstance(record.get("controller_revision"), str)
                or not re.fullmatch("[0-9a-f]{40}", record["controller_revision"])
                or record.get("submission_rpc") != reservation.get("l1_rpc")
                or record.get("previous_service_sha256") != digest(specification)
                or type(record.get("attempt_budget")) is not int or record["attempt_budget"] != 1
                or phase not in ("attempted", "completed", "failed")
                or phase == "failed" and record.get("operator_reconciliation_required") is not True
                or not isinstance(roles, list) or not roles or len(roles) != len(set(roles))
                or not set(roles) <= {"portal", "token_bridge"}):
            return unavailable
        summary = {"status": "recorded", "attempt_barrier_present": True, "phase": phase,
                   "controller_revision": record["controller_revision"], "record_sha256": digest(record),
                   "operator_reconciliation_required": phase != "completed"}
        if version == 2:
            summary["submission_roles"] = record["submission_roles"]
        return summary
    except (OSError, ValueError, TypeError, KeyError):
        return unavailable


def upload_reconciliation_status(control, plan, reservation, specification):
    summary = upload_attempt_status(control, plan, reservation, specification, "program-upload-reconcile")
    extra = any(os.path.lexists(control / name) for name in
                ("program-upload-reconcile-retry.json", "program-upload-reconcile-retry-report.json"))
    if summary["status"] == "not-recorded" and extra:
        return {"status": "unavailable"}
    if summary["status"] == "recorded":
        summary["additional_pass"] = upload_attempt_status(control, plan, reservation, specification,
            "program-upload-reconcile-retry", summary["record_sha256"]) if summary["phase"] == "failed" else (
                {"status": "unavailable"} if extra else {"status": "not-recorded", "attempt_barrier_present": False})
    return summary


def fresh_continuation_attempt(control, plan, reservation, specification, programs, prefix, previous_sha256=None):
    marker = control / (prefix + ".json")
    if not os.path.lexists(marker):
        return {"status": "not-recorded", "attempt_barrier_present": False}
    unavailable = {"status": "unavailable"}
    try:
        record = private_json(marker)
        if not isinstance(record, dict):
            return unavailable
        version, phase = record.get("version"), record.get("phase")
        keys = {"version", "kind", "source_revision", "controller_revision", "plan_sha256", "programs_sha256",
                "previous_service_sha256", "previous_pid", "previous_start_time", "preflight_rpc", "qualification_rpc", "phase"}
        if phase == "failed":
            keys.add("operator_reconciliation_required")
            for flag in ("quarantine_requested", "quarantine_verified"):
                if flag in record:
                    keys.add(flag)
                    if type(record[flag]) is not bool:
                        return unavailable
        if previous_sha256 is not None:
            keys.add("previous_failed_continuation_sha256")
            if version != 2 or record.get("previous_failed_continuation_sha256") != previous_sha256:
                return unavailable
        elif version != 1:
            return unavailable
        if (set(record) != keys or type(version) is not int or record.get("kind") != "prepared-fresh-controller"
                or record.get("source_revision") != plan["source_revision"] or record.get("plan_sha256") != digest(plan)
                or record.get("programs_sha256") != digest(programs)
                or record.get("previous_service_sha256") != digest(specification)
                or type(record.get("previous_pid")) is not int or record["previous_pid"] != reservation["original"].get("pid")
                or record.get("previous_start_time") != reservation["original"].get("start_time")
                or record.get("preflight_rpc") != reservation.get("l1_rpc")
                or record.get("qualification_rpc") != reservation.get("devnet_rpc")
                or not isinstance(record.get("controller_revision"), str)
                or not re.fullmatch("[0-9a-f]{40}", record["controller_revision"])
                or phase not in ("attempted", "qualified", "failed")
                or phase == "failed" and record.get("operator_reconciliation_required") is not True):
            return unavailable
        return {"status": "recorded", "attempt_barrier_present": True, "phase": phase,
                "controller_revision": record["controller_revision"], "record_sha256": digest(record),
                "quarantine_requested": record.get("quarantine_requested", False),
                "operator_reconciliation_required": phase != "qualified"}
    except (OSError, ValueError, TypeError, KeyError):
        return unavailable


def fresh_continuation_status(control, plan, reservation, specification, programs):
    summary = fresh_continuation_attempt(control, plan, reservation, specification, programs, "fresh-controller-continuation")
    extra = os.path.lexists(control / "fresh-controller-continuation-retry.json")
    if summary["status"] == "not-recorded" and extra:
        return {"status": "unavailable"}
    if summary["status"] == "recorded":
        summary["additional_pass"] = fresh_continuation_attempt(control, plan, reservation, specification, programs,
            "fresh-controller-continuation-retry", summary["record_sha256"]) if summary["phase"] == "failed" else (
                {"status": "unavailable"} if extra else {"status": "not-recorded", "attempt_barrier_present": False})
    return summary


def pending_fresh_upload(root, revision, specification):
    report = {"status": "unavailable", "onchain_state_checked": False, "recovery_performed": False}
    if not re.fullmatch("[0-9a-f]{40}", revision) or not specification:
        return report
    try:
        namespace = root / revision
        control = namespace / "control"
        allowed = {os.getuid()}
        if os.getuid() == 0 and os.environ.get("SUDO_UID", "").isdigit():
            allowed.add(int(os.environ["SUDO_UID"]))
        for directory in (namespace, control):
            info = directory.lstat()
            if not stat.S_ISDIR(info.st_mode) or stat.S_ISLNK(info.st_mode) or info.st_uid not in allowed:
                return report
        owner = control.lstat().st_uid
        if control.lstat().st_mode & 0o077:
            return report
        plan = private_json(control / "plan.json")
        reservation = private_json(control / "reservation.json")
        progress = private_json(control / "fresh-progress.json")
        replacement = reservation["replacement"]
        artifacts = plan["artifacts"]
        sdk = plan.get("sdk_revision")
        expected_profile = digest({"version": 2, "source": revision, "artifacts": artifacts, "sdk": sdk,
                                   "proof_acceptance_qualified": False})
        if (plan.get("version") != 2 or plan.get("mode") != "fresh-stack" or plan.get("genesis") != DEVNET
                or reservation.get("version") != 2 or plan.get("source_revision") != reservation.get("source_revision")
                or plan["source_revision"] != revision or not isinstance(sdk, str) or not re.fullmatch("[0-9a-f]{40}", sdk)
                or reservation.get("sdk_revision") != sdk or reservation.get("plan_sha256") != digest(plan)
                or plan.get("profile") != expected_profile or plan.get("original_sha256") != digest(reservation["original"])
                or plan.get("replacement_sha256") != digest(replacement) or replacement.get("version") != 1
                or replacement.get("service") != specification
                or any(reservation["original"].get(key) != value for key, value in specification.items())
                or set(artifacts) != set(ARTIFACTS)
                or any(not isinstance(value, str) or not re.fullmatch("[0-9a-f]{64}", value) for value in artifacts.values())
                or progress != {"version": 1, "plan_sha256": digest(plan), "phase": "prepared"}
                or any(os.path.lexists(control / name) for name in ("fresh-starts", "activation.json", "activation-installed.json"))
                or os.path.lexists(namespace / "runtime")):
            return report
        if set(plan["programs"]) != {"portal", "token_bridge", "portal-buffer", "token_bridge-buffer"}:
            return report
        for value in plan["programs"].values():
            public_bytes(value)
        state = private_json(control / "programs.json")
        if state.get("version") != 2 or not isinstance(state.get("roles"), dict) or not set(state["roles"]) <= {"portal", "token_bridge"}:
            return report
        uploads = {"portal": "not-recorded", "token_bridge": "not-recorded"}
        for role, entry in state["roles"].items():
            if (not isinstance(entry, dict) or set(entry) != {"elf_sha256", "complete"}
                    or entry["elf_sha256"] != artifacts[role] or entry["complete"] is not None and not isinstance(entry["complete"], dict)):
                return report
            uploads[role] = "unresolved" if entry["complete"] is None else "recorded-complete"
        report.update(status="inspected", source_revision=revision, phase="prepared", previous_service_matches=True, native_start_recorded=False,
                      uploads=uploads, operation_errors=upload_operation_errors(control / "operations.log", owner),
                      upload_reconciliation=upload_reconciliation_status(control, plan, reservation, specification),
                      fresh_continuation=fresh_continuation_status(control, plan, reservation, specification, state))
    except (OSError, ValueError, TypeError, KeyError):
        pass
    return report


def collect_report(run=execute, root=STATE_ROOT, dropin=DROPIN, lock=LOCK, pending_revision=""):
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
        bound_arguments, bound_specification = [], {}
        report["managed_records"] = managed_records(values.get("ExecStart", ""), root, dropin, retained_start, bound_arguments, bound_specification)
        records = report["managed_records"]
        if (bound_arguments and any(values.get(name) in ("append", "file", "truncate") and not values.get(name + "File")
                                    for name in ("StandardOutput", "StandardError"))):
            values.update(unit_log_sinks(run, values))
        report["operational"] = operational_details(values, root, records, bound_arguments)
        if pending_revision:
            report["pending_fresh_upload"] = pending_fresh_upload(root, pending_revision,
                bound_specification if values.get("ActiveState") == "active" else {})
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
    import argparse
    parser = argparse.ArgumentParser(description="Read-only native lifecycle and optional pending-upload evidence")
    parser.add_argument("--pending-revision", default="")
    args = parser.parse_args()
    try:
        report = collect_report(pending_revision=args.pending_revision)
    except Exception:
        report = {"version": 1, "read_only": True, "status": "diagnostics_unavailable"}
    print(json.dumps(report, sort_keys=True))


if __name__ == "__main__":
    main()

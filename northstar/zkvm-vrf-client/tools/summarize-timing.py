#!/usr/bin/env python3
"""Summarize opt-in SP1 close-span logs without treating diagnostics as acceptance."""
import argparse
from collections import defaultdict
from datetime import datetime
import hashlib
import json
from pathlib import Path
import re

ANSI = re.compile(r"\x1b\[[0-9;]*m")
SPAN = re.compile(
    r"^(\S+)\s+(?:TRACE|DEBUG|INFO|WARN|ERROR) (.+): ([\w:]+): "
    r"close time\.busy=(\S+) time\.idle=(\S+)$"
)
DURATION = re.compile(r"^(\d+(?:\.\d+)?)(ns|µs|us|ms|s)$")
SCALE = {"ns": 1e-9, "µs": 1e-6, "us": 1e-6, "ms": 1e-3, "s": 1.0}


def seconds(value):
    match = DURATION.fullmatch(value)
    if not match:
        raise ValueError(f"unsupported span duration: {value}")
    return float(match[1]) * SCALE[match[2]]


def interval_union(intervals):
    total = 0.0
    end = float("-inf")
    for start, stop in sorted(intervals):
        total += max(0.0, stop - max(start, end))
        end = max(end, stop)
    return total


def summarize(log, measurements):
    spans = defaultdict(list)
    for raw in log.splitlines():
        match = SPAN.fullmatch(ANSI.sub("", raw))
        if not match:
            continue
        timestamp, ancestry, target, busy, idle = match.groups()
        end = datetime.fromisoformat(timestamp.replace("Z", "+00:00")).timestamp()
        busy, idle = seconds(busy), seconds(idle)
        name = ancestry.rsplit(":", 1)[-1].split("{", 1)[0]
        spans[(target, name)].append((end - busy - idle, end, busy, idle))

    stages = []
    for (target, name), records in sorted(spans.items()):
        stages.append({
            "target": target,
            "span": name,
            "count": len(records),
            "busy_s_sum": round(sum(row[2] for row in records), 6),
            "idle_s_sum": round(sum(row[3] for row in records), 6),
            "span_s_max": round(max(row[2] + row[3] for row in records), 6),
            "interval_union_s": round(interval_union((r[0], r[1]) for r in records), 6),
        })

    phases = measurements.get("phases", [])
    proofs = [{key: phase.get(key) for key in (
        "case_id", "sample", "attempt_sequence", "process_cold", "prove_and_wrap_ms",
        "verify_ms", "sdk_verified", "proof_sha256", "public_inputs_sha256",
    )} for phase in phases if phase.get("phase") == "groth16"]
    attempts = measurements.get("proof_attempts_started", 0)
    complete = (
        measurements.get("status") == "completed"
        and isinstance(attempts, int) and not isinstance(attempts, bool) and attempts > 0
        and attempts == len(proofs)
        and all(p["sdk_verified"] is True for p in proofs)
    )
    times = [p["prove_and_wrap_ms"] for p in proofs]
    times_valid = all(isinstance(t, int) and not isinstance(t, bool) and t > 0 for t in times)
    return {
        "schema": "northstar-sp1-timing-summary-v1",
        "classification": "diagnostic-characterization-not-fresh-acceptance",
        "production_acceptance": False,
        "fresh_session_acceptance_established": False,
        "measurement_status": measurements.get("status"),
        "error_present": bool(measurements.get("error")),
        "proof_attempts_started": attempts,
        "recorded_verified_proofs": sum(p["sdk_verified"] is True for p in proofs),
        "all_attempts_verified": complete,
        "all_recorded_proofs_within_120s": complete and times_valid and all(t <= 120000 for t in times),
        "client_init_ms": measurements.get("client_init_ms"),
        "sdk_total_wall_ms": measurements.get("total_wall_ms"),
        "cache_state": measurements.get("cache_state"),
        "elf_sha256": measurements.get("elf_sha256"),
        "program_vkey_hash": measurements.get("program_vkey_hash"),
        "setup_ms": [p.get("wall_ms") for p in phases if p.get("phase") == "setup"],
        "cpu_preflight_ms": [p.get("wall_ms") for p in phases if p.get("phase") == "execute"],
        "proofs": proofs,
        "span_timing_boundary": (
            "Rounded tracing durations; busy is entered-span wall time, not CPU time. "
            "Idle includes async waiting. Nested/concurrent spans overlap: never sum stages. "
            "interval_union_s merges intervals only within each target/name. "
            "The unchanged SDK prove-and-wrap timer is authoritative."
        ),
        "stages": stages,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--log", required=True, type=Path)
    parser.add_argument("--measurements", required=True, type=Path)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    raw_log, raw_measurements = args.log.read_bytes(), args.measurements.read_bytes()
    result = summarize(raw_log.decode("utf8", errors="replace"), json.loads(raw_measurements))
    result["log_sha256"] = hashlib.sha256(raw_log).hexdigest()
    result["measurements_sha256"] = hashlib.sha256(raw_measurements).hexdigest()
    with args.out.open("x") as output:
        json.dump(result, output, indent=2)
        output.write("\n")


if __name__ == "__main__":
    main()

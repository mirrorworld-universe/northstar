#!/usr/bin/env python3
import csv
import json
import re
import statistics
import sys
from pathlib import Path

PROFILES = ("1k", "10k", "100k")
CUSTOM_FIELDS = (
    "tracing_ms",
    "witness_generation_ms",
    "native_execution_ms",
    "constraint_generation_ms",
    "setup_ms",
    "prove_ms",
    "verify_ms",
    "trace_bytes",
    "witness_bytes",
    "rows",
    "constraints",
    "constraints_per_row",
    "proving_key_bytes",
    "verifying_key_bytes",
    "proof_bytes",
)
TARGET_CU = (50_000, 200_000, 1_400_000)


def measured(values):
    return {
        "median": statistics.median(values),
        "min": min(values),
        "max": max(values),
        "runs": len(values),
    }


def successful_runs(directory):
    runs = []
    for path in sorted(directory.glob("run-*.json")):
        exit_path = path.with_suffix(".exit")
        if exit_path.exists() and exit_path.read_text().strip() == "0":
            runs.append(json.loads(path.read_text()))
    return runs


def phase(run, name):
    return next(item for item in run["phases"] if item["phase"] == name)


def peak_gpu_mib(path):
    values = []
    if not path.exists():
        return None
    for row in csv.reader(path.open()):
        if len(row) > 1:
            values.append(float(row[1].strip()))
    return max(values) if values else None


def peak_rss_kib(path):
    by_timestamp = {}
    if not path.exists():
        return None
    for row in csv.reader(path.open()):
        if len(row) >= 3:
            timestamp, rss = row[0], int(row[2])
            by_timestamp[timestamp] = by_timestamp.get(timestamp, 0) + rss
    return max(by_timestamp.values()) if by_timestamp else None


def time_rss_kib(path):
    if not path.exists():
        return None
    match = re.search(r"Maximum resident set size \(kbytes\): (\d+)", path.read_text())
    return int(match.group(1)) if match else None


def linear_fit(points):
    xs = [float(point[0]) for point in points]
    ys = [float(point[1]) for point in points]
    x_mean = statistics.mean(xs)
    y_mean = statistics.mean(ys)
    denominator = sum((x - x_mean) ** 2 for x in xs)
    slope = sum((x - x_mean) * (y - y_mean) for x, y in zip(xs, ys)) / denominator
    intercept = y_mean - slope * x_mean
    return intercept, slope


def main():
    root = Path(sys.argv[1])
    output = Path(sys.argv[2]) if len(sys.argv) > 2 else root / "summary.json"
    fixture_hashes = []
    for line in (root / "fixture-sha256.txt").read_text().splitlines():
        digest, path = line.split(maxsplit=1)
        fixture_hashes.append({"sha256": digest, "file": Path(path).name})
    summary = {
        "schema": "northstar-transaction-proof-scaled-summary-v1",
        "environment": (root / "environment.txt").read_text(),
        "fixture_sha256": fixture_hashes,
        "profiles": {},
    }

    projection_series = {
        "sp1_execute_cycles": [],
        "sp1_execute_wall_ms": [],
        "sp1_core_prove_ms": [],
        "sp1_groth16_prove_and_wrap_ms": [],
        "custom_constraint_generation_ms": [],
        "custom_setup_ms": [],
        "custom_prove_ms": [],
    }

    for profile_name in PROFILES:
        profile_dir = root / profile_name
        custom_runs = successful_runs(profile_dir / "custom")
        execute_runs = successful_runs(profile_dir / "sp1-execute")
        core_runs = successful_runs(profile_dir / "sp1-core")
        groth16_runs = successful_runs(profile_dir / "sp1-groth16")
        if not all((custom_runs, execute_runs, core_runs, groth16_runs)):
            raise SystemExit(f"incomplete successful runs for {profile_name}")

        custom = {
            field: measured([run[field] for run in custom_runs]) for field in CUSTOM_FIELDS
        }
        fixed_custom = {
            field: custom_runs[0][field]
            for field in (
                "iterations",
                "opcodes",
                "alu_rows",
                "branch_rows",
                "load_rows",
                "store_rows",
                "call_rows",
                "exit_rows",
                "syscalls",
                "accounts",
                "account_data_bytes",
                "executed_units",
            )
        }
        custom["peak_process_rss_kib"] = measured(
            [
                time_rss_kib(profile_dir / "custom" / f"run-{index}.time.txt")
                for index in range(1, len(custom_runs) + 1)
            ]
        )
        custom["trace_sha256"] = sorted({run["trace_sha256"] for run in custom_runs})
        custom["witness_sha256"] = sorted({run["witness_sha256"] for run in custom_runs})
        custom["fixed"] = fixed_custom

        execute_phases = [phase(run, "execute") for run in execute_runs]
        execute = {
            key: measured([item[key] for item in execute_phases])
            for key in ("wall_ms", "cycles", "gas", "syscalls", "touched_memory_addresses")
        }
        labels = sorted(execute_phases[0]["cycle_tracker"])
        execute["cycle_tracker"] = {
            label: measured([item["cycle_tracker"][label] for item in execute_phases])
            for label in labels
        }

        def proof_summary(mode, runs):
            mode_phases = [phase(run, mode) for run in runs]
            setup_phases = [phase(run, "setup") for run in runs]
            fields = (
                ("core", ("prove_ms", "verify_ms", "artifact_bytes"))
                if mode == "core"
                else (
                    "groth16",
                    ("prove_and_wrap_ms", "verify_ms", "onchain_proof_bytes", "artifact_bytes"),
                )
            )[1]
            result = {"setup_ms": measured([item["wall_ms"] for item in setup_phases])}
            result.update({key: measured([item[key] for item in mode_phases]) for key in fields})
            return result

        core = proof_summary("core", core_runs)
        groth16 = proof_summary("groth16", groth16_runs)

        for mode, runs, target in (
            ("execute", execute_runs, execute),
            ("core", core_runs, core),
            ("groth16", groth16_runs, groth16),
        ):
            directory = profile_dir / f"sp1-{mode}"
            gpu_values = [peak_gpu_mib(directory / f"run-{index}.gpu.csv") for index in range(1, len(runs) + 1)]
            rss_values = [peak_rss_kib(directory / f"run-{index}.rss.csv") for index in range(1, len(runs) + 1)]
            target["peak_gpu_memory_mib"] = measured([value for value in gpu_values if value is not None])
            target["peak_combined_rss_kib"] = measured([value for value in rss_values if value is not None])

        rows = fixed_custom["executed_units"] - 10
        summary["profiles"][profile_name] = {
            "rows": rows,
            "custom": custom,
            "sp1_execute": execute,
            "sp1_core": core,
            "sp1_groth16": groth16,
        }
        projection_series["sp1_execute_cycles"].append((rows, execute["cycles"]["median"]))
        projection_series["sp1_execute_wall_ms"].append((rows, execute["wall_ms"]["median"]))
        projection_series["sp1_core_prove_ms"].append((rows, core["prove_ms"]["median"]))
        projection_series["sp1_groth16_prove_and_wrap_ms"].append(
            (rows, groth16["prove_and_wrap_ms"]["median"])
        )
        projection_series["custom_constraint_generation_ms"].append(
            (rows, custom["constraint_generation_ms"]["median"])
        )
        projection_series["custom_setup_ms"].append((rows, custom["setup_ms"]["median"]))
        projection_series["custom_prove_ms"].append((rows, custom["prove_ms"]["median"]))

    projections = {}
    for name, points in projection_series.items():
        intercept, slope = linear_fit(points)
        projections[name] = {
            "method": "ordinary least squares over measured 999/9999/99999-row loop fixtures",
            "intercept": intercept,
            "per_row": slope,
            "targets": {
                str(cu): max(0.0, intercept + slope * max(1, cu - 10)) for cu in TARGET_CU
            },
        }
    summary["projections"] = {
        "classification": "projected",
        "assumption": "benchmark loop uses executed CU = VM rows + 10; transaction mixes can differ materially",
        "series": projections,
        "solana_groth16_verifier_cu_baseline": 108_915,
    }
    output.write_text(json.dumps(summary, indent=2) + "\n")
    print(output)


if __name__ == "__main__":
    main()

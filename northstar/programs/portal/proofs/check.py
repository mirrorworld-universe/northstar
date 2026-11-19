#!/usr/bin/env python3
"""Check the production bond planner, then verify that three isolated controls fail."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

PORTAL = Path(__file__).resolve().parents[1]
DRIVER = Path("proofs/checkpoint_bond.rs")
PLANNER = Path("src/bond.rs")
VERUS_COMMIT = "8bf68ee741df833ea5d0fe6a2255126fdea87e47"
Z3_VERSION = "4.16.0"
FLAGS = ["--crate-type", "lib", "--edition=2021", "--no-cheating", "--output-json", "--time-expanded"]
CONTROLS = {
    "missing_debit": ("checkpoint: before.checkpoint - bond,", "checkpoint: before.checkpoint,"),
    "missing_rent_floor": ("if before.checkpoint < minimum {", "if before.checkpoint < bond {"),
    "nonterminal_status": ("Resolution::Slash => CheckpointBondStatus::Slashed,",
                           "Resolution::Slash => CheckpointBondStatus::Locked,"),
}


def require(condition, message):
    if not condition:
        raise SystemExit(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--verus", type=Path, required=True, help="verus executable from the pinned release")
    parser.add_argument("--logs", type=Path, required=True, help="directory for raw output and summary.json")
    args = parser.parse_args()
    verus = args.verus.resolve()
    z3 = verus.with_name("z3")
    info = json.loads(subprocess.check_output([verus, "--version", "--output-json"], text=True))["verus"]
    z3_info = subprocess.check_output([z3, "--version"], text=True).strip()
    require(info["commit"] == VERUS_COMMIT and info["profile"] == "release", f"Unexpected Verus: {info}")
    require(z3_info.split()[:3] == ["Z3", "version", Z3_VERSION], f"Unexpected solver: {z3_info}")
    env = dict(os.environ, VERUS_Z3_PATH=str(z3))
    args.logs.mkdir(parents=True, exist_ok=True)
    summary_path = args.logs / "summary.json"
    summary_path.unlink(missing_ok=True)  # Never leave a stale pass for an incomplete run.
    summary = {
        "verus": info["version"], "verus_commit": VERUS_COMMIT, "z3": z3_info, "flags": FLAGS,
        "source_sha256": {str(p): hashlib.sha256((PORTAL / p).read_bytes()).hexdigest() for p in (PLANNER, DRIVER)},
        "runs": {},
    }

    def check(name, root, *, positive):
        proc = subprocess.run([verus, *FLAGS, DRIVER], cwd=root, env=env, text=True,
                              capture_output=True, timeout=120, check=False)
        (args.logs / f"{name}.json").write_text(proc.stdout)
        (args.logs / f"{name}.stderr").write_text(proc.stderr)
        try:
            result = json.loads(proc.stdout)["verification-results"]
        except (KeyError, ValueError):
            sys.exit(f"{name}: no verifier result. See {args.logs}/{name}.stderr")
        whole_crate = result["is-verifying-entire-crate"] and not result["encountered-vir-error"]
        if positive:
            ok = (proc.returncode == 0 and result["success"] and result["errors"] == 0
                  and not result["encountered-error"] and result["verified"] >= 3)
        else:
            diag = proc.stderr.lower()
            ok = (proc.returncode == 1 and not result["success"] and result["errors"] == 1
                  and "error: postcondition not satisfied" in diag and "ensures crate::bond_spec::correct_result" in diag
                  and not any(term in diag for term in ("resource limit", "timeout", "timed out", "unknown result")))
        require(whole_crate and ok, f"{name}: unexpected verifier result. Inspect {args.logs}/{name}.stderr")
        verdict = "verified" if positive else "postcondition rejected as expected"
        print(f"{name}: {verdict} ({result['verified']} verified, {result['errors']} errors)")
        summary["runs"][name] = {"verdict": verdict, "verified": result["verified"], "errors": result["errors"]}

    check("production", PORTAL, positive=True)
    source = (PORTAL / PLANNER).read_text()
    with tempfile.TemporaryDirectory(prefix="bond-proof-controls-") as tmp:
        root = Path(tmp)
        (root / PLANNER.parent).mkdir()
        (root / DRIVER.parent).mkdir()
        shutil.copyfile(PORTAL / DRIVER, root / DRIVER)
        for name, (old, new) in CONTROLS.items():
            require(source.count(old) == 1, f"{name}: update the control to match the reviewed source")
            (root / PLANNER).write_text(source.replace(old, new))
            check(name, root, positive=False)
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")


if __name__ == "__main__":
    main()

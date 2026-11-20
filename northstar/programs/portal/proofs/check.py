#!/usr/bin/env python3
"""Check production accounting and its reachability and mutation controls with pinned Verus/Z3."""

import argparse
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

PROGRAMS = Path(__file__).resolve().parents[2]
VERUS_COMMIT = "8bf68ee741df833ea5d0fe6a2255126fdea87e47"
Z3_VERSION = "4.16.0"
FLAGS = ["--crate-type", "lib", "--edition=2021", "--no-cheating", "--output-json", "--time-expanded"]


@dataclass(frozen=True)
class Suite:
    package: str
    driver: str
    planner: str
    count: int
    functions: tuple[str, ...]
    # The diagnostic must point to this postcondition, not a build or resource error.
    controls: dict[str, tuple[str, str, str]]


BOND = "ensures crate::bond_spec::correct_result"
CLASSIFY = "ensures crate::receipt_spec::classification"
FUND = "ensures crate::receipt_spec::funding"
TRANSFER = "ensures crate::bridge_spec::transfer"
WITHDRAWAL = "ensures crate::bridge_spec::withdrawal"
AUTH = "ensures crate::auth_spec::transition"
SUITES = {
    "bond": Suite("portal", "checkpoint_bond", "bond", 8,
        ("bond::plan_resolution", "bond_spec::cannot_pay_twice", "bond_spec::both_actions_reachable"), {
            "missing_debit": ("checkpoint: before.checkpoint - bond,", "checkpoint: before.checkpoint,", BOND),
            "missing_rent_floor": ("if before.checkpoint < minimum {", "if before.checkpoint < bond {", BOND),
            "nonterminal_status": ("Resolution::Slash => CheckpointBondStatus::Slashed,",
                                   "Resolution::Slash => CheckpointBondStatus::Locked,", BOND),
        }),
    "receipt": Suite("portal", "sol_receipt", "receipt", 9,
        ("receipt::same_binding", "receipt::payout_delta", "receipt::within_deposit_cap", "receipt::plan_payment",
         "receipt_spec::settled_update_cannot_pay_twice", "receipt_spec::reachable_receipts"), {
            "wrong_binding": ("&& a.bump == b.bump\n}", "&& a.bump != b.bump\n}", "ensures result == ((a.session@"),
            "exact_retry_is_update": ("&& next.withdrawn <= old.withdrawn {", "&& next.withdrawn < old.withdrawn {", CLASSIFY),
            "reject_exact_delta": ("if delta != claimed {", "if delta >= claimed {", CLASSIFY),
            "wrong_rent_boundary": ("if next.balance > remaining {", "if next.balance >= remaining {", FUND),
            "missing_debit": ("receipt: before.receipt - payout,", "receipt: before.receipt,", FUND),
            "reject_exact_cap": ("Some(sum) => sum <= total,", "Some(sum) => sum < total,",
                                 "ensures result == (balance as int"),
        }),
    "authorization": Suite("portal", "token_authorization", "withdrawal_auth", 3,
        ("withdrawal_auth::mark_consumed", "auth_spec::matching_once_for_every_key"), {
            "wrong_pre_state": ("if *consumed", "if !*consumed", AUTH),
            "wrong_digest_comparison": ("stored.tuple_hash.as_slice() != requested.tuple_hash.as_slice()",
                                        "stored.tuple_hash.as_slice() == requested.tuple_hash.as_slice()", AUTH),
            "missing_consumption": ("*consumed = true;", "*consumed = false;", AUTH),
        }),
    "bridge": Suite("token-bridge", "token_accounting", "accounting", 5,
        ("accounting::transfer_access", "accounting::transfer_balances", "accounting::withdrawal_target",
         "bridge_spec::retry_cannot_request_payment", "bridge_spec::reachable_steps"), {
            "wrong_signer_flag": ("if !signed {", "if signed {", "ensures crate::bridge_spec::access"),
            "wrong_mint_comparison": ("source.mint.as_slice() != destination.mint.as_slice()",
                                      "source.mint.as_slice() == destination.mint.as_slice()", TRANSFER),
            "missing_credit": ("Ok((debit, credit))", "Ok((debit, destination.amount))", TRANSFER),
            "equal_counter_not_replay": ("if proposed <= stored {", "if proposed < stored {", WITHDRAWAL),
            "reject_exact_step": ("if proposed != next ||", "if proposed == next ||", WITHDRAWAL),
            "reject_exact_vault_cap": ("|| next > deposited {", "|| next >= deposited {", WITHDRAWAL),
        }),
    "custody": Suite("token-bridge", "vault_custody", "custody", 11,
        ("custody::empty", "custody::prepare", "custody::Expected::confirm", "custody_spec::initial_backing",
         "custody_spec::backing_step", "custody_spec::cannot_post_one_sided", "custody_spec::donation_cannot_reduce_backing",
         "custody_spec::round_trip_with_surplus"), {
            "nonzero_initial_claim": ("deposited: 0,", "deposited: 1,", "ensures r.deposited == 0"),
            "wrong_deposit_posting": ("withdrawn: old.withdrawn,", "withdrawn: deposited,", "ensures crate::custody_spec::posting"),
            "wrong_payout_capacity": ("if withdrawn > old.deposited {", "if withdrawn >= old.deposited {", "ensures crate::custody_spec::posting"),
            "wrong_peer_observation": ("|| self.amounts.peer != actual.peer", "|| self.amounts.peer == actual.peer", "ensures crate::custody_spec::observed"),
            "wrong_vault_observation": ("if self.amounts.vault != actual.vault", "if self.amounts.vault == actual.vault", "ensures crate::custody_spec::observed"),
        }),
    "credit": Suite("token-bridge", "delegated_credit", "deposit_credit", 9,
        ("deposit_credit::record", "deposit_credit::retire", "deposit_credit::advance", "credit_spec::partition_independent",
         "credit_spec::two_requests_share_one_budget", "credit_spec::immediate_and_deferred_agree", "credit_spec::reachable_credit"), {
            "wrong_credit_branch": ("if delegated {", "if !delegated {", "ensures crate::credit_spec::deposit"),
            "missing_retirement": ("holding.checked_sub(amount)", "Some(holding)", "ensures crate::credit_spec::debit"),
            "equal_cursor_reapplied": ("target <= cursor {", "target < cursor {", "ensures crate::credit_spec::claim"),
            "wrong_origin_boundary": ("if cursor < origin {", "if cursor <= origin {", "ensures crate::credit_spec::claim"),
            "wrong_persisted_cursor": ("cursor: target,", "cursor,", "ensures crate::credit_spec::claim"),
        }),
    "batch": Suite("portal", "batch_settlement", "lamport_batch", 11,
        ("lamport_batch::empty", "lamport_batch::add", "lamport_batch::finish", "lamport_batch::guard",
         "batch_spec::bounded_batch", "batch_spec::guarded_replay", "batch_spec::reachable_surplus"), {
            "wrong_rent_boundary": ("if e.target < e.floor {", "if e.target <= e.floor {", "ensures crate::batch_spec::row"),
            "wrong_target_sum": (".checked_add(e.target as u128)", ".checked_add(e.current as u128)", "ensures crate::batch_spec::row"),
            "lost_vault_credit": ("Ok(Effect::Write(Some(next)))", "Ok(Effect::Write(Some(v)))", "ensures crate::batch_spec::effect"),
            "wrong_retry_guard": ("if actual.as_slice() == next.as_slice() {", "if actual.as_slice() != next.as_slice() {", "ensures crate::batch_spec::decision"),
        }),
    "range": Suite("portal", "dispute_geometry", "dispute_range", 11,
        ("dispute_range::respond", "dispute_range::isolated", "dispute_range::select_half", "dispute_range::next_deadline",
         "dispute_range::live", "range_spec::both_halves", "range_spec::four_rounds", "range_spec::deadline_partition",
         "range_spec::reachable_rounds"), {
            "wrong_midpoint": ("+ width / 2", "+ width / 3", "ensures crate::range_spec::response"),
            "missing_round": ("round.checked_add(1)", "round.checked_add(0)", "ensures crate::range_spec::selection"),
            "wrong_half": ("let next = if upper {", "let next = if !upper {", "ensures crate::range_spec::selection"),
            "wrong_deadline_cap": ("Some(if end < hard { end } else { hard })", "Some(end)", "ensures crate::range_spec::deadline"),
            "includes_deadline": ("    now < turn && now < hard\n", "    now <= turn && now < hard\n", "ensures r == (now < turn"),
        }),
    "prefix": Suite("portal", "continuity_prefix", "ingress_prefix", 6,
        ("ingress_prefix::next_sequence", "ingress_prefix::permits", "prefix_spec::cannot_apply_advancing_prefix_twice",
         "prefix_spec::adjacent_budgets_add", "prefix_spec::reachable_prefixes"), {
            "sequence_stalls": ("count.checked_add(1)", "Some(count)", "ensures crate::prefix_spec::increment"),
            "overrestrict_step_budget": ("    used <= steps\n", "    used < steps\n", "ensures r == crate::prefix_spec::allowed"),
            "wrong_tip_root": ("end.root.as_slice() == tip.root.as_slice()", "end.root.as_slice() != tip.root.as_slice()", "ensures r == crate::prefix_spec::allowed"),
            "wrong_consumed_root": ("start.root.as_slice() != consumed.root.as_slice()", "start.root.as_slice() == consumed.root.as_slice()", "ensures r == crate::prefix_spec::allowed"),
        }),
}


def require(condition, message):
    if not condition:
        raise SystemExit(message)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite", choices=SUITES, default="bond")
    parser.add_argument("--verus", type=Path, required=True, help="verus executable from the pinned release")
    parser.add_argument("--logs", type=Path, required=True, help="a separate directory per suite for raw output and summary.json")
    args = parser.parse_args()
    suite = SUITES[args.suite]
    package = PROGRAMS / suite.package
    driver = Path("proofs", suite.driver + ".rs")
    planner = Path("src", suite.planner + ".rs")
    args.logs.mkdir(parents=True, exist_ok=True)
    summary_path = args.logs / "summary.json"
    summary_path.unlink(missing_ok=True)  # Never leave a stale pass for an incomplete run.
    verus = args.verus.resolve()
    z3 = verus.with_name("z3")
    info = json.loads(subprocess.check_output([verus, "--version", "--output-json"], text=True))["verus"]
    z3_info = subprocess.check_output([z3, "--version"], text=True).strip()
    require(info["commit"] == VERUS_COMMIT and info["profile"] == "release", f"Unexpected Verus: {info}")
    require(z3_info.split()[:3] == ["Z3", "version", Z3_VERSION], f"Unexpected solver: {z3_info}")
    env = dict(os.environ, VERUS_Z3_PATH=str(z3))
    summary = {
        "verus": info["version"], "verus_commit": VERUS_COMMIT, "z3": z3_info, "flags": FLAGS,
        "source_sha256": {str(p): hashlib.sha256((package / p).read_bytes()).hexdigest() for p in (planner, driver)},
        "runs": {},
    }

    def check(name, root, expected_failure=None):
        proc = subprocess.run([verus, *FLAGS, driver], cwd=root, env=env, text=True,
                              capture_output=True, timeout=120, check=False)
        (args.logs / f"{name}.json").write_text(proc.stdout)
        (args.logs / f"{name}.stderr").write_text(proc.stderr)
        try:
            data = json.loads(proc.stdout)
            result = data["verification-results"]
        except (KeyError, ValueError):
            raise SystemExit(f"{name}: no verifier result. See {args.logs}/{name}.stderr") from None
        diag = proc.stderr.lower()
        require(result["is-verifying-entire-crate"] and not result["encountered-vir-error"], f"{name}: incomplete crate")
        require(not any(term in diag for term in ("resource limit", "timeout", "timed out", "unknown result")),
                f"{name}: inconclusive verifier run")
        if expected_failure is None:
            ok = (proc.returncode == 0 and result["success"] and not result["encountered-error"]
                  and result["errors"] == 0 and result["verified"] == suite.count)
            for function in suite.functions:
                record = data["func-details"].get(f"{suite.driver}::{function}")
                require(record is not None and not record["failed_proof_notes"], f"{name}: missing or failed {function}")
            verdict = "verified"
        else:
            ok = (proc.returncode == 1 and not result["success"] and result["encountered-error"]
                  and result["errors"] == 1 and result["verified"] == suite.count - 1
                  and "error: postcondition not satisfied" in diag and f"src/{suite.planner}.rs:" in diag
                  and expected_failure.lower() in diag)
            verdict = "postcondition rejected as expected"
        require(ok, f"{name}: unexpected verifier result. Inspect {args.logs}/{name}.stderr")
        print(f"{name}: {verdict} ({result['verified']} verified, {result['errors']} errors)")
        summary["runs"][name] = {"verdict": verdict, "verified": result["verified"], "errors": result["errors"]}

    check("production", package)
    source = (package / planner).read_text()
    with tempfile.TemporaryDirectory(prefix=f"{args.suite}-proof-controls-") as tmp:
        root = Path(tmp)
        (root / planner.parent).mkdir()
        (root / driver.parent).mkdir()
        shutil.copyfile(package / driver, root / driver)
        for name, (old, new, expected) in suite.controls.items():
            require(source.count(old) == 1, f"{name}: update the control to match the reviewed source")
            (root / planner).write_text(source.replace(old, new))
            check(name, root, expected)
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")


if __name__ == "__main__":
    main()

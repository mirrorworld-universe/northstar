#!/usr/bin/env python3
"""Audit real Coordinator journals/wires; preserve missing timing and atomic-state gaps.

This tool validates local consistency, not cryptography or authenticated acceptance.
"""

import argparse
import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import sys

SPEC = importlib.util.spec_from_file_location("coordinator_contract", Path(__file__).with_name("vrf-coordinator-fixtures.py"))
contract = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(contract)
Invalid = contract.Invalid
require = contract.require
COMMIT = contract.COMMIT
MAX_JOURNAL = 256 * 1024 * 1024
MAX_RECORD = 2 * 1024 * 1024
ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"
CURVE_FEATURE = "7rcw5UtqgDTBBv2EcynNfYckgdAaH1MAsCjKgXMkN7Ri"
COMPUTE_BUDGET = "ComputeBudget111111111111111111111111111111"


def sha(raw):
    return hashlib.sha256(raw).hexdigest()


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode()


def b58(raw):
    number = int.from_bytes(raw, "big")
    out = ""
    while number:
        number, rest = divmod(number, 58)
        out = ALPHABET[rest] + out
    return "1" * (len(raw) - len(raw.lstrip(b"\0"))) + out


def byte_array(value, length=None, label="bytes"):
    require(type(value) is list and all(type(b) is int and 0 <= b <= 255 for b in value), f"{label}: invalid byte array")
    require(length is None or len(value) == length, f"{label}: length drift")
    return bytes(value)


def file_identity(path):
    path = Path(path)
    size = path.stat().st_size
    require(path.is_file() and size <= MAX_JOURNAL, "artifact missing/oversize")
    hasher = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            hasher.update(block)
    return dict(file=path.name, bytes=size, sha256=hasher.hexdigest())


def read_journal(path):
    path = Path(path)
    require(path.stat().st_size <= MAX_JOURNAL, "journal exceeds byte budget")
    rows = []
    with path.open("rb") as stream:
        while True:
            line = stream.readline(MAX_RECORD + 1)
            if not line:
                break
            require(len(line) <= MAX_RECORD and line.endswith(b"\n"), "oversize or incomplete trailing journal record")
            row = json.loads(line, object_pairs_hook=contract.strict_object, parse_constant=contract.reject_constant)
            require(type(row) is dict and type(row.get("kind")) is str, "journal record kind missing")
            rows.append(row)
            require(len(rows) <= 100000, "too many journal records")
    require(rows and rows[0]["kind"] == "setup" and rows[-1]["kind"] in ("summary", "atomic_capture_end"), "complete setup/summary required")
    require(sum(r["kind"] == "setup" for r in rows) == 1 and sum(r["kind"] == "summary" for r in rows) == 1,
            "duplicate setup/summary")
    return rows


def parse_wire(raw):
    require(len(raw) <= 1232, "signed wire exceeds packet bound")
    cursor = 0

    def take(count):
        nonlocal cursor
        require(count >= 0 and cursor + count <= len(raw), "truncated signed legacy wire")
        result = raw[cursor:cursor + count]
        cursor += count
        return result

    def short():
        value, start = 0, cursor
        for shift in (0, 7, 14):
            b = take(1)[0]
            require(shift != 14 or b <= 3, "shortvec overflow")
            value |= (b & 127) << shift
            if b < 128:
                require(cursor - start == 1 or b != 0, "noncanonical shortvec")
                return value
        raise Invalid("shortvec overflow")

    signature_count = short()
    require(0 < signature_count <= 16, "signature count bound")
    signatures = [take(64) for _ in range(signature_count)]
    header = take(3)
    require(header[0] & 128 == 0, "versioned wire unsupported by this capture profile")
    require(header[0] == len(signatures) and 0 < len(signatures) <= 16, "signature/header count mismatch")
    key_count = short()
    require(0 < key_count <= 32, "compiled account count bound")
    keys = [b58(take(32)) for _ in range(key_count)]
    require(header[1] <= header[0] <= len(keys) and header[2] <= len(keys) - header[0], "invalid account header")
    require(len(set(keys)) == len(keys), "duplicate compiled account key")
    recent_blockhash = b58(take(32))
    instructions = []
    instruction_count = short()
    require(0 < instruction_count <= 4, "instruction count bound")
    for _ in range(instruction_count):
        program = take(1)[0]
        account_count = short()
        require(account_count <= 32, "instruction account count bound")
        accounts = list(take(account_count))
        data = take(short())
        require(program < len(keys) and all(i < len(keys) for i in accounts), "compiled index out of range")
        instructions.append(dict(program=keys[program], accounts=accounts, data=data))
    require(cursor == len(raw), "trailing signed-wire bytes")
    return dict(signature=b58(signatures[0]), signature_count=len(signatures), header=list(header), keys=keys,
                instructions=instructions, recent_blockhash=recent_blockhash)


def classify(parsed, manifest):
    found = []
    for instruction_index, instruction in enumerate(parsed["instructions"]):
        if instruction["program"] == COMPUTE_BUDGET:
            require(instruction["data"] and instruction["data"][0] in (2, 3), "unsupported compute-budget instruction")
            continue
        matches = [(name, value) for name, value in manifest["instructions"].items()
                   if manifest["programs"][value["program"]]["id"] == instruction["program"]
                   and bytes.fromhex(value["discriminator_hex"]) == instruction["data"][:8]]
        require(len(matches) == 1, "unsupported program/discriminator in capture")
        name, definition = matches[0]
        fixed = len(definition["accounts"])
        extra_limit = 16 if name in ("fulfill", "make_request") else 0
        require(fixed <= len(instruction["accounts"]) <= fixed + extra_limit, f"{name}: instruction account length drift")
        required, readonly_signed, readonly_unsigned = parsed["header"]
        for index, declared in zip(instruction["accounts"], definition["accounts"]):
            signer = index < required
            writable = index < required - readonly_signed or required <= index < len(parsed["keys"]) - readonly_unsigned
            require(not declared["signer"] or signer, f"{name}: required signer flag missing")
            require(not declared["writable"] or writable, f"{name}: required writable flag missing")
        found.append(dict(name=name, instruction_index=instruction_index, **instruction))
    require(len(found) == 1, "one non-budget outer instruction required")
    return found[0]


def inner_calls(transaction, keys):
    result = []
    for group in transaction["observation"]["meta"].get("innerInstructions") or []:
        for row in group["instructions"]:
            require(type(row.get("programIdIndex")) is int and 0 <= row["programIdIndex"] < len(keys), "inner program index invalid")
            accounts = row.get("accounts", [])
            require(all(type(i) is int and 0 <= i < len(keys) for i in accounts), "inner account index invalid")
            result.append(dict(program=keys[row["programIdIndex"]], data=contract.base58_decode(row["data"]),
                               accounts=[keys[i] for i in accounts]))
    return result


def audit_journal(path, wires_root=None, manifest=None, receipt=None):
    manifest = contract.validate_manifest(manifest if manifest is not None else
                                          contract.load_json(contract.FIXTURES / "compatibility-manifest.json"), contract.FIXTURES)
    path = Path(path)
    root = Path(wires_root or path.with_suffix(".transactions")).resolve(strict=True)
    rows = read_journal(path)
    setup, summary = rows[0], next(r for r in rows if r["kind"] == "summary")
    require(setup.get("source_revision") == COMMIT, "library/source pin drift")
    require(setup.get("level") == "local-er-diagnostic", "unsupported runtime provenance level")
    require(setup.get("delegated_handoff") is False and setup.get("production_verifier") is False, "legacy capture provenance drift")
    require(type(setup.get("cycles")) is int and 1 <= setup["cycles"] <= 2000, "cycle count invalid")
    scenario = setup.get("scenario")
    require(scenario in ("success", "callback_failure"), "unsupported capture scenario")
    require(setup.get("execution_proof") is False, "runtime journal cannot claim an execution proof")
    known = {"setup", "program", "runtime", "accounts_before", "accounts_after", "wire_transaction", "submission",
             "transaction", "request", "invariant", "callback", "summary", "candidate_capture", "atomic_capture_start", "atomic_capture_end", "genesis_identity",
             "runtime_starting", "runtime_started", "runtime_stopped"}
    require(all(r["kind"] in known for r in rows), "unknown journal record kind")

    def records(kind, key="label"):
        selected = [r for r in rows if r["kind"] == kind]
        require(len({r[key] for r in selected}) == len(selected), f"duplicate {kind} {key}")
        return {r[key]: r for r in selected}

    programs = records("program", "program_id")
    require(set(programs) == {p["id"] for p in manifest["programs"].values()}, "program ID set drift")
    require(all(re.fullmatch("[0-9a-f]{64}", p["elf_sha256"]) and p["elf_bytes"] > 0 for p in programs.values()), "program binary identity invalid")
    runtimes = [r for r in rows if r["kind"] == "runtime"]
    require(len(runtimes) == 1, "runtime metadata missing/duplicate")
    runtime = runtimes[0]
    require([CURVE_FEATURE, 0] in runtime.get("active_features", []), "required curve feature missing")
    transactions, wires, submissions = records("transaction"), records("wire_transaction"), records("submission")
    before, after = records("accounts_before"), records("accounts_after")
    require(set(transactions) == set(wires) == set(submissions) == set(before) == set(after), "omitted transaction/wire/submission/account observation")
    atomic_collector = any(r["kind"] == "atomic_capture_start" for r in rows)
    default_controls = ("embedded_expected" if "invariant_invalid_proof_preserves_pending_request" in transactions
                        else "separate_controls") if atomic_collector else "embedded_expected"
    controls = setup.get("negative_control_policy", default_controls)
    require(controls in ("embedded_expected", "separate_controls"), "unknown negative-control policy")
    expected_total = 4 + setup["cycles"] * 3 + (1 if controls == "embedded_expected" else 0)
    require(len(transactions) == expected_total, "transaction count drift or omitted failure")
    decoded, attempts, rejected, identities, inodes = {}, [], [], [], set()
    for label, transaction in transactions.items():
        wire_record, submission = wires[label], submissions[label]
        require(submission["result"] == f"Ok({submission['signature']})", f"{label}: submission was not observed successful")
        wire_path = root / wire_record["file"]
        files = contract.checked_files([dict(path=wire_record["file"], bytes=wire_record["bytes"], sha256=wire_record["sha256"])], root)
        stat = files[wire_record["file"]].stat()
        inode = (stat.st_dev, stat.st_ino)
        require(inode not in inodes, "wire file alias")
        inodes.add(inode)
        raw = wire_path.read_bytes()
        require(sha(raw) == wire_record["sha256"] and len(raw) == wire_record["bytes"], "wire changed while reading")
        parsed = parse_wire(raw)
        require(parsed["signature"] == wire_record["signature"] == transaction["signature"] == submission["signature"], "wire/signature drift")
        observation = transaction["observation"]
        require(observation["transaction"][1] == "base64" and base64.b64decode(observation["transaction"][0], validate=True) == raw, "wire/RPC bytes mismatch")
        outer = classify(parsed, manifest)
        decoded[label] = dict(parsed=parsed, outer=outer, inner=inner_calls(transaction, parsed["keys"]))
        error, expected = observation["meta"]["err"], transaction["expected_error"]
        if expected is None:
            require(error is None, f"{label}: unexpected transaction failure")
        else:
            require(type(expected) is int and error is not None
                    and error.get("InstructionError") == [outer["instruction_index"], {"Custom": expected}], "expected error mismatch")
            rejected.append(dict(label=label, code=expected, wire_sha256=sha(raw)))
        identities.append(dict(label=label, bytes=len(raw), sha256=sha(raw), instruction=outer["name"]))
        attempts.append(dict(label=label, status="success" if error is None else "rejected", expected_error=expected,
                             submitted_ns=submission.get("submitted_ns", submission.get("monotonic_ns")),
                             observed_ns=transaction.get("observed_ns", transaction.get("monotonic_ns")), wire_sha256=sha(raw)))
    require(len({row["sha256"] for row in identities}) == len(identities), "duplicate signed-wire bytes")
    expected_rejected = [("invariant_invalid_proof_preserves_pending_request", 6003)] if controls == "embedded_expected" else []
    if scenario == "callback_failure":
        expected_rejected.append(("0:fulfill", 6001))
    require([(r["label"], r["code"]) for r in rejected] == expected_rejected, "omitted/extra negative-control failure")
    invariants = [r for r in rows if r["kind"] == "invariant"]
    require(len(invariants) == (1 if controls == "embedded_expected" else 0), "negative preservation observation omitted/extra")
    for invariant in invariants:
        require(invariant.get("name") == "invalid_proof_preserves_pending_request" and invariant.get("expected_error") == 6003
                and invariant.get("request_unchanged") is True and invariant.get("deliveries") == 0
                and invariant.get("fee_paid") == 0, "negative preservation observation drift")
    requests, callbacks = records("request", "cycle"), records("callback", "cycle")
    require(list(requests) == list(range(setup["cycles"])), "request cycle omitted/duplicated/out of order")
    require(len({r["address"] for r in requests.values()}) == len(requests), "duplicate request PDA")
    require(set(callbacks) == (set(requests) if scenario == "success" else set()), "lost/duplicate callback observations")
    coordinator = manifest["programs"]["coordinator"]["id"]
    verifier = manifest["programs"]["reference_verifier"]["id"]
    consumer = manifest["programs"]["test_consumer"]["id"]
    cycles = []
    for cycle, request in requests.items():
        pending = byte_array(request["account"]["data"], 434, "pending request")
        proof = request["proof"]
        key, pi, output = (byte_array(proof[name], size, name) for name, size in (("key", 32), ("proof", 80), ("output", 64)))
        address = contract.base58_decode(request["address"])
        require(len(address) == 32 and b58(byte_array(request["account"]["owner"], 32, "request owner")) == coordinator, "request address/owner drift")
        require(pending[:8] == hashlib.sha256(b"account:RequestAccount").digest()[:8]
                and pending[8:11] == b"\x01\x00\x00" and pending[227] == 1, "request layout/mode/algorithm drift")
        alpha = hashlib.sha256(b"sonic-vrf/v1" + contract.base58_decode(coordinator) + address + pending[83:115] + pending[115:163]).digest()
        require(pending[163:195] == alpha and pending[195:227] == key, "request alpha/key binding drift")
        request_tx = decoded[f"{cycle}:make_request"]
        fulfill_tx = decoded[f"{cycle}:fulfill"]
        require(request_tx["outer"]["name"] == "make_request" and fulfill_tx["outer"]["name"] == "fulfill", "request/fulfill instruction order drift")
        require(request_tx["outer"]["data"][8:72] == pending[51:115], "request key/seed binding drift")
        request_accounts = [request_tx["parsed"]["keys"][i] for i in request_tx["outer"]["accounts"]]
        fulfill_accounts = [fulfill_tx["parsed"]["keys"][i] for i in fulfill_tx["outer"]["accounts"]]
        require(request_accounts[5] == request["address"] and request_accounts[3] == consumer,
                "request account order drift")
        require(fulfill_accounts[2] == request["address"] and fulfill_accounts[3] == b58(pending[276:308])
                and fulfill_accounts[5:7] == [consumer, verifier], "fulfill account order drift")
        require(int.from_bytes(pending[393:397], "little") == 1 and pending[429] == 1
                and int.from_bytes(pending[430:434], "little") == 0, "unsupported stored callback layout")
        callback_slot = b58(pending[397:429])
        require(fulfill_accounts[7:] == [callback_slot] and request_accounts[1] == callback_slot
                and request_accounts[4] == fulfill_accounts[1], "stored callback/config account order drift")
        require(fulfill_tx["outer"]["data"] == contract.discriminator("fulfill") + (80).to_bytes(4, "little") + pi, "fulfill proof bytes drift")
        verify_data = contract.discriminator("verify") + b"\x01" + key + alpha + (80).to_bytes(4, "little") + pi
        require(sum(i["program"] == verifier and i["data"] == verify_data and i["accounts"] == []
                    for i in fulfill_tx["inner"]) == 1, "verifier CPI input binding drift")
        callback = callbacks.get(cycle)
        if callback is not None:
            delivered = byte_array(callback["account"]["data"], label="consumer state")
            require(b58(byte_array(callback["account"]["owner"], 32, "consumer owner")) == consumer and len(delivered) >= 150, "consumer state layout/owner drift")
            require(delivered[:8] == hashlib.sha256(b"account:Slot").digest()[:8]
                    and delivered[40] == 0 and delivered[41:73] == address and delivered[73:137] == output
                    and int.from_bytes(delivered[145:149], "little") == 1, "consumer callback output/delivery binding drift")
            require(callback["deliveries"] == 1 and callback["request_closed"] is True and callback["rent_refunded"] is True
                    and callback["fee_paid"] == int.from_bytes(pending[236:244], "little"), "callback close/fee/refund drift")
            callback_data = pending[308:316] + address + output
            require(callback["slot"] == callback_slot and sum(i["program"] == consumer and i["data"] == callback_data
                    and i["accounts"] == [fulfill_accounts[4], callback_slot] for i in fulfill_tx["inner"]) == 1,
                    "callback CPI data/account order binding drift")
        cycles.append(dict(cycle=cycle, request_address=request["address"], alpha_hex=alpha.hex(),
                           request_wire_sha256=wires[f"{cycle}:make_request"]["sha256"],
                           fulfill_wire_sha256=wires[f"{cycle}:fulfill"]["sha256"],
                           callback_success=callback is not None,
                           submitted_ns=submissions[f"{cycle}:make_request"].get("submitted_ns", submissions[f"{cycle}:make_request"].get("monotonic_ns")),
                           callback_observed_ns=None if callback is None else callback.get("observed_ns", callback.get("monotonic_ns"))))
    require(summary.get("completed") == len(callbacks), "summary/observations count mismatch")
    if scenario == "success":
        require(summary.get("lost_callbacks") == 0 and summary.get("duplicate_callbacks") == 0, "summary lost/duplicate callback claim")
    else:
        require(summary.get("callback_failure_rejected") is True and summary.get("pending_request_preserved") is True
                and summary.get("request_closed") is False and summary.get("fee_paid") == 0, "rollback summary drift")
    journal_identity = file_identity(path)
    if receipt is not None:
        require(receipt["journal"]["sha256"] == journal_identity["sha256"] and receipt["journal"]["bytes"] == journal_identity["bytes"], "sealed journal receipt mismatch/omitted failure")
    missing = []
    clock = setup.get("clock_domain", setup.get("clock_id"))
    observed = lambda r: r.get("observed_ns", r.get("monotonic_ns"))
    domain = lambda r: r.get("clock_domain", r.get("clock_id"))
    require(clock is None or type(clock) is str and clock, "clock identity invalid")
    for row in rows:
        if observed(row) is not None:
            uint(observed(row), "capture timestamp", 2**63 - 1)
        if domain(row) is not None:
            require(type(domain(row)) is str and domain(row), "record clock identity invalid")
            require(clock is None or domain(row) == clock, "mixed collector clock domain")
        if "clock_id" in row and "clock_domain" in row:
            require(row["clock_id"] == row["clock_domain"], "conflicting clock aliases")
        if "monotonic_ns" in row and "observed_ns" in row:
            uint(row["monotonic_ns"], "monotonic timestamp", 2**63 - 1)
            require(row["monotonic_ns"] <= row["observed_ns"], "clock alias timestamp drift")
    if any("capture_sequence" in r for r in rows):
        start = rows[0].get("capture_sequence")
        require(type(start) is int and start in (0, 1)
                and all(type(r.get("capture_sequence")) is int for r in rows)
                and [r.get("capture_sequence") for r in rows] == list(range(start, start + len(rows))), "capture sequence drift")
    if clock is None or any(observed(r) is None or domain(r) is None for r in rows):
        missing.append("collector_wide_monotonic_clock")
    else:
        times = [observed(r) for r in rows]
        require(times == sorted(times), "capture clock drift")
        for attempt in attempts:
            require(type(attempt["submitted_ns"]) is int and 0 <= attempt["submitted_ns"] <= attempt["observed_ns"], "submission clock missing/drift")
    if any("observation_id" in row for row in rows):
        require(all(row.get("observation_id") == f"{clock}:journal:{row.get('capture_sequence')}" for row in rows), "journal observation identity drift")
    lifecycle = dict(runtime_constructor_ns=None, operational_startup_ready_ns=None, cold_start_ns=None, recovery_ns=None)
    starts = [r for r in rows if r["kind"] == "runtime_starting"]
    ready = [r for r in rows if r["kind"] == "runtime_started"]
    if starts or ready:
        require(len(starts) == len(ready) == 1, "runtime constructor markers missing/duplicate")
        start_ns, ready_ns = starts[0]["runtime_start_ns"], ready[0]["runtime_started_ns"]
        uint(start_ns, "runtime start", 2**63 - 1)
        uint(ready_ns, "runtime constructed", 2**63 - 1)
        require(start_ns <= ready_ns, "runtime constructor clock drift")
        lifecycle["runtime_constructor_ns"] = ready_ns - start_ns
    if runtime.get("atomic_execution_snapshots") is not True:
        missing.append("authoritative_atomic_prestate_and_computed_poststate")
    for key in ("runtime_revision", "runtime_binary_sha256", "er_genesis_hash", "feature_set_sha256"):
        if runtime.get(key) is None:
            missing.append(key)
    for key in ("guest_sha256", "verification_key_sha256", "relation_version", "domain", "profile_sha256"):
        missing.append(key)
    blockers = list(missing)
    if setup["cycles"] < 100 or scenario != "success":
        blockers.append("complete_100_success_er_window")
    if rejected:
        blockers.append("negative_controls_embedded_in_success_window" if scenario == "success" else "negative_control_only")
    return dict(schema_version="northstar-vrf-coordinator-journal-audit/v2", status="blocked", final_acceptance="not_established",
                library_commit=COMMIT, algorithm=contract.ALGORITHM, suite=1, compatibility_sha256=contract.EXPECTED_MANIFEST_SHA256,
                journal=journal_identity, scenario=scenario, provenance="actual_local_er_with_synthetic_genesis",
                signed_transactions=len(transactions), successful_callbacks=len(callbacks), expected_rejections=rejected,
                negative_control_policy=controls,
                signature_cryptography_verified=False, complete_execution_witness=False, clock_id=clock, lifecycle_observations=lifecycle,
                missing=sorted(set(missing)), blockers=sorted(set(blockers)),
                programs=[dict(program_id=p["program_id"], binary_sha256=p["elf_sha256"], bytes=p["elf_bytes"]) for p in programs.values()],
                active_features=runtime["active_features"], observed_feature_set_sha256=sha(canonical(runtime["active_features"])),
                cycles=cycles, attempts=attempts, wires=identities,
                trust_boundary="Recomputed local byte/observation consistency only. No signature/VRF/proof verification or capture authentication; no human acceptance.")


CAPTURE_DOMAIN = "northstar.er-vrf-candidate-capture.v1"
CANDIDATE_PROOF_DOMAIN = bytes(8) + b"northstar-er-step-v1" + bytes(2) + bytes((3, 1))
LOADER = "BPFLoaderUpgradeab1e11111111111111111111111"
EXPECTED_ELFS = {
    "FTDEMxTj2RsKBSCGETrFXDiSLu1nJLRfAYDsjJ6GXQ4N": "441d8f463ecfcc398d2151f1bb4a1e221cff8b5fff1d9f185db90de6f78b8cde",
    "FUBz1nvSSoHnMqFoK2CA3jF5ckpYPdF3ZT4SoLzDNzh1": "225fc076dc79661768bf6a48bbac0d26596d00ad15a8bddcf99dc4a76ad4aa88",
    "HFg1j9ZMixGkMo42ZoeqNpqBZpGcVbsRwfFBLcGCn5Kt": "b7662ad1e4ebca17f1a8f2e222f18fcb1dcacf1e8daaad7d709f67f92798c4a1",
}
SNAPSHOT_FIELDS = {
    "version", "domain", "profile", "slot", "epoch", "signed_transaction", "message_account_keys", "accounts", "programs",
    "sysvars", "active_features", "inactive_features", "reserved_account_keys", "blockhash_queue_bincode", "fee_rate_governor_bincode",
    "fee_structure", "recent_blockhashes", "latest_blockhash", "blockhash_lamports_per_signature", "transaction_hash_age", "max_processing_age",
    "transaction_max_age", "epoch_total_stake", "compute_unit_limit", "compute_budget_override", "lamports_per_signature", "zero_fee_policy",
    "vm_config_debug", "syscall_registry",
}
SIDECAR_FIELDS = {
    "domain", "version", "sequence", "clock_id", "clock_domain", "start_ns", "before_commit_ns", "end_ns", "atomic_bank_operation_lock",
    "prestate_rechecked_before_commit", "capture_rejected", "input", "execution", "committed_accounts", "execution_verified", "legacy_proof_input",
    "collector_sha256", "signed_transaction_sha256", "input_sha256", "output_sha256", "digest_encoding", "output_digest_scope",
    "candidate_proof_kind", "candidate_proof_version", "candidate_guest_sha256", "candidate_key_sha256", "commit_metadata",
    "checkpoint_state_scope", "checkpoint_pre_accounts", "checkpoint_committed_accounts", "observation_id",
}
EXECUTION_FIELDS = {"accounts", "tentative_accounts", "rollback_accounts", "touched_flags", "status", "executed_units", "fee",
                    "compute_unit_limit", "heap_size", "logs", "inner_instructions", "return_data_program_id", "return_data", "loaded_accounts_data_size", "execution_cost_debug"}


def closed(value, keys, label):
    require(type(value) is dict and set(value) == set(keys), f"{label}: unknown/missing fields")


def uint(value, label, maximum=2**64 - 1):
    require(type(value) is int and 0 <= value <= maximum, f"{label}: invalid unsigned integer")


def pubkey(value):
    require(type(value) is str and len(contract.base58_decode(value)) == 32, "invalid public key")
    return value


def account_map(entries, label, maximum_entries=64):
    require(type(entries) is list and len(entries) <= maximum_entries, f"{label}: account bound")
    result, total = {}, 0
    for row in entries:
        closed(row, ("pubkey", "account"), label)
        key = pubkey(row["pubkey"])
        require(key not in result, f"{label}: duplicate account")
        state = row["account"]
        if state is not None:
            closed(state, ("lamports", "data", "owner", "executable", "rent_epoch"), label)
            uint(state["lamports"], "lamports")
            uint(state["rent_epoch"], "rent epoch")
            pubkey(state["owner"])
            require(type(state["executable"]) is bool, "executable must be boolean")
            total += len(byte_array(state["data"], label="account data"))
            require(total <= 2 * 1024 * 1024, "account byte bound")
        result[key] = state
    return result


def canonical_capture(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def validate_atomic_sidecar(sidecar, expected_wire_sha256=None, collector_sha256=None):
    closed(sidecar, SIDECAR_FIELDS, "sidecar")
    require(sidecar["domain"] == CAPTURE_DOMAIN and type(sidecar["version"]) is int and sidecar["version"] == 1, "capture domain/version drift")
    require(sidecar["capture_rejected"] is None and sidecar["atomic_bank_operation_lock"] is True
            and sidecar["prestate_rechecked_before_commit"] is True, "capture not authoritative/rechecked")
    require(sidecar["execution_verified"] is False and sidecar["legacy_proof_input"] is False, "capture cannot assert execution verification/legacy input")
    uint(sidecar["sequence"], "capture sequence", 511)
    require(sidecar["observation_id"] == f"{sidecar['clock_domain']}:transaction:{sidecar['sequence']}", "capture observation identity drift")
    require(type(sidecar["clock_domain"]) is str and sidecar["clock_domain"]
            and sidecar["clock_id"] == sidecar["clock_domain"], "capture clock missing/mismatch")
    require(type(sidecar["candidate_proof_kind"]) is int and sidecar["candidate_proof_kind"] == 3
            and type(sidecar["candidate_proof_version"]) is int and sidecar["candidate_proof_version"] == 1, "candidate selector drift")
    require(sidecar["digest_encoding"] == "sorted-key-utf8-json-no-whitespace-v1"
            and sidecar["output_digest_scope"] == ["execution", "committed_accounts"], "capture digest encoding/scope drift")
    for key in ("collector_sha256", "signed_transaction_sha256", "input_sha256", "output_sha256"):
        require(type(sidecar[key]) is str and re.fullmatch("[0-9a-f]{64}", sidecar[key]), "capture digest format")
    for key in ("candidate_guest_sha256", "candidate_key_sha256"):
        require(sidecar[key] is None or type(sidecar[key]) is str and re.fullmatch("[0-9a-f]{64}", sidecar[key]), "candidate build identity format")
    if collector_sha256 is not None:
        require(sidecar["collector_sha256"] == collector_sha256, "sidecar collector hash binding drift")
    for key in ("start_ns", "before_commit_ns", "end_ns"):
        uint(sidecar[key], key, 2**63 - 1)
    require(sidecar["start_ns"] <= sidecar["before_commit_ns"] <= sidecar["end_ns"], "atomic boundary clock drift")
    snapshot, execution = sidecar["input"], sidecar["execution"]
    closed(snapshot, SNAPSHOT_FIELDS, "input")
    closed(execution, EXECUTION_FIELDS, "execution")
    for value, label in ((snapshot["vm_config_debug"], "VM config"), (execution["execution_cost_debug"], "execution cost")):
        require(type(value) is str and 0 < len(value) <= 32768, f"{label}: missing/oversize captured debug context")
    registry = snapshot["syscall_registry"]
    require(type(registry) is list and 1 <= len(registry) <= 256, "syscall registry bound")
    names, keys = [], []
    for row in registry:
        require(type(row) is list and len(row) == 2, "syscall registry entry")
        uint(row[0], "syscall registry key", 2**32 - 1)
        name = byte_array(row[1], label="syscall name")
        require(0 < len(name) <= 128, "syscall name bound")
        names.append(name)
        keys.append(row[0])
    require(keys == sorted(set(keys)) and len(set(names)) == len(names), "syscall registry order/alias drift")
    require(type(snapshot["version"]) is int and snapshot["version"] == 1 and snapshot["domain"] == CAPTURE_DOMAIN, "snapshot domain/version drift")
    require(snapshot["profile"] in ("consumer.make_request", "coordinator.fulfill"), "unsupported snapshot profile")
    wire = byte_array(snapshot["signed_transaction"], label="signed transaction")
    require(len(wire) <= 1232, "signed wire size")
    require(sha(wire) == sidecar["signed_transaction_sha256"], "declared signed-wire hash drift")
    require(sha(canonical_capture(snapshot)) == sidecar["input_sha256"], "declared atomic input hash drift")
    require(sha(canonical_capture({"execution": execution, "committed_accounts": sidecar["committed_accounts"]}))
            == sidecar["output_sha256"], "declared atomic output hash drift")
    if expected_wire_sha256 is not None:
        require(sha(wire) == expected_wire_sha256, "atomic signed-wire identity mismatch")
    parsed = parse_wire(wire)
    require(parsed["signature_count"] == 1 and parsed["keys"] == snapshot["message_account_keys"], "message/account order drift")
    require(len(parsed["instructions"]) == 2 and parsed["instructions"][0]["program"] == COMPUTE_BUDGET, "compute prefix required")
    target = parsed["instructions"][1]
    expected_target = ("HFg1j9ZMixGkMo42ZoeqNpqBZpGcVbsRwfFBLcGCn5Kt", "make_request") if snapshot["profile"] == "consumer.make_request" else ("FTDEMxTj2RsKBSCGETrFXDiSLu1nJLRfAYDsjJ6GXQ4N", "fulfill")
    require(target["program"] == expected_target[0] and target["data"][:8] == contract.discriminator(expected_target[1]), "snapshot profile/program/discriminator drift")
    budget = parsed["instructions"][0]
    require(budget["accounts"] == [] and len(budget["data"]) == 5 and budget["data"][0] == 2, "unsupported compute budget")
    limit = int.from_bytes(budget["data"][1:], "little")
    require(0 < limit <= 1400000 and snapshot["compute_unit_limit"] == execution["compute_unit_limit"] == limit, "compute limit drift")
    require(snapshot["zero_fee_policy"] is True and snapshot["compute_budget_override"] is False
            and snapshot["lamports_per_signature"] == execution["fee"] == 0, "unsupported fee/budget profile")
    require(execution["heap_size"] == 32768, "unsupported heap")
    for key in ("slot", "epoch", "blockhash_lamports_per_signature", "transaction_hash_age", "max_processing_age", "transaction_max_age", "epoch_total_stake"):
        uint(snapshot[key], key)
    require(snapshot["transaction_hash_age"] <= snapshot["transaction_max_age"], "expired captured blockhash")
    pubkey(snapshot["latest_blockhash"])
    for key in ("blockhash_queue_bincode", "fee_rate_governor_bincode"):
        require(0 < len(byte_array(snapshot[key], label=key)) <= 256 * 1024, "missing/oversize runtime policy bytes")
    closed(snapshot["fee_structure"], {"lamports_per_signature", "lamports_per_write_lock", "compute_fee_bins"}, "fee policy")
    require(snapshot["fee_structure"] == dict(lamports_per_signature=0, lamports_per_write_lock=0, compute_fee_bins=[]), "nonzero fee policy unsupported")
    recent = snapshot["recent_blockhashes"]
    require(type(recent) is list and 1 <= len(recent) <= 4096, "recent blockhash queue incomplete/oversize")
    seen_hashes = set()
    for entry in recent:
        closed(entry, {"blockhash", "age", "lamports_per_signature"}, "recent blockhash")
        pubkey(entry["blockhash"])
        uint(entry["age"], "blockhash age")
        uint(entry["lamports_per_signature"], "blockhash fee")
        require(entry["blockhash"] not in seen_hashes, "duplicate recent blockhash")
        seen_hashes.add(entry["blockhash"])
    require(any(entry["blockhash"] == parsed["recent_blockhash"] and entry["age"] == snapshot["transaction_hash_age"] for entry in recent), "signed recent-blockhash/age binding drift")
    accounts = account_map(snapshot["accounts"], "prestate")
    committed = account_map(sidecar["committed_accounts"], "committed")
    require(set(accounts) == set(committed), "poststate omitted account/absence")
    require(sidecar["checkpoint_state_scope"] == "ER delegated/touched plus current writable keys", "checkpoint state scope drift")
    checkpoint_pre = account_map(sidecar["checkpoint_pre_accounts"], "checkpoint prestate", 1024)
    checkpoint_post = account_map(sidecar["checkpoint_committed_accounts"], "checkpoint poststate", 1024)
    require(set(checkpoint_pre) == set(checkpoint_post), "checkpoint state/absence omission")
    required, readonly_signed, readonly_unsigned = parsed["header"]
    writable = {key for index, key in enumerate(parsed["keys"])
                if index < required - readonly_signed or required <= index < len(parsed["keys"]) - readonly_unsigned}
    require(writable <= checkpoint_pre.keys(), "checkpoint omitted writable account/absence")
    for key in checkpoint_pre:
        if key in accounts:
            require(checkpoint_pre[key] == accounts[key] and checkpoint_post[key] == committed[key], "checkpoint/VM state binding drift")
        else:
            require(checkpoint_pre[key] == checkpoint_post[key], "checkpoint changed state outside transaction input")
    for key in ("accounts", "tentative_accounts"):
        account_map(execution[key], key)
    rollback = account_map(execution["rollback_accounts"], "rollback accounts")
    require(type(execution["touched_flags"]) is list and all(type(v) is bool for v in execution["touched_flags"]), "invalid touched flags")
    uint(execution["executed_units"], "executed CU", limit)
    uint(execution["loaded_accounts_data_size"], "loaded account bytes", 2 * 1024 * 1024)
    metadata = sidecar["commit_metadata"]
    closed(metadata, {"loaded_accounts_data_size", "loaded_accounts_count", "fee_payer_post_balance"}, "commit metadata")
    require(metadata["loaded_accounts_data_size"] == execution["loaded_accounts_data_size"], "commit/execution loaded-size drift")
    uint(metadata["loaded_accounts_count"], "loaded account count", 64)
    uint(metadata["fee_payer_post_balance"], "fee payer balance")
    payer_post = committed[parsed["keys"][0]]
    require(metadata["fee_payer_post_balance"] == (0 if payer_post is None else payer_post["lamports"]), "commit/Bank payer balance drift")
    pubkey(execution["return_data_program_id"])
    require(len(byte_array(execution["return_data"], label="return data")) <= 1024, "return data bound")
    require(type(execution["status"]) is dict and set(execution["status"]) in ({"Ok"}, {"Err"}), "execution status shape")
    if "Err" in execution["status"]:
        payer = parsed["keys"][0]
        require(set(rollback) == {payer}, "unsupported rollback account set")
        for key, state in accounts.items():
            post = committed[key]
            if key != payer:
                require(state == post, "failed execution did not preserve nonpayer Bank state")
            else:
                require(state is not None and post is not None and post == rollback[key], "fee payer rollback boundary mismatch")
                # Agave can persist loader rent_epoch normalization even with zero fees.
                require({k: v for k, v in state.items() if k != "rent_epoch"}
                        == {k: v for k, v in post.items() if k != "rent_epoch"}
                        and post["rent_epoch"] in (state["rent_epoch"], 2**64 - 1), "zero-fee rollback altered payer effects")
    else:
        require(execution["status"]["Ok"] is None, "success status encoding")
    require(type(snapshot["programs"]) is list and len(snapshot["programs"]) == 3, "program set incomplete")
    seen_programs, extra_keys = set(), set()
    for program in snapshot["programs"]:
        closed(program, ("program_id", "programdata_address", "deployment_slot", "elf_sha256", "elf_offset"), "program")
        program_id, data_id = pubkey(program["program_id"]), pubkey(program["programdata_address"])
        require(program_id in EXPECTED_ELFS and program_id not in seen_programs, "pinned program set drift")
        seen_programs.add(program_id)
        extra_keys.update((program_id, data_id))
        require(program["elf_sha256"] == EXPECTED_ELFS[program_id] and program["elf_offset"] == 45, "pinned ELF/loader offset drift")
        program_account, data_account = accounts.get(program_id), accounts.get(data_id)
        require(program_account is not None and data_account is not None, "program/ProgramData absence")
        require(program_account["owner"] == data_account["owner"] == LOADER and program_account["executable"] is True
                and data_account["executable"] is False, "program loader/owner binding")
        program_bytes, data_bytes = bytes(program_account["data"]), bytes(data_account["data"])
        require(program_bytes[:4] == (2).to_bytes(4, "little") and program_bytes[4:36] == contract.base58_decode(data_id), "program/ProgramData linkage")
        require(data_bytes[:4] == (3).to_bytes(4, "little") and len(data_bytes) > 45, "ProgramData header missing")
        slot = int.from_bytes(data_bytes[4:12], "little")
        require(slot == program["deployment_slot"] and slot < snapshot["slot"], "ProgramData visibility")
        require(sha(data_bytes[45:]) == program["elf_sha256"], "actual pinned ELF bytes mismatch")
    require(type(snapshot["sysvars"]) is list and len(snapshot["sysvars"]) == 7, "sysvar set incomplete")
    sysvar_ids = set()
    for sysvar in snapshot["sysvars"]:
        closed(sysvar, ("pubkey", "cached_data"), "sysvar")
        key = pubkey(sysvar["pubkey"])
        require(key not in sysvar_ids and key in accounts, "sysvar duplicate/absence omitted")
        sysvar_ids.add(key)
        account = accounts[key]
        cached = None if sysvar["cached_data"] is None else byte_array(sysvar["cached_data"], label="cached sysvar")
        require(cached == (None if account is None else bytes(account["data"])), "sysvar cache/account drift")
    required_sysvars = {"SysvarC1ock11111111111111111111111111111111", "SysvarRent111111111111111111111111111111111", "SysvarEpochSchedu1e111111111111111111111111", "SysvarS1otHashes111111111111111111111111111"}
    expected_sysvars = required_sysvars | {"SysvarStakeHistory1111111111111111111111111", "SysvarEpochRewards1111111111111111111111111", "SysvarLastRestartS1ot1111111111111111111111"}
    require(sysvar_ids == expected_sysvars and all(accounts[key] is not None for key in required_sysvars), "required sysvar missing/identity drift")
    expected_keys = set(parsed["keys"]) | extra_keys | sysvar_ids | {LOADER, "Sysvar1nstructions1111111111111111111111111"}
    require(set(accounts) == expected_keys, "complete prestate key/absence set mismatch")
    require([contract.base58_decode(row["pubkey"]) for row in snapshot["accounts"]] == sorted(contract.base58_decode(k) for k in accounts), "prestate account order drift")
    active = snapshot["active_features"]
    require(type(active) is list and all(type(row) is list and len(row) == 2 for row in active), "active feature shape")
    for key, activation in active:
        pubkey(key)
        uint(activation, "activation slot")
    require(len(active) <= 512 and active == sorted(active)
            and [CURVE_FEATURE, 0] in active and len({row[0] for row in active}) == len(active), "curve feature missing/duplicate/order drift")
    inactive = snapshot["inactive_features"]
    require(type(inactive) is list and len(inactive) <= 512 and inactive == sorted(inactive)
            and len(set(inactive)) == len(inactive) and not ({r[0] for r in active} & set(inactive)), "inactive feature drift")
    reserved = snapshot["reserved_account_keys"]
    require(type(reserved) is list and len(reserved) <= 512 and reserved == sorted(set(reserved)), "reserved account set drift")
    for key in inactive + reserved:
        pubkey(key)
    return dict(wire_sha256=sha(wire), input_sha256=sidecar["input_sha256"], output_sha256=sidecar["output_sha256"],
                sequence=sidecar["sequence"], clock_domain=sidecar["clock_domain"], start_ns=sidecar["start_ns"], end_ns=sidecar["end_ns"],
                profile=snapshot["profile"], status=execution["status"], compute_units=execution["executed_units"],
                runtime_policy=dict(transaction_max_age=snapshot["transaction_max_age"], bank_queue_retention=snapshot["max_processing_age"],
                                    compute_unit_limit=limit, heap_bytes=execution["heap_size"], zero_transaction_fees=True),
                feature_set_sha256=sha(canonical(active)),
                candidate_feature_context_sha256=sha(canonical({"active": active, "inactive": inactive})),
                feature_set_digest_encoding="JSON-sorted-base58-active-pairs-v1",
                atomic_source_shape_validated=True, execution_verified=False)


def load_sidecar(path):
    with Path(path).open("rb") as stream:
        raw = stream.read(16 * 1024 * 1024 + 1)
    require(len(raw) <= 16 * 1024 * 1024, "sidecar exceeds 16 MiB")
    return json.loads(raw, object_pairs_hook=contract.strict_object, parse_constant=contract.reject_constant)


def source_provenance_issues(identities):
    require(type(identities) is dict and identities.get("execution_verified") is False, "collector cannot assert execution verification")
    issues = []
    compiled = identities.get("harness_source_sha256")
    if compiled is None:
        issues.append("compiled harness source receipt missing")
    else:
        require(type(compiled) is str and re.fullmatch("[0-9a-f]{64}", compiled), "compiled harness source digest format")
    sources = identities.get("runtime_untracked_sources", [])
    require(type(sources) is list and len(sources) <= 1024, "source receipt bound")
    for source in sources:
        closed(source, {"path", "sha256"}, "source receipt")
        name = source["path"]
        require(type(name) is str and name and not name.startswith("/") and ".." not in name.split("/") and "\\" not in name,
                "source receipt must use in-repo relative path")
        require(type(source["sha256"]) is str and re.fullmatch("[0-9a-f]{64}", source["sha256"]), "source receipt digest")
        if name == "northstar/tests/vrf_coordinator_runtime.rs" and source["sha256"] != compiled:
            issues.append("compiled harness hash differs from captured source receipt")
    return issues


def audit_sidecars(directory, audit):
    directory = Path(directory).resolve(strict=True)
    collector, end = (load_sidecar(directory / name) for name in ("collector.json", "collector-end.json"))
    closed(collector, {"domain", "clock_id", "clock_domain", "collector_start_unix_ns", "start_ns", "observer_installed_ns", "max_records", "identities", "execution_verified", "production_acceptance"}, "collector")
    closed(end, {"domain", "clock_id", "clock_domain", "end_ns", "records", "errors", "execution_verified"}, "collector end")
    require(collector["clock_id"] == collector["clock_domain"] and end["clock_id"] == end["clock_domain"], "collector clock aliases drift")
    require(collector.get("domain") == end.get("domain") == CAPTURE_DOMAIN, "collector domain drift")
    require(collector["production_acceptance"] is False, "collector cannot assert production acceptance")
    uint(collector["start_ns"], "collector start", 2**63 - 1)
    uint(end["end_ns"], "collector end", 2**63 - 1)
    uint(collector["observer_installed_ns"], "observer installation", 2**63 - 1)
    require(collector["start_ns"] <= collector["observer_installed_ns"] <= end["end_ns"], "observer clock bounds")
    uint(collector["max_records"], "collector capacity", 512)
    uint(end["records"], "collector records", collector["max_records"])
    require(collector["start_ns"] <= end["end_ns"], "collector clock bounds")
    clock = collector.get("clock_domain")
    require(type(clock) is str and clock and end.get("clock_domain") == clock, "collector clock mismatch")
    require(audit["clock_id"] is not None and audit["clock_id"] == clock, "journal/atomic collector clock mismatch or missing")
    require(collector.get("execution_verified") is False and end.get("execution_verified") is False, "collector cannot assert execution validity")
    require(end.get("errors") == [], "collector retained capture errors")
    expected = {r["sha256"]: r for r in audit["wires"] if r["instruction"] in ("make_request", "fulfill")}
    identities, bound, end_ns = [], set(), collector["start_ns"]
    filenames = sorted(p.name for p in directory.iterdir() if p.suffix == ".json" and p.name not in ("collector.json", "collector-end.json"))
    require(len(filenames) == end["records"] and len(filenames) <= 512, "sidecar omission/count drift")
    for sequence, name in enumerate(filenames):
        require(name == f"{sequence:05}.json", "rejected/omitted/nonconsecutive sidecar")
        path = directory / name
        require(path.resolve().is_relative_to(directory) and path.is_file(), "sidecar path escape")
        sidecar = load_sidecar(path)
        result = validate_atomic_sidecar(sidecar, collector_sha256=file_identity(directory / "collector.json")["sha256"])
        wire = result["wire_sha256"]
        require(wire in expected and wire not in bound, "sidecar unknown/duplicate signed wire")
        bound.add(wire)
        attempt = next(row for row in audit["attempts"] if row["wire_sha256"] == wire)
        expected_status = {"Ok": None} if attempt["status"] == "success" else {"Err": {"InstructionError": [1, {"Custom": attempt["expected_error"]}]}}
        require(result["status"] == expected_status, "atomic/RPC execution outcome drift")
        require(result["sequence"] == sequence and result["clock_domain"] == clock, "sidecar sequence/clock drift")
        require(end_ns <= result["start_ns"] <= result["end_ns"] <= end["end_ns"], "sidecar clock ordering drift")
        end_ns = result["end_ns"]
        identities.append(dict(label=expected[wire]["label"], artifact=file_identity(path), **result))
    missing = sorted(expected[wire]["label"] for wire in expected.keys() - bound)
    require(not missing, f"authoritative sidecars omitted: {missing[:8]}")
    return dict(schema_version="northstar-vrf-atomic-sidecar-audit/v1", clock_domain=clock,
                collector=file_identity(directory / "collector.json"), collector_end=file_identity(directory / "collector-end.json"),
                identities=collector.get("identities", {}),
                source_provenance_issues=source_provenance_issues(collector.get("identities", {})),
                captures=identities, missing=[], execution_verified=False,
                final_acceptance="not_established")


def attach_atomic_capture(audit, atomic):
    audit["atomic_capture"] = atomic
    observed = atomic["identities"]
    require(type(observed) is dict and observed.get("library_source_revision") == COMMIT, "collector library provenance drift")
    satisfied = {"authoritative_atomic_prestate_and_computed_poststate", "domain", "relation_version"}
    audit["candidate_selector"] = dict(proof_kind=3, proof_version=1, domain_hex=CANDIDATE_PROOF_DOMAIN.hex(),
                                       provenance="derived from explicit sidecar kind/version; not a generated proof")
    for source, gap, length in (("runtime_source_revision", "runtime_revision", 40),
                                ("runtime_binary_sha256", "runtime_binary_sha256", 64),
                                ("genesis_config_sha256", "er_genesis_hash", 64)):
        value = observed.get(source)
        if value is not None:
            require(type(value) is str and re.fullmatch(f"[0-9a-f]{{{length}}}", value), "collector identity format")
            satisfied.add(gap)
    if observed.get("feature_set_sha256") is not None:
        require(observed.get("feature_set_digest_encoding") == "JSON-sorted-base58-active-pairs-v1"
                and all(row["feature_set_sha256"] == observed["feature_set_sha256"] for row in atomic["captures"]),
                "collector feature-set digest drift")
        satisfied.add("feature_set_sha256")
    audit["missing"] = [gap for gap in audit["missing"] if gap not in satisfied]
    audit["blockers"] = [gap for gap in audit["blockers"] if gap not in satisfied]
    audit["blockers"].extend(atomic.get("source_provenance_issues", []))
    audit["genesis_identity"] = dict(kind="bincode_genesis_config_sha256", sha256=observed.get("genesis_config_sha256"))
    runtime_gaps = {"collector_wide_monotonic_clock", "authoritative_atomic_prestate_and_computed_poststate",
                    "runtime_revision", "runtime_binary_sha256", "er_genesis_hash", "feature_set_sha256",
                    "complete_100_success_er_window", "negative_controls_embedded_in_success_window", "negative_control_only",
                    "compiled harness source receipt missing", "compiled harness hash differs from captured source receipt"}
    audit["reported_runtime_window"] = "unmet" if runtime_gaps & set(audit["blockers"]) else "consistent_as_reported_requires_external_verification"
    return audit


def load_receipt(path, run=None):
    if path is None:
        require(run is None, "--receipt-run requires --receipt")
        return None
    receipt = contract.load_json(path)
    if run is not None:
        require(receipt.get("source_commit") == COMMIT and receipt.get("provenance") == "actual_local_er_with_synthetic_genesis",
                "receipt source/provenance drift")
        require(run in receipt.get("runs", {}), "unknown retained receipt run")
        receipt = receipt["runs"][run]
    require(type(receipt) is dict and type(receipt.get("journal")) is dict, "per-run journal receipt required")
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--journal", type=Path, required=True)
    parser.add_argument("--wires-root", type=Path)
    parser.add_argument("--receipt", type=Path)
    parser.add_argument("--receipt-run")
    parser.add_argument("--sidecars-dir", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--structure-only", action="store_true")
    args = parser.parse_args()
    try:
        audit = audit_journal(args.journal, args.wires_root, receipt=load_receipt(args.receipt, args.receipt_run))
        if args.sidecars_dir is not None:
            attach_atomic_capture(audit, audit_sidecars(args.sidecars_dir, audit))
        code = 0 if args.structure_only else 1
    except (Invalid, OSError, ValueError, KeyError, TypeError, IndexError, RuntimeError) as error:
        audit = dict(status="invalid", errors=[str(error)], final_acceptance="not_established")
        code = 2
    output = json.dumps(audit, indent=2) + "\n"
    if args.output is None:
        print(output, end="")
    else:
        args.output.write_text(output)
        print(json.dumps({k: audit[k] for k in ("status", "final_acceptance")}))
    return code


if __name__ == "__main__":
    sys.exit(main())

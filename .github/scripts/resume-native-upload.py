#!/usr/bin/env python3
"""Resume only the reviewed, pre-activation native upload without changing its identities."""
import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import stat
import time
from types import SimpleNamespace

from continuity_devnet import Deployment, DROPIN, LOADER, ROOT, public_bytes
from continuity_rollout_state import (
    ARTIFACTS, DEVNET, atomic_json, canonical, checked_directory, file_hash,
    immutable_json, private_json, require,
)

REVISION = "b545c054c35861b6ed01e3e4baf9939cffedec47"
BUFFER_HASH = "c830aba8f8b91f1f484c358e09bcb9480de0a222e07f1e1d2bfa12cb52545915"
PROGRAMS = {
    "portal": "AqMUacr8J4f7NDB5pTedi24Qhv3tuwCppRv1p6E3wjsr",
    "portal-buffer": "J1bfBKHNK6ccJ6HFusTq7k8br1LSGPYnNYRogj6tutdV",
    "token_bridge": "egpPwiECqeuuTEKVnyjPdAC7vsnEWh5iuZMXih3ujJv",
    "token_bridge-buffer": "6GTGrcGtMaxJVKzUH6xdtxZ3uzcf1yTL7uzqmbWubvqB",
}


def validate_buffer(account, size, funder, expected_hash):
    require(account is not None and account["owner"] == LOADER and not account["executable"], "unexpected buffer account")
    raw = account["raw"]
    require(len(raw) == size + 37 and raw[:4] == (1).to_bytes(4, "little") and raw[4] == 1
            and raw[5:37] == public_bytes(funder), "buffer size or authority differs")
    require(hashlib.sha256(raw[37:]).hexdigest() == expected_hash, "buffer changed since reviewed inspection")


def validate_local(local, upstream):
    require(local.call("getGenesisHash") == DEVNET and upstream.call("getGenesisHash") == DEVNET, "recovery is devnet-only")
    require(local.call("getHealth") == "ok", "local L1 is not healthy")
    observed = local.call("getSlot", [{"commitment": "confirmed"}])
    remote = upstream.call("getSlot", [{"commitment": "confirmed"}])
    require(type(observed) is int and type(remote) is int and 0 <= remote <= observed + 32 and observed >= 0,
            "local L1 is behind the bound network")
    return {"local_slot": observed, "upstream_slot": remote}


def recover(sdk, descriptor):
    root = Path("/solana/northstar/continuity-v2")
    control = root / REVISION / "control"
    checked_directory(control, private=True)
    reservation = private_json(control / "reservation.json")
    saved = private_json(control / "plan.json")
    require(reservation["plan_sha256"] == hashlib.sha256(canonical(saved)).hexdigest(), "retained plan differs")
    require(saved["source_revision"] == REVISION and saved["programs"] == PROGRAMS, "reviewed program identities differ")
    require(reservation["devnet_rpc"] == "https://api.devnet.solana.com"
            and reservation["l1_rpc"] == "http://127.0.0.1:8899" and reservation["er_rpc"] == "http://127.0.0.1:8910",
            "retained RPC bindings differ")
    require(not os.path.lexists(DROPIN) and not os.path.lexists(control / "activation.json"), "native activation already began")
    args = SimpleNamespace(stage=Path(reservation["stage"]), revision=REVISION, sdk_checkout=sdk,
                           state_root=root, funder_keypair=Path("/home/ubuntu/.config/solana/id.json"),
                           devnet_rpc=reservation["devnet_rpc"], l1_rpc=reservation["l1_rpc"], er_rpc=reservation["er_rpc"],
                           max_estimated_cost_lamports=saved["max_estimated_cost_lamports"], lock_descriptor=descriptor)
    deployment = Deployment(args)
    deployment.prepare()
    require(deployment.plan == saved, "recovery must preserve the complete original plan")
    require(deployment.inventory() == deployment.original, "original native service changed")
    require(deployment.l1.call("getIdentity")["identity"] == saved["validator"], "L1 validator identity differs")
    slots = validate_local(deployment.l1, deployment.upstream)
    state = private_json(control / "programs.json")
    require(state == {"version": 2, "roles": {"portal": {"elf_sha256": saved["artifacts"]["portal"], "complete": None}}},
            "upload state changed since reviewed inspection")
    require(deployment.upstream.account(PROGRAMS["portal"]) is None
            and deployment.upstream.account(PROGRAMS["token_bridge"]) is None
            and deployment.upstream.account(PROGRAMS["token_bridge-buffer"]) is None, "unexpected existing native program or bridge buffer")
    size = (deployment.stage / ARTIFACTS["portal"]).stat().st_size
    for rpc in (deployment.upstream, deployment.l1):
        validate_buffer(rpc.account(PROGRAMS["portal-buffer"]), size, saved["funder"], BUFFER_HASH)
    def rent(length):
        value = deployment.upstream.call("getMinimumBalanceForRentExemption", [length])
        require(type(value) is int and value > 0, "invalid recovery rent")
        return value
    sizes = {role: (deployment.stage / ARTIFACTS[role]).stat().st_size for role in ("portal", "token_bridge")}
    quote = 500_000_000 + sum(rent(length + 45) + rent(36) for length in sizes.values()) + max(rent(length + 37) for length in sizes.values())
    balance = deployment.upstream.call("getBalance", [saved["funder"], {"commitment": "finalized"}])["value"]
    require(type(balance) is int and balance >= quote and quote <= saved["max_estimated_cost_lamports"], "recovery funding quote is not satisfied")
    immutable_json(control / "upload-recovery.json", {
        "version": 1, "plan_sha256": reservation["plan_sha256"], "observed_buffer_sha256": BUFFER_HASH,
        "upload_rpc": args.l1_rpc, "programs": PROGRAMS, "source_sha256": file_hash(Path(__file__)),
        "estimated_lamports": quote,
    })
    deployment.execute(["sudo", "-n", "python3", "-B", ROOT / ".github/scripts/stop-vrf-service.py"])
    print(json.dumps({"phase": "reviewed-upload-resume", "source_revision": REVISION, **slots}), flush=True)
    for role, length in sizes.items():
        validate_local(deployment.l1, deployment.upstream)
        if role == "portal":
            validate_buffer(deployment.upstream.account(PROGRAMS["portal-buffer"]), length, saved["funder"], BUFFER_HASH)
        require(private_json(control / "programs.json") == state, "upload record changed concurrently")
        if role not in state["roles"]:
            require(deployment.upstream.account(PROGRAMS[role]) is None
                    and deployment.upstream.account(PROGRAMS[role + "-buffer"]) is None, "bridge upload already exists")
            state["roles"][role] = {"elf_sha256": saved["artifacts"][role], "complete": None}
            atomic_json(control / "programs.json", state)
        deployment.execute([deployment.stage / ARTIFACTS["cli"], "--url", args.l1_rpc,
                            "--keypair", args.funder_keypair, "program", "deploy", deployment.stage / ARTIFACTS[role],
                            "--program-id", control / "keys" / (role + ".json"), "--buffer", control / "keys" / (role + "-buffer.json"),
                            "--max-len", str(length), "--use-rpc", "--max-sign-attempts", "10"], timeout=1200)
        deadline = time.monotonic() + 300
        while time.monotonic() < deadline:
            observed = deployment.program_view(role)
            if observed is not None:
                state["roles"][role]["complete"] = observed
                atomic_json(control / "programs.json", state)
                break
            time.sleep(1)
        else:
            raise RuntimeError("recovered program is not finalized; original upload barrier retained")
    require(all(deployment.program_view(role) == entry["complete"] for role, entry in state["roles"].items()), "final program observation changed")
    immutable_json(control / "upload-recovery-complete.json", {"version": 1, "plan_sha256": reservation["plan_sha256"], "roles": state["roles"]})
    print(json.dumps({"phase": "retained-uploads-verified", "source_revision": REVISION, "programs": PROGRAMS}), flush=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--sdk-checkout", type=Path, required=True)
    parser.add_argument("--confirmation", choices=("resume-retained-uploads",), required=True)
    args = parser.parse_args()
    descriptor = os.open("/solana/northstar/continuity-v2-deploy.lock", os.O_RDWR | os.O_NOFOLLOW)
    try:
        info = os.fstat(descriptor)
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077, "invalid private deployment lock")
        fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        recover(args.sdk_checkout, descriptor)
    finally:
        os.close(descriptor)


if __name__ == "__main__":
    main()

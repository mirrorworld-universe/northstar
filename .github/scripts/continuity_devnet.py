#!/usr/bin/env python3
"""Sonic: Isolated first-v2 activation; retained state and uncertain outcomes fail closed."""

import argparse
import base64
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import time
import urllib.request
from urllib.parse import urlsplit

from continuity_rollout_state import (
    ARTIFACTS, DEVNET, SDK_REVISION, append_private, atomic_json, canonical, checked_directory, file_hash, flag_value,
    geyser_event_source, geyser_events_config, geyser_plugin_flag,
    immutable_json, inspect_paths, private_json, release_mode_arguments, require, retained_fingerprint, service_arguments,
    service_dropin, sync_paths, verify_artifacts,
)

ROOT = Path(__file__).resolve().parents[2]
LOADER = "BPFLoaderUpgradeab1e11111111111111111111111"
SERVICE = "northstar-validator.service"
DROPIN = Path("/etc/systemd/system/northstar-validator.service.d/zzzz-northstar-continuity-v2.conf")
ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz"


def public_bytes(value):
    number = 0
    for character in value:
        number = number * 58 + ALPHABET.index(character)
    raw = b"\0" * (len(value) - len(value.lstrip("1"))) + (number.to_bytes((number.bit_length() + 7) // 8, "big") if number else b"")
    require(len(raw) == 32, "public key must encode 32 bytes")
    return raw


def public_text(raw):
    number, text = int.from_bytes(raw, "big"), ""
    while number:
        number, digit = divmod(number, 58)
        text = ALPHABET[digit] + text
    return "1" * (len(raw) - len(raw.lstrip(b"\0"))) + text


class Rpc:
    def __init__(self, url):
        self.url = url
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def call(self, method, params=None):
        request = urllib.request.Request(self.url, data=canonical({"jsonrpc": "2.0", "id": 1, "method": method, "params": params or []}),
                                         headers={"content-type": "application/json"})
        with self.opener.open(request, timeout=20) as response:
            raw = response.read(16 * 1024 * 1024 + 1)
        require(len(raw) <= 16 * 1024 * 1024, "RPC response exceeds bound")
        result = json.loads(raw)
        require(isinstance(result, dict) and result.get("error") is None and "result" in result, "RPC failed: " + method)
        return result["result"]

    def account(self, address):
        floor = self.call("getSlot", [{"commitment": "finalized"}])
        require(type(floor) is int and floor >= 0, "invalid finalized slot")
        response = self.call("getAccountInfo", [address, {"encoding": "base64", "commitment": "finalized", "minContextSlot": floor}])
        require(isinstance(response, dict) and "value" in response and type(response.get("context", {}).get("slot")) is int
                and response["context"]["slot"] >= floor, "stale or malformed account response")
        account = response["value"]
        if account is None:
            return None
        require(isinstance(account, dict) and type(account.get("executable")) is bool and type(account.get("lamports")) is int
                and isinstance(account.get("owner"), str) and isinstance(account.get("data"), list)
                and len(account["data"]) == 2 and account["data"][1] == "base64", "invalid account value")
        return account | {"raw": base64.b64decode(account["data"][0], validate=True)}


def command(arguments, timeout=120, pass_fds=()):
    return subprocess.run(list(map(str, arguments)), capture_output=True, timeout=timeout, check=False, pass_fds=pass_fds)


def inventory_service():
    def property_value(name):
        result = command(["systemctl", "show", "--property=" + name, "--value", SERVICE])
        require(result.returncode == 0, "cannot inspect validator service")
        return result.stdout.decode().strip()
    pid = int(property_value("MainPID"))
    require(pid > 0 and property_value("ActiveState") == "active", "validator must be running for initial inventory")
    proc = Path("/proc") / str(pid)
    before = (proc / "stat").read_text().rsplit(") ", 1)[1].split()[19]
    executable = os.readlink(proc / "exe")
    require(not executable.endswith(" (deleted)"), "running validator binary was replaced before inventory")
    arguments = (proc / "cmdline").read_bytes().rstrip(b"\0").decode().split("\0")
    require(Path(executable).name == "agave-validator" and Path(arguments[0]).resolve() == Path(executable), "unexpected validator executable")
    environment = dict(item.split("=", 1) for item in (proc / "environ").read_bytes().decode().split("\0") if "=" in item)
    require(before == (proc / "stat").read_text().rsplit(") ", 1)[1].split()[19]
            and str(pid) == property_value("MainPID"), "validator process changed during inventory")
    cwd = Path(os.readlink(proc / "cwd"))
    ledger = Path(flag_value(arguments, "--ledger"))
    ledger = ledger if ledger.is_absolute() else cwd / ledger
    paths = [str(ledger / "northstar-checkpoint-plans"), str(ledger / "northstar-unsettled-state"),
             executable, property_value("FragmentPath"),
             "/solana/northstar/portal-keypair.json", "/solana/northstar/token-bridge-keypair.json"]
    paths.extend(property_value("DropInPaths").split())
    for name in ("NORTHSTAR_CHECKPOINT_PLAN_DIR", "NORTHSTAR_UNSETTLED_STATE_DIR"):
        if environment.get(name):
            configured = Path(environment[name])
            paths.append(str(configured if configured.is_absolute() else cwd / configured))
    info = proc.stat()
    return {"pid": pid, "start_time": before, "arguments": arguments, "binary_sha256": file_hash(executable),
            "uid": info.st_uid, "gid": info.st_gid, "retained_paths": sorted(set(filter(None, paths))),
            "state_environment": {name: environment.get(name) for name in ("NORTHSTAR_CHECKPOINT_PLAN_DIR", "NORTHSTAR_UNSETTLED_STATE_DIR")}}


class Deployment:
    def __init__(self, args):
        self.args = args
        self.upstream, self.l1, self.er = Rpc(args.devnet_rpc), Rpc(args.l1_rpc), Rpc(args.er_rpc)
        self.namespace = args.state_root / args.revision
        self.control = self.namespace / "control"
        self.runtime = self.namespace / "runtime"
        self.log = None
        self.fresh = None

    def execute(self, arguments, timeout=120, allowed=False):
        if self.log:
            append_private(self.log, b"")
        try:
            descriptor = getattr(self.args, "lock_descriptor", None)
            result = command(arguments, timeout, (descriptor,) if descriptor is not None else ())
        except subprocess.TimeoutExpired as error:
            if self.log:
                append_private(self.log, (error.stdout or b"") + (error.stderr or b""))
            raise RuntimeError("deployment operation timed out; private diagnostics retained") from error
        if self.log:
            append_private(self.log, result.stdout + result.stderr)
        require(allowed or result.returncode == 0, "deployment operation failed; private diagnostics retained")
        return result.stdout.decode()

    def inventory(self):
        return json.loads(self.execute(["sudo", "-n", "python3", "-B", __file__, "inventory"]))

    def retention(self):
        path = self.control / "retention-input.json"
        immutable_json(path, {"paths": self.original["retained_paths"]})
        return json.loads(self.execute(["sudo", "-n", "python3", "-B", __file__, "fingerprint", "--input", path], timeout=300))

    def inspect(self, paths):
        if self.fresh is not None and not self.control.exists():
            return self.fresh.inspect_temporary(paths)
        request = self.control / "inspection-input.json"
        atomic_json(request, {"paths": list(map(str, paths))})
        return json.loads(self.execute(["sudo", "-n", "python3", "-B", __file__, "inspect", "--input", request]))

    def empty_l1(self, paths):
        request = self.control / "empty-l1-input.json"
        atomic_json(request, {"paths": list(map(str, paths))})
        return json.loads(self.execute(["sudo", "-n", "python3", "-B", __file__, "empty-l1", "--input", request]))

    def flush(self, paths):
        request = self.control / "flush-input.json"
        atomic_json(request, {"paths": list(map(str, paths))})
        self.execute(["sudo", "-n", "python3", "-B", __file__, "flush", "--input", request])

    def key(self, role, established):
        path = self.control / "keys" / (role + ".json")
        require(not established or path.is_file(), "established native program key is missing")
        if not os.path.lexists(path):
            self.execute(["solana-keygen", "new", "--silent", "--no-bip39-passphrase", "--outfile", path])
        raw = private_json(path)
        require(isinstance(raw, list) and len(raw) == 64 and all(type(value) is int and 0 <= value <= 255 for value in raw), "invalid native key file")
        sync_paths([path])
        value = self.execute(["solana-keygen", "pubkey", path]).strip()
        public_bytes(value)
        return value

    def verify_sdk(self):
        require(self.execute(["git", "-C", self.args.sdk_checkout, "rev-parse", "HEAD"]).strip() == SDK_REVISION, "SDK revision differs")
        self.execute(["git", "-C", self.args.sdk_checkout, "diff", "--quiet", "HEAD"])
        require(file_hash(self.args.sdk_checkout / "package-lock.json") == "78697b65487a7c5932151165ec887b37246e25a3571af752b8c151cc636cd00c", "SDK dependency lock differs")

    def activation_arguments(self, original, executable, portal, geyser_config, event_source):
        if self.fresh is not None:
            return self.fresh.arguments(original, executable, portal, geyser_config, event_source)
        return service_arguments(original, executable, portal, geyser_config, event_source)

    def prepare(self):
        require(self.upstream.call("getGenesisHash") == DEVNET, "native rollout is devnet-only")
        require(re.fullmatch("[0-9a-f]{40}", self.args.revision), "full source revision required")
        self.verify_sdk()
        verify_artifacts(self.args.stage, self.args.revision)
        require(private_json(self.args.funder_keypair) is not None, "native funder is missing")
        funder = self.execute(["solana-keygen", "pubkey", self.args.funder_keypair]).strip()
        public_bytes(funder)
        self.args.state_root.mkdir(mode=0o711, parents=True, exist_ok=True)
        checked_directory(self.args.state_root)
        if os.path.lexists(self.namespace):
            checked_directory(self.namespace)
            checked_directory(self.control, private=True)
            reservation = private_json(self.control / "reservation.json")
            require(reservation.get("version") == 2 and "plan_sha256" in reservation, "unknown native reservation schema")
            if reservation["plan_sha256"] is not None:
                require(hashlib.sha256(canonical(private_json(self.control / "plan.json"))).hexdigest() == reservation["plan_sha256"],
                        "established native plan is missing or changed")
            require(reservation["source_revision"] == self.args.revision and reservation["funder"] == funder
                    and reservation["sdk_revision"] == SDK_REVISION and reservation["l1_rpc"] == self.args.l1_rpc
                    and reservation["er_rpc"] == self.args.er_rpc and reservation["devnet_rpc"] == self.args.devnet_rpc,
                    "native reservation identity differs")
            self.original = reservation["original"]
            self.stage = Path(reservation["stage"])
            self.manifest = verify_artifacts(self.stage, self.args.revision)
            if self.fresh is not None:
                self.fresh.adopt_reservation(reservation)
        else:
            if self.fresh is None:
                require(not os.path.lexists(DROPIN), "an existing managed native activation requires explicit review")
            else:
                self.fresh.admit()
            require(self.l1.call("getGenesisHash") == DEVNET, "colocated L1 network differs")
            self.original = self.inventory()
            if self.fresh is not None:
                self.original = self.fresh.archive_inventory(self.original)
            for retained in self.original["retained_paths"]:
                require(Path(retained).parent != DROPIN.parent or Path(retained).name < DROPIN.name,
                        "an existing service drop-in would override isolated activation")
            validator = self.l1.call("getIdentity")["identity"]
            public_bytes(validator)
            old_portal = flag_value(self.original["arguments"], "--portal")
            derive = "const{createRequire}=require('node:module');const{PublicKey}=createRequire(process.argv[1]+'/package.json')('@solana/web3.js');console.log(PublicKey.findProgramAddressSync([Buffer.from('session')],new PublicKey(process.argv[2]))[0].toBase58())"
            canonical_session = self.execute(["node", "-e", derive, self.args.sdk_checkout, old_portal]).strip()
            public_bytes(canonical_session)
            reported_session = self.er.call("getSessionPda")
            require(reported_session is None or reported_session == canonical_session, "old ER session address differs from its configured Portal")
            frame = self.upstream.account(canonical_session)
            if self.fresh is None:
                require(frame is None or (frame["owner"] == old_portal and not frame["raw"].startswith(b"NSSESS02")),
                        "existing v2 context requires explicit upgrade review, not fresh replacement")
            else:
                self.fresh.previous_session(frame, old_portal, canonical_session)
            self.namespace.mkdir(mode=0o711)
            self.control.mkdir(mode=0o700)
            (self.control / "keys").mkdir(mode=0o700)
            self.stage = self.args.stage
            self.manifest = verify_artifacts(self.stage, self.args.revision)
            reservation = {"version": 2, "plan_sha256": None, "source_revision": self.args.revision, "funder": funder, "sdk_revision": SDK_REVISION,
                           "stage": str(self.stage), "original": self.original, "validator": validator, "old_portal": old_portal,
                           "l1_rpc": self.args.l1_rpc, "er_rpc": self.args.er_rpc, "devnet_rpc": self.args.devnet_rpc}
            if self.fresh is not None:
                reservation["replacement"] = self.fresh.previous
            immutable_json(self.control / "reservation.json", reservation)
        requested_source = getattr(self.args, "event_source", None)
        activation = self.control / "activation.json"
        event_source = geyser_event_source(private_json(activation)) if activation.exists() else requested_source or "l1"
        require(requested_source is None or requested_source == event_source, "established event source differs")
        self.activation_arguments(self.original["arguments"], self.runtime / ARTIFACTS["validator"],
                                  flag_value(self.original["arguments"], "--portal"), self.runtime / "geyser-events.json", event_source)
        self.log = self.control / "operations.log"
        if not self.log.exists():
            descriptor = os.open(self.log, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            os.close(descriptor)
        checked_directory(self.control / "keys", private=True)
        private_json_guard = self.control / "plan.json"
        established = private_json_guard.exists()
        require(established or not any(os.path.lexists(self.control / name) for name in ("programs.json", "activation.json", "sdk-smoke")),
                "established native deployment plan is missing")
        require(not os.path.lexists(self.runtime) or (self.control / "activation.json").exists(), "runtime state lacks its activation record")
        ids = {role: self.key(role, established) for role in ("portal", "token_bridge", "portal-buffer", "token_bridge-buffer")}
        require(len(set(ids.values())) == 4, "native program/buffer identities must differ")
        profile = hashlib.sha256(canonical({"version": 2, "source": self.args.revision, "artifacts": self.manifest["sha256"],
                                           "sdk": SDK_REVISION, "proof_acceptance_qualified": False})).hexdigest()
        self.plan = {"version": 2, "source_revision": self.args.revision, "sdk_revision": SDK_REVISION, "genesis": DEVNET,
                     "validator": reservation["validator"], "funder": funder, "programs": ids, "profile": profile,
                     "artifacts": self.manifest["sha256"], "max_estimated_cost_lamports": self.args.max_estimated_cost_lamports,
                     "original_sha256": hashlib.sha256(canonical(self.original)).hexdigest()}
        if self.fresh is not None:
            self.plan.update(mode="fresh-stack", replacement_sha256=hashlib.sha256(canonical(self.fresh.previous)).hexdigest())
            self.fresh.check_new_identities(ids)
        immutable_json(private_json_guard, self.plan)
        digest = hashlib.sha256(canonical(self.plan)).hexdigest()
        require(reservation["plan_sha256"] is None or reservation["plan_sha256"] == digest, "native reservation plan binding changed")
        reservation["plan_sha256"] = digest
        atomic_json(self.control / "reservation.json", reservation)
        sync_paths([self.control / "keys", self.control, self.namespace, self.args.state_root, self.args.state_root.parent])
        print(json.dumps({"phase": "prepared", "portal": ids["portal"], "token_bridge": ids["token_bridge"], "validator": self.plan["validator"]}), flush=True)

    def program_view(self, role, *, stage=None, plan=None):
        plan = self.plan if plan is None else plan
        stage = self.stage if stage is None else stage
        ids = plan["programs"]
        program = self.upstream.account(ids[role])
        if program is None:
            return None
        require(program["owner"] == LOADER and program["executable"] and len(program["raw"]) == 36
                and program["raw"][:4] == (2).to_bytes(4, "little"), "unexpected native program account")
        address = public_text(program["raw"][4:])
        stored = self.upstream.account(address)
        require(stored is not None and stored["owner"] == LOADER and not stored["executable"] and len(stored["raw"]) >= 45
                and stored["raw"][:4] == (3).to_bytes(4, "little") and stored["raw"][12] == 1
                and stored["raw"][13:45] == public_bytes(plan["funder"]), "native upgrade authority differs")
        elf = (stage / ARTIFACTS[role]).read_bytes()
        require(stored["raw"][45:45 + len(elf)] == elf and not any(stored["raw"][45 + len(elf):]), "deployed native bytes differ")
        require(self.upstream.account(ids[role + "-buffer"]) is None, "native upload buffer remains; reconcile it before activation")
        return {"programdata": address, "deployment_slot": int.from_bytes(stored["raw"][4:12], "little"),
                "sha256": hashlib.sha256(stored["raw"]).hexdigest()}

    def deploy_programs(self):
        marker = self.control / "programs.json"
        state = private_json(marker) if marker.exists() else {"version": 2, "roles": {}}
        lengths = {role: (self.stage / ARTIFACTS[role]).stat().st_size for role in ("portal", "token_bridge")}
        require(isinstance(state, dict) and state.get("version") == 2 and isinstance(state.get("roles"), dict)
                and set(state["roles"]) <= set(lengths), "unknown native upload state schema")
        for role, entry in state["roles"].items():
            require(isinstance(entry, dict) and set(entry) == {"elf_sha256", "complete"}
                    and entry["elf_sha256"] == self.manifest["sha256"][role], "native upload record differs from frozen artifact")
            observed = self.program_view(role)
            require(observed is not None, "retained upload is unresolved; inspect original buffer, never replace it automatically")
            if entry["complete"] is not None:
                require(entry["complete"] == observed, "completed native upload observation changed")
            else:
                entry["complete"] = observed
        if state["roles"]:
            atomic_json(marker, state)
        def rent(size):
            value = self.upstream.call("getMinimumBalanceForRentExemption", [size])
            require(type(value) is int and 0 < value < 1 << 64, "invalid native rent quote")
            return value
        permanent = {role: rent(size + 45) + rent(36) for role, size in lengths.items()}
        buffers = {role: rent(size + 37) for role, size in lengths.items()}
        quote = 500_000_000 + sum(permanent.values()) + max(buffers.values())
        require(quote <= self.args.max_estimated_cost_lamports, "native deployment estimate exceeds approved quote limit")
        missing = [role for role in lengths if role not in state["roles"]]
        if missing:
            remaining = 500_000_000 + sum(permanent[role] for role in missing) + max(buffers[role] for role in missing)
            balance = self.upstream.call("getBalance", [self.plan["funder"], {"commitment": "finalized"}])["value"]
            print(json.dumps({"phase": "funding-quote", "address": self.plan["funder"], "available_lamports": balance,
                              "estimated_total_lamports": quote, "required_remaining_lamports": remaining}), flush=True)
            require(type(balance) is int and balance >= remaining, "native deployer requires additional devnet funding")
        self.execute(["sudo", "-n", "python3", "-B", ROOT / ".github/scripts/stop-vrf-service.py"])
        for role, size in lengths.items():
            if role in state["roles"]:
                continue
            require(self.upstream.account(self.plan["programs"][role]) is None, "unrecorded native deployment exists")
            state["roles"][role] = {"elf_sha256": self.manifest["sha256"][role], "complete": None}
            atomic_json(marker, state)
            self.execute([self.stage / ARTIFACTS["cli"], "--url", self.args.devnet_rpc,
                          "--keypair", self.args.funder_keypair, "program", "deploy", self.stage / ARTIFACTS[role],
                          "--program-id", self.control / "keys" / (role + ".json"), "--buffer", self.control / "keys" / (role + "-buffer.json"),
                          "--max-len", str(size), "--use-rpc"], timeout=900)
            end = time.monotonic() + 300
            while time.monotonic() < end:
                view = self.program_view(role)
                if view is not None:
                    state["roles"][role]["complete"] = view
                    atomic_json(marker, state)
                    break
                time.sleep(0.5)
            else:
                raise RuntimeError("native program finalization timed out; upload barrier retained")

    def stop(self):
        if self.fresh is not None:
            self.fresh.before_stop()
        self.execute(["sudo", "-n", "systemctl", "stop", SERVICE], timeout=180)
        state = self.execute(["systemctl", "show", "--property=ActiveState", "--value", SERVICE]).strip()
        pid = self.execute(["systemctl", "show", "--property=MainPID", "--value", SERVICE]).strip()
        require(state in ("inactive", "failed") and pid == "0", "validator inactivity was not confirmed")

    def event_listener_ready(self):
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        with opener.open("http://127.0.0.1:9101/health", timeout=5) as response:
            return response.status == 200 and response.read(16) == b"ok"

    def start(self):
        if self.fresh is not None:
            return self.fresh.start(self.start_runtime)
        return self.start_runtime()

    def start_runtime(self):
        uploads = private_json(self.control / "programs.json")
        floor = max(uploads["roles"][role]["complete"]["deployment_slot"] for role in ("portal", "token_bridge"))
        require(type(floor) is int and floor >= 0, "invalid native program deployment floor")
        self.execute(["sudo", "-n", "systemctl", "start", SERVICE], timeout=180)
        end = time.monotonic() + 1200
        while time.monotonic() < end:
            state = self.execute(["systemctl", "show", "--property=ActiveState", "--value", SERVICE]).strip()
            require(state != "failed", "validator failed during startup")
            try:
                local = self.l1.call("getSlot", [{"commitment": "confirmed"}])
                upstream = self.upstream.call("getSlot", [{"commitment": "confirmed"}])
                finalized = self.l1.call("getSlot", [{"commitment": "finalized"}])
                if (self.l1.call("getHealth") == "ok" and self.l1.call("getGenesisHash") == DEVNET and
                    self.l1.call("getIdentity")["identity"] == self.plan["validator"] and
                    min(local, finalized) >= floor and abs(local - upstream) <= 50):
                    self.er.call("getGenesisHash")
                    if self.event_listener_ready():
                        return
            except (OSError, ValueError):
                pass
            time.sleep(1)
        raise RuntimeError("native startup/catchup timed out")

    def activate(self):
        binary = self.runtime / ARTIFACTS["validator"]
        plugin = self.runtime / ARTIFACTS["geyser_events"]
        plugin_config = self.runtime / "geyser-events.json"
        record = self.control / "activation.json"
        saved = private_json(record) if record.exists() else None
        requested_source = getattr(self.args, "event_source", None)
        event_source = geyser_event_source(saved) if saved else requested_source or "l1"
        require(requested_source is None or requested_source == event_source, "established event source differs")
        configuration = geyser_events_config(plugin, event_source)
        config_digest = hashlib.sha256(canonical(configuration) + b"\n").hexdigest()
        arguments = self.activation_arguments(self.original["arguments"], binary, self.plan["programs"]["portal"], plugin_config, event_source)
        expected = {"NORTHSTAR_CHECKPOINT_PLAN_DIR": str(self.runtime / "checkpoints"),
                    "NORTHSTAR_UNSETTLED_STATE_DIR": str(self.runtime / "unsettled")}
        spec = {"version": 2, "arguments": arguments, "state_environment": expected, "artifacts": self.plan["artifacts"],
                "geyser_config_sha256": config_digest}
        if saved is None or "event_source" in saved:
            spec["event_source"] = event_source
        if not record.exists():
            actual = self.inventory() if self.fresh is None else self.fresh.original_inventory()
            require(actual["arguments"] == self.original["arguments"] and actual["binary_sha256"] == self.original["binary_sha256"], "original native service changed before activation")
            self.stop()
            try:
                immutable_json(self.control / "retained.json", self.retention())
                immutable_json(record, spec)
            except BaseException:
                if self.fresh is None:
                    self.execute(["sudo", "-n", "systemctl", "start", SERVICE], timeout=180, allowed=True)
                raise
        else:
            require(private_json(record) == spec, "established activation specification changed")
            retained = private_json(self.control / "retained.json")
            require(retained == self.retention(), "retained legacy state changed")
            active = self.execute(["systemctl", "show", "--property=ActiveState", "--value", SERVICE]).strip()
            if active == "active":
                require(self.l1.call("getGenesisHash") == DEVNET and self.l1.call("getIdentity")["identity"] == self.plan["validator"],
                        "running native network identity changed before retry")
                actual = self.inventory()
                original = actual["arguments"] == self.original["arguments"] and actual["binary_sha256"] == self.original["binary_sha256"]
                managed = (actual["arguments"] == arguments and actual["binary_sha256"] == self.manifest["sha256"]["validator"]
                           and actual["state_environment"] == expected)
                require(original or managed, "running native service changed before retry")
            else:
                require(active in ("inactive", "failed") and
                        self.execute(["systemctl", "show", "--property=MainPID", "--value", SERVICE]).strip() == "0",
                        "running native service changed before retry")
            if os.path.lexists(self.control / "sdk-smoke"):
                installed_state = private_json(self.control / "activation-installed.json")
                require(self.inspect([DROPIN])[str(DROPIN)].get("sha256") == installed_state.get("sha256"),
                        "established activation drop-in is missing or changed")
            self.stop()
            require(retained == self.retention(), "retained legacy state changed")
        uid, gid = str(self.original["uid"]), str(self.original["gid"])
        directories = (self.runtime, self.runtime / "bin", self.runtime / "lib", self.runtime / "checkpoints", self.runtime / "unsettled")
        inspected = self.inspect([*directories, binary, plugin, plugin_config])
        for directory in directories:
            observed = inspected[str(directory)]
            if observed.get("absent"):
                self.execute(["sudo", "-n", "install", "-d", "-m", "700", "-o", uid, "-g", gid, directory])
            else:
                require(observed.get("directory") is True and observed.get("uid") == int(uid) and observed.get("mode") == 0o700,
                        "established runtime directory identity differs")
        for role, label in (("validator", "binary"), ("geyser_events", "event plugin")):
            target = self.runtime / ARTIFACTS[role]
            installed = inspected[str(target)]
            if installed.get("absent"):
                self.execute(["sudo", "-n", "install", "-m", "500", "-o", uid, "-g", gid, self.stage / ARTIFACTS[role], target])
            else:
                require(installed.get("sha256") == self.manifest["sha256"][role]
                        and installed.get("uid") == int(uid) and installed.get("mode") == 0o500,
                        "retained native " + label + " bytes or ownership differ")
        config_source = self.control / "geyser-events.json"
        immutable_json(config_source, configuration)
        installed = inspected[str(plugin_config)]
        if installed.get("absent"):
            self.execute(["sudo", "-n", "install", "-m", "400", "-o", uid, "-g", gid, config_source, plugin_config])
        else:
            require(installed.get("sha256") == config_digest and installed.get("uid") == int(uid)
                    and installed.get("mode") == 0o400, "retained event plugin configuration differs")
        dropin = self.control / "validator.conf"
        text = service_dropin(arguments, self.runtime / "checkpoints", self.runtime / "unsettled")
        if dropin.exists():
            require(dropin.read_text() == text, "activation drop-in changed")
        else:
            descriptor = os.open(dropin, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(descriptor, "w") as output:
                output.write(text)
        installed_marker = self.control / "activation-installed.json"
        previous = self.inspect([DROPIN])[str(DROPIN)]
        digest = hashlib.sha256(text.encode()).hexdigest()
        if installed_marker.exists():
            require(private_json(installed_marker) == {"sha256": digest} and previous.get("sha256") == digest,
                    "established service drop-in is missing or changed")
        elif not previous.get("absent"):
            if self.fresh is None:
                require(previous.get("sha256") == digest, "unrecorded service drop-in differs")
            else:
                self.fresh.replacing_dropin(previous, digest)
        self.execute(["sudo", "-n", "install", "-d", "-m", "755", DROPIN.parent])
        self.execute(["sudo", "-n", "install", "-m", "600", dropin, str(DROPIN) + ".new"])
        self.execute(["sudo", "-n", "mv", str(DROPIN) + ".new", DROPIN])
        self.execute(["sudo", "-n", "systemctl", "daemon-reload"])
        effective = self.execute(["systemctl", "show", "--property=ExecStart", "--value", SERVICE])
        effective_paths = re.findall(r"(?:^|[ {])path=([^;]+);", effective)
        require(len(effective_paths) == 1 and effective_paths[0].strip() == "/usr/bin/env" and str(binary) in effective
                and self.plan["programs"]["portal"] in effective and geyser_plugin_flag(event_source) in effective
                and str(plugin_config) in effective, "effective service start differs from isolated activation")
        immutable_json(installed_marker, {"sha256": digest})
        self.flush([binary, plugin, plugin_config, *reversed(directories), self.namespace, DROPIN, DROPIN.parent])
        self.start()
        observed = self.inventory()
        require(observed["arguments"] == arguments and observed["state_environment"] == expected
                and observed["binary_sha256"] == self.manifest["sha256"]["validator"], "running native service does not match isolated specification")

    def smoke(self):
        er_genesis = self.er.call("getGenesisHash")
        public_bytes(er_genesis)
        config = self.plan | {"er_genesis": er_genesis, "l1_rpc": self.args.l1_rpc, "er_rpc": self.args.er_rpc,
                             "portal": self.plan["programs"]["portal"], "token_bridge": self.plan["programs"]["token_bridge"],
                             "sdk_checkout": str(self.args.sdk_checkout), "smoke_directory": str(self.control / "sdk-smoke"),
                             "funder_keypair": str(self.args.funder_keypair), "grid_id": "0", "ttl_slots": "78840000",
                             "fee_cap": "1000000000", "settlement_interval_slots": "75"}
        config_path = self.control / "sdk-config.json"
        atomic_json(config_path, config)
        phases = []
        for phase in ("bootstrap", "prepare-sol", "resume-sol", "prepare-spl", "resume-spl"):
            if phase.startswith("resume-") and not (Path(config["smoke_directory"]) / (phase + ".json")).exists():
                self.stop()
                self.start()
            print(json.dumps({"phase": phase, "status": "starting"}), flush=True)
            raw = self.execute(["node", ROOT / ".github/scripts/devnet-continuity-smoke.mjs", config_path, phase], timeout=1200)
            phases.append(json.loads(raw.splitlines()[-1]))
        require(private_json(self.control / "retained.json") == self.retention(), "legacy state changed during native qualification")
        report = {"version": 2, "result": "passed", "qualification_kind": "first-activation",
                  "scope": "devnet native SOL/SPL payout and restart; not oracle or SP1 qualification",
                  "source_revision": self.args.revision, "sdk_revision": SDK_REVISION, "genesis": DEVNET,
                  "portal": config["portal"], "token_bridge": config["token_bridge"], "validator": self.plan["validator"],
                  "profile": self.plan["profile"], "context": phases[0]["context"], "phases": phases,
                  "legacy_state_unchanged": True, "proof_acceptance_qualified": False}
        activation = private_json(self.control / "activation.json")
        if "event_source" in activation:
            report["event_source"] = geyser_event_source(activation)
        if self.fresh is not None:
            report = self.fresh.report(report)
        atomic_json(self.control / "report.json", report)
        print(json.dumps(report), flush=True)
        output = os.environ.get("GITHUB_OUTPUT")
        if output:
            with open(output, "a") as stream:
                for name in ("portal", "token_bridge", "context", "profile", "qualification_kind"):
                    stream.write(name + "=" + report[name] + "\n")
                stream.write("report_path=" + str(self.control / "report.json") + "\n")

    def run(self):
        if getattr(self.args, "replace_context", None) is not None or getattr(self.args, "abandon_previous_state", False):
            from fresh_devnet import FreshStack
            self.fresh = FreshStack(self, SERVICE, DROPIN)
            return self.fresh.run()
        pending_upgrade = self.args.state_root / "upgrades" / self.args.revision
        same_activation = os.path.lexists(self.control / "activation.json")
        if getattr(self.args, "reconcile_failed_l1", False):
            require(getattr(self.args, "upgrade_existing", False)
                    and getattr(self.args, "recover_failed_l1_slot", None) is not None
                    and os.path.lexists(pending_upgrade / "control/plan.json") and not same_activation,
                    "reconciliation requires an explicit retained L1 recovery")
        if getattr(self.args, "recover_failed_l1_slot", None) is not None:
            require(getattr(self.args, "upgrade_existing", False) and os.path.lexists(DROPIN) and not same_activation,
                    "L1 recovery requires an existing managed native activation and explicit upgrade opt-in")
        if getattr(self.args, "upgrade_existing", False) and (os.path.lexists(pending_upgrade) or os.path.lexists(DROPIN) and not same_activation):
            from native_upgrade import NativeUpgrade
            return NativeUpgrade(self, SERVICE, DROPIN).run()
        self.prepare()
        self.deploy_programs()
        self.activate()
        self.smoke()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subcommands = parser.add_subparsers(dest="operation", required=True)
    subcommands.add_parser("inventory")
    mode = subcommands.add_parser("mode")
    mode.add_argument("--recovery-slot", default="")
    mode.add_argument("--replace-context", default="")
    mode.add_argument("--l1-rpc-profile", default="")
    for name in ("fingerprint", "inspect", "flush", "empty-l1"):
        inspector = subcommands.add_parser(name)
        inspector.add_argument("--input", type=Path, required=True)
    run = subcommands.add_parser("run")
    run.add_argument("--stage", type=Path)
    run.add_argument("--revision", required=True)
    run.add_argument("--sdk-checkout", type=Path, required=True)
    run.add_argument("--state-root", type=Path, default=Path("/solana/northstar/continuity-v2"))
    run.add_argument("--funder-keypair", type=Path, default=Path("/home/ubuntu/.config/solana/id.json"))
    run.add_argument("--devnet-rpc", default="https://api.devnet.solana.com")
    run.add_argument("--l1-rpc", default="http://127.0.0.1:8899")
    run.add_argument("--er-rpc", default="http://127.0.0.1:8910")
    run.add_argument("--max-estimated-cost-lamports", type=int, default=10_000_000_000)
    run.add_argument("--upgrade-existing", action="store_true", help="Permit a guarded native-only upgrade of a known managed v2 activation.")
    run.add_argument("--l1-rpc-profile", choices=("full",), help="Bind unrestricted program, token-owner, and token-mint indexes to a native-only L1 upgrade.")
    run.add_argument("--replace-context", help="Exact reviewed SessionV2 context to abandon during a guarded fresh devnet cutover.")
    run.add_argument("--abandon-previous-state", action="store_true", help="Explicitly leave old on-chain state unused while retaining its local files.")
    run.add_argument("--event-source", choices=("l1", "er"), help="Select the managed SSE source. New plans default to L1; established plans retain their recorded source.")
    run.add_argument("--recover-failed-l1-slot", type=int, help="Recover a stopped validator past the bound feature activation using fresh L1 directories; preserve all prior data.")
    run.add_argument("--reconcile-failed-l1", action="store_true", help="Authorize one additional start of a reviewed stopped L1 recovery, using its retained artifacts and data.")
    run.add_argument("--reconcile-stopped-at", help="Exact reviewed systemd ExecMainExitTimestamp; required for explicit reconciliation.")
    args = parser.parse_args()
    if args.operation == "mode":
        print(" ".join(release_mode_arguments(args.recovery_slot, args.replace_context, args.l1_rpc_profile)))
    elif args.operation == "inventory":
        print(json.dumps(inventory_service()))
    elif args.operation == "empty-l1":
        from native_l1_recovery import inspect_empty_paths
        print(json.dumps(inspect_empty_paths(private_json(args.input)["paths"])))
    elif args.operation in ("fingerprint", "inspect", "flush"):
        inspect_function = {"fingerprint": retained_fingerprint, "inspect": inspect_paths, "flush": sync_paths}[args.operation]
        print(json.dumps(inspect_function(private_json(args.input)["paths"])))
    else:
        require(args.state_root.is_absolute() and not any(character.isspace() for character in str(args.state_root))
                and re.fullmatch("[0-9a-f]{40}", args.revision)
                and 0 < args.max_estimated_cost_lamports <= 50_000_000_000, "invalid rollout root, revision or funding estimate limit")
        require(not args.l1_rpc_profile or args.upgrade_existing and args.event_source in (None, "l1")
                and args.recover_failed_l1_slot is None and args.replace_context is None,
                "L1 RPC profile requires an ordinary L1 native upgrade")
        if args.reconcile_failed_l1:
            require(args.stage is None and args.upgrade_existing and args.recover_failed_l1_slot is not None
                    and args.reconcile_stopped_at is not None, "reconciliation requires retained artifacts and explicit stopped-state evidence")
        else:
            require(args.stage is not None and args.reconcile_stopped_at is None, "a stage is required outside explicit reconciliation")
        for configured in (args.l1_rpc, args.er_rpc):
            url = urlsplit(configured)
            require(url.scheme in ("http", "https") and url.hostname in ("localhost", "127.0.0.1", "::1")
                    and not url.username and not url.password and url.path in ("", "/") and not url.query and not url.fragment,
                    "native control RPC must be a colocated loopback root")
        upstream = urlsplit(args.devnet_rpc)
        require(upstream.scheme == "https" and upstream.hostname and not upstream.username and not upstream.password
                and not upstream.query and not upstream.fragment, "upstream devnet RPC requires unauthenticated HTTPS")
        lock = Path("/solana/northstar/continuity-v2-deploy.lock")
        descriptor = os.open(lock, os.O_RDWR | os.O_NOFOLLOW | (0 if args.reconcile_failed_l1 else os.O_CREAT), 0o600)
        try:
            info = os.fstat(descriptor)
            require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and not info.st_mode & 0o077, "deployment lock is not private regular state")
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
            args.lock_descriptor = descriptor
            if args.reconcile_failed_l1:
                control = args.state_root / "upgrades" / args.revision / "control"
                for directory in (args.state_root, control.parent.parent, control.parent, control):
                    checked_directory(directory, private=directory == control)
                saved = private_json(control / "plan.json")
                require(saved.get("version") == 1 and saved.get("kind") == "native-upgrade"
                        and saved.get("source_revision") == args.revision
                        and saved.get("recovery_failed_slot") == args.recover_failed_l1_slot, "retained reconciliation plan differs")
                args.stage = Path(saved["stage"])
            Deployment(args).run()
        finally:
            os.close(descriptor)


if __name__ == "__main__":
    main()

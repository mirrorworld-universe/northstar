"""Sonic: Upgrade native binaries without replacing a managed protocol context."""
import hashlib
import json
import os
from pathlib import Path
import re
import time

import native_l1_recovery as recovery
import vrf_relay_profile as relay

from continuity_rollout_state import (
    ARTIFACTS, DEVNET, SDK_REVISION, append_private, atomic_json, canonical, checked_directory,
    file_hash, flag_value, geyser_event_source, geyser_events_config, geyser_plugin_flag, immutable_json, private_json, require,
    l1_rpc_arguments, service_dropin, upgrade_arguments, verify_artifacts,
)

ROOT = Path(__file__).resolve().parents[2]
STATE_KEYS = ("NORTHSTAR_CHECKPOINT_PLAN_DIR", "NORTHSTAR_UNSETTLED_STATE_DIR")


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def service_spec(inventory):
    return {name: inventory[name] for name in ("arguments", "binary_sha256", "state_environment", "uid", "gid")}


def start_floor(path, plan):
    value = private_json(path)
    require(set(value) == {"plan_sha256", "l1_floor"} and value["plan_sha256"] == digest(plan)
            and type(value["l1_floor"]) is int and value["l1_floor"] >= plan["l1_floor"],
            "established native start barrier is missing or changed")
    return value["l1_floor"]


class NativeUpgrade:
    def __init__(self, operations, service, dropin):
        self.ops, self.args = operations, operations.args
        self.service, self.dropin = service, Path(dropin)
        self.parent = self.args.state_root / "upgrades"
        self.namespace = self.parent / self.args.revision
        self.control, self.runtime = self.namespace / "control", self.namespace / "runtime"
        self.record = self.control / "plan.json"
        self.frozen = self.control / "frozen.json"
        self.started = self.control / "start-attempted.json"
        self.installed = self.control / "installed.json"
        self.progress = self.control / "progress.json"
        self.retrying = False
        self.rpc_profile = None

    def phase(self):
        value = private_json(self.progress)
        require(value.get("version") == 1 and value.get("plan_sha256") == digest(self.plan)
                and value.get("phase") in ("prepared", "frozen", "installed", "start-attempted", "qualified"),
                "native upgrade progress binding differs")
        return value["phase"]

    def advance(self, allowed, phase):
        require(self.phase() in allowed, "native upgrade phase transition differs")
        atomic_json(self.progress, {"version": 1, "plan_sha256": digest(self.plan), "phase": phase})

    def state(self):
        active = self.ops.execute(["systemctl", "show", "--property=ActiveState", "--value", self.service]).strip()
        pid = self.ops.execute(["systemctl", "show", "--property=MainPID", "--value", self.service]).strip()
        require(active == "active" and pid.isdigit() and int(pid) > 0 or active in ("inactive", "failed") and pid == "0",
                "native service is not in a stable active/inactive state")
        return active

    def inspect(self, paths):
        return self.ops.inspect(paths)

    def fingerprint(self, paths):
        request = self.control / "fingerprint-input.json"
        atomic_json(request, {"paths": list(map(str, paths))})
        return json.loads(self.ops.execute(["sudo", "-n", "python3", "-B", ROOT / ".github/scripts/continuity_devnet.py",
                                           "fingerprint", "--input", request], timeout=300))

    def snapshot(self, mode):
        config = {
            "version": 2, "genesis": DEVNET, "sdk_revision": SDK_REVISION,
            "sdk_checkout": str(self.args.sdk_checkout), "devnet_rpc": self.args.devnet_rpc,
            "l1_rpc": self.args.l1_rpc, "er_rpc": self.args.er_rpc,
            "portal": self.base_plan["programs"]["portal"], "token_bridge": self.base_plan["programs"]["token_bridge"],
            "validator": self.base_plan["validator"], "profile": self.base_plan["profile"],
        }
        path = self.control / "snapshot-input.json"
        atomic_json(path, config)
        raw = self.ops.execute(["node", ROOT / ".github/scripts/native-upgrade-snapshot.mjs", path, mode], timeout=120)
        result = json.loads(raw.splitlines()[-1])
        identity = result.get("identity", {})
        require(identity.get("version") == 2 and all(identity.get(name) == config[name] for name in
                ("genesis", "portal", "token_bridge", "validator", "profile"))
                and isinstance(identity.get("context"), str) and re.fullmatch("[0-9a-f]{64}", identity["context"])
                and isinstance(identity.get("nonce"), str) and identity["nonce"].isdigit() and int(identity["nonce"]) > 0,
                "retained session snapshot identity differs")
        return result

    def programs(self):
        for role in ("portal", "token_bridge"):
            require(self.base_manifest["sha256"][role] == self.manifest["sha256"][role],
                    "native upgrade cannot change on-chain program artifacts")
        self.retained_programs()

    def retained_programs(self):
        self.program_floor = 0
        uploads = private_json(self.base_control / "programs.json")
        require(uploads.get("version") == 2 and set(uploads.get("roles", {})) == {"portal", "token_bridge"},
                "retained program upload records are incomplete")
        for role in ("portal", "token_bridge"):
            entry = uploads["roles"][role]
            observed = self.ops.program_view(role, stage=self.base_stage, plan=self.base_plan)
            require(entry.get("elf_sha256") == self.base_manifest["sha256"][role]
                    and observed is not None and entry.get("complete") == observed,
                    "retained on-chain program or upload binding differs")
            self.program_floor = max(self.program_floor, observed["deployment_slot"])

    def load_base(self, before):
        self.load_retained_base(before)
        self.programs()

    def load_retained_base(self, before):
        environment = before["state_environment"]
        require(set(environment) == set(STATE_KEYS) and all(isinstance(environment[key], str) for key in STATE_KEYS),
                "managed runtime state bindings are missing")
        self.base = Path(environment[STATE_KEYS[0]]).parent.parent
        require(self.base.parent == self.args.state_root and re.fullmatch("[0-9a-f]{40}", self.base.name)
                and environment == dict(zip(STATE_KEYS, (str(self.base / "runtime/checkpoints"), str(self.base / "runtime/unsettled")))),
                "managed journal namespace differs")
        checked_directory(self.base)
        self.base_control = self.base / "control"
        checked_directory(self.base_control, private=True)
        reservation = private_json(self.base_control / "reservation.json")
        self.base_plan = private_json(self.base_control / "plan.json")
        self.base_activation = private_json(self.base_control / "activation.json")
        self.base_stage = Path(reservation["stage"])
        checked_directory(self.base_stage)
        self.base_manifest = verify_artifacts(self.base_stage, self.base.name, retained=True)
        plan = self.base_plan
        require(reservation.get("version") == 2 and reservation.get("source_revision") == self.base.name
                and reservation.get("plan_sha256") == digest(plan)
                and all(reservation.get(name) == getattr(self.args, name) for name in ("l1_rpc", "er_rpc", "devnet_rpc"))
                and plan.get("version") == 2 and plan.get("source_revision") == self.base.name
                and isinstance(plan.get("sdk_revision"), str) and re.fullmatch("[0-9a-f]{40}", plan["sdk_revision"])
                and reservation.get("sdk_revision") == plan["sdk_revision"] and plan.get("genesis") == DEVNET
                and plan.get("artifacts") == self.base_manifest["sha256"]
                and plan.get("original_sha256") == digest(reservation["original"]), "retained native plan binding differs")
        expected_profile = digest({"version": 2, "source": self.base.name, "artifacts": plan["artifacts"],
                                   "sdk": plan["sdk_revision"], "proof_acceptance_qualified": False})
        require(plan.get("profile") == expected_profile and self.base_activation.get("version") == 2
                and self.base_activation.get("artifacts") == plan["artifacts"]
                and self.base_activation.get("state_environment") == environment
                and flag_value(self.base_activation["arguments"], "--portal") == plan["programs"]["portal"]
                and self.base_activation["arguments"][0] == str(self.base / "runtime" / ARTIFACTS["validator"])
                and before["uid"] == reservation["original"]["uid"] and before["gid"] == reservation["original"]["gid"],
                "retained activation specification differs")

    def binding(self, before):
        binary = Path(before["arguments"][0])
        base_binary = self.base / "runtime" / ARTIFACTS["validator"]
        if binary == base_binary:
            expected = {"arguments": self.base_activation["arguments"], "state_environment": self.base_activation["state_environment"],
                        "binary_sha256": self.base_manifest["sha256"]["validator"], "uid": before["uid"], "gid": before["gid"]}
            require(service_spec(before) == expected, "running native service differs from retained activation")
            marker = private_json(self.base_control / "activation-installed.json")
            config = self.base / "runtime/geyser-events.json" if "geyser_config_sha256" in self.base_activation else None
            artifacts = self.base_manifest["sha256"]
            self.previous_event_source = geyser_event_source(self.base_activation)
            self.previous_rpc_profile = self.base_activation.get("l1_rpc_profile")
            self.previous_relay = self.base_activation.get("vrf_relay")
        else:
            previous = binary.parent.parent.parent
            require(previous.parent == self.parent and re.fullmatch("[0-9a-f]{40}", previous.name)
                    and previous != self.namespace and binary == previous / "runtime" / ARTIFACTS["validator"],
                    "unknown managed native executable")
            checked_directory(previous)
            checked_directory(previous / "control", private=True)
            saved = private_json(previous / "control/plan.json")
            report = private_json(previous / "control/report.json")
            require(private_json(previous / "control/progress.json") == {"version": 1, "plan_sha256": digest(saved), "phase": "qualified"}
                    and start_floor(previous / "control/start-attempted.json", saved) >= saved["l1_floor"]
                    and private_json(previous / "control/frozen.json").get("plan_sha256") == digest(saved),
                    "previous native upgrade control records are incomplete")
            require(saved.get("version") == 1 and saved.get("kind") == "native-upgrade"
                    and saved.get("base_revision") == self.base.name and saved.get("base_plan_sha256") == digest(self.base_plan)
                    and saved.get("source_revision") == previous.name and service_spec(before) == saved["desired"]
                    and report.get("result") == "passed" and report.get("upgrade_plan_sha256") == digest(saved),
                    "previous native upgrade is not a qualified retained binding")
            artifacts = verify_artifacts(saved["stage"], previous.name)["sha256"]
            require(saved["artifacts"] == artifacts, "previous native artifact binding differs")
            config = previous / "runtime/geyser-events.json"
            marker = private_json(previous / "control/installed.json")
            self.previous_event_source = geyser_event_source(saved)
            self.previous_rpc_profile = saved.get("l1_rpc_profile")
            self.previous_relay = saved.get("vrf_relay")
        require(self.previous_rpc_profile in (None, "full"), "retained L1 RPC profile differs")
        require(before["binary_sha256"] == artifacts["validator"], "retained native manifest and executable differ")
        text = service_dropin(before["arguments"], *[before["state_environment"][key] for key in STATE_KEYS])
        require(marker == {"sha256": hashlib.sha256(text.encode()).hexdigest()}, "retained service drop-in binding differs")
        paths = [binary, *[Path(before["state_environment"][key]) for key in STATE_KEYS]]
        if config is not None:
            paths.extend([config, binary.parent.parent / ARTIFACTS["geyser_events"]])
        previous_relay_config = flag_value(before["arguments"], relay.FLAG) if self.previous_relay is not None else None
        require(self.previous_relay is not None or not any(value == relay.FLAG or value.startswith(relay.FLAG + "=") for value in before["arguments"]),
                "unrecorded native relay configuration requires explicit review")
        if previous_relay_config is not None:
            require(previous_relay_config == str(binary.parent.parent / "vrf-relay.json"), "retained relay path differs")
            paths.extend([Path(previous_relay_config), relay.JOURNAL])
        observed = self.inspect(paths)
        if previous_relay_config is not None:
            info = observed[previous_relay_config]
            require(info.get("sha256") == relay.configuration_hash(self.previous_relay) and info.get("uid") == before["uid"]
                    and info.get("mode") == 0o400, "retained relay configuration differs")
        require(observed[str(binary)].get("sha256") == before["binary_sha256"]
                and observed[str(binary)].get("mode") == 0o500 and observed[str(binary)].get("uid") == before["uid"],
                "retained native executable bytes or ownership differ")
        for key in STATE_KEYS:
            require(observed[before["state_environment"][key]].get("directory") is True
                    and observed[before["state_environment"][key]].get("uid") == before["uid"]
                    and observed[before["state_environment"][key]].get("mode") == 0o700, "retained journal directory differs")
        if config is not None:
            plugin = binary.parent.parent / ARTIFACTS["geyser_events"]
            expected_config = canonical(geyser_events_config(plugin, self.previous_event_source)) + b"\n"
            require((binary != base_binary or self.base_activation.get("geyser_config_sha256") == hashlib.sha256(expected_config).hexdigest())
                    and observed[str(config)].get("sha256") == hashlib.sha256(expected_config).hexdigest()
                    and observed[str(config)].get("mode") == 0o400 and observed[str(config)].get("uid") == before["uid"]
                    and observed[str(plugin)].get("sha256") == artifacts["geyser_events"]
                    and observed[str(plugin)].get("mode") == 0o500 and observed[str(plugin)].get("uid") == before["uid"],
                    "retained event plugin binding differs")
        binding = {"dropin_sha256": marker["sha256"], "config": str(config) if config else None}
        if previous_relay_config is not None:
            binding["relay_config"] = previous_relay_config
        return binding

    def prepare(self):
        require(re.fullmatch("[0-9a-f]{40}", self.args.revision), "full native source revision required")
        require(self.args.stage.is_absolute() and self.args.sdk_checkout.is_absolute(), "native input directories must be absolute")
        require(self.ops.upstream.call("getGenesisHash") == DEVNET, "native upgrade is devnet-only")
        self.ops.verify_sdk()
        self.manifest = verify_artifacts(self.args.stage, self.args.revision)
        checked_directory(self.args.state_root)
        for directory, private in ((self.parent, False), (self.namespace, False), (self.control, True)):
            if not os.path.lexists(directory):
                directory.mkdir(mode=0o700 if private else 0o711)
            checked_directory(directory, private=private)
        self.ops.control = self.control
        self.ops.log = self.control / "operations.log"
        append_private(self.ops.log, b"")
        saved = private_json(self.record) if self.record.exists() else None
        requested_source = getattr(self.args, "event_source", None)
        self.event_source = geyser_event_source(saved) if saved else requested_source or "l1"
        require(requested_source is None or requested_source == self.event_source, "established event source differs")
        requested_recovery = getattr(self.args, "recover_failed_l1_slot", None)
        self.recovery_slot = saved.get("recovery_failed_slot") if saved else requested_recovery
        require(requested_recovery is None or requested_recovery == self.recovery_slot,
                "established L1 recovery slot differs")
        if self.recovery_slot is not None:
            recovery.feature(self.ops, self.recovery_slot, self.args.revision)
        active = self.state() == "active"
        actual = self.ops.inventory() if active else None
        if saved:
            require(saved.get("version") == 1 and saved.get("kind") == "native-upgrade"
                    and saved.get("source_revision") == self.args.revision
                    and saved.get("artifacts") == self.manifest["sha256"], "established native upgrade plan differs")
            before = saved["before"]
            self.stage = Path(saved["stage"])
            require(verify_artifacts(self.stage, self.args.revision)["sha256"] == saved["artifacts"], "retained upgrade stage differs")
        else:
            require((active and self.recovery_slot is None or not active and self.recovery_slot is not None)
                    and not os.path.lexists(self.runtime) and not any(os.path.lexists(path) for path in
                    (self.frozen, self.started, self.installed, self.progress, self.control / "report.json")),
                    "native upgrade lacks an original active service or established plan")
            before = recovery.offline_inventory(self.ops, self.args.state_root, self.service, self.dropin) if self.recovery_slot is not None else actual
            self.stage = self.args.stage
        self.load_base(before)
        binding = self.binding(before)
        requested_profile = getattr(self.args, "l1_rpc_profile", None)
        self.rpc_profile = saved.get("l1_rpc_profile") if saved else requested_profile or self.previous_rpc_profile
        require(self.rpc_profile in (None, "full") and (requested_profile is None or requested_profile == self.rpc_profile),
                "established L1 RPC profile differs")
        require(not self.rpc_profile or self.event_source == "l1" and self.recovery_slot is None,
                "L1 RPC profile requires an ordinary L1 native upgrade")
        if self.recovery_slot is not None:
            recovery.verify_service_files(self.ops, self.base, self.service, self.dropin)
        config = self.runtime / "geyser-events.json"
        desired_arguments = l1_rpc_arguments(upgrade_arguments(before["arguments"], self.runtime / ARTIFACTS["validator"], config, binding["config"],
                                              previous_source=self.previous_event_source, event_source=self.event_source), self.rpc_profile)
        requested_relay = getattr(self.args, "vrf_relay_profile", None)
        requested_relay_sha = getattr(self.args, "vrf_relay_profile_sha256", None)
        require(bool(requested_relay) == bool(requested_relay_sha), "relay source and digest must be supplied together")
        loaded_relay = relay.read_profile(requested_relay, requested_relay_sha) if requested_relay else None
        self.relay_profile = saved.get("vrf_relay") if saved else loaded_relay or self.previous_relay
        require(loaded_relay is None or loaded_relay == self.relay_profile, "established relay profile differs")
        if self.relay_profile is not None:
            require(self.recovery_slot is None and self.event_source == "l1", "relay requires an ordinary retained native upgrade")
            snapshot_relay = self.snapshot("online")
            relay.validate(self.relay_profile, snapshot_relay["identity"], snapshot_relay["er_genesis"], self.args.l1_rpc, self.args.er_rpc)
            relay.preflight(self, self.relay_profile)
            desired_arguments = relay.arguments(desired_arguments, self.runtime / "vrf-relay.json", binding.get("relay_config"))
        if self.recovery_slot is not None:
            desired_arguments = recovery.arguments(desired_arguments, self.runtime)
        desired = {**service_spec(before), "arguments": desired_arguments,
                   "binary_sha256": self.manifest["sha256"]["validator"]}
        self.ops.manifest = self.manifest
        self.ops.stage = self.stage
        if saved:
            require(saved.get("desired") == desired and saved.get("binding") == binding
                    and saved.get("base_revision") == self.base.name and saved.get("base_plan_sha256") == digest(self.base_plan),
                    "established native upgrade specification differs")
            self.plan = saved
        else:
            dropin = self.inspect([self.dropin])[str(self.dropin)]
            require(dropin.get("sha256") == binding["dropin_sha256"] and dropin.get("mode") == 0o600, "managed service drop-in changed")
            snapshot = self.snapshot("l1" if self.recovery_slot is not None else "online")
            if self.recovery_slot is not None:
                snapshot = recovery.retained_snapshot(self.base_control, self.base_plan, self.args, snapshot)
            slots = [] if self.recovery_slot is not None else [self.ops.l1.call("getSlot", [{"commitment": commitment}]) for commitment in ("processed", "confirmed", "finalized")]
            slots.extend([self.ops.upstream.call("getSlot", [{"commitment": "confirmed"}]), self.program_floor, self.recovery_slot or 0])
            require(all(type(slot) is int and slot >= 0 for slot in slots), "invalid native recovery slot floor")
            floor = max(slots)
            retained = (set(before["retained_paths"]) - {str(self.dropin)}) | {str(self.base_control), before["arguments"][0], *before["state_environment"].values()}
            if binding["config"]:
                retained.update([binding["config"], str(Path(binding["config"]).parent / ARTIFACTS["geyser_events"])])
            if binding.get("relay_config"):
                retained.update([binding["relay_config"], str(relay.JOURNAL)])
            retained = sorted(retained)
            self.plan = {"version": 1, "kind": "native-upgrade", "source_revision": self.args.revision, "stage": str(self.stage),
                         "event_source": self.event_source,
                         "artifacts": self.manifest["sha256"], "base_revision": self.base.name, "base_plan_sha256": digest(self.base_plan),
                         "before": service_spec(before), "binding": binding, "desired": desired, "retained_paths": retained,
                         "identity": snapshot["identity"], "er_genesis": snapshot["er_genesis"], "l1_floor": floor}
            if self.recovery_slot is not None:
                self.plan["recovery_failed_slot"] = self.recovery_slot
            if self.rpc_profile:
                self.plan["l1_rpc_profile"] = self.rpc_profile
            if self.relay_profile is not None:
                self.plan["vrf_relay"] = self.relay_profile
            immutable_json(self.record, self.plan)
            immutable_json(self.progress, {"version": 1, "plan_sha256": digest(self.plan), "phase": "prepared"})
        phase = self.phase()
        if self.rpc_profile or self.relay_profile is not None:
            base_report = private_json(self.base_control / "report.json")
            identity = self.plan["identity"]
            require(base_report.get("result") == "passed" and base_report.get("source_revision") == self.base.name
                    and base_report.get("qualification_kind") in ("first-activation", "fresh-stack")
                    and all(base_report.get(key) == identity[key] for key in ("portal", "token_bridge", "validator", "profile", "context")),
                    "L1 RPC profile requires a qualified retained context")
            if "replacement" in private_json(self.base_control / "reservation.json"):
                require(base_report.get("qualification_kind") == "fresh-stack"
                        and base_report.get("fresh_plan_sha256") == digest(self.base_plan)
                        and private_json(self.base_control / "fresh-progress.json") ==
                        {"version": 1, "plan_sha256": digest(self.base_plan), "phase": "qualified"},
                        "fresh L1 RPC base lacks qualified progress")
        reconcile = getattr(self.args, "reconcile_failed_l1", False)
        require(not reconcile or self.recovery_slot is not None and phase == "start-attempted" and not active,
                "reconciliation requires an unqualified stopped L1 recovery")
        if phase != "prepared":
            require(private_json(self.frozen).get("plan_sha256") == digest(self.plan), "native frozen inventory binding differs")
        if phase in ("installed", "start-attempted", "qualified"):
            expected_dropin = service_dropin(desired["arguments"], *[desired["state_environment"][key] for key in STATE_KEYS])
            require(private_json(self.installed) == {"sha256": hashlib.sha256(expected_dropin.encode()).hexdigest()},
                    "established native installation marker differs")
        if phase in ("start-attempted", "qualified"):
            start_floor(self.started, self.plan)
        else:
            require(not os.path.lexists(self.started), "unexpected native start barrier")
        if phase == "qualified":
            report = private_json(self.control / "report.json")
            require(report.get("upgrade_plan_sha256") == digest(self.plan) and report.get("result") == "passed"
                    and report.get("qualification_kind") == self.qualification_kind() and report.get("source_revision") == self.args.revision,
                    "established native report differs")
        self.current = service_spec(actual) if actual else None
        require(self.current is None or self.current in (self.plan["before"], desired), "running native service changed before upgrade")
        if self.current == desired:
            require(phase in ("start-attempted", "qualified"), "running native upgrade lacks its start barrier")
        elif phase in ("start-attempted", "qualified"):
            require(reconcile, "native start was already attempted; reconcile the service before retry")
        if not active:
            require(self.frozen.exists() or self.recovery_slot is not None and phase == "prepared",
                    "inactive native upgrade lacks a frozen journal inventory")
        require(self.snapshot("l1")["identity"] == self.plan["identity"], "retained session identity changed before upgrade")
        if reconcile:
            from native_l1_recovery_retry import prepare
            prepare(self)

    def freeze(self):
        if self.current is not None:
            self.ops.stop()
        if self.frozen.exists():
            self.verify_frozen()
            if self.phase() == "prepared":
                self.advance(("prepared",), "frozen")
            return
        try:
            retained = self.fingerprint(self.plan["retained_paths"])
            controls = {name: value for name, value in retained.items() if Path(name) == self.base_control or self.base_control in Path(name).parents}
            immutable_json(self.frozen, {"plan_sha256": digest(self.plan), "retained": retained, "protocol_controls": controls})
            self.advance(("prepared",), "frozen")
        except BaseException:
            # Only the original runtime may resume before any configuration change.
            if self.current is not None and self.recovery_slot is None and self.inspect([self.dropin])[str(self.dropin)].get("sha256") == self.plan["binding"]["dropin_sha256"]:
                self.ops.execute(["sudo", "-n", "systemctl", "start", self.service], timeout=180, allowed=True)
            raise

    def verify_frozen(self):
        record = private_json(self.frozen)
        require(record.get("plan_sha256") == digest(self.plan)
                and record.get("retained") == self.fingerprint(self.plan["retained_paths"]), "retained journals or protocol state changed while stopped")

    def install(self):
        before = self.plan["before"]
        uid, gid = str(before["uid"]), str(before["gid"])
        directories = (self.runtime, self.runtime / "bin", self.runtime / "lib", *recovery.paths(self.runtime)) if self.recovery_slot is not None else (self.runtime, self.runtime / "bin", self.runtime / "lib")
        targets = {role: self.runtime / ARTIFACTS[role] for role in ("validator", "geyser_events")}
        config = self.runtime / "geyser-events.json"
        observed = self.inspect([*directories, *targets.values(), config, self.dropin])
        expected_text = service_dropin(self.plan["desired"]["arguments"], *[before["state_environment"][key] for key in STATE_KEYS])
        dropin_digest = hashlib.sha256(expected_text.encode()).hexdigest()
        require(observed[str(self.dropin)].get("sha256") in (self.plan["binding"]["dropin_sha256"], dropin_digest)
                and observed[str(self.dropin)].get("mode") == 0o600, "managed drop-in changed during upgrade")
        for directory in directories:
            info = observed[str(directory)]
            if info.get("absent"):
                self.ops.execute(["sudo", "-n", "install", "-d", "-m", "700", "-o", uid, "-g", gid, directory])
            else:
                require(info.get("directory") is True and info.get("uid") == before["uid"] and info.get("mode") == 0o700, "native upgrade directory differs")
        for role, target in targets.items():
            info = observed[str(target)]
            if info.get("absent"):
                self.ops.execute(["sudo", "-n", "install", "-m", "500", "-o", uid, "-g", gid, self.stage / ARTIFACTS[role], target])
            else:
                require(info.get("sha256") == self.plan["artifacts"][role] and info.get("uid") == before["uid"] and info.get("mode") == 0o500,
                        "retained upgrade native artifact differs")
        configuration = geyser_events_config(targets["geyser_events"], self.event_source)
        source = self.control / "geyser-events.json"
        immutable_json(source, configuration)
        info = observed[str(config)]
        if info.get("absent"):
            self.ops.execute(["sudo", "-n", "install", "-m", "400", "-o", uid, "-g", gid, source, config])
        else:
            require(info.get("sha256") == hashlib.sha256(canonical(configuration) + b"\n").hexdigest()
                    and info.get("uid") == before["uid"] and info.get("mode") == 0o400, "native upgrade plugin configuration differs")
        relay.install(self)
        source_dropin = self.control / "validator.conf"
        if os.path.lexists(source_dropin):
            require(file_hash(source_dropin) == dropin_digest and source_dropin.lstat().st_mode & 0o777 == 0o600,
                    "retained upgrade drop-in text or mode differs")
        else:
            descriptor = os.open(source_dropin, os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW, 0o600)
            with os.fdopen(descriptor, "w") as output:
                output.write(expected_text)
        self.verify_frozen()
        temporary = self.inspect([str(self.dropin) + ".new"])[str(self.dropin) + ".new"]
        require(temporary.get("absent") or temporary.get("sha256") == dropin_digest, "unknown pending service drop-in")
        self.ops.execute(["sudo", "-n", "install", "-m", "600", source_dropin, str(self.dropin) + ".new"])
        self.ops.execute(["sudo", "-n", "mv", str(self.dropin) + ".new", self.dropin])
        self.ops.execute(["sudo", "-n", "systemctl", "daemon-reload"])
        require(self.inspect([self.dropin])[str(self.dropin)].get("sha256") == dropin_digest, "installed native upgrade drop-in differs")
        self.effective_start()
        self.native_files()
        immutable_json(self.installed, {"sha256": dropin_digest})
        self.advance(("frozen", "installed"), "installed")
        self.ops.flush([*targets.values(), config, source_dropin, self.record, self.frozen, self.installed, self.progress, self.control,
                        *reversed(directories), self.namespace, self.parent, self.args.state_root, self.dropin, self.dropin.parent])
        self.verify_frozen()

    def effective_start(self):
        effective = self.ops.execute(["systemctl", "show", "--property=ExecStart", "--value", self.service])
        paths = re.findall(r"(?:^|[ {])path=([^;]+);", effective)
        require(len(paths) == 1 and paths[0].strip() == "/usr/bin/env"
                and all(str(value) in effective for value in (self.runtime / ARTIFACTS["validator"], self.runtime / "geyser-events.json",
                "--portal", geyser_plugin_flag(self.event_source), self.base_plan["programs"]["portal"], *self.plan["before"]["state_environment"].values())),
                "another systemd override replaces the native upgrade")
        if self.recovery_slot is not None:
            require(all(str(path) in effective for path in recovery.paths(self.runtime)[1:]),
                    "effective L1 recovery paths differ")
            recovery.verify_service_files(self.ops, self.base, self.service, self.dropin)

    def native_files(self):
        targets = {role: self.runtime / ARTIFACTS[role] for role in ("validator", "geyser_events")}
        config = self.runtime / "geyser-events.json"
        observed = self.inspect([*targets.values(), config])
        for role, target in targets.items():
            require(observed[str(target)].get("sha256") == self.plan["artifacts"][role]
                    and observed[str(target)].get("mode") == 0o500 and observed[str(target)].get("uid") == self.plan["before"]["uid"],
                    "installed native upgrade artifact differs")
        require(observed[str(config)].get("sha256") == hashlib.sha256(canonical(geyser_events_config(targets["geyser_events"], self.event_source)) + b"\n").hexdigest()
                and observed[str(config)].get("mode") == 0o400 and observed[str(config)].get("uid") == self.plan["before"]["uid"],
                "installed native upgrade plugin configuration differs")

    def verify_running_files(self):
        self.native_files()
        relay.verify(self)
        require(private_json(self.installed) == {"sha256": hashlib.sha256(service_dropin(self.plan["desired"]["arguments"],
                *[self.plan["before"]["state_environment"][key] for key in STATE_KEYS]).encode()).hexdigest()}, "native installation marker differs")
        dropin = self.inspect([self.dropin])[str(self.dropin)]
        require(dropin.get("sha256") == private_json(self.installed)["sha256"] and dropin.get("mode") == 0o600, "native installed drop-in changed")
        require(service_spec(self.ops.inventory()) == self.plan["desired"], "running native upgrade specification differs")

    def wait_ready(self):
        floor = start_floor(self.started, self.plan)
        if self.retrying:
            from native_l1_recovery_retry import retry_floor
            floor = max(floor, retry_floor(self))
        end = time.monotonic() + (3600 if self.recovery_slot is not None else 1200)
        while time.monotonic() < end:
            require(self.state() == "active", "native upgraded runtime failed during startup")
            status = None
            try:
                local = self.ops.l1.call("getSlot", [{"commitment": "confirmed"}])
                finalized = self.ops.l1.call("getSlot", [{"commitment": "finalized"}])
                upstream = self.ops.upstream.call("getSlot", [{"commitment": "confirmed"}])
                if (self.ops.l1.call("getHealth") == "ok" and self.ops.l1.call("getGenesisHash") == DEVNET
                        and self.ops.l1.call("getIdentity")["identity"] == self.base_plan["validator"]
                        and min(local, finalized) >= floor and abs(local - upstream) <= 50
                        and self.ops.event_listener_ready()):
                    status = self.ops.er.call("northstarSysGetSyncStatus")
            except (OSError, ValueError):
                pass
            if status is not None:
                if self.recovery_slot is not None:
                    recovery.verify_snapshot(self.ops, self.recovery_slot)
                require(status.get("checkpointError") is None, "native checkpoint quarantine blocks upgrade qualification")
                if (status.get("isSyncing") is False and type(status.get("latestSyncedSlot")) is int
                        and type(status.get("latestL1Slot")) is int
                        and status["latestSyncedSlot"] >= status["latestL1Slot"] >= floor):
                    try:
                        snapshot = self.snapshot("online")
                    except (OSError, ValueError):
                        time.sleep(1)
                        continue
                    require(snapshot["identity"] == self.plan["identity"] and snapshot["er_genesis"] == self.plan["er_genesis"],
                            "native upgraded session or ER genesis differs")
                    return
            time.sleep(1)
        raise RuntimeError("native upgrade readiness timed out; no automatic rollback is permitted")

    def qualification_kind(self):
        return "native-l1-recovery" if self.recovery_slot is not None else "native-upgrade"

    def qualify(self):
        self.verify_running_files()
        self.wait_ready()
        self.verify_running_files()
        self.programs()
        frozen = private_json(self.frozen)
        require(frozen.get("plan_sha256") == digest(self.plan)
                and frozen["protocol_controls"] == self.fingerprint([self.base_control]), "retained protocol control records changed")
        identity = self.plan["identity"]
        report = {"version": 2, "result": "passed", "qualification_kind": self.qualification_kind(),
                  "scope": "native binary/plugin and retained session readiness; not a fresh SOL/SPL, oracle, or SP1 qualification",
                  "source_revision": self.args.revision, "base_source_revision": self.base.name,
                  "upgrade_plan_sha256": digest(self.plan), "portal": identity["portal"], "token_bridge": identity["token_bridge"],
                  "validator": identity["validator"], "session": identity["session"], "context": identity["context"], "profile": identity["profile"],
                  "controller_onchain_transactions": 0, "program_deployments": 0, "session_replaced": False,
                  "journal_paths_preserved": True, "protocol_controls_unchanged": True, "sse_health": "ok",
                  "proof_acceptance_qualified": False}
        if "event_source" in self.plan:
            report["event_source"] = self.event_source
        if self.rpc_profile:
            report["l1_rpc_profile"] = self.rpc_profile
        if self.plan.get("vrf_relay") is not None:
            report["vrf_relay_enabled"] = True
            report["vrf_callback_qualified"] = False
        if self.recovery_slot is not None:
            report.update(recovery_failed_slot=self.recovery_slot, prior_l1_data_preserved=True)
        immutable_json(self.control / "report.json", report)
        self.advance(("start-attempted", "qualified"), "qualified")
        if os.environ.get("GITHUB_OUTPUT"):
            with open(os.environ["GITHUB_OUTPUT"], "a") as output:
                for name in ("portal", "token_bridge", "context", "profile", "qualification_kind"):
                    output.write(name + "=" + report[name] + "\n")
                output.write("report_path=" + str(self.control / "report.json") + "\n")
        return report

    def quarantine(self):
        # Cancel pending restarts too, but never revert journals or configuration.
        try:
            text = service_dropin(self.plan["desired"]["arguments"], *[self.plan["desired"]["state_environment"][key] for key in STATE_KEYS])
            if self.inspect([self.dropin])[str(self.dropin)].get("sha256") == hashlib.sha256(text.encode()).hexdigest():
                self.effective_start()
                self.ops.stop()
                return True
        except Exception:
            pass
        append_private(self.ops.log, b"Native failure stop requires operator reconciliation.\n")
        return False

    def run(self):
        self.prepare()
        if self.retrying:
            from native_l1_recovery_retry import run
            return run(self)
        if self.current != self.plan["desired"]:
            self.freeze()
            self.install()
            if self.recovery_slot is not None:
                recovery.verify_empty_paths(self.ops, self.runtime)
            require(self.snapshot("l1")["identity"] == self.plan["identity"], "retained session changed while stopped")
            floor = self.ops.upstream.call("getSlot", [{"commitment": "processed"}])
            require(type(floor) is int and floor >= 0, "invalid pre-start L1 slot floor")
            self.advance(("installed",), "start-attempted")
            immutable_json(self.started, {"plan_sha256": digest(self.plan), "l1_floor": max(floor, self.plan["l1_floor"])})
            self.ops.flush([self.started, self.progress, self.control])
        else:
            start_floor(self.started, self.plan)
        try:
            if self.current != self.plan["desired"]:
                self.ops.execute(["sudo", "-n", "systemctl", "start", self.service], timeout=180)
            report = self.qualify()
        except BaseException:
            self.quarantine()
            raise
        print(json.dumps(report), flush=True)
        return report

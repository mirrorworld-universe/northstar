"""Sonic: Explicit fresh SessionV2 activation with archived prior state and bounded starts."""
import copy
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import tempfile
import time

from continuity_rollout_state import (
    ARTIFACTS, DEVNET, SDK_REVISION, atomic_json, canonical, checked_directory, flag_value,
    geyser_events_config, immutable_json, private_json, require, service_dropin, upgrade_arguments,
)
from native_runtime_diagnostics import effective_arguments_match, read_dropin
from native_upgrade import NativeUpgrade, STATE_KEYS, digest, service_spec

ROOT = Path(__file__).resolve().parents[2]


class FreshStack:
    def __init__(self, operations, service, dropin):
        self.ops, self.args = operations, operations.args
        self.service, self.dropin = service, Path(dropin)
        self.control, self.runtime = self.ops.control, self.ops.runtime
        self.progress = self.control / "fresh-progress.json"
        self.starts = self.control / "fresh-starts"
        self.previous = None

    def authorize(self):
        require(getattr(self.args, "abandon_previous_state", False) is True
                and isinstance(getattr(self.args, "replace_context", None), str)
                and re.fullmatch("[0-9a-f]{64}", self.args.replace_context)
                and not getattr(self.args, "upgrade_existing", False)
                and getattr(self.args, "recover_failed_l1_slot", None) is None
                and not getattr(self.args, "reconcile_failed_l1", False)
                and getattr(self.args, "event_source", None) in (None, "l1"),
                "fresh cutover requires explicit context abandonment and no other activation mode")
        require(not os.path.lexists(self.args.state_root / "upgrades" / self.args.revision),
                "source revision already has a native-upgrade namespace")
        self.args.event_source = "l1"

    def inspect_temporary(self, paths):
        with tempfile.TemporaryDirectory(prefix="northstar-fresh-inspect-") as directory:
            request = Path(directory) / "input.json"
            atomic_json(request, {"paths": list(map(str, paths))})
            return json.loads(self.ops.execute(["sudo", "-n", "python3", "-B", ROOT / ".github/scripts/continuity_devnet.py",
                                                "inspect", "--input", request]))

    def effective(self, specification):
        text = service_dropin(specification["arguments"], *[specification["state_environment"][key] for key in STATE_KEYS])
        observed = self.ops.inspect([self.dropin])[str(self.dropin)]
        require(observed.get("sha256") == hashlib.sha256(text.encode()).hexdigest()
                and observed.get("mode") == 0o600, "managed fresh service drop-in differs")
        start = self.ops.execute(["systemctl", "show", "--property=ExecStart", "--value", self.service])
        require(effective_arguments_match(start, text), "effective fresh service start differs")
        return text

    def archived_binding(self, specification):
        verifier = NativeUpgrade(self.ops, self.service, self.dropin)
        verifier.load_retained_base(specification)
        verifier.retained_programs()
        binding = verifier.binding(specification)
        namespace = Path(specification["arguments"][0]).parent.parent.parent
        report_path = namespace / "control/report.json"
        require(report_path.is_file(), "previous activation lacks a qualified report")
        report = private_json(report_path)
        require(report.get("version") == 2 and report.get("result") == "passed"
                and report.get("source_revision") == namespace.name
                and isinstance(report.get("context"), str) and re.fullmatch("[0-9a-f]{64}", report["context"]),
                "previous activation is not qualified")
        if namespace == verifier.base:
            require(report.get("qualification_kind") in ("first-activation", "fresh-stack")
                    and all(report.get(key) == verifier.base_plan[key] for key in ("validator", "profile"))
                    and all(report.get(role) == verifier.base_plan["programs"][role] for role in ("portal", "token_bridge")),
                    "previous activation report identity differs")
            if verifier.base_plan.get("mode") == "fresh-stack":
                progress = private_json(namespace / "control/fresh-progress.json")
                require(progress.get("phase") == "qualified" and progress.get("plan_sha256") == digest(verifier.base_plan)
                        and report.get("fresh_plan_sha256") == digest(verifier.base_plan),
                        "previous fresh activation is incomplete")
        else:
            plan = private_json(namespace / "control/plan.json")
            require(report["context"] == plan["identity"]["context"], "previous qualified context differs")
        verifier.qualified_context = report["context"]
        return verifier, binding

    def snapshot(self, plan):
        config = {"version": 2, "genesis": DEVNET, "sdk_revision": SDK_REVISION,
                  "sdk_checkout": str(self.args.sdk_checkout), "devnet_rpc": self.args.devnet_rpc,
                  "l1_rpc": self.args.l1_rpc, "er_rpc": self.args.er_rpc,
                  "portal": plan["programs"]["portal"], "token_bridge": plan["programs"]["token_bridge"],
                  "validator": plan["validator"], "profile": plan["profile"]}
        with tempfile.TemporaryDirectory(prefix="northstar-fresh-snapshot-") as directory:
            request = Path(directory) / "input.json"
            atomic_json(request, config)
            raw = self.ops.execute(["node", ROOT / ".github/scripts/native-upgrade-snapshot.mjs", request, "online"], timeout=120)
        result = json.loads(raw.splitlines()[-1])
        identity = result.get("identity", {})
        require(identity.get("version") == 2 and all(identity.get(key) == config[key] for key in
                ("genesis", "portal", "token_bridge", "validator", "profile"))
                and isinstance(identity.get("context"), str) and re.fullmatch("[0-9a-f]{64}", identity["context"])
                and identity.get("nonce") == "1", "fresh snapshot identity differs")
        from continuity_devnet import public_bytes
        public_bytes(identity["session"])
        public_bytes(result["er_genesis"])
        return result

    def admit(self):
        require(os.path.lexists(self.dropin), "fresh cutover requires a retained managed activation")
        before = self.ops.inventory()
        verifier, binding = self.archived_binding(before)
        text = self.effective(before)
        snapshot = self.snapshot(verifier.base_plan)
        require(snapshot["identity"]["context"] == self.args.replace_context == verifier.qualified_context,
                "reviewed previous context differs")
        binary_namespace = Path(before["arguments"][0]).parent.parent.parent
        controls = sorted(set(map(str, (verifier.base_control, binary_namespace / "control"))))
        self.previous = {"version": 1, "service": service_spec(before), "binding": binding,
                         "base_revision": verifier.base.name, "base_plan_sha256": digest(verifier.base_plan),
                         "controls": controls, "snapshot": snapshot, "dropin_text": text,
                         "programs": verifier.base_plan["programs"], "event_source": verifier.previous_event_source,
                         "l1_floor": max(self.ops.l1.call("getSlot", [{"commitment": level}])
                                         for level in ("processed", "confirmed", "finalized"))}

    def adopt_reservation(self, reservation):
        previous = reservation.get("replacement")
        require(isinstance(previous, dict) and previous.get("version") == 1
                and previous.get("snapshot", {}).get("identity", {}).get("context") == self.args.replace_context,
                "fresh reservation lacks the reviewed replacement binding")
        verifier, binding = self.archived_binding(previous["service"])
        require(previous["base_revision"] == verifier.base.name and previous["base_plan_sha256"] == digest(verifier.base_plan)
                and previous["binding"] == binding and previous["event_source"] == verifier.previous_event_source
                and previous["programs"] == verifier.base_plan["programs"]
                and previous["controls"] == sorted(set(map(str, (verifier.base_control,
                    Path(previous["service"]["arguments"][0]).parent.parent.parent / "control"))))
                and type(previous.get("l1_floor")) is int and 0 <= previous["l1_floor"] < 1 << 64
                and previous["snapshot"]["identity"]["portal"] == verifier.base_plan["programs"]["portal"]
                and previous["snapshot"]["identity"]["context"] == verifier.qualified_context
                and previous["dropin_text"] == service_dropin(previous["service"]["arguments"],
                    *[previous["service"]["state_environment"][key] for key in STATE_KEYS]),
                "archived fresh reservation provenance differs")
        self.previous = previous

    def archive_inventory(self, inventory):
        require(service_spec(inventory) == self.previous["service"], "previous service changed during fresh admission")
        result = copy.deepcopy(inventory)
        result["retained_paths"] = sorted(set(path for path in [*result["retained_paths"], *self.previous["controls"]]
                                             if path != str(self.dropin)))
        return result

    def previous_session(self, frame, portal, session):
        identity = self.previous["snapshot"]["identity"]
        require(frame is not None and frame["owner"] == portal and frame["raw"].startswith(b"NSSESS02")
                and identity["portal"] == portal and identity["session"] == session,
                "previous SessionV2 differs from reviewed abandonment")

    def check_new_identities(self, identities):
        old = self.previous["snapshot"]["identity"]
        protected = {*self.previous["programs"].values(), old["session"], self.ops.plan["funder"], self.ops.plan["validator"]}
        require(not set(identities.values()).intersection(protected),
                "fresh programs and buffers must not reuse protected identities")

    def arguments(self, original, executable, portal, config, event_source):
        require(event_source == "l1", "fresh public SSE must use L1 callbacks")
        require(flag_value(original, "--portal") == self.previous["snapshot"]["identity"]["portal"],
                "previous Portal argument differs")
        result = upgrade_arguments(original, executable, config, self.previous["binding"]["config"],
                                   previous_source=self.previous["event_source"], event_source=event_source)
        for index, value in enumerate(result):
            if value == "--portal":
                result[index + 1] = portal
            elif value.startswith("--portal="):
                result[index] = "--portal=" + portal
        return result

    def phase(self):
        state = private_json(self.progress)
        require(state.get("version") == 1 and state.get("plan_sha256") == digest(self.ops.plan)
                and state.get("phase") in ("prepared", "freezing", "running", "qualified", "failed"),
                "fresh progress barrier differs")
        return state["phase"]

    def advance(self, phase, **details):
        atomic_json(self.progress, {"version": 1, "plan_sha256": digest(self.ops.plan), "phase": phase, **details})

    def state(self):
        active = self.ops.execute(["systemctl", "show", "--property=ActiveState", "--value", self.service]).strip()
        pid = self.ops.execute(["systemctl", "show", "--property=MainPID", "--value", self.service]).strip()
        require(active == "active" and pid.isdigit() and int(pid) > 0 or active in ("inactive", "failed") and pid == "0",
                "fresh service is not in a stable state")
        return active

    def original_inventory(self):
        if self.state() == "active":
            return self.ops.inventory()
        require(self.phase() == "freezing", "inactive previous service lacks its freeze barrier")
        self.effective(self.previous["service"])
        return self.ops.original

    def desired(self):
        activation = private_json(self.control / "activation.json")
        arguments = self.arguments(self.ops.original["arguments"], self.runtime / ARTIFACTS["validator"],
                                   self.ops.plan["programs"]["portal"], self.runtime / "geyser-events.json", "l1")
        environment = dict(zip(STATE_KEYS, (str(self.runtime / "checkpoints"), str(self.runtime / "unsettled"))))
        require(activation.get("version") == 2 and activation.get("arguments") == arguments
                and activation.get("state_environment") == environment and activation.get("event_source") == "l1"
                and activation.get("artifacts") == self.ops.plan["artifacts"], "fresh activation binding differs")
        return {"arguments": arguments, "state_environment": environment,
                "binary_sha256": self.ops.plan["artifacts"]["validator"],
                "uid": self.ops.original["uid"], "gid": self.ops.original["gid"]}

    def verify_files(self):
        specification = self.desired()
        self.effective(specification)
        targets = {role: self.runtime / ARTIFACTS[role] for role in ("validator", "geyser_events")}
        config = self.runtime / "geyser-events.json"
        observed = self.ops.inspect([*targets.values(), config, *map(Path, specification["state_environment"].values())])
        for role, path in targets.items():
            require(observed[str(path)].get("sha256") == self.ops.plan["artifacts"][role]
                    and observed[str(path)].get("mode") == 0o500 and observed[str(path)].get("uid") == specification["uid"]
                    and observed[str(path)].get("gid") == specification["gid"],
                    "fresh installed artifact differs")
        config_hash = hashlib.sha256(canonical(geyser_events_config(targets["geyser_events"], "l1")) + b"\n").hexdigest()
        require(observed[str(config)].get("sha256") == config_hash and observed[str(config)].get("mode") == 0o400
                and observed[str(config)].get("uid") == specification["uid"]
                and observed[str(config)].get("gid") == specification["gid"], "fresh plugin configuration differs")
        for path in specification["state_environment"].values():
            require(observed[path].get("directory") is True and observed[path].get("mode") == 0o700
                    and observed[path].get("uid") == specification["uid"]
                    and observed[path].get("gid") == specification["gid"], "fresh journal namespace differs")
        return specification

    def verify_desired(self, running=False):
        specification = self.verify_files()
        text = service_dropin(specification["arguments"], *[specification["state_environment"][key] for key in STATE_KEYS])
        require(private_json(self.control / "activation-installed.json") == {"sha256": hashlib.sha256(text.encode()).hexdigest()},
                "fresh installation marker differs")
        if running:
            require(self.state() == "active" and service_spec(self.ops.inventory()) == specification,
                    "running fresh service differs")
        return specification

    def archive_unchanged(self):
        require(private_json(self.control / "retained.json") == self.ops.retention(),
                "archived previous protocol state changed")

    def before_stop(self):
        active = self.state()
        if active == "active":
            current = service_spec(self.ops.inventory())
            if current == self.previous["service"]:
                self.effective(current)
                verifier, _binding = self.archived_binding(current)
                snapshot = self.snapshot(verifier.base_plan)
                require(snapshot["identity"] == self.previous["snapshot"]["identity"]
                        and snapshot["er_genesis"] == self.previous["snapshot"]["er_genesis"],
                        "previous context changed before native cutover")
                require(self.phase() in ("prepared", "freezing"), "old service resumed after fresh activation")
                self.advance("freezing")
            else:
                require(current == self.desired(), "refusing to stop an unverified fresh service")
                self.verify_desired(running=True)
        else:
            observed = self.ops.inspect([self.dropin])[str(self.dropin)]
            if observed.get("sha256") == self.previous["binding"]["dropin_sha256"]:
                require(self.phase() == "freezing", "previous service lacks its durable freeze intent")
                self.effective(self.previous["service"])
            else:
                self.verify_desired()

    def replacing_dropin(self, observed, desired_hash):
        require(self.phase() == "freezing" and self.state() in ("inactive", "failed")
                and not self.starts.exists() and observed.get("mode") == 0o600
                and observed.get("sha256") in (self.previous["binding"]["dropin_sha256"], desired_hash),
                "previous managed drop-in differs before fresh installation")

    def start_label(self):
        smoke = self.control / "sdk-smoke"
        if not (self.starts / "activate.json").exists():
            return "activate"
        for name in ("sol", "spl"):
            if (smoke / ("prepare-" + name + ".json")).exists() and not (smoke / ("resume-" + name + ".json")).exists():
                return "resume-" + name
        raise ValueError("fresh start has no unattempted qualification phase")

    def start(self, operation):
        require(self.phase() in ("freezing", "running") and self.state() in ("inactive", "failed"),
                "fresh start requires a verified stopped runtime")
        self.verify_desired()
        self.archive_unchanged()
        label = self.start_label()
        self.starts.mkdir(mode=0o700, exist_ok=True)
        checked_directory(self.starts, private=True)
        barrier = self.starts / (label + ".json")
        require(not os.path.lexists(barrier), "fresh start was already attempted; operator reconciliation required")
        upstream = self.ops.upstream.call("getSlot", [{"commitment": "processed"}])
        require(type(upstream) is int and 0 <= upstream < 1 << 64, "invalid fresh startup L1 floor")
        floor = max(upstream, self.previous["l1_floor"])
        intent = {"version": 1, "plan_sha256": digest(self.ops.plan), "phase": label, "l1_floor": floor}
        immutable_json(barrier, intent)
        self.ops.flush([barrier, self.starts, self.control])
        operation()
        self.verify_desired(running=True)
        end = time.monotonic() + 1200
        while time.monotonic() < end:
            require(self.state() == "active", "fresh runtime stopped during readiness")
            if min(self.ops.l1.call("getSlot", [{"commitment": level}]) for level in ("confirmed", "finalized")) >= floor:
                break
            time.sleep(1)
        else:
            raise RuntimeError("fresh finalized startup floor timed out")
        self.archive_unchanged()
        immutable_json(self.starts / (label + "-ready.json"), intent)
        self.advance("running")

    def report(self, report):
        self.verify_desired(running=True)
        self.archive_unchanged()
        self.archived_binding(self.previous["service"])
        snapshot = self.snapshot(self.ops.plan)
        require(snapshot["identity"]["context"] == report["context"] != self.args.replace_context,
                "fresh qualification did not establish a new context")
        expected = ("activate", "resume-sol", "resume-spl")
        for label in expected:
            started = private_json(self.starts / (label + ".json"))
            require(set(started) == {"version", "plan_sha256", "phase", "l1_floor"} and started["version"] == 1
                    and started.get("plan_sha256") == digest(self.ops.plan) and started["phase"] == label
                    and type(started["l1_floor"]) is int and self.previous["l1_floor"] <= started["l1_floor"] < 1 << 64
                    and started == private_json(self.starts / (label + "-ready.json")),
                    "fresh qualification start evidence is incomplete")
        report.update(qualification_kind="fresh-stack", fresh_plan_sha256=digest(self.ops.plan),
                      previous_context=self.args.replace_context, previous_state_abandoned=True,
                      previous_delegations_retired=False, archived_state_unchanged=True, old_programs_modified=False,
                      identity=snapshot["identity"], session=snapshot["identity"]["session"],
                      er_genesis=snapshot["er_genesis"], program_deployments=2)
        return report

    def quarantine(self):
        try:
            specification = self.verify_files()
            active = self.ops.execute(["systemctl", "show", "--property=ActiveState", "--value", self.service]).strip()
            pid = self.ops.execute(["systemctl", "show", "--property=MainPID", "--value", self.service]).strip()
            if active == "active":
                require(pid.isdigit() and int(pid) > 0 and service_spec(self.ops.inventory()) == specification,
                        "running quarantine binding differs")
            else:
                require(active in ("inactive", "failed", "activating", "deactivating") and pid == "0",
                        "quarantine cannot verify the pending runtime")
            self.ops.execute(["sudo", "-n", "systemctl", "stop", self.service], timeout=180)
            require(self.state() in ("inactive", "failed"), "fresh quarantine stop was not confirmed")
            return True
        except Exception:
            return False

    def fail(self, force=False):
        if force or self.starts.exists() and any(self.starts.glob("*.json")):
            quarantined = self.quarantine()
            self.advance("failed", quarantined=quarantined, operator_reconciliation_required=True)

    def run(self):
        self.authorize()
        def interrupted(_signum, _frame):
            raise InterruptedError("fresh activation interrupted")
        previous_handler = signal.signal(signal.SIGTERM, interrupted)
        try:
            return self.run_activation()
        finally:
            signal.signal(signal.SIGTERM, previous_handler)

    def run_activation(self):
        existing = self.control / "reservation.json"
        if existing.exists():
            self.adopt_reservation(private_json(existing))
        self.ops.prepare()
        if not self.progress.exists():
            require(not any(os.path.lexists(self.control / name) for name in ("programs.json", "activation.json", "fresh-starts")),
                    "fresh progress barrier is missing")
            immutable_json(self.progress, {"version": 1, "plan_sha256": digest(self.ops.plan), "phase": "prepared"})
        phase = self.phase()
        if phase == "qualified":
            try:
                report = private_json(self.control / "report.json")
                require(report.get("result") == "passed" and report.get("fresh_plan_sha256") == digest(self.ops.plan)
                        and report.get("qualification_kind") == "fresh-stack", "fresh qualification record differs")
                require(self.report(copy.deepcopy(report)) == report, "running fresh qualification identity differs")
                print(json.dumps(report), flush=True)
                return report
            except BaseException:
                self.fail(force=True)
                raise
        if phase in ("running", "failed") or self.starts.exists() and any(self.starts.glob("*.json")):
            if phase != "failed":
                self.fail(force=True)
            raise ValueError("fresh activation was interrupted or failed; operator reconciliation required")
        try:
            self.ops.deploy_programs()
            self.ops.activate()
            self.ops.smoke()
            report = private_json(self.control / "report.json")
            self.advance("qualified")
            self.ops.flush([self.progress, self.control / "report.json", self.control])
            return report
        except BaseException:
            self.fail()
            raise

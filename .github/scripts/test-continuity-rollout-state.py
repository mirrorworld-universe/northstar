#!/usr/bin/env python3
"""Sonic: Check isolation and retained-state guards without systemd or chain access."""
import json
import os
from pathlib import Path
import tempfile
import unittest

from continuity_rollout_state import (
    ARTIFACTS, append_private, atomic_json, checked_directory, file_hash, flag_value, geyser_events_config, immutable_json,
    geyser_event_source, inspect_paths, l1_rpc_arguments, private_json, release_mode_arguments, retained_fingerprint, service_arguments, service_dropin,
    sync_paths, systemd_argument, upgrade_arguments, verify_artifacts,
)


class StateTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def test_release_mode_defaults_to_native_upgrade_and_scopes_recovery(self):
        self.assertEqual(release_mode_arguments(), ["--upgrade-existing"])
        self.assertEqual(release_mode_arguments("519264000"), ["--upgrade-existing", "--recover-failed-l1-slot", "519264000"])
        for value in ("0", "-1", "18446744073709551616", "519264000;true"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                release_mode_arguments(value)

    def test_fresh_release_requires_an_exact_context_and_excludes_recovery(self):
        context = "ab" * 32
        self.assertEqual(release_mode_arguments("", context), ["--replace-context", context, "--abandon-previous-state"])
        for value in ("ab", "ab" * 31, "AB" * 32, " ", context + "\n"):
            with self.subTest(value=value), self.assertRaises(ValueError):
                release_mode_arguments("", value)
        with self.assertRaises(ValueError):
            release_mode_arguments("519264000", context)

    def test_l1_rpc_release_profile_is_explicit_and_excludes_fresh_or_recovery(self):
        self.assertEqual(release_mode_arguments("", "", "full"), ["--upgrade-existing", "--l1-rpc-profile", "full"])
        for recovery, context, profile in (("519264000", "", "full"), ("", "ab" * 32, "full"), ("", "", "other")):
            with self.subTest(recovery=recovery, context=context, profile=profile), self.assertRaises(ValueError):
                release_mode_arguments(recovery, context, profile)

    def test_full_l1_rpc_indexes_preserve_unrelated_native_and_plugin_arguments(self):
        original = ["/native", "--portal", "portal", "--geyser-plugin-config", "/plugin.json", "--rpc-port", "8899",
                    "--expected-genesis-hash", "devnet", "--account-index=program-id", "--log", "-"]
        expected = original[:9] + original[10:] + ["--account-index", "program-id", "spl-token-owner", "spl-token-mint"]
        actual = l1_rpc_arguments(original, "full")
        self.assertEqual(actual, expected)
        self.assertEqual(l1_rpc_arguments(actual, "full"), actual)
        self.assertEqual(l1_rpc_arguments(original), original)
        for extra in (["--account-index"], ["--account-index", "other"],
                      ["--account-index-include-key", "key"], ["--account-index-exclude-key=key"]):
            with self.subTest(extra=extra), self.assertRaises(ValueError):
                l1_rpc_arguments(["/native", *extra], "full")

    def test_metadata_is_immutable_and_private(self):
        filename = self.root / "plan.json"
        immutable_json(filename, {"version": 2, "identity": "original"})
        immutable_json(filename, {"identity": "original", "version": 2})
        self.assertEqual(filename.stat().st_mode & 0o777, 0o600)
        with self.assertRaisesRegex(ValueError, "changed"):
            immutable_json(filename, {"version": 2, "identity": "different"})
        self.assertEqual(private_json(filename)["identity"], "original")

    def test_nonregular_and_public_control_inputs_are_rejected(self):
        filename = self.root / "public.json"
        filename.write_text("{}")
        filename.chmod(0o644)
        with self.assertRaises(ValueError):
            private_json(filename)
        pipe = self.root / "pipe"
        os.mkfifo(pipe, 0o600)
        with self.assertRaises(ValueError):
            private_json(pipe)
        with self.assertRaises(ValueError):
            file_hash(pipe)

    def test_control_symlinks_are_not_followed(self):
        original = self.root / "original.json"
        atomic_json(original, {"retained": True})
        alias = self.root / "alias.json"
        alias.symlink_to(original)
        with self.assertRaises(OSError):
            atomic_json(alias, {"retained": False})
        self.assertTrue(private_json(original)["retained"])

    def test_original_service_arguments_are_preserved_except_binary_and_portal(self):
        original = ["/retained/agave-validator", "--ledger", "/retained/ledger", "--identity", "/retained/id.json", "--portal", "old"]
        expected = original[:]
        expected[0], expected[-1] = "/isolated/bin/agave-validator", "new"
        self.assertEqual(service_arguments(original, expected[0], "new"), expected)
        self.assertEqual(original[-1], "old")
        self.assertEqual(flag_value(original, "--ledger"), "/retained/ledger")
        with self.assertRaises(ValueError):
            service_arguments(original + ["--portal=other"], expected[0], "new")

    def test_managed_event_plugin_is_added_without_changing_other_service_flags(self):
        original = ["/retained/agave-validator", "--portal=old", "--ledger", "/retained/ledger",
                    "--geyser-plugin-config", "/retained/l1-plugin.json"]
        configured = service_arguments(original, "/isolated/bin/agave-validator", "new", "/isolated/geyser-events.json")
        self.assertEqual(configured, ["/isolated/bin/agave-validator", "--portal=new", *original[2:],
                                      "--er-geyser-plugin-config", "/isolated/geyser-events.json"])
        self.assertEqual(original[1], "--portal=old")
        with self.assertRaisesRegex(ValueError, "config path.*absolute"):
            service_arguments(original, "/isolated/bin/agave-validator", "new", "relative.json")
        for existing in (["--er-geyser-plugin-config", "/retained/er.json"], ["--er-geyser-plugin-config=/retained/er.json"]):
            with self.subTest(existing=existing), self.assertRaisesRegex(ValueError, "explicit deployment review"):
                service_arguments(original + existing, "/isolated/bin/agave-validator", "new", "/isolated/geyser-events.json")
        self.assertEqual(geyser_events_config("/isolated/lib/events.so"), {
            "libpath": "/isolated/lib/events.so", "name": "er-events", "listen_addr": "0.0.0.0:9101", "commitment": "confirmed"})
        with self.assertRaisesRegex(ValueError, "plugin path.*absolute"):
            geyser_events_config("relative.so")

    def test_l1_delivery_uses_standard_geyser_and_keeps_legacy_er_hashes(self):
        original = ["/retained/agave-validator", "--portal", "old", "--geyser-plugin-config", "/retained/other.json"]
        arguments = service_arguments(original, "/isolated/bin/agave-validator", "new", "/isolated/events.json", "l1")
        self.assertEqual(arguments[-2:], ["--geyser-plugin-config", "/isolated/events.json"])
        self.assertEqual(arguments[3:-2], original[3:])
        self.assertEqual(geyser_events_config("/isolated/events.so", "l1")["name"], "l1-events")
        self.assertEqual(geyser_events_config("/isolated/events.so")["name"], "er-events")
        self.assertEqual(geyser_event_source({}), "er")
        self.assertEqual(geyser_event_source({"event_source": "l1"}), "l1")
        for source in (None, "mainnet", "devnet", "unknown"):
            with self.subTest(source=source), self.assertRaisesRegex(ValueError, "unknown event source"):
                geyser_event_source({"event_source": source})

    def test_l1_upgrade_replaces_only_the_manifest_bound_plugin(self):
        original = ["/retained/agave-validator", "--portal", "retained", "--ledger", "/retained/ledger",
                    "--geyser-plugin-config", "/retained/third-party.json"]
        expected = ["/upgrade/bin/agave-validator", *original[1:], "--geyser-plugin-config", "/upgrade/events.json"]
        for source, suffix in (("er", ["--er-geyser-plugin-config", "/retained/events.json"]),
                               ("er", ["--er-geyser-plugin-config=/retained/events.json"]),
                               ("l1", ["--geyser-plugin-config", "/retained/events.json"]),
                               ("l1", ["--accountsdb-plugin-config=/retained/events.json"])):
            with self.subTest(source=source, suffix=suffix):
                self.assertEqual(upgrade_arguments(original + suffix, expected[0], expected[-1], "/retained/events.json",
                                                   previous_source=source, event_source="l1"), expected)
                for invalid in (original, original + suffix + suffix):
                    with self.assertRaises(ValueError):
                        upgrade_arguments(invalid, expected[0], expected[-1], "/retained/events.json",
                                          previous_source=source, event_source="l1")

    def test_l1_configuration_preserves_native_rpc_and_each_solana_cluster(self):
        for genesis in ("EtWTRABZaYq6iMfeYKouRu166VU2xqa1wcaWoxPkrZBG",
                        "5eykt4UsFv8P8NJdTREpY1vzqKqZKvdpKuc147dw2N9d"):
            with self.subTest(genesis=genesis):
                original = ["/retained/agave-validator", "--portal", "retained", "--expected-genesis-hash", genesis,
                            "--rpc-port", "8899", "--full-rpc-api", "--enable-rpc-transaction-history"]
                configured = upgrade_arguments(original, "/upgrade/bin/agave-validator", "/upgrade/events.json", event_source="l1")
                self.assertEqual(configured[1:-2], original[1:])
                self.assertEqual(configured[-2:], ["--geyser-plugin-config", "/upgrade/events.json"])
                self.assertEqual(flag_value(configured, "--expected-genesis-hash"), genesis)
                self.assertEqual(flag_value(configured, "--rpc-port"), "8899")

    def test_native_upgrade_preserves_portal_and_replaces_only_its_owned_plugin(self):
        original = ["/retained/agave-validator", "--portal=existing", "--ledger", "/retained/ledger",
                    "--geyser-plugin-config", "/retained/l1.json"]
        expected = ["/upgrade/bin/agave-validator", *original[1:], "--er-geyser-plugin-config", "/upgrade/events.json"]
        self.assertEqual(upgrade_arguments(original, expected[0], expected[-1]), expected)
        for suffix in (["--er-geyser-plugin-config", "/retained/events.json"], ["--er-geyser-plugin-config=/retained/events.json"]):
            self.assertEqual(upgrade_arguments(original + suffix, expected[0], expected[-1], "/retained/events.json"), expected)
        self.assertEqual(original[1], "--portal=existing")

    def test_native_upgrade_rejects_unknown_or_duplicate_plugin_flags(self):
        original = ["/retained/agave-validator", "--portal", "existing"]
        for suffix, previous in ((["--er-geyser-plugin-config", "/unknown.json"], None),
                                 (["--er-geyser-plugin-config", "/unknown.json"], "/retained/events.json"),
                                 (["--er-geyser-plugin-config=/retained/events.json"] * 2, "/retained/events.json"),
                                 (["--er-geyser-plugin-config"], None)):
            with self.subTest(suffix=suffix), self.assertRaises(ValueError):
                upgrade_arguments(original + suffix, "/upgrade/bin/agave-validator", "/upgrade/events.json", previous)

    def test_dropin_overrides_only_native_state_environment(self):
        arguments = ["/isolated/bin/agave-validator", "--portal", "new", "--ledger", "/retained/ledger"]
        content = service_dropin(arguments, "/isolated/checkpoints", "/isolated/unsettled")
        self.assertIn('ExecStart=\nExecStart="/usr/bin/env"', content)
        self.assertIn('"NORTHSTAR_CHECKPOINT_PLAN_DIR=/isolated/checkpoints"', content)
        self.assertIn('"NORTHSTAR_UNSETTLED_STATE_DIR=/isolated/unsettled"', content)
        self.assertIn('"--ledger" "/retained/ledger"', content)
        self.assertNotIn("WorkingDirectory", content)
        with self.assertRaises(ValueError):
            service_dropin(arguments, "relative", "/isolated/unsettled")

    def test_systemd_argument_expansion_is_literal(self):
        self.assertEqual(systemd_argument('a$b%c"d\\e'), '"a$$b%%c\\"d\\\\e"')
        with self.assertRaises(ValueError):
            systemd_argument("line\nbreak")

    def test_retained_tree_fingerprint_never_changes_original_bytes(self):
        retained = self.root / "retained"
        retained.mkdir()
        source = retained / "journal.bin"
        source.write_bytes(b"original")
        before = retained_fingerprint([str(retained)])
        self.assertEqual(source.read_bytes(), b"original")
        self.assertEqual(before, retained_fingerprint([str(retained)]))
        source.write_bytes(b"changed")
        self.assertNotEqual(before, retained_fingerprint([str(retained)]))
        alias = retained / "alias"
        alias.symlink_to(source)
        with self.assertRaisesRegex(ValueError, "symlink"):
            retained_fingerprint([str(retained)])

    def test_private_diagnostic_append_rejects_aliases(self):
        target = self.root / "diagnostics.log"
        append_private(target, b"first\n")
        append_private(target, b"second\n")
        self.assertEqual(target.read_bytes(), b"first\nsecond\n")
        alias = self.root / "alias.log"
        alias.symlink_to(target)
        with self.assertRaises(OSError):
            append_private(alias, b"changed")
        self.assertEqual(target.read_bytes(), b"first\nsecond\n")

    def test_namespace_permissions_and_aliases_fail_closed(self):
        directory = self.root / "private"
        directory.mkdir(mode=0o700)
        checked_directory(directory, private=True)
        directory.chmod(0o755)
        with self.assertRaises(ValueError):
            checked_directory(directory, private=True)
        alias = self.root / "alias"
        alias.symlink_to(directory)
        with self.assertRaises(ValueError):
            checked_directory(alias)
        with self.assertRaises(ValueError):
            inspect_paths([str(alias)])

    def test_durable_publication_flushes_only_regular_files_and_directories(self):
        filename = self.root / "metadata.json"
        atomic_json(filename, {"version": 2})
        sync_paths([filename, self.root])
        alias = self.root / "alias"
        alias.symlink_to(filename)
        with self.assertRaises(ValueError):
            sync_paths([alias])

    def test_complete_artifact_identity_and_hashes_are_required(self):
        stage = self.root / "stage"
        stage.mkdir()
        checksums = {}
        for role, filename in ARTIFACTS.items():
            target = stage / filename
            target.parent.mkdir(exist_ok=True)
            target.write_bytes(b"\x7fELF" + role.encode())
            checksums[role] = file_hash(target)
        manifest = {"version": 2, "protocol": "continuity-v2", "source_revision": "a" * 40,
                    "platform_tools": "v1.56", "arch": "v0", "activation_performed": False,
                    "hosted_qualified": False, "proof_acceptance_qualified": False, "sha256": checksums}
        atomic_json(stage / "manifest.json", manifest)
        self.assertEqual(verify_artifacts(stage, "a" * 40), manifest)
        without_plugin = dict(manifest, sha256={role: digest for role, digest in checksums.items() if role != "geyser_events"})
        atomic_json(stage / "manifest.json", without_plugin)
        with self.assertRaisesRegex(ValueError, "artifact roles"):
            verify_artifacts(stage, "a" * 40)
        self.assertEqual(verify_artifacts(stage, "a" * 40, retained=True), without_plugin)
        atomic_json(stage / "manifest.json", manifest)
        (stage / ARTIFACTS["geyser_events"]).write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "changed"):
            verify_artifacts(stage, "a" * 40)
        (stage / ARTIFACTS["geyser_events"]).write_bytes(b"\x7fELFgeyser_events")
        (stage / ARTIFACTS["portal"]).write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "changed"):
            verify_artifacts(stage, "a" * 40)


if __name__ == "__main__":
    unittest.main()

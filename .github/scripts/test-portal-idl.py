#!/usr/bin/env python3
"""Sonic: Check the pinned generator and workflow's exact IDL post-processing."""
import copy
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ROOT / ".github/workflows/generate-portal-idl.yml"
PORTAL = ROOT / "northstar/programs/portal"
CATALOG = PORTAL / "idl/custom-types.json"


def workflow_python(name):
    step = WORKFLOW.read_text().split("      - name: " + name + "\n", 1)[1].split("\n      - ", 1)[0]
    body = step.split("python3 - <<'PY'\n", 1)[1].rsplit("\n          PY", 1)[0]
    return textwrap.dedent(body)


class PortalIdlTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "raw.json"
            result = subprocess.run([
                "cargo", "pinocchio-idl", "generate", "--manifest-path", str(PORTAL / "Cargo.toml"), "--out", str(output),
            ], cwd=ROOT, capture_output=True, text=True, timeout=60)
            if result.returncode:
                raise RuntimeError("pinned pinocchio-idl generator failed: " + result.stderr)
            cls.generated = json.loads(output.read_text())

    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.output = self.root / "target/idl/northstar_portal.json"
        self.output.parent.mkdir(parents=True)
        self.output.write_text(json.dumps(self.generated))
        self.catalog = self.root / "northstar/programs/portal/idl/custom-types.json"
        self.catalog.parent.mkdir(parents=True)
        self.catalog.write_bytes(CATALOG.read_bytes())

    def run_step(self, name):
        return subprocess.run([sys.executable, "-c", workflow_python(name)], cwd=self.root,
                              capture_output=True, text=True, timeout=30)

    def resolve(self):
        result = self.run_step("Resolve Portal custom types")
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(self.output.read_text())

    def test_current_generator_output_resolves_and_validates(self):
        resolved = self.resolve()
        result = self.run_step("Validate Portal IDL")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(sorted(instruction["discriminator"][0] for instruction in resolved["instructions"]), list(range(38)))
        definitions = {definition["name"] for definition in resolved["types"]}
        self.assertTrue({"OpenSession", "ProposeCheckpoint", "StateRootOpeningV2"} <= definitions)
        original = {instruction["name"]: instruction for instruction in self.generated["instructions"]}
        for instruction in resolved["instructions"]:
            self.assertEqual(instruction["discriminator"], original[instruction["name"]]["discriminator"])
            self.assertEqual(instruction["accounts"], original[instruction["name"]]["accounts"])

    def test_v2_payload_definitions_match_rust_field_order_and_types(self):
        catalog = {definition["name"]: definition for definition in json.loads(CATALOG.read_text())}
        sources = {
            "OpenSession": PORTAL / "src/instruction.rs",
            "ProposeCheckpoint": PORTAL / "src/instruction.rs",
            "StateRootOpeningV2": ROOT / "northstar/zk-types/src/continuity.rs",
        }
        for name, source in sources.items():
            with self.subTest(name=name):
                self.assertIn(name, catalog)
                body = re.search(r"pub struct " + name + r"\s*\{([^}]+)\}", source.read_text()).group(1)
                expected = [(field, "pubkey" if kind.strip() == "Pubkey" else kind.strip())
                            for field, kind in re.findall(r"pub\s+(\w+):\s*([^,]+),", body)]
                actual = [(field["name"], field["type"]) for field in catalog[name]["type"]["fields"]]
                self.assertEqual(actual, expected)

    def test_unknown_type_is_rejected_without_rewriting_output(self):
        generated = copy.deepcopy(self.generated)
        generated["instructions"][0]["args"] = [{"name": "value", "type": "UnknownType"}]
        self.output.write_text(json.dumps(generated))
        original = self.output.read_bytes()
        result = self.run_step("Resolve Portal custom types")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unresolved IDL type: UnknownType", result.stderr)
        self.assertEqual(self.output.read_bytes(), original)

    def test_missing_v2_catalog_definition_is_rejected(self):
        catalog = [definition for definition in json.loads(CATALOG.read_text()) if definition["name"] != "StateRootOpeningV2"]
        self.catalog.write_text(json.dumps(catalog))
        original = self.output.read_bytes()
        result = self.run_step("Resolve Portal custom types")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("missing IDL definition: StateRootOpeningV2", result.stderr)
        self.assertEqual(self.output.read_bytes(), original)

    def test_unused_catalog_definition_is_rejected(self):
        catalog = json.loads(CATALOG.read_text())
        catalog.append({"name": "UnusedType", "type": {"kind": "struct", "fields": []}})
        self.catalog.write_text(json.dumps(catalog))
        result = self.run_step("Resolve Portal custom types")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unused custom IDL types: ['UnusedType']", result.stderr)

    def test_validator_rejects_missing_and_duplicated_instruction_tags(self):
        resolved = self.resolve()
        for tags in (list(range(37)), [*range(37), 0]):
            with self.subTest(tags=tags):
                altered = copy.deepcopy(resolved)
                altered["instructions"] = altered["instructions"][:len(tags)]
                for instruction, tag in zip(altered["instructions"], tags):
                    instruction["discriminator"] = [tag]
                self.output.write_text(json.dumps(altered))
                self.assertNotEqual(self.run_step("Validate Portal IDL").returncode, 0)


if __name__ == "__main__":
    unittest.main()

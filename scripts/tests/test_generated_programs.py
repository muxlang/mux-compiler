#!/usr/bin/env python3

import importlib.util
import random
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]


def load_script(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(
        name, REPO_ROOT / "scripts" / filename
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(f"could not load {filename}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


generator = load_script("mux_generate_programs", "generate-programs.py")
runner = load_script("mux_run_generated_programs", "run-generated-programs.py")


class GeneratorTests(unittest.TestCase):
    def generate(self, seed: int) -> str:
        return generator.Gen(random.Random(seed)).program(seed)

    def generated(self, seed: int):
        return generator.Gen(random.Random(seed)).generate(seed)

    def test_seed_is_deterministic(self):
        first = generator.Gen(random.Random(1143)).generate(1143)
        second = generator.Gen(random.Random(1143)).generate(1143)
        self.assertEqual(first.source, second.source)
        self.assertEqual(first.manifest.as_json(), second.manifest.as_json())

    def test_every_program_has_a_differential_oracle(self):
        for seed in range(1, 101):
            program = self.generate(seed)
            self.assertIn("@@oracle:", program)
            self.assertIn(":p1", program)
            self.assertIn(":p2", program)

    def test_manifest_records_required_oracle_expectations(self):
        generated = self.generated(1)
        self.assertEqual(generated.manifest.seed, 1)
        self.assertTrue(generated.manifest.required_oracles)
        self.assertTrue(generated.manifest.feature_oracles)
        self.assertLessEqual(
            generated.manifest.required_oracles,
            generated.manifest.expected_oracles.keys(),
        )
        self.assertLessEqual(
            set(generated.manifest.feature_oracles.values()),
            generated.manifest.required_oracles,
        )

    def test_core_features_are_scheduled_across_seed_cycle(self):
        features = set()
        for seed in range(1, len(generator.CORE_FEATURES) + 1):
            features.update(self.generated(seed).manifest.features)
        self.assertEqual(features, set(generator.CORE_FEATURES))

    def test_string_expressions_are_not_quoted_as_source_text(self):
        programs = "".join(self.generate(seed) for seed in range(1, 301))
        self.assertNotIn('Holder<string>.from("\\"', programs)
        self.assertNotIn('Shape.Label("\\"', programs)
        self.assertIn('.split("', programs)

    def test_existing_case_prevents_partial_generation(self):
        with tempfile.TemporaryDirectory() as tmp:
            out_dir = Path(tmp)
            (out_dir / "case_00002.mux").write_text("existing", encoding="utf-8")
            result = subprocess.run(
                [
                    sys.executable,
                    str(REPO_ROOT / "scripts/generate-programs.py"),
                    "--out",
                    str(out_dir),
                    "--count",
                    "3",
                    "--start-seed",
                    "1",
                ],
                capture_output=True,
                text=True,
                check=False,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((out_dir / "case_00001.mux").exists())
            self.assertFalse((out_dir / "case_00001.json").exists())
            self.assertFalse((out_dir / "case_00003.mux").exists())
            self.assertFalse((out_dir / "case_00003.json").exists())


class RunnerOracleTests(unittest.TestCase):
    def test_accepts_repeated_oracle_from_a_loop(self):
        output = """@@oracle:oracle1:p1: 4
@@oracle:oracle1:p2: 4
@@oracle:oracle1:p1: 8
@@oracle:oracle1:p2: 8
done"""
        self.assertIsNone(runner.validate_output(output))

    def test_accepts_manifest_expected_oracle_value(self):
        manifest = runner.ProgramManifest(
            seed=1,
            features=set(),
            feature_oracles={},
            expected_oracles={"oracle1": "4"},
            required_oracles={"oracle1"},
        )
        output = "@@oracle:oracle1:p1: 4\n@@oracle:oracle1:p2: 4\ndone\n"
        self.assertIsNone(runner.validate_output(output, manifest))

    def test_rejects_manifest_expected_oracle_value_mismatch(self):
        manifest = runner.ProgramManifest(
            seed=1,
            features=set(),
            feature_oracles={},
            expected_oracles={"oracle1": "4"},
            required_oracles={"oracle1"},
        )
        failure = runner.validate_output(
            "@@oracle:oracle1:p1: 5\n@@oracle:oracle1:p2: 5\ndone\n",
            manifest,
        )
        self.assertIsNotNone(failure)
        self.assertEqual(failure.kind, "wrong-answer")

    def test_rejects_missing_required_oracle(self):
        manifest = runner.ProgramManifest(
            seed=1,
            features=set(),
            feature_oracles={},
            expected_oracles={"oracle1": "4", "oracle2": "5"},
            required_oracles={"oracle2"},
        )
        failure = runner.validate_output(
            "@@oracle:oracle1:p1: 4\n@@oracle:oracle1:p2: 4\ndone\n",
            manifest,
        )
        self.assertIsNotNone(failure)
        self.assertEqual(failure.kind, "wrong-answer")

    def test_rejects_a_wrong_answer(self):
        failure = runner.validate_output(
            "@@oracle:oracle1:p1: 4\n@@oracle:oracle1:p2: 5\ndone\n"
        )
        self.assertIsNotNone(failure)
        self.assertEqual(failure.kind, "wrong-answer")

    def test_rejects_missing_oracle(self):
        failure = runner.validate_output("done\n")
        self.assertIsNotNone(failure)
        self.assertEqual(failure.kind, "wrong-answer")

    def test_rejects_incomplete_execution(self):
        failure = runner.validate_output(
            "@@oracle:oracle1:p1: 4\n@@oracle:oracle1:p2: 4\n"
        )
        self.assertIsNotNone(failure)
        self.assertEqual(failure.kind, "incomplete-output")

    def test_classifies_leak_runtime_failure(self):
        failure = runner.classify_process_failure(
            101, "mux-runtime rc-leak-check: 2 blocks still live at exit"
        )
        self.assertEqual(failure.kind, "leak")


if __name__ == "__main__":
    unittest.main()

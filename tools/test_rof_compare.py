import json
import tempfile
import unittest
from pathlib import Path

from rof_compare import compare, main


def experiment():
    protocol = {
        "dataset": "tb4.1.0",
        "release_hash": "a" * 64,
        "task_hashes": {"t1": "b" * 64, "t2": "c" * 64},
        "model": "model-x",
        "endpoint": "endpoint-x",
        "runner": "harbor-0.23",
        "grade_path": "native",
        "budgets": {"tokens": 1000, "steps": 20},
        "splits": {"development": ["t1", "t2"]},
        "allowed_difference": {"key": "retention", "value": 32},
        "ledger": {"path": "ledger.json", "sha256": "d" * 64},
        "pairs": [
            {"task_id": "t1", "replicate_id": "r1"},
            {"task_id": "t2", "replicate_id": "r1"},
        ],
    }
    return {
        "protocol": protocol,
        "arms": {
            "control": {
                "runtime_sha256": "e" * 64,
                "config": {"retention": 5, "workflow": "legacy"},
            },
            "treatment": {
                "runtime_sha256": "f" * 64,
                "config": {"retention": 32, "workflow": "legacy"},
            },
        },
        "rows": [
            row("control", "t1", 0, True, True, 100, 20),
            row("treatment", "t1", 1, True, True, 200, 30),
            row("control", "t2", 0, True, True, 100, 20),
            row("treatment", "t2", 0, True, True, 200, 30),
        ],
    }


def row(arm, task, reward, completed, accounted, input_tokens, output_tokens):
    return {
        "arm": arm,
        "task_id": task,
        "attempt_id": arm + "-" + task,
        "replicate_id": "r1",
        "artifact_reward": reward,
        "run_status": "budget_exhausted" if not completed else "completed",
        "run_completed": completed,
        "accounting_complete": accounted,
        "usage": {
            "input_tokens": input_tokens,
            "cached_input_tokens": 10,
            "output_tokens": output_tokens,
        }
        if accounted
        else None,
        "artifact_path": "artifacts/" + arm + "/" + task,
        "artifact_sha256": "1" * 64,
    }


class CompareTests(unittest.TestCase):
    def test_reports_raw_outcomes_and_completed_accounted_comparisons(self):
        data = experiment()
        data["rows"][1]["run_completed"] = False
        data["rows"][1]["accounting_complete"] = False
        data["rows"][1]["usage"] = None
        result = compare(data)
        self.assertEqual(result["raw"]["treatment_solved"], 1)
        self.assertEqual(
            result["comparison"]["incomplete_pairs"],
            [{"task_id": "t1", "replicate_id": "r1"}],
        )
        self.assertEqual(result["comparison"]["rescue"], [])
        self.assertEqual(result["raw"]["artifact_transitions"]["01"], 1)
        self.assertEqual(result["comparison"]["transitions"]["01"], 0)
        interrupted = next(
            r
            for r in result["attempts"]
            if r["arm"] == "treatment" and r["task_id"] == "t1"
        )
        self.assertEqual(interrupted["artifact_reward"], 1)

    def test_00_01_10_11_transitions(self):
        data = experiment()
        data["protocol"]["pairs"] = []
        data["rows"] = []
        for i, (control, treatment) in enumerate(((0, 0), (0, 1), (1, 0), (1, 1))):
            task = "t1"
            rep = "r" + str(i + 1)
            data["protocol"]["pairs"].append({"task_id": task, "replicate_id": rep})
            for arm, reward in (("control", control), ("treatment", treatment)):
                r = row(arm, task, reward, True, True, 100, 10)
                r["replicate_id"] = rep
                r["attempt_id"] = arm + "-" + task + "-" + rep
                data["rows"].append(r)
        result = compare(data)["comparison"]
        self.assertEqual(len(result["rescue"]), 1)
        self.assertEqual(len(result["harm"]), 1)
        self.assertEqual(len(result["unchanged"]), 2)

    def test_all_attempt_usage_includes_failures_and_cached_is_not_added_twice(self):
        data = experiment()
        data["rows"][1]["run_completed"] = False
        result = compare(data)
        self.assertEqual(result["usage"]["treatment"]["input_tokens"], 400)
        self.assertEqual(result["usage"]["treatment"]["output_tokens"], 60)
        self.assertEqual(result["usage"]["treatment"]["gross_tokens"], 460)

    def test_equal_task_weighting_and_zero_solve_ratio(self):
        data = experiment()
        data["rows"][1]["artifact_reward"] = 0
        data["rows"][3]["artifact_reward"] = 0
        result = compare(data)
        self.assertIsNone(result["usage"]["treatment"]["tokens_per_solved_task"])
        self.assertIsNone(result["usage"]["treatment_vs_control_ratio"])
        self.assertEqual(result["comparison"]["task_weighted_reward"]["treatment"], 0)

    def test_rejects_duplicate_pair_and_bad_usage(self):
        data = experiment()
        data["rows"].append(dict(data["rows"][0]))
        with self.assertRaises(ValueError):
            compare(data)
        data = experiment()
        data["rows"][0]["usage"]["cached_input_tokens"] = 101
        with self.assertRaises(ValueError):
            compare(data)

    def test_rejects_unknown_task_and_protocol_mismatch(self):
        data = experiment()
        data["rows"][0]["task_id"] = "other"
        with self.assertRaises(ValueError):
            compare(data)
        data = experiment()
        data["arms"]["treatment"]["config"]["workflow"] = "minimal"
        with self.assertRaises(ValueError):
            compare(data)

    def test_rejects_inconsistent_pair_task_hash_or_missing_pair_rows_are_visible(self):
        data = experiment()
        data["protocol"]["pairs"] = data["protocol"]["pairs"][:1]
        with self.assertRaises(ValueError):
            compare(data)
        data = experiment()
        data["rows"] = data["rows"][:2]
        result = compare(data)
        self.assertEqual(
            result["comparison"]["incomplete_pairs"],
            [{"task_id": "t2", "replicate_id": "r1"}],
        )

    def test_unknown_reward_makes_solved_denominator_unavailable(self):
        data = experiment()
        data["rows"][0]["artifact_reward"] = 1
        data["rows"][3]["artifact_reward"] = None
        result = compare(data)
        self.assertIsNone(result["usage"]["treatment"]["tokens_per_solved_task"])
        self.assertIsNone(result["usage"]["treatment_vs_control_ratio"])
        self.assertEqual(result["usage"]["treatment"]["gross_tokens"], 460)

    def test_invalid_arm_type_is_a_validation_error(self):
        data = experiment()
        data["rows"][0]["arm"] = {}
        with self.assertRaises(ValueError):
            compare(data)

    def test_promotion_is_not_automatic(self):
        result = compare(experiment())
        self.assertFalse(result["retained_for_research"])
        self.assertFalse(result["eligible_for_promotion"])

    def test_repetitions_are_averaged_inside_task_before_task_weighting(self):
        data = experiment()
        pair = {"task_id": "t1", "replicate_id": "r2"}
        data["protocol"]["pairs"].append(pair)
        extra = row("treatment", "t1", 1, True, True, 200, 30)
        extra["replicate_id"] = "r2"
        extra["attempt_id"] = "treatment-t1-r2"
        data["rows"].append(extra)
        self.assertAlmostEqual(
            compare(data)["raw"]["treatment_task_weighted_reward"], 0.5
        )

    def test_cli_writes_deterministic_json(self):
        with tempfile.TemporaryDirectory() as directory:
            source, first, second = (
                Path(directory) / name
                for name in ("input.json", "one.json", "two.json")
            )
            source.write_text(json.dumps(experiment()), encoding="utf-8")
            self.assertEqual(main(["--input", str(source), "--output", str(first)]), 0)
            self.assertEqual(main(["--input", str(source), "--output", str(second)]), 0)
            self.assertEqual(first.read_bytes(), second.read_bytes())

    def test_unknown_reward_or_missing_expected_rows_are_incomplete(self):
        data = experiment()
        data["rows"][1]["artifact_reward"] = None
        result = compare(data)
        self.assertEqual(result["raw"]["artifact_transitions"]["incomplete"], 1)
        self.assertEqual(
            result["comparison"]["incomplete_pairs"],
            [{"task_id": "t1", "replicate_id": "r1"}],
        )
        data = experiment()
        data["rows"] = data["rows"][:2]
        result = compare(data)
        self.assertFalse(result["usage"]["control"]["usage_complete"])
        self.assertIsNone(result["usage"]["control"]["input_tokens"])
        self.assertEqual(
            result["usage"]["control"]["known_subtotals"]["input_tokens"], 100
        )

    def test_failed_task_cost_stays_in_tps_when_another_task_is_solved(self):
        data = experiment()
        result = compare(data)
        self.assertEqual(result["usage"]["treatment"]["tokens_per_solved_task"], 460)


if __name__ == "__main__":
    unittest.main()

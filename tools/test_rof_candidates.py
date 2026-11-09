import json
from pathlib import Path
import sys
import tempfile
import unittest

from rof_candidates import run_plan


class CandidateRunnerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.source = self.root / "source"
        self.source.mkdir()
        (self.source / "seed.txt").write_text("original\n")
        (self.source / "check.py").write_text(
            "from pathlib import Path\n"
            "raise SystemExit(0 if Path('result.txt').read_text().strip() == 'good' else 1)\n"
        )
        self.rof = self.root / "fake-rof"
        self.rof.write_text(
            "#!" + sys.executable + "\n"
            "import json, pathlib, sys, time\n"
            "args=sys.argv[1:]\n"
            "def val(k): return args[args.index(k)+1]\n"
            "work=pathlib.Path(val('--workdir'))\n"
            "goal=val('--goal')\n"
            "if goal == 'slow': time.sleep(10)\n"
            "if goal == 'tamper': (work/'check.py').write_text(\"raise SystemExit(0)\\n\")\n"
            "(work/'result.txt').write_text(goal+'\\n')\n"
            "(work/'args.json').write_text(json.dumps(args))\n"
            "events=pathlib.Path(val('--dump-events'))\n"
            "events.write_text('\\n'.join([\n"
            "json.dumps({'type':'MessageEnd','usage':{'input_tokens':3,'output_tokens':2}}),\n"
            "json.dumps({'type':'TurnEnd','usage_totals':{'input_tokens':3,'output_tokens':2,'cache_read_tokens':0,'reasoning_tokens':None,'cost_usd':None}}),\n"
            "json.dumps({'type':'MessageEnd','usage':{'input_tokens':4,'output_tokens':1}}),\n"
            "json.dumps({'type':'TurnEnd','usage_totals':{'input_tokens':7,'output_tokens':3,'cache_read_tokens':0,'reasoning_tokens':None,'cost_usd':None}}),\n"
            "json.dumps({'type':'RunEnd','outcome':'Passed','messages':[]}),\n"
            "])+'\\n')\n"
        )
        self.rof.chmod(0o755)
        self.out = self.root / "out"
        self.plan = {
            "source_dir": str(self.source),
            "binary": str(self.rof),
            "out_dir": str(self.out),
            "model": "local-test",
            "goal": "good",
            "alternative_goal": "bad",
            "check": [sys.executable, str(self.source / "check.py")],
            "budget_tokens": 9,
            "budget_steps": 5,
            "budget_actions": 7,
            "total_timeout_seconds": 5,
        }

    def tearDown(self):
        self.temp.cleanup()

    def run_plan_fixture(self):
        plan_path = self.root / "plan.json"
        plan_path.write_text(json.dumps(self.plan))
        return run_plan(plan_path)

    def test_runs_two_isolated_candidates_and_selects_first_public_check_pass(self):
        result = self.run_plan_fixture()
        self.assertEqual(result["selected_candidate"], "candidate-1")
        self.assertFalse(result["fallback"])
        self.assertEqual((self.source / "seed.txt").read_text(), "original\n")
        first = self.out / "candidate-1" / "worktree"
        second = self.out / "candidate-2" / "worktree"
        self.assertEqual((first / "result.txt").read_text(), "good\n")
        self.assertEqual((second / "result.txt").read_text(), "bad\n")
        args = json.loads((first / "args.json").read_text())
        self.assertEqual(args[args.index("--budget-tokens") + 1], "4")
        self.assertEqual(args[args.index("--budget-steps") + 1], "2")
        self.assertEqual(args[args.index("--budget-actions") + 1], "3")
        self.assertTrue((self.out / "candidate-1" / "events.jsonl").is_file())
        self.assertEqual(result["attempts"][0]["usage"]["input_tokens"], 7)
        self.assertIsNone(result["attempts"][0]["usage"]["cost_usd"])

    def test_falls_back_to_control_when_neither_check_passes(self):
        self.plan["goal"] = "bad"
        self.plan["alternative_goal"] = "also-bad"
        result = self.run_plan_fixture()
        self.assertEqual(result["selected_candidate"], "candidate-1")
        self.assertTrue(result["fallback"])
        self.assertFalse(any(a["check"]["passed"] for a in result["attempts"]))

    def test_rejects_budget_override_and_existing_or_nested_output(self):
        self.plan["extra_args"] = ["--budget-tokens", "999"]
        with self.assertRaisesRegex(ValueError, "extra_args"):
            self.run_plan_fixture()
        self.plan.pop("extra_args")
        self.plan["out_dir"] = str(self.source / "output")
        with self.assertRaisesRegex(ValueError, "inside source|contain source"):
            self.run_plan_fixture()
        self.plan["out_dir"] = str(self.out)
        self.out.mkdir()
        with self.assertRaisesRegex(ValueError, "must not exist"):
            self.run_plan_fixture()

    def test_rejects_escaping_source_symlink(self):
        outside = self.root / "secret.txt"
        outside.write_text("secret")
        (self.source / "escape").symlink_to(outside)
        with self.assertRaisesRegex(ValueError, "escapes source"):
            self.run_plan_fixture()

    def test_excludes_git_pointer_file_from_linked_worktree_snapshot(self):
        metadata = self.root / "real-git-metadata"
        metadata.mkdir()
        (metadata / "keep").write_text("untouched")
        (self.source / ".git").write_text(f"gitdir: {metadata}\n")
        result = self.run_plan_fixture()
        self.assertFalse((Path(result["attempts"][0]["worktree"]) / ".git").exists())
        self.assertEqual((metadata / "keep").read_text(), "untouched")

    def test_total_timeout_marks_infrastructure_and_preserves_fallback(self):
        self.plan["goal"] = "slow"
        self.plan["alternative_goal"] = "slow"
        self.plan["total_timeout_seconds"] = 0.25
        result = self.run_plan_fixture()
        self.assertEqual(result["selected_candidate"], "candidate-1")
        self.assertTrue(result["fallback"])
        self.assertEqual(result["attempts"][0]["run"]["status"], "timeout")
        self.assertIn("not_started", result["attempts"][1]["run"]["status"])

    def test_candidate_cannot_replace_the_frozen_public_check_file(self):
        self.plan["goal"] = "tamper"
        self.plan["alternative_goal"] = "bad"
        result = self.run_plan_fixture()
        self.assertTrue(result["fallback"])
        self.assertFalse(any(a["check"]["passed"] for a in result["attempts"]))

    def test_modified_relative_check_input_cannot_pass(self):
        self.plan["goal"] = "tamper"
        self.plan["alternative_goal"] = "bad"
        self.plan["check"] = [sys.executable, "check.py"]
        result = self.run_plan_fixture()
        self.assertTrue(result["fallback"])
        self.assertEqual(result["attempts"][0]["check"]["status"], "check_modified")


if __name__ == "__main__":
    unittest.main()

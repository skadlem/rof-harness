import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest

from rof_procedures import build_prompt, execute


class ProcedureDistillationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.success = self.root / "success.trace"
        self.failure = self.root / "failure.trace"
        self.success.write_text("cargo test passed after adding the missing parser branch.\n" + "success evidence\n" * 200)
        self.failure.write_text("cargo test failed because the branch was absent.\n" + "failure evidence\n" * 200)
        self.corpus = self.root / "corpus.json"
        self.corpus.write_text(json.dumps({"pairs": [{
            "task_id": "issue-17",
            "success": {"repo_id": "repo-a", "path": "success.trace"},
            "failure": {"repo_id": "repo-a", "path": "failure.trace"},
        }]}))

    def tearDown(self):
        self.temp.cleanup()

    def test_prepares_bounded_contrastive_prompt_with_source_hashes(self):
        prepared = build_prompt(self.corpus)
        self.assertIn("success.trace", prepared["prompt"])
        self.assertIn("failure.trace", prepared["prompt"])
        self.assertIn("issue-17", prepared["prompt"])
        self.assertIn("excerpt truncated", prepared["prompt"])
        self.assertLess(len(prepared["prompt"]), 48000)
        traces = prepared["provenance"]["traces"]
        self.assertEqual(traces[0]["sha256"], hashlib.sha256(self.success.read_bytes()).hexdigest())
        self.assertEqual({t["repo_id"] for t in traces}, {"repo-a"})
        self.assertIn("unvalidated", prepared["prompt"].lower())

    def test_refuses_protected_paths_and_secret_content(self):
        protected = self.root / "test_outputs" / "trace.txt"
        protected.parent.mkdir()
        protected.write_text("must not be read")
        self.corpus.write_text(json.dumps({"pairs": [{
            "task_id": "issue-17",
            "success": {"repo_id": "a", "path": "test_outputs/trace.txt"},
            "failure": {"repo_id": "b", "path": "failure.trace"},
        }]}))
        with self.assertRaisesRegex(ValueError, "protected path"):
            build_prompt(self.corpus)
        self.success.write_text("api_key = sk-abcdefghijklmnopqrstuvwxyz123456")
        self.corpus.write_text(json.dumps({"pairs": [{
            "task_id": "issue-17",
            "success": {"repo_id": "a", "path": "success.trace"},
            "failure": {"repo_id": "b", "path": "failure.trace"},
        }]}))
        with self.assertRaisesRegex(ValueError, "secret-like"):
            build_prompt(self.corpus)
        self.success.write_text("benchmark reward payload: private result")
        with self.assertRaisesRegex(ValueError, "grader-like"):
            build_prompt(self.corpus)
        self.success.write_text("ordinary success trace")
        self.corpus.write_text(json.dumps({"pairs": [{
            "task_id": "issue-17",
            "success": {"repo_id": "repo-a", "path": "success.trace"},
            "failure": {"repo_id": "repo-b", "path": "failure.trace"},
        }]}))
        with self.assertRaisesRegex(ValueError, "share a repository"):
            build_prompt(self.corpus)

    def test_execute_is_explicit_budgeted_and_writes_rof_compatible_library(self):
        binary = self.root / "fake-rof"
        capture = self.root / "invocation.json"
        binary.write_text(
            "#!" + sys.executable + "\n"
            "import json,pathlib,sys\n"
            "a=sys.argv[1:]\n"
            "def val(k): return a[a.index(k)+1]\n"
            "w=pathlib.Path(val('--workdir'))\n"
            "assert 'Use the write tool' in val('--goal') and 'procedures.json' in val('--goal')\n"
            "(w/'procedures.json').write_text(json.dumps([{'id':'focused-tests','cue':'cargo test','instruction':'Run the smallest relevant test first.','source_repositories':['repo-a']}]))\n"
            "pathlib.Path(val('--dump-events')).write_text(json.dumps({'type':'MessageEnd','usage':{'input_tokens':11,'output_tokens':7,'cache_read_tokens':2}})+'\\n')\n"
            f"pathlib.Path({str(capture)!r}).write_text(json.dumps(a))\n"
        )
        binary.chmod(0o755)
        output = self.root / "library"
        result = execute(self.corpus, output, binary=binary, model="local", endpoint="http://local/v1", budget_tokens=37)
        self.assertEqual(result["status"], "executed")
        proposal = json.loads((output / "procedure-001.json").read_text())
        self.assertEqual(set(proposal), {"id", "cue", "instruction", "source_repositories"})
        self.assertEqual(proposal["source_repositories"], ["repo-a"])
        metadata = json.loads((output / "provenance.json").read_text())
        self.assertEqual(metadata["validation"], "unvalidated")
        self.assertEqual(metadata["traces"][0]["sha256"], hashlib.sha256(self.success.read_bytes()).hexdigest())
        self.assertEqual(metadata["run"]["usage_totals"]["input_tokens"], 11)
        self.assertTrue((output / "events.jsonl").is_file())
        args = json.loads(capture.read_text())
        self.assertEqual(args[args.index("--budget-tokens") + 1], "37")
        self.assertEqual(args[args.index("--model") + 1], "local")
        self.assertEqual(args[args.index("--endpoint") + 1], "http://local/v1")


if __name__ == "__main__":
    unittest.main()

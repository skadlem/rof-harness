import json
import shlex
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from harbor.models.agent.context import AgentContext
from harbor_rof import RofAgent, RofOptions


class FakeEnvironment:
    def __init__(self, exit_code=0):
        self.commands = []
        self.exit_code = exit_code

    async def exec(self, *, command, **kwargs):
        self.commands.append(command)
        return SimpleNamespace(stdout=f"ROF_EXIT_CODE={self.exit_code}\n", return_code=0)


class HarborRofOptionsTest(unittest.IsolatedAsyncioTestCase):
    async def run_agent(self, exit_code=0, **options):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = FakeEnvironment(exit_code)
            agent = RofAgent(
                logs_dir=root,
                model_name="openai/model-id",
                runtime_dir=root,
                **options,
            )
            connection = SimpleNamespace(
                configured_base_url="https://example.invalid/v1", api_key="secret"
            )
            with patch.object(
                RofAgent,
                "model_connection",
                new_callable=lambda: property(lambda self: connection),
            ):
                await agent.run("goal", environment, AgentContext())
            shell_command = environment.commands[0]
            command = shell_command.split("set +e; ", 1)[1].split(" > ", 1)[0]
            return shlex.split(command)

    async def test_defaults_keep_recipe_without_new_switches(self):
        argv = await self.run_agent()
        self.assertNotIn("--archive-output", argv)
        self.assertNotIn("--shell", argv)
        for flag in (
            "--repo-lookup",
            "--repo-evidence",
            "--collapse-hysteresis",
            "--completion-check",
            "--deliverable",
            "--deliverable-steer",
        ):
            self.assertNotIn(flag, argv)
        self.assertIn("--workflow", argv)
        self.assertEqual(argv[argv.index("--workflow") + 1], "legacy")

    async def test_archive_output_and_shell_are_independent(self):
        archived = await self.run_agent(archive_output=True)
        self.assertEqual(archived[archived.index("--archive-output") + 1], "on")
        self.assertNotIn("--shell", archived)

        shell = await self.run_agent(shell=True)
        self.assertNotIn("--archive-output", shell)
        self.assertEqual(shell[shell.index("--shell") + 1], "on")

        both = await self.run_agent(archive_output=True, shell=True)
        self.assertEqual(both[both.index("--archive-output") + 1], "on")
        self.assertEqual(both[both.index("--shell") + 1], "on")

    async def test_existing_controls_keep_propagating(self):
        argv = await self.run_agent(
            workflow="minimal",
            preserve_failed_checks=True,
            observation_keep=32,
            thinking_keep=256,
            max_tokens=4096,
            budget_line="requests",
        )
        self.assertEqual(argv[argv.index("--budget-line") + 1], "requests")
        self.assertNotIn("--budget-line", await self.run_agent())
        self.assertEqual(argv[argv.index("--workflow") + 1], "minimal")
        self.assertEqual(argv[argv.index("--preserve-failed-checks") + 1], "on")
        self.assertEqual(argv[argv.index("--observation-keep") + 1], "32")
        self.assertEqual(argv[argv.index("--thinking-keep") + 1], "256")
        self.assertEqual(argv[argv.index("--max-tokens") + 1], "4096")

    async def test_xhigh_reasoning_effort_reaches_argv(self):
        argv = await self.run_agent(reasoning_effort="xhigh")
        self.assertEqual(argv[argv.index("--reasoning-effort") + 1], "xhigh")

    async def test_install_uses_no_install_recommends_for_git_on_apt(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in (
                "rof",
                "ld-linux-x86-64.so.2",
                "libc.so.6",
                "libm.so.6",
                "libgcc_s.so.1",
            ):
                (root / name).write_bytes(b"x")

            class AptEnv:
                def __init__(self):
                    self.commands = []
                    self.has_git = False

                async def exec(self, *, command, **kwargs):
                    self.commands.append(command)
                    if command == "command -v git":
                        return SimpleNamespace(stdout="", return_code=0 if self.has_git else 1)
                    if command == "command -v apt-get":
                        return SimpleNamespace(stdout="/usr/bin/apt-get\n", return_code=0)
                    if "apt-get install" in command:
                        self.has_git = True
                    return SimpleNamespace(stdout="", return_code=0)

                async def upload_file(self, src, dst):
                    pass

            env = AptEnv()
            agent = RofAgent(logs_dir=root, model_name="openai/m", runtime_dir=root)
            await agent.install(env)
            self.assertIn(
                "apt-get update && apt-get install -y --no-install-recommends git",
                env.commands,
            )

    def test_populate_context_handles_unicode_line_separators_in_jsonl(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            rof_dir = root / "rof"
            rof_dir.mkdir()
            (rof_dir / "exit-code").write_text("3\n")
            events = [
                '{"type":"ToolEnd","id":"1","name":"view","content":"a\u2028b\u2029c"}',
                '{"type":"TurnEnd","turn":1,"reason":"BudgetExceeded","usage_totals":{"input_tokens":100,"output_tokens":20,"cache_read_tokens":40,"reasoning_tokens":5,"cost_usd":0.01}}',
            ]
            (rof_dir / "events.jsonl").write_text("\n".join(events) + "\n")
            agent = RofAgent(logs_dir=root, model_name="openai/m", runtime_dir=root)
            context = AgentContext()
            agent.populate_context_post_run(context)
            self.assertEqual(context.n_input_tokens, 100)
            self.assertEqual(context.n_cache_tokens, 40)
            self.assertEqual(context.n_output_tokens, 20)
            self.assertEqual(context.cost_usd, 0.01)

    async def test_lookup_and_hysteresis_are_independent(self):
        lookup = await self.run_agent(repo_lookup=True)
        self.assertIn("--repo-lookup", lookup)
        self.assertEqual(lookup[lookup.index("--repo-lookup") + 1], "on")
        self.assertNotIn("--collapse-hysteresis", lookup)
        fold = await self.run_agent(collapse_hysteresis=3)
        self.assertIn("--collapse-hysteresis", fold)
        self.assertEqual(fold[fold.index("--collapse-hysteresis") + 1], "3")
        self.assertNotIn("--repo-lookup", fold)

    async def test_evidence_and_public_completion_command_reach_argv(self):
        command = "python3 -m unittest 'a b'"
        argv = await self.run_agent(
            repo_lookup=True, repo_evidence=True, completion_check=command
        )
        self.assertIn("--repo-evidence", argv)
        self.assertIn("--completion-check", argv)
        self.assertEqual(argv[argv.index("--repo-evidence") + 1], "on")
        self.assertEqual(argv[argv.index("--completion-check") + 1], command)
        self.assertEqual(argv[argv.index("--goal") + 1], "goal")

    async def test_deliverables_and_steer_reach_argv(self):
        argv = await self.run_agent(
            deliverables=["primers.fasta", "vm.js"], deliverable_steer=True
        )
        flagged = [argv[i + 1] for i, a in enumerate(argv) if a == "--deliverable"]
        self.assertEqual(flagged, ["primers.fasta", "vm.js"])
        self.assertEqual(argv[argv.index("--deliverable-steer") + 1], "on")
        shadow = await self.run_agent(deliverables=["vm.js"])
        self.assertIn("--deliverable", shadow)
        self.assertNotIn("--deliverable-steer", shadow)

    def test_evidence_requires_lookup_and_completion_is_nonempty(self):
        with self.assertRaises(ValueError):
            RofOptions(runtime_dir=Path("/tmp"), repo_evidence=True)
        with self.assertRaises(ValueError):
            RofOptions(runtime_dir=Path("/tmp"), completion_check="  ")

    async def test_history_and_compaction_options_reach_the_run(self):
        argv = await self.run_agent(
            history_read=True,
            compaction=0.7,
            context_capacity=131072,
            output_reserve=32768,
            checkpoint_sources=True,
        )
        for flag, value in (
            ("--history-read", "on"),
            ("--compaction", "0.7"),
            ("--context-capacity", "131072"),
            ("--output-reserve", "32768"),
            ("--checkpoint-sources", "on"),
        ):
            self.assertIn(flag, argv)
            self.assertEqual(argv[argv.index(flag) + 1], value)
        default = await self.run_agent()
        for flag in (
            "--history-read",
            "--compaction",
            "--context-capacity",
            "--output-reserve",
            "--checkpoint-sources",
        ):
            self.assertNotIn(flag, default)

    def test_compaction_requires_known_capacity_and_output_headroom(self):
        for options in (
            {"compaction": 0.7},
            {"checkpoint_sources": True},
            {"compaction": 0.7, "context_capacity": 32768, "output_reserve": 32768},
            {"compaction": 0.7, "context_capacity": 131072, "output_reserve": 8000},
            {
                "compaction": 0.7,
                "context_capacity": 131072,
                "output_reserve": 32768,
                "max_tokens": 65536,
            },
        ):
            with self.subTest(options=options), self.assertRaises(ValueError):
                RofOptions(runtime_dir=Path("/tmp"), **options)

    async def test_native_task_workdir_is_forwarded(self):
        argv = await self.run_agent(workdir="/workspace")
        self.assertEqual(argv[argv.index("--workdir") + 1], "/workspace")
        with self.assertRaises(ValueError):
            RofOptions(runtime_dir=Path("/tmp"), workdir="relative")

    def test_zero_numeric_values_are_rejected_by_pydantic(self):
        for name in (
            "observation_keep",
            "thinking_keep",
            "collapse_hysteresis",
        ):
            with self.subTest(name=name), self.assertRaises(ValueError):
                RofOptions(runtime_dir=Path("/tmp"), **{name: 0})

    async def test_uncapped_tokens_usd_budget_and_omitted_max_tokens(self):
        default = await self.run_agent()
        self.assertNotIn("--budget-spend-cents", default)
        self.assertEqual(default[default.index("--budget-tokens") + 1], "300000")
        self.assertEqual(default[default.index("--max-tokens") + 1], "8000")

        argv = await self.run_agent(budget_tokens=0, budget_usd=1.25, max_tokens=0)
        self.assertEqual(argv[argv.index("--budget-tokens") + 1], "0")
        self.assertEqual(argv[argv.index("--budget-spend-cents") + 1], "125")
        self.assertEqual(argv[argv.index("--max-tokens") + 1], "0")

        omitted = await self.run_agent(budget_usd=0.0, max_tokens=None)
        self.assertEqual(omitted[omitted.index("--budget-spend-cents") + 1], "0")
        self.assertEqual(omitted[omitted.index("--max-tokens") + 1], "0")

        with self.assertRaises(ValueError):
            RofOptions(
                runtime_dir=Path("/tmp"),
                compaction=0.7,
                context_capacity=131072,
                output_reserve=32768,
                max_tokens=0,
            )

    async def test_output_and_rollback_scope_reach_argv_only_when_set(self):
        default = await self.run_agent()
        self.assertNotIn("--exec-output", default)
        self.assertNotIn("--rollback-scope", default)
        argv = await self.run_agent(exec_output="head-tail", rollback_scope="errors")
        self.assertEqual(argv[argv.index("--exec-output") + 1], "head-tail")
        self.assertEqual(argv[argv.index("--rollback-scope") + 1], "errors")

    async def test_only_graded_exit_codes_pass(self):
        for code in (0, 3, 8):
            await self.run_agent(exit_code=code)
        for code in (2, 4, 7, 101, 137):
            with self.subTest(code=code), self.assertRaises(RuntimeError):
                await self.run_agent(exit_code=code)

    def test_usage_is_withheld_after_unknown_usage(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "rof").mkdir()
            turn = {"type": "TurnEnd", "usage_totals": {"input_tokens": 5, "cache_read_tokens": 1, "output_tokens": 2}}
            unknown = {"type": "Error", "error": {"code": "provider-usage-unknown", "message": "x"}}
            (root / "rof" / "events.jsonl").write_text(
                "\n".join(json.dumps(e) for e in (unknown, turn)) + "\n"
            )
            agent = RofAgent(logs_dir=root, model_name="openai/m", runtime_dir=root)
            context = AgentContext()
            agent.populate_context_post_run(context)
            self.assertIsNone(context.n_input_tokens)
            self.assertEqual(context.metadata, {"usage_unknown": True})

    def test_version_names_the_runtime_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            agent = RofAgent(logs_dir=root, model_name="openai/m", runtime_dir=root)
            self.assertEqual(agent.version(), "runtime-bundle")
            (root / "rof").write_bytes(b"binary")
            self.assertRegex(agent.version(), r"^runtime-bundle-[0-9a-f]{16}$")


if __name__ == "__main__":
    unittest.main()

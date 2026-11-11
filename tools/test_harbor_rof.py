import shlex
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from harbor.models.agent.context import AgentContext
from harbor_rof import RofAgent, RofOptions


class FakeEnvironment:
    def __init__(self):
        self.commands = []

    async def exec(self, *, command, **kwargs):
        self.commands.append(command)
        return SimpleNamespace(stdout="ROF_EXIT_CODE=0\n", return_code=0)


class HarborRofOptionsTest(unittest.IsolatedAsyncioTestCase):
    async def run_agent(self, **options):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            environment = FakeEnvironment()
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
        )
        self.assertEqual(argv[argv.index("--workflow") + 1], "minimal")
        self.assertEqual(argv[argv.index("--preserve-failed-checks") + 1], "on")
        self.assertEqual(argv[argv.index("--observation-keep") + 1], "32")
        self.assertEqual(argv[argv.index("--thinking-keep") + 1], "256")
        self.assertEqual(argv[argv.index("--max-tokens") + 1], "4096")

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

    def test_evidence_requires_lookup_and_completion_is_nonempty(self):
        with self.assertRaises(ValueError):
            RofOptions(runtime_dir=Path("/tmp"), repo_evidence=True)
        with self.assertRaises(ValueError):
            RofOptions(runtime_dir=Path("/tmp"), completion_check="  ")

    def test_zero_numeric_values_are_rejected_by_pydantic(self):
        for name in (
            "max_tokens",
            "observation_keep",
            "thinking_keep",
            "collapse_hysteresis",
        ):
            with self.subTest(name=name), self.assertRaises(ValueError):
                RofOptions(runtime_dir=Path("/tmp"), **{name: 0})


if __name__ == "__main__":
    unittest.main()

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

    def test_zero_numeric_values_are_rejected_by_pydantic(self):
        for name in ("max_tokens", "observation_keep", "thinking_keep"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                RofOptions(runtime_dir=Path("/tmp"), **{name: 0})


if __name__ == "__main__":
    unittest.main()

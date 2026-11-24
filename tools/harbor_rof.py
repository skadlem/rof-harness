"""Harbor adapter that runs ROF inside Harbor's task container."""

import hashlib
import json
import shlex
import tempfile
from pathlib import Path, PurePosixPath
from typing import Literal, override

from harbor.agents.installed.base import BaseInstalledAgent
from harbor.agents.model_connection import ModelConnectionSpec
from harbor.agents.options import InstalledAgentOptions
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext
from pydantic import Field, model_validator

_RUNTIME_FILES = (
    "rof",
    "ld-linux-x86-64.so.2",
    "libc.so.6",
    "libm.so.6",
    "libgcc_s.so.1",
)
_REMOTE = PurePosixPath("/opt/rof-runtime")


class RofOptions(InstalledAgentOptions):
    runtime_dir: Path = Field(
        description="Host directory containing the frozen ROF runtime bundle."
    )
    workdir: str = Field(default="/app", pattern=r"^/.+")
    budget_tokens: int = Field(default=300000, gt=0)
    budget_steps: int = Field(default=60, gt=0)
    budget_actions: int = Field(default=120, gt=0)
    max_tokens: int = Field(default=8000, gt=0)
    budget_shape: Literal["gross", "miss-plus-out"] = "gross"
    observation_keep: int | None = Field(default=None, gt=0)
    preserve_failed_checks: bool = False
    budget_line: Literal["legacy", "requests"] = "legacy"
    archive_output: bool = False
    shell: bool = False
    repo_lookup: bool = False
    repo_evidence: bool = False
    collapse_hysteresis: int | None = Field(default=None, gt=0)
    completion_check: str | None = Field(default=None, pattern=r"\S")
    deliverables: list[str] = []
    deliverable_steer: bool = False
    history_read: bool = False
    compaction: float | None = Field(default=None, gt=0, le=1)
    context_capacity: int | None = Field(default=None, gt=0)
    output_reserve: int | None = Field(default=None, ge=0)
    checkpoint_sources: bool = False
    workflow: Literal["legacy", "minimal"] = "legacy"
    reasoning_effort: Literal["low", "high", "xhigh", "max"] | None = None
    thinking_keep: int | None = Field(default=None, gt=0)
    exec_output: Literal["legacy", "head-tail"] = "legacy"
    rollback_scope: Literal["all", "errors"] = "all"

    @model_validator(mode="after")
    def validate_evidence(self):
        if self.repo_evidence and not self.repo_lookup:
            raise ValueError("repo_evidence requires repo_lookup")
        if self.checkpoint_sources and self.compaction is None:
            raise ValueError("checkpoint_sources requires compaction")
        if self.compaction is not None:
            maximum_output = max(self.max_tokens, 32768)
            if (
                self.context_capacity is None
                or self.output_reserve is None
                or not maximum_output <= self.output_reserve < self.context_capacity
            ):
                raise ValueError(
                    "compaction requires context_capacity and output_reserve covering the provider output ceiling below capacity"
                )
        return self


class RofAgent(BaseInstalledAgent):
    MODEL_CONNECTION = ModelConnectionSpec(default_provider="openai", passthrough=True)
    options_model = RofOptions
    options: RofOptions

    @staticmethod
    @override
    def name() -> str:
        return "rof"

    @override
    def version(self) -> str:
        # Identify the frozen binary, so trials from different builds never pool.
        try:
            digest = hashlib.sha256((self.options.runtime_dir / "rof").read_bytes())
        except OSError:
            return "runtime-bundle"
        return f"runtime-bundle-{digest.hexdigest()[:16]}"

    @override
    async def install(self, environment: BaseEnvironment) -> None:
        await self.ensure_system_dependencies(environment, ("git",))
        git = await environment.exec(command="command -v git", user="root")
        if git.return_code:
            raise RuntimeError("ROF requires git in the task environment")
        missing = [
            name
            for name in _RUNTIME_FILES
            if not (self.options.runtime_dir / name).is_file()
        ]
        if missing:
            raise FileNotFoundError(f"ROF runtime bundle missing: {', '.join(missing)}")
        await environment.exec(command=f"mkdir -p {_REMOTE}", user="root")
        for name in _RUNTIME_FILES:
            await environment.upload_file(
                self.options.runtime_dir / name, str(_REMOTE / name)
            )
        await environment.exec(
            command=f"chmod 755 {_REMOTE}/rof {_REMOTE}/ld-linux-x86-64.so.2",
            user="root",
        )

    @override
    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        if not self.model_name or "/" not in self.model_name:
            raise ValueError("model_name must be provider/model-id")
        access = self.model_connection
        if not access.configured_base_url or not access.api_key:
            raise ValueError("Harbor must provide OPENAI_BASE_URL and OPENAI_API_KEY")

        model = self.model_name.split("/", 1)[1]
        logs = self.environment_logs_dir / "rof"
        workdir = self.options.workdir
        quoted_workdir = shlex.quote(workdir)
        args = [
            str(_REMOTE / "ld-linux-x86-64.so.2"),
            "--library-path",
            str(_REMOTE),
            str(_REMOTE / "rof"),
            "run",
            "--goal",
            instruction,
            "--workdir",
            workdir,
            "--model",
            model,
            "--endpoint",
            access.configured_base_url,
            "--api-key-env",
            "OPENAI_API_KEY",
            "--budget-tokens",
            str(self.options.budget_tokens),
            "--budget-steps",
            str(self.options.budget_steps),
            "--budget-actions",
            str(self.options.budget_actions),
            "--budget-shape",
            self.options.budget_shape,
            "--max-tokens",
            str(self.options.max_tokens),
            "--exec-policy",
            "any",
            "--steer",
            "off",
            "--streak-steer",
            "off",
            "--keep-thinking",
            "--allow-dirty-workdir",
            "--preserve-failed-checks",
            "on" if self.options.preserve_failed_checks else "off",
            "--workflow",
            self.options.workflow,
            "--dump-events",
            str(logs / "events.jsonl"),
            "--log-path",
            str(logs / ".rof-events.jsonl"),
        ]
        if self.options.archive_output:
            args.extend(("--archive-output", "on"))
        if self.options.shell:
            args.extend(("--shell", "on"))
        if self.options.repo_lookup:
            args.extend(("--repo-lookup", "on"))
        if self.options.repo_evidence:
            args.extend(("--repo-evidence", "on"))
        if self.options.collapse_hysteresis is not None:
            args.extend(
                ("--collapse-hysteresis", str(self.options.collapse_hysteresis))
            )
        if self.options.completion_check is not None:
            args.extend(("--completion-check", self.options.completion_check))
        for path in self.options.deliverables:
            args.extend(("--deliverable", path))
        if self.options.deliverable_steer:
            args.extend(("--deliverable-steer", "on"))
        if self.options.history_read:
            args.extend(("--history-read", "on"))
        if self.options.compaction is not None:
            args.extend(("--compaction", str(self.options.compaction)))
        if self.options.context_capacity is not None:
            args.extend(("--context-capacity", str(self.options.context_capacity)))
        if self.options.output_reserve is not None:
            args.extend(("--output-reserve", str(self.options.output_reserve)))
        if self.options.checkpoint_sources:
            args.extend(("--checkpoint-sources", "on"))
        if self.options.budget_line != "legacy":
            args.extend(("--budget-line", self.options.budget_line))
        if self.options.observation_keep is not None:
            args.extend(("--observation-keep", str(self.options.observation_keep)))
        if self.options.thinking_keep is not None:
            args.extend(("--thinking-keep", str(self.options.thinking_keep)))
        if self.options.reasoning_effort is not None:
            args.extend(("--reasoning-effort", self.options.reasoning_effort))
        if self.options.exec_output != "legacy":
            args.extend(("--exec-output", self.options.exec_output))
        if self.options.rollback_scope != "all":
            args.extend(("--rollback-scope", self.options.rollback_scope))
        command = (
            "set -e; "
            f"mkdir -p {shlex.quote(str(logs))}; "
            f"if ! git -C {quoted_workdir} rev-parse --is-inside-work-tree >/dev/null 2>&1; then "
            f"git -C {quoted_workdir} init -q && git -C {quoted_workdir} add -A && "
            f"git -C {quoted_workdir} -c user.name=Harbor -c user.email=harbor@localhost "
            "commit --allow-empty -qm baseline; fi; "
            "set +e; "
            f"{shlex.join(args)} > {shlex.quote(str(logs / 'rof.log'))} 2>&1; "
            "rc=$?; "
            f"printf '%s' \"$rc\" > {shlex.quote(str(logs / 'exit-code'))}; "
            "printf 'ROF_EXIT_CODE=%s\\n' \"$rc\""
        )
        result = await environment.exec(
            command=command,
            cwd=workdir,
            env={"OPENAI_API_KEY": access.api_key},
        )
        marker = "ROF_EXIT_CODE="
        if marker not in result.stdout:
            raise RuntimeError(
                f"ROF launcher/bootstrap failed (exit {result.return_code})"
            )
        try:
            exit_code = int(result.stdout.split(marker)[-1].strip())
        except ValueError as exc:
            raise RuntimeError("ROF launcher returned an invalid exit code") from exc
        # 0 done, 3 halt/cancel/provider failure, 8 unmet completion check are
        # graded outcomes; anything else (2/4-7, a panic 101, a kill 137) is infra.
        if exit_code not in (0, 3, 8):
            raise RuntimeError(f"ROF infrastructure/startup failure (exit {exit_code})")

    @override
    def populate_context_post_run(self, context: AgentContext) -> None:
        logs = self.logs_dir / "rof"
        try:
            context.metadata = {"exit_code": int((logs / "exit-code").read_text())}
        except (OSError, ValueError):
            pass
        events = logs / "events.jsonl"
        try:
            parsed = [json.loads(line) for line in events.read_text().splitlines()]
            usage = next(
                event["usage_totals"]
                for event in reversed(parsed)
                if event.get("type") == "TurnEnd"
            )
        except (OSError, ValueError, KeyError, StopIteration):
            return
        # Same rule as the rof eval report: an attempt with unknown usage makes
        # the totals a lower bound, so report none rather than a wrong number.
        if any(
            event.get("type") == "Error"
            and (event.get("error") or {}).get("code")
            in ("wallclock-usage-unknown", "provider-usage-unknown")
            for event in parsed
        ):
            context.metadata = {**(context.metadata or {}), "usage_unknown": True}
            return
        context.n_input_tokens = usage["input_tokens"]
        context.n_cache_tokens = usage["cache_read_tokens"]
        context.n_output_tokens = usage["output_tokens"]
        context.cost_usd = usage.get("cost_usd")


def _check_usage_context() -> None:
    assert RofOptions(runtime_dir=Path("/tmp")).reasoning_effort is None
    assert (
        RofOptions(runtime_dir=Path("/tmp"), reasoning_effort="max").reasoning_effort
        == "max"
    )
    default = RofOptions(runtime_dir=Path("/tmp"))
    assert (
        default.budget_tokens,
        default.budget_steps,
        default.budget_actions,
        default.budget_shape,
    ) == (300000, 60, 120, "gross")
    assert default.observation_keep is None
    assert default.thinking_keep is None
    assert RofOptions(runtime_dir=Path("/tmp"), thinking_keep=256).thinking_keep == 256
    try:
        RofOptions(runtime_dir=Path("/tmp"), thinking_keep=-1)
    except ValueError:
        pass
    else:
        raise AssertionError("thinking_keep must be positive")
    assert (
        RofOptions(runtime_dir=Path("/tmp"), observation_keep=32).observation_keep == 32
    )
    try:
        RofOptions(runtime_dir=Path("/tmp"), observation_keep=-1)
    except ValueError:
        pass
    else:
        raise AssertionError("observation_keep must be positive")
    profile = RofOptions(
        runtime_dir=Path("/tmp"),
        budget_tokens=1200000,
        budget_steps=120,
        budget_actions=240,
        budget_shape="miss-plus-out",
    )
    assert (
        profile.budget_tokens,
        profile.budget_steps,
        profile.budget_actions,
        profile.budget_shape,
    ) == (1200000, 120, 240, "miss-plus-out")
    try:
        RofOptions(runtime_dir=Path("/tmp"), budget_steps=0)
    except ValueError:
        pass
    else:
        raise AssertionError("budget_steps must be positive")
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        events = root / "rof" / "events.jsonl"
        events.parent.mkdir()
        events.write_text(
            json.dumps(
                {
                    "type": "TurnEnd",
                    "usage_totals": {
                        "input_tokens": 12,
                        "cache_read_tokens": 3,
                        "output_tokens": 7,
                        "cost_usd": None,
                    },
                }
            )
            + "\n"
        )
        (events.parent / "exit-code").write_text("0")
        agent = RofAgent(logs_dir=root, model_name="openai/model", runtime_dir=root)
        context = AgentContext()
        agent.populate_context_post_run(context)
        assert (
            context.n_input_tokens,
            context.n_cache_tokens,
            context.n_output_tokens,
        ) == (12, 3, 7)
        assert context.metadata == {"exit_code": 0}


if __name__ == "__main__":
    _check_usage_context()

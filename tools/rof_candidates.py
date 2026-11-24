#!/usr/bin/env python3
"""Run two goal variants on isolated copies and select by a public check."""

import json
import hashlib
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import time


CONTROLLED_FLAGS = {
    "--goal", "--workdir", "--model", "--endpoint", "--dump-events",
    "--log-path", "--budget-tokens", "--budget-steps", "--budget-actions",
    "--no-log",
}
SIDECARS = {".rof-events.jsonl"}


def _resolve_binary(value):
    path = Path(value).expanduser()
    if path.exists():
        path = path.resolve()
        if not path.is_file():
            raise ValueError(f"binary is not a file: {path}")
        return str(path)
    found = shutil.which(value)
    if not found:
        raise ValueError(f"binary not found: {value}")
    return found


def _load_plan(path):
    plan = json.loads(Path(path).read_text())
    required = {
        "source_dir", "binary", "out_dir", "model", "goal",
        "alternative_goal", "check", "budget_tokens", "budget_steps",
        "budget_actions", "total_timeout_seconds",
    }
    optional = {"endpoint", "extra_args"}
    if not isinstance(plan, dict) or required - plan.keys() or plan.keys() - required - optional:
        missing = sorted(required - plan.keys()) if isinstance(plan, dict) else sorted(required)
        extra = sorted(plan.keys() - required - optional) if isinstance(plan, dict) else []
        raise ValueError(f"plan fields invalid; missing={missing}, unknown={extra}")
    source = Path(plan["source_dir"]).expanduser().resolve(strict=True)
    if not source.is_dir():
        raise ValueError("source_dir must be a directory")
    out_arg = Path(plan["out_dir"]).expanduser()
    out = out_arg.resolve(strict=False)
    if os.path.lexists(out_arg):
        raise ValueError(f"out_dir must not exist: {out}")
    if source == out or source in out.parents:
        raise ValueError("out_dir must not be inside source_dir")
    if out in source.parents:
        raise ValueError("out_dir must not contain source_dir")
    plan["source_dir"] = source
    plan["out_dir"] = out
    plan["binary"] = _resolve_binary(plan["binary"])
    for key in ("model", "goal", "alternative_goal"):
        if not isinstance(plan[key], str) or not plan[key].strip():
            raise ValueError(f"{key} must be a non-empty string")
    for key in ("budget_tokens", "budget_steps", "budget_actions"):
        if type(plan[key]) is not int or plan[key] < 2:
            raise ValueError(f"{key} must be an integer >= 2")
    plan["requested_budgets"] = {
        key: plan[key] for key in ("budget_tokens", "budget_steps", "budget_actions")
    }
    for key in ("budget_tokens", "budget_steps", "budget_actions"):
        plan[key] //= 2
    if not isinstance(plan["total_timeout_seconds"], (int, float)) or plan["total_timeout_seconds"] <= 0:
        raise ValueError("total_timeout_seconds must be positive")
    if not isinstance(plan["check"], list) or not plan["check"] or any(not isinstance(x, str) or not x for x in plan["check"]):
        raise ValueError("check must be a non-empty argv array of strings")
    extra_args = plan.get("extra_args", [])
    if not isinstance(extra_args, list) or any(not isinstance(x, str) for x in extra_args):
        raise ValueError("extra_args must be an array of strings")
    for arg in extra_args:
        flag = arg.split("=", 1)[0]
        if flag in CONTROLLED_FLAGS:
            raise ValueError(f"extra_args may not override controlled flag {flag}")
    plan["extra_args"] = extra_args
    if plan.get("endpoint") is not None and (not isinstance(plan["endpoint"], str) or not plan["endpoint"].strip()):
        raise ValueError("endpoint must be a non-empty string")
    return plan


def _ignored(name):
    return name in SIDECARS or name.startswith(".rof-dump")


def _copy_tree(source, destination, source_root, destination_root=None):
    if destination_root is None:
        destination_root = destination
    destination.mkdir(parents=True)
    with os.scandir(source) as entries:
        for entry in entries:
            if _ignored(entry.name) or entry.name == ".git":
                continue
            src = Path(entry.path)
            dst = destination / entry.name
            if entry.is_symlink():
                try:
                    target = src.resolve(strict=False)
                except (OSError, RuntimeError) as exc:
                    raise ValueError(f"cannot resolve source symlink {src}: {exc}") from exc
                if target != source_root and source_root not in target.parents:
                    raise ValueError(f"symlink escapes source: {src} -> {target}")
                relative_target = target.relative_to(source_root)
                mapped_target = destination_root / relative_target
                link = os.path.relpath(mapped_target, dst.parent)
                dst.symlink_to(link, target_is_directory=target.is_dir())
            elif entry.is_dir(follow_symlinks=False):
                _copy_tree(src, dst, source_root, destination_root)
            elif entry.is_file(follow_symlinks=False):
                shutil.copy2(src, dst)
            else:
                raise ValueError(f"unsupported source entry: {src}")


def _validate_source(source, source_root):
    with os.scandir(source) as entries:
        for entry in entries:
            if _ignored(entry.name) or entry.name == ".git":
                continue
            path = Path(entry.path)
            if entry.is_symlink():
                try:
                    target = path.resolve(strict=False)
                except (OSError, RuntimeError) as exc:
                    raise ValueError(f"cannot resolve source symlink {path}: {exc}") from exc
                if target != source_root and source_root not in target.parents:
                    raise ValueError(f"symlink escapes source: {path} -> {target}")
            elif entry.is_dir(follow_symlinks=False):
                _validate_source(path, source_root)
            elif not entry.is_file(follow_symlinks=False):
                raise ValueError(f"unsupported source entry: {path}")


def _run(argv, cwd, deadline, stdout_path, stderr_path):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        return {"status": "not_started", "returncode": None}
    started = time.monotonic()
    with open(stdout_path, "wb") as stdout, open(stderr_path, "wb") as stderr:
        try:
            proc = subprocess.Popen(argv, cwd=cwd, stdout=stdout, stderr=stderr, start_new_session=True)
        except OSError as exc:
            return {"status": "launch_error", "error": str(exc), "returncode": None}
        try:
            code = proc.wait(timeout=remaining)
            status = "completed"
        except subprocess.TimeoutExpired:
            if os.name == "posix":
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            else:
                proc.kill()
            proc.wait()
            code, status = None, "timeout"
    return {"status": status, "returncode": code, "elapsed_seconds": round(time.monotonic() - started, 3)}


def _read_events(events_path):
    totals, has_run_end = None, False
    try:
        lines = events_path.read_text().splitlines()
    except OSError:
        return None, False
    for line in lines:
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue
        kind = event.get("type")
        if kind == "TurnEnd" and isinstance(event.get("usage_totals"), dict):
            totals = event["usage_totals"]
        elif kind == "RunEnd":
            has_run_end = True
    usage = None if totals is None else {key: totals.get(key) for key in (
        "input_tokens", "output_tokens", "cache_read_tokens", "reasoning_tokens", "cost_usd"
    )}
    return usage, has_run_end


def _digest(path):
    path = Path(path)
    h = hashlib.sha256()
    if path.is_symlink():
        h.update(os.readlink(path).encode())
    elif path.is_file():
        with path.open("rb") as stream:
            for chunk in iter(lambda: stream.read(8192), b""):
                h.update(chunk)
    elif path.is_dir():
        for current, dirs, files in os.walk(path, followlinks=False):
            dirs[:] = sorted(d for d in dirs if d != ".git")
            for name in sorted(dirs + files):
                child = Path(current) / name
                h.update(str(child.relative_to(path)).encode())
                h.update(_digest(child).encode())
    else:
        return None
    return h.hexdigest()


def _check_inputs(argv, worktree):
    paths = []
    for arg in argv:
        if arg.startswith("-"):
            continue
        path = Path(arg).expanduser()
        if not path.is_absolute():
            path = worktree / path
        if path.exists() or path.is_symlink():
            paths.append(path)
    return list(dict.fromkeys(paths))


def _attempt(plan, index, goal, deadline, frozen_checks):
    name = f"candidate-{index}"
    base = plan["out_dir"] / name
    worktree = base / "worktree"
    events = base / "events.jsonl"
    log = base / "rof.log"
    run_argv = [
        plan["binary"], "run", "--goal", goal, "--workdir", str(worktree),
        "--model", plan["model"], "--budget-tokens", str(plan["budget_tokens"]),
        "--budget-steps", str(plan["budget_steps"]), "--budget-actions", str(plan["budget_actions"]),
        "--dump-events", str(events), "--log-path", str(log), *plan["extra_args"],
    ]
    if plan.get("endpoint"):
        run_argv.extend(["--endpoint", plan["endpoint"]])
    run = _run(run_argv, worktree, deadline, base / "run.stdout", base / "run.stderr")
    check = {"status": "not_started", "passed": False, "returncode": None}
    if time.monotonic() < deadline:
        check_run = _run(plan["check"], worktree, deadline, base / "check.stdout", base / "check.stderr")
        check = {
            "status": check_run["status"],
            "passed": check_run["status"] == "completed" and check_run["returncode"] == 0,
            "returncode": check_run.get("returncode"),
        }
        if "error" in check_run:
            check["error"] = check_run["error"]
    check_after = {path: _digest(path) for path in frozen_checks}
    modified_checks = [path for path, digest in frozen_checks.items() if check_after.get(path) != digest]
    if modified_checks:
        check["passed"] = False
        check["status"] = "check_modified"
        check["modified_inputs"] = modified_checks
    usage, has_run_end = _read_events(events)
    attempt = {
        "candidate": name,
        "goal": goal,
        "budget_tokens": plan["budget_tokens"],
        "budget_steps": plan["budget_steps"],
        "budget_actions": plan["budget_actions"],
        "run": run,
        "infrastructure_failures": [],
        "check": check,
        "usage": usage,
        "worktree": str(worktree),
        "logs": {"run_stdout": str(base / "run.stdout"), "run_stderr": str(base / "run.stderr"),
                 "check_stdout": str(base / "check.stdout"), "check_stderr": str(base / "check.stderr"),
                 "events": str(events), "rof_log": str(log)},
    }
    if run["status"] != "completed":
        attempt["infrastructure_failures"].append(f"rof process {run['status']}")
    # exit 8 (unmet completion check) is unreachable: this script never passes --completion-check
    elif run["returncode"] not in (0, 3):
        attempt["infrastructure_failures"].append(f"rof exited {run['returncode']}")
    elif not has_run_end:
        attempt["infrastructure_failures"].append("events have no RunEnd")
    if check["status"] in ("not_started", "timeout", "launch_error"):
        attempt["infrastructure_failures"].append(f"public check {check['status']}")
    return attempt


def run_plan(plan_path):
    plan = _load_plan(plan_path)
    source = plan["source_dir"]
    _validate_source(source, source)
    if (source / ".git").exists():
        check = subprocess.run(["git", "-C", str(source), "status", "--porcelain", "--untracked-files=all"],
                               capture_output=True, text=True, check=False)
        if check.returncode == 0 and check.stdout.strip():
            raise ValueError("source_dir must be clean; commit or remove uncommitted changes first")
    deadline = time.monotonic() + plan["total_timeout_seconds"]
    plan["out_dir"].parent.mkdir(parents=True, exist_ok=True)
    plan["out_dir"].mkdir()
    for index in (1, 2):
        base = plan["out_dir"] / f"candidate-{index}"
        base.mkdir()
        _copy_tree(source, base / "worktree", source, base / "worktree")
    frozen_checks = []
    for index in (1, 2):
        worktree = plan["out_dir"] / f"candidate-{index}" / "worktree"
        frozen_checks.append({str(path): _digest(path) for path in _check_inputs(plan["check"], worktree)})
    attempts = []
    for index, goal in enumerate((plan["goal"], plan["alternative_goal"]), 1):
        name = f"candidate-{index}"
        if time.monotonic() >= deadline:
            base = plan["out_dir"] / name
            attempts.append({"candidate": name, "goal": goal, "run": {"status": "not_started", "returncode": None},
                             "infrastructure_failures": ["shared total deadline expired"],
                             "budget_tokens": plan["budget_tokens"],
                             "budget_steps": plan["budget_steps"],
                             "budget_actions": plan["budget_actions"],
                             "check": {"status": "not_started", "passed": False, "returncode": None},
                             "usage": None,
                             "worktree": str(base / "worktree")})
            continue
        attempts.append(_attempt(plan, index, goal, deadline, frozen_checks[index - 1]))
    winner = next((a for a in attempts if a.get("check", {}).get("passed")), None)
    fallback = winner is None
    winner = winner or attempts[0]
    result = {
        "selected_candidate": winner["candidate"],
        "selected_path": winner.get("worktree", str(plan["out_dir"] / winner["candidate"] / "worktree")),
        "fallback": fallback,
        "selection_basis": "first candidate with a passing public independent check; candidate-1 control fallback if neither passes",
        "source_dir": str(source),
        "task": {"goal": plan["goal"], "alternative_goal": plan["alternative_goal"]},
        "public_check_argv": plan["check"],
        "allocated_budget_per_candidate": {
            "tokens": plan["budget_tokens"], "steps": plan["budget_steps"], "actions": plan["budget_actions"]
        },
        "requested_total_budgets": plan["requested_budgets"],
        "total_timeout_seconds": plan["total_timeout_seconds"],
        "attempts": attempts,
        "limitations": [
            "host process-group cleanup is not container isolation",
            "rof token caps inherit the harness soft grace behavior",
            "the public check is the only selection signal; model prose and hidden graders are not evaluated",
        ],
    }
    (plan["out_dir"] / "result.json").write_text(json.dumps(result, indent=2) + "\n")
    return result


def main(argv=None):
    argv = sys.argv[1:] if argv is None else argv
    if len(argv) != 1:
        print("usage: python3 tools/rof_candidates.py PLAN.json", file=sys.stderr)
        return 2
    try:
        result = run_plan(argv[0])
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"rof candidates: {exc}", file=sys.stderr)
        return 2
    print(json.dumps({k: result[k] for k in ("selected_candidate", "selected_path", "fallback")}, indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

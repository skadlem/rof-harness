"""Deterministic paired analysis for explicitly supplied ROF experiment evidence."""

import argparse
import json
import math
import re
import sys
from pathlib import Path

HASH = re.compile(r"^[0-9a-f]{64}$")
USAGE_KEYS = {"input_tokens", "cached_input_tokens", "output_tokens"}


def _need(condition, message):
    if not condition:
        raise ValueError(message)


def _hash(value, label):
    _need(
        isinstance(value, str) and HASH.fullmatch(value) is not None,
        f"{label} must be a lowercase SHA-256",
    )


def _nonempty(value, label):
    _need(
        isinstance(value, str) and bool(value.strip()),
        f"{label} must be a nonempty string",
    )


def _reward(value, label):
    _need(
        value is None
        or (
            isinstance(value, (int, float))
            and not isinstance(value, bool)
            and math.isfinite(value)
        ),
        f"{label} must be a finite number or null",
    )


def _usage(value, label):
    if value is None:
        return
    _need(
        isinstance(value, dict) and set(value) == USAGE_KEYS,
        f"{label} must contain exactly {sorted(USAGE_KEYS)}",
    )
    for key, count in value.items():
        _need(
            type(count) is int and count >= 0,
            f"{label}.{key} must be a nonnegative integer",
        )
    _need(
        value["cached_input_tokens"] <= value["input_tokens"],
        f"{label}.cached_input_tokens exceeds input_tokens",
    )


def _validate(experiment):
    _need(
        isinstance(experiment, dict)
        and set(experiment) == {"protocol", "arms", "rows"},
        "input must contain protocol, arms, and rows only",
    )
    p, arms, rows = experiment["protocol"], experiment["arms"], experiment["rows"]
    _need(isinstance(p, dict), "protocol must be an object")
    for key in ("dataset", "model", "endpoint", "runner", "grade_path"):
        _nonempty(p.get(key), f"protocol.{key}")
    _hash(p.get("release_hash"), "protocol.release_hash")
    _need(
        isinstance(p.get("task_hashes"), dict) and p["task_hashes"],
        "protocol.task_hashes must be a nonempty object",
    )
    for task, digest in p["task_hashes"].items():
        _nonempty(task, "task hash key")
        _hash(digest, f"protocol.task_hashes.{task}")
    _need(
        isinstance(p.get("budgets"), dict) and isinstance(p.get("splits"), dict),
        "protocol budgets and splits must be objects",
    )
    diff = p.get("allowed_difference")
    _need(
        isinstance(diff, dict) and set(diff) == {"key", "value"},
        "protocol.allowed_difference must contain key and value",
    )
    _nonempty(diff["key"], "protocol.allowed_difference.key")
    _need(
        isinstance(p.get("ledger"), dict),
        "protocol.ledger must identify the financial ledger",
    )
    _nonempty(p["ledger"].get("path"), "protocol.ledger.path")
    _hash(p["ledger"].get("sha256"), "protocol.ledger.sha256")
    _need(
        isinstance(p.get("pairs"), list) and p["pairs"],
        "protocol.pairs must be a nonempty array",
    )
    pairs = []
    for pair in p["pairs"]:
        _need(
            isinstance(pair, dict) and set(pair) == {"task_id", "replicate_id"},
            "each pair must contain task_id and replicate_id",
        )
        task, rep = pair["task_id"], pair["replicate_id"]
        _nonempty(task, "pair.task_id")
        _nonempty(rep, "pair.replicate_id")
        _need(task in p["task_hashes"], f"pair task {task!r} has no pinned task hash")
        key = (task, rep)
        _need(key not in pairs, f"duplicate expected pair {key!r}")
        pairs.append(key)
    _need(
        isinstance(arms, dict) and set(arms) == {"control", "treatment"},
        "arms must contain control and treatment",
    )
    for name, arm in arms.items():
        _need(
            isinstance(arm, dict) and set(arm) == {"runtime_sha256", "config"},
            f"arm {name} must contain runtime_sha256 and config",
        )
        _hash(arm["runtime_sha256"], f"arm {name}.runtime_sha256")
        _need(isinstance(arm["config"], dict), f"arm {name}.config must be an object")
    c, t = arms["control"]["config"], arms["treatment"]["config"]
    keys = set(c) | set(t)
    changed = {
        key
        for key in keys
        if c.get(key) != t.get(key) or (key not in c) != (key not in t)
    }
    _need(
        changed == {diff["key"]},
        "arm configs must differ only in the allowed treatment field",
    )
    _need(
        diff["key"] in t and t[diff["key"]] == diff["value"],
        "treatment config does not match protocol.allowed_difference",
    )
    _need(isinstance(rows, list), "rows must be an array")
    seen, attempts = set(), set()
    for i, row in enumerate(rows):
        label = f"rows[{i}]"
        _need(isinstance(row, dict), f"{label} must be an object")
        required = {
            "arm",
            "task_id",
            "attempt_id",
            "replicate_id",
            "artifact_reward",
            "run_status",
            "run_completed",
            "accounting_complete",
            "usage",
            "artifact_path",
            "artifact_sha256",
        }
        _need(set(row) == required, f"{label} fields do not match the row schema")
        _nonempty(row["arm"], f"{label}.arm")
        _need(row["arm"] in arms, f"{label}.arm is unknown")
        task, rep = row["task_id"], row["replicate_id"]
        _nonempty(task, f"{label}.task_id")
        _nonempty(rep, f"{label}.replicate_id")
        _nonempty(row["attempt_id"], f"{label}.attempt_id")
        _need(
            row["attempt_id"] not in attempts,
            f"duplicate attempt_id {row['attempt_id']!r}",
        )
        attempts.add(row["attempt_id"])
        _need((task, rep) in pairs, f"{label} has unexpected task/replicate pair")
        key = (row["arm"], task, rep)
        _need(key not in seen, f"duplicate row for arm/pair {key!r}")
        seen.add(key)
        _reward(row["artifact_reward"], f"{label}.artifact_reward")
        _nonempty(row["run_status"], f"{label}.run_status")
        _need(
            type(row["run_completed"]) is bool
            and type(row["accounting_complete"]) is bool,
            f"{label} completion flags must be booleans",
        )
        _usage(row["usage"], f"{label}.usage")
        _need(
            row["accounting_complete"] == (row["usage"] is not None),
            f"{label} accounting_complete must agree with usage availability",
        )
        _nonempty(row["artifact_path"], f"{label}.artifact_path")
        _hash(row["artifact_sha256"], f"{label}.artifact_sha256")
    return p, arms, pairs, rows


def _solved(reward):
    return reward is not None and reward >= 1


def _expected_task_average(arm, lookup, pairs, completed_only=False):
    by_task = {}
    for task, rep in pairs:
        row = lookup.get((arm, task, rep))
        reward = None if row is None else row["artifact_reward"]
        if (
            completed_only
            and row is not None
            and not (row["run_completed"] and row["accounting_complete"])
        ):
            reward = None
        by_task.setdefault(task, []).append(0 if reward is None else reward)
    return sum(sum(values) / len(values) for values in by_task.values()) / len(by_task)


def compare(experiment: dict) -> dict:
    p, arms, pairs, rows = _validate(experiment)
    lookup = {(r["arm"], r["task_id"], r["replicate_id"]): r for r in rows}
    out_pairs, rescue, harm, unchanged, incomplete = [], [], [], [], []
    raw_transitions = {"00": 0, "01": 0, "10": 0, "11": 0, "incomplete": 0}
    for task, rep in sorted(pairs):
        control = lookup.get(("control", task, rep))
        treatment = lookup.get(("treatment", task, rep))
        item = {
            "task_id": task,
            "replicate_id": rep,
            "control": control,
            "treatment": treatment,
        }
        out_pairs.append(item)
        if (
            control is None
            or treatment is None
            or control["artifact_reward"] is None
            or treatment["artifact_reward"] is None
        ):
            raw_transitions["incomplete"] += 1
        else:
            cell = f"{int(_solved(control['artifact_reward']))}{int(_solved(treatment['artifact_reward']))}"
            raw_transitions[cell] += 1
        if (
            control is None
            or treatment is None
            or not all(
                r["run_completed"]
                and r["accounting_complete"]
                and r["artifact_reward"] is not None
                for r in (control, treatment)
            )
        ):
            incomplete.append({"task_id": task, "replicate_id": rep})
            continue
        c, t = (
            _solved(control["artifact_reward"]),
            _solved(treatment["artifact_reward"]),
        )
        (rescue if not c and t else harm if c and not t else unchanged).append(
            {
                "task_id": task,
                "replicate_id": rep,
                "control_solved": c,
                "treatment_solved": t,
            }
        )
    raw = {}
    for arm in arms:
        arm_rows = [r for r in rows if r["arm"] == arm]
        raw[arm + "_solved"] = sum(_solved(r["artifact_reward"]) for r in arm_rows)
        raw[arm + "_attempts"] = len(arm_rows)
        raw[arm + "_task_weighted_reward"] = _expected_task_average(arm, lookup, pairs)
    raw["artifact_transitions"] = raw_transitions
    raw["task_weighted_reward_basis"] = (
        "expected pairs; missing/unknown rewards contribute zero"
    )
    usage = {}
    for arm in arms:
        arm_rows = [r for r in rows if r["arm"] == arm]
        missing = any(
            lookup.get((arm, task, rep)) is None
            or lookup[(arm, task, rep)]["usage"] is None
            for task, rep in pairs
        )
        known = {
            key: sum(r["usage"][key] for r in arm_rows if r["usage"] is not None)
            for key in USAGE_KEYS
        }
        values = dict(known) if not missing else {key: None for key in USAGE_KEYS}
        values["gross_tokens"] = (
            values["input_tokens"] + values["output_tokens"] if not missing else None
        )
        task_costs, task_solve_fractions = {}, {}
        for current_task in sorted({task for task, _ in pairs}):
            expected = [(task, rep) for task, rep in pairs if task == current_task]
            task_rows = [lookup.get((arm, t, rep)) for t, rep in expected]
            task_costs[current_task] = (
                None
                if any(r is None or r["usage"] is None for r in task_rows)
                else sum(
                    r["usage"]["input_tokens"] + r["usage"]["output_tokens"]
                    for r in task_rows
                )
                / len(expected)
            )
            task_solve_fractions[current_task] = sum(
                float(
                    r is not None
                    and r["run_completed"]
                    and r["accounting_complete"]
                    and _solved(r["artifact_reward"])
                )
                for r in task_rows
            ) / len(expected)
        total_task_cost = None if missing else sum(task_costs.values())
        total_task_solves = sum(task_solve_fractions.values())
        outcomes_known = all(
            lookup.get((arm, task, rep)) is not None
            and lookup[(arm, task, rep)]["artifact_reward"] is not None
            for task, rep in pairs
        )
        tps = (
            None
            if missing or not outcomes_known or total_task_solves == 0
            else total_task_cost / total_task_solves
        )
        usage[arm] = {
            **values,
            "known_subtotals": known,
            "usage_complete": not missing,
            "task_costs": task_costs,
            "task_solve_fractions": task_solve_fractions,
            "tokens_per_solved_task": tps,
        }
    ctp, ttp = (
        usage["control"]["tokens_per_solved_task"],
        usage["treatment"]["tokens_per_solved_task"],
    )
    usage["treatment_vs_control_ratio"] = (
        None if ctp is None or ctp == 0 or ttp is None else ttp / ctp
    )
    comparison = {
        "pairs": out_pairs,
        "rescue": rescue,
        "harm": harm,
        "unchanged": unchanged,
        "incomplete_pairs": incomplete,
        "transitions": {
            "00": sum(
                not x["control_solved"] and not x["treatment_solved"] for x in unchanged
            ),
            "01": len(rescue),
            "10": len(harm),
            "11": sum(x["control_solved"] and x["treatment_solved"] for x in unchanged),
        },
        "task_weighted_reward": {
            arm: _expected_task_average(arm, lookup, pairs, completed_only=True)
            for arm in arms
        },
        "task_weighted_reward_basis": "completed and accounted expected pairs; other cells contribute zero",
    }
    return {
        "protocol": p,
        "arms": arms,
        "raw": raw,
        "comparison": comparison,
        "attempts": sorted(
            rows,
            key=lambda r: (r["arm"], r["task_id"], r["replicate_id"], r["attempt_id"]),
        ),
        "usage": usage,
        "retained_for_research": False,
        "eligible_for_promotion": False,
    }


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", required=True)
    parser.add_argument("--output", required=True)
    args = parser.parse_args(argv)
    try:
        report = compare(json.loads(Path(args.input).read_text(encoding="utf-8")))
        Path(args.output).write_text(
            json.dumps(report, sort_keys=True, indent=2, allow_nan=False) + "\n",
            encoding="utf-8",
        )
    except (OSError, json.JSONDecodeError, ValueError) as exc:
        print(f"rof_compare: {exc}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

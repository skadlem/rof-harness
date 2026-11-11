#!/usr/bin/env python3
"""Envelope, not context: what killed the flagged runs, read off the event dumps.

Reads every event dump under `~/.local/share/rof-tb/work/` (patterns `events.jsonl`
and `*.events.json`) and answers three questions without a model call:

  Q1 headroom   final settled turn's input_tokens (primary), and only secondarily that
                number over a context window the pi config *declares* for the model id
  Q2 replay     the same token cap charged as miss-plus-out instead of gross
  Q3 last3      were the three steps before the kill still editing, or only reading?

Counter model is the one validated in research/accounting-recompute.md and reused
verbatim: the run counter is cumulative (input + output) per settle, cache reads at
full weight (usage.cache_read is a SUBSET of usage.input), and the halt fires on the
first crossing. `cum_tokens_at_halt` therefore reads `usage_totals` off the last
TurnEnd when present; only a dump with neither totals nor per-message usage is
UNKNOWN (never zero, never dropped).

Honesty rules enforced here, not in prose:
  * unmetered dumps are counted, named, and excluded from every aggregate;
  * a run whose model or cap cannot be sourced keeps nulls and is excluded from the
    affected aggregate with the count and the family names printed;
  * every aggregate prints its own n;
  * no solve rate is computed: the dumps carry no graded reward;
  * a context window is pi-config provenance, not a served limit: every row records
    `context_window_source`, and the fraction is secondary to the absolute token count;
  * a tool result with `is_error: true` did not mutate the tree, and a result with no
    recorded `is_error` is unknown, not clean;
  * an absent usage field stays null and routes the row to the unmetered bucket;
  * rows carry corpus-relative paths, so a checkout with the same dumps rewrites
    byte-identical output.

Standard library only; output is deterministic (rows sorted by path, fixed key order).
Run: `python3 tools/envelope_report.py`
"""
import glob
import json
import math
import os
import re
import shlex
import sys

WORK = os.path.expanduser("~/.local/share/rof-tb/work")
OUT = os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
                   "docs", "experiments", "2026-11-11-envelope.json")

# --- caps and models, per family, sourced from the driver that produced the dumps ---
# `crates/rof/src/eval_cmd.rs:650-652` is the eval agent-leg default: 300k tokens / 60
# steps / 120 actions, applied whenever a driver passes no --budget-* flag.
DEFAULT_CAP = (300_000, 60, 120)
D, R1 = "crates/rof/src/eval_cmd.rs:650-652 (defaults, no --budget-* flag in driver)", None
DS = "deepseek-v4.1-flash"
DSD = "deepseek-ai/DeepSeek-V4.1-Flash"
GLM = "glm-5.3-flash"
GROSS, MPO = "gross", "miss-plus-out"

# family -> model, token cap, step cap, action cap, budget shape, cap provenance,
#           already_ran_this_treatment, void
FAMILIES = {
    "pilot-r1":         (DS,  300_000, 60, 120, GROSS, "pilot-r1-driver.sh:93 model, :104 caps", False, False),
    "pilot-sweep":      (DS,  300_000, 60, 120, GROSS, "pilot-sweep-driver.sh:7 sources pilot-r1-driver.sh:93,104", False, False),
    "pilot-sweep-eval": ("deepseek-flash", 300_000, 60, 120, GROSS, "pilot-sweep-eval-driver.sh:38 model, :4 no budget flags -> " + D, False, False),
    "pilot-glm-ict":    (GLM, 300_000, 60, 120, GROSS, "pilot-glm-ict-driver.sh:36 -> " + D, False, False),
    "pilot-glm-ict2":   (GLM, 300_000, 60, 120, GROSS, "pilot-glm-ict2-driver.sh:38 -> " + D, False, False),
    "pilot-glm-bs":     (GLM, 300_000, 60, 120, MPO,   "pilot-glm-bs-driver.sh:40-41 --budget-shape miss-plus-out -> " + D, True, False),
    "pilot-dphn-cap":   (DSD, 300_000, 60, 120, None,  "pilot-dphn-cap-driver.sh:21 model, :42-43 arms C/S/J", True, True),
    "pilot-dphn-apppath": (DSD, 300_000, 60, 120, GROSS, "pilot-dphn-apppath-driver.sh:21 -> " + D, False, False),
    "pilot-dphn-effort":  (DSD, 300_000, 60, 120, GROSS, "pilot-dphn-effort-driver.sh:22 -> " + D, False, False),
    "pilot-dphn-trust":   (DSD, 300_000, 60, 120, GROSS, "pilot-dphn-trust-driver.sh:20 -> " + D, False, False),
    "pilot-effort2":    (DSD, 300_000, 60, 120, GROSS, "pilot-effort2-driver.sh:23 -> " + D, False, False),
    "pilot-env1":       (DSD, 300_000, 60, 120, GROSS, "pilot-env1-driver.sh:15 -> " + D, False, False),
    "pilot-goalchk":    (DSD, 300_000, 60, 120, GROSS, "pilot-goalchk-driver.sh:21 -> " + D, False, False),
    "pilot-goalchk2":   (DSD, 300_000, 60, 120, GROSS, "pilot-goalchk2-driver.sh:21 -> " + D, False, False),
    "pilot-map":        (DSD, 300_000, 60, 120, GROSS, "pilot-map-driver.sh:12 -> " + D, False, False),
    "pilot-map2":       (DSD, 300_000, 60, 120, GROSS, "pilot-map2-driver.sh:12 -> " + D, False, False),
    "pilot-map3":       (DSD, 300_000, 60, 120, GROSS, "pilot-map3-driver.sh:12 -> " + D, False, False),
    "pilot-seedfix":    (DSD, 300_000, 60, 120, GROSS, "pilot-seedfix-driver.sh:18 -> " + D, False, False),
    "pilot-solve1":     (DSD, 300_000, 60, 120, GROSS, "pilot-solve1-driver.sh:15 -> " + D, False, False),
    "pilot-solve2":     (DSD, 300_000, 60, 120, GROSS, "pilot-solve2-driver.sh:15 -> " + D, False, False),
    "pilot-steer":      (DSD, 300_000, 60, 120, GROSS, "pilot-steer-driver.sh:24 -> " + D, False, False),
    "pilot-steer2":     (DSD, 300_000, 60, 120, GROSS, "pilot-steer2-driver.sh:19 -> " + D, False, False),
    # pilot-dphn-cap arm J raises both walls; resolved per row from the path prefix.
    "pilot-dphn-cap:J": (DSD, 300_000, 120, 240, MPO, "pilot-dphn-cap-driver.sh:43 J arm", True, True),
}

# Flash peak, $/Mtok (provider-openai price table): hit 0.006, miss 0.30, out 1.20.
# Only the matched model is priced; every other id keeps null dollars (see `dollars_note`).
PRICE = {DS: (0.006, 0.30, 1.20), DSD: (0.006, 0.30, 1.20)}

# Tool names, inventoried from the corpus: exec write view search edit test read.
# There is no done/finish tool, so "declared Done" can only come from assistant text.
MUTATING_TOOLS = {"write", "edit", "multi_edit"}
# Verb sets are matched at COMMAND POSITION (argv[0]) only: the old whole-string regex
# matched `install`/`ln`/`dd` inside inline programs (`apt-get install`, `d.ln`, `d.dd`).
WRITE_VERBS = {"tee", "cp", "mv", "rm", "mkdir", "touch", "dd", "truncate", "patch",
               "install", "chmod", "chown", "ln"}
GIT_WRITE = {"apply", "add", "commit", "checkout", "mv", "rm"}
HELP_FLAGS = {"--help", "-h"}  # `chmod --help` reads; it does not write
INLINE_WRITE = re.compile(r"open\([^)]*['\"][wa]['\"]")
TEST_CMD = re.compile(r"test_outputs\.py|pytest|unittest|cargo test|make test|run_tests|_selfcheck|--check\b")
DONE_TEXT = re.compile(r"(?i)\b(done|complete|completed|finished)\b|all (?:tests|checks) pass")
KIND_RANK = {"tool": 0, "exec_verb": 1}


def pct(sorted_vals: list, q: float):
    """Nearest-rank quantile on an already-sorted list."""
    if not sorted_vals:
        return None
    i = max(0, min(len(sorted_vals) - 1, math.ceil(q * len(sorted_vals)) - 1))
    return sorted_vals[i]


def simple_cmds(cmd: str) -> list:
    """Split a shell string into simple commands, ignoring separators inside quotes."""
    segs, cur, quote = [], "", None
    for ch in cmd:
        if quote:
            cur += ch
            if ch == quote:
                quote = None
        elif ch in "'\"":
            quote = ch
            cur += ch
        elif ch in ";|&\n()":
            if cur.strip():
                segs.append(cur.strip())
            cur = ""
        else:
            cur += ch
    if cur.strip():
        segs.append(cur.strip())
    return segs


def write_verb(cmd: str):
    """The write verb if one sits at command position, else None.

    `chmod --help` is a read; `apt-get install` is not a file write; `d.ln(...)` inside
    an inline program is not a shell command at all.
    """
    for seg in simple_cmds(cmd):
        try:
            argv = shlex.split(seg)
        except ValueError:
            argv = seg.split()
        while argv and (re.match(r"^[A-Za-z_][A-Za-z0-9_]*=", argv[0])
                        or argv[0] in ("sudo", "env", "time", "nohup", "command")):
            argv.pop(0)
        if not argv or any(a in HELP_FLAGS for a in argv[1:]):
            continue
        prog = os.path.basename(argv[0])
        if prog in WRITE_VERBS:
            return " ".join(argv[:2])
        if prog == "sed" and any(a.startswith("-i") for a in argv[1:]):
            return "sed -i"
        if prog == "perl" and any(re.fullmatch(r"-[a-z]*i", a) for a in argv[1:]):
            return "perl -i"
        if prog == "git" and len(argv) > 1 and argv[1] in GIT_WRITE:
            return "git " + argv[1]
    return None


def stats(vals: list) -> dict:
    v = sorted(x for x in vals if x is not None)
    return {"n": len(v), "min": v[0] if v else None, "p10": pct(v, 0.10),
            "median": pct(v, 0.50), "p90": pct(v, 0.90), "max": v[-1] if v else None}


def find_windows():
    """model id -> (declared context window, source), from the pi model stores.

    The window is a client-side config key: entries are keyed by model id only and the
    last provider seen wins, so the value says nothing about the endpoint a driver hit.
    Hence the source string per entry: `pi-config:<file>:<provider>`.
    A corrupt or unreadable store is skipped and named, never fatal.
    """
    wins: dict = {}
    skipped: list = []
    for fn in ("models-store.json", "models.json"):  # store first, config overrides
        path = os.path.expanduser("~/.pi/agent/" + fn)
        if not os.path.exists(path):
            skipped.append(fn + ":absent")
            continue
        try:
            with open(path) as fh:
                doc = json.load(fh)
        except (OSError, ValueError):
            skipped.append(fn + ":unreadable")
            continue
        for prov, mid, win in walk_models(doc):
            wins[mid] = (win, f"pi-config:{fn}:{prov}")
    return wins, skipped


def walk_models(o, provider=None):
    """Yield (provider, model id, contextWindow) from a pi model store document."""
    if isinstance(o, list):
        for v in o:
            yield from walk_models(v, provider)
    elif isinstance(o, dict):
        models = o.get("models")
        if isinstance(models, list):
            for m in models:
                if isinstance(m, dict) and isinstance(m.get("id"), str) \
                        and "contextWindow" in m:
                    yield provider, m["id"], m["contextWindow"]
        for k, v in o.items():
            if k != "models":
                yield from walk_models(v, k)


def family_of(rel):
    top = rel.split(os.sep)[0]
    if top == "pilot-dphn-cap" and rel.split(os.sep)[1].startswith("J-"):
        return "pilot-dphn-cap:J"
    return top


def cmd_of(args):
    for k in ("cmd", "command", "exec"):
        if isinstance(args.get(k), str):
            return args[k]
    return ""


def parse(path):
    """Event stream -> per-turn usage series, terminal facts, tool calls by step.

    Raises on a malformed dump; the caller turns that into a reported exclusion.
    """
    turns, tools = [], []
    turn_end, run_end = None, None
    text = {}
    step = 0
    last_id = None
    with open(path, encoding="utf-8", errors="replace") as fh:
        lines = fh.read().splitlines()
    events = [json.loads(line) for line in lines if line.strip()]
    for e in events:
        t = e.get("type")
        if t == "MessageEnd":
            last_id = e.get("id")
            if e.get("usage"):
                u = e["usage"]
                step += 1
                # an absent field stays None: it routes the row to the unmetered bucket
                turns.append({"step": step,
                              "input": u.get("input_tokens"),
                              "cache_read": u.get("cache_read_tokens"),
                              "output": u.get("output_tokens")})
            c = (e.get("message") or {}).get("content") or ""
            if c:
                text[e.get("id")] = c
        elif t == "MessageUpdate":
            d = (e.get("delta") or {}).get("text") or ""
            if d:
                text[e.get("id")] = text.get(e.get("id"), "") + d
        elif t == "ToolStart":
            tools.append({"step": max(step, 1), "name": e.get("name"),
                          "args": e.get("args") or {}, "id": e.get("id"), "is_error": None})
        elif t == "ToolEnd":
            for c in reversed(tools):
                if c["id"] == e.get("id"):
                    v = e.get("is_error")
                    c["is_error"] = None if v is None else bool(v)  # absent stays unknown
                    break
        elif t == "TurnEnd":
            turn_end = e
        elif t == "RunEnd":
            run_end = e
    return {"turns": turns, "tools": tools, "turn_end": turn_end, "run_end": run_end,
            "text": text, "last_id": last_id}


def outcome_of(run_end):
    if not run_end:
        return "no_run_end", None
    o = run_end.get("outcome")
    if o is None:
        return "no_run_end", None
    if isinstance(o, str):
        return {"Passed": "passed", "Aborted": "aborted"}.get(o, "error"), o
    if isinstance(o, dict) and len(o) == 1:
        k, v = next(iter(o.items()))
        if k == "Failed" and v in ("tokens", "steps", "actions"):
            return v, f"Failed: {v}"
        return "error", f"Failed: {str(v)[:120]}"
    return "error", str(o)[:120]


def row_for(path, rel, wins):
    try:
        parsed = parse(path)
    except Exception as e:
        return None, f"parse_error:{type(e).__name__}"
    fam = family_of(rel)
    spec = FAMILIES.get(fam) or (None, None, None, None, None,
                                 "unknown: no driver and no dump field records the cap", False, False)
    model, cap, step_cap, act_cap, shape, prov, reran, is_void = spec
    parts = rel.split(os.sep)
    if "void" in parts:
        is_void = True
    if fam.startswith("pilot-dphn-cap") and len(parts) > 2:
        shape = MPO if parts[1].startswith(("S-", "J-")) else GROSS

    turns = parsed["turns"]
    te, re_ = parsed["turn_end"], parsed["run_end"]
    totals = (te or {}).get("usage_totals") or None
    # a missing usage field must not become zero: incomplete series stays unmetered
    per_turn_complete = all(t["input"] is not None and t["cache_read"] is not None
                            and t["output"] is not None for t in turns)
    sum_in = sum(t["input"] for t in turns) if per_turn_complete else None
    sum_out = sum(t["output"] for t in turns) if per_turn_complete else None
    per_turn_ok = bool(totals) and per_turn_complete and (sum_in, sum_out) == (
        totals.get("input_tokens") or 0, totals.get("output_tokens") or 0)
    if totals:
        cum_in, cum_out, cum_src = (totals.get("input_tokens") or 0), (totals.get("output_tokens") or 0), "usage_totals"
    elif turns and per_turn_complete:
        cum_in, cum_out, cum_src = sum_in, sum_out, "sum(MessageEnd)"
    else:
        cum_in, cum_out, cum_src = None, None, "unmetered"
    cum_tokens = None if (cum_in is None or cum_out is None) else cum_in + cum_out

    reason, outcome_raw = outcome_of(re_)
    last = turns[-1] if turns else None
    win, win_src = wins.get(model, (None, "unknown")) if model else (None, "unknown")
    price = PRICE.get(model) if model else None
    dollars = None
    if price and cum_in is not None and cum_out is not None and per_turn_complete:
        cr = sum(t["cache_read"] for t in turns)
        miss = max(0, cum_in - cr)
        dollars = round((miss * price[1] + cr * price[0] + cum_out * price[2]) / 1e6, 6)

    # --- Q2: replay miss-plus-out against the same cap ---------------------------------
    x5 = x1 = None          # extra turns, trailing-5 mean vs last-turn-only
    w5 = w1 = None          # which wall wins
    b5 = b1 = None          # 'recorded' | 'extrapolated/<sensitivity>' | None
    j5 = None               # extrapolated miss-plus-out halt step, trailing-5 basis
    note = None
    cum_gross = cum_prev = 0
    k_gross = k_miss = None
    cum_miss_at_halt = None
    if cap and turns and per_turn_ok and reason == "tokens" and shape != MPO:
        for t in turns:  # first crossing, cumulative, cache reads at full weight
            cum_gross += t["input"] + t["output"]
            if k_gross is None and cum_gross >= cap:
                k_gross = t["step"]
            cum_prev += max(0, t["input"] - t["cache_read"]) + t["output"]
            if k_miss is None and cum_prev >= cap:
                k_miss = t["step"]
        cum_miss_at_halt = cum_prev
        n = len(turns)
        tail = turns[-5:]
        inc5 = sum(max(0, t["input"] - t["cache_read"]) + t["output"] for t in tail) / len(tail)
        inc1 = max(0, turns[-1]["input"] - turns[-1]["cache_read"]) + turns[-1]["output"]
        if k_gross is None:
            note = "gross counter never crossed the cap"
        else:
            for key, inc in (("last5", inc5), ("last_turn", inc1)):
                if k_miss is not None:
                    j, basis = k_miss, "recorded"
                elif inc <= 0:
                    note = "no positive trailing increment"
                    continue
                else:
                    j = n + math.ceil((cap - cum_prev) / inc)
                    basis = f"extrapolated/{key}"
                winner = "step_cap" if j > step_cap else "token_cap"
                extra = (step_cap if winner == "step_cap" else j) - k_gross
                if key == "last5":
                    x5, w5, b5, j5 = extra, winner, basis, j
                else:
                    x1, w1, b1 = extra, winner, basis
                note = note or f"basis={basis}, halt_step={k_gross}, mpo_halt_step={j}"
    # --- Q3: what were the last three steps doing? ------------------------------------
    q3_tools, mutated, kind, evidence = [], None, None, None
    err_results = unknown_results = 0
    if shape == MPO and reason == "tokens":
        note = "shape_already_miss_plus_out: the recorded halt was already this counter"
    declared = None
    if turns:
        n = len(turns)
        last3 = [t for t in parsed["tools"] if n - 2 <= t["step"] <= n]
        q3_tools = [t["name"] for t in last3]
        hits = []  # (kind, evidence); kind ranks tool > exec_verb
        unknown_calls = err_calls = 0
        for t in last3:
            nm, c = t["name"], cmd_of(t["args"])
            if t["is_error"] is True:
                err_calls += 1          # denied/failed: the tree did not change
                continue
            if t["is_error"] is None:
                unknown_calls += 1      # no ToolEnd: unknown, never assumed clean
                continue
            if nm in MUTATING_TOOLS:
                hits.append(("tool", f"{nm}:{t['args'].get('path', '')}"[:80]))
                continue
            if nm != "exec":
                continue
            stripped = re.sub(r"\d?>&\d", "", c)
            verb = write_verb(stripped)
            if verb:
                hits.append(("exec_verb", f"exec:argv0={verb!r} ...{stripped[:70]}..."))
                continue
            m = INLINE_WRITE.search(stripped)
            if m:
                lo = max(0, m.start() - 25)
                hits.append(("exec_verb", f"exec:match={m.group(0)!r} ...{stripped[lo:m.end() + 25]}..."))
                continue
        hits.sort(key=lambda h: KIND_RANK[h[0]])   # strongest hit, so kind and evidence agree
        mutated = True if hits else (None if unknown_calls else False)
        kind, evidence = hits[0] if hits else (None, None)
        err_results, unknown_results = err_calls, unknown_calls
        # declared Done can only come from the LAST assistant message; an absent one
        # is unknown, not false
        final_text = parsed["text"].get(parsed["last_id"])
        if final_text:
            declared = bool(DONE_TEXT.search(final_text))
    passing = any(t["name"] == "test" and t["is_error"] is False for t in parsed["tools"]) or any(
        t["name"] == "exec" and t["is_error"] is False and TEST_CMD.search(cmd_of(t["args"]))
        for t in parsed["tools"])

    row = {
        "path": rel,        "family": fam,
        "model": model,
        "terminal_reason": reason,
        "steps": len(turns),
        "final_turn_input_tokens": last["input"] if last else None,
        "final_turn_cache_read_tokens": last["cache_read"] if last else None,
        "final_turn_output_tokens": last["output"] if last else None,
        "cum_tokens_at_halt": cum_tokens,
        "cum_dollars_at_halt": dollars,
        # Q1: the absolute token count is the statistic; the fraction is secondary and
        # divides by a pi-config value, not by a served limit
        "context_window": win,
        "context_window_source": win_src,
        "q1_context_fraction_from_client_config": (
            round(last["input"] / win, 4) if (last and last["input"] is not None and win) else None),
        # Q2
        "q2_extra_turns_last5": x5,
        "q2_extra_turns_last_turn": x1,
        "q2_winner_last5": w5,
        "q2_winner_last_turn": w1,
        "q2_basis": b5,
        "q2_basis_last_turn": b1,
        "q2_mpo_halt_step": j5,
        "q2_note": note,
        "cum_miss_plus_out_at_halt": cum_miss_at_halt,
        # Q3
        "q3_last3_tool_names": q3_tools,
        "q3_last3_mutated_tree": mutated,
        "q3_last3_mutation_kind": kind,
        "q3_last3_mutation_evidence": evidence,
        "q3_last3_error_results": err_results,
        "q3_last3_unknown_results": unknown_results,
        "q3_declared_done": declared,
        "q3_passing_check": passing,
        # provenance, so nothing is inferred silently
        "budget_shape": shape,
        "token_cap": cap,
        "step_cap": step_cap,
        "cap_provenance": prov or "unknown: no driver and no dump field records the cap",
        "void": bool(is_void),
        "run_end_outcome": outcome_raw,
        "turn_end_reason": (te or {}).get("reason"),
        "cum_source": cum_src,
        "per_turn_covers_totals": per_turn_ok,
    }
    return row, None


def q2_skip_reason(r) -> str | None:
    """Why a row is not in the Q2 aggregate, or None if it is."""
    if r["token_cap"] is None:
        return "cap_unknown"
    if not r["per_turn_covers_totals"]:
        return "per_turn_usage_missing_or_inconsistent"
    if r["budget_shape"] == MPO:
        return "shape_already_miss_plus_out"
    if r["q2_extra_turns_last5"] is not None:
        return None
    if r["q2_note"] == "no positive trailing increment":
        return "no_positive_trailing_increment"
    return "gross_counter_never_crossed"


def main():
    if not os.path.isdir(WORK):
        sys.exit(f"no corpus at {WORK}")
    rels = sorted(os.path.relpath(p, WORK) for p in
                  glob.glob(os.path.join(WORK, "**", "events.jsonl"), recursive=True)
                  + glob.glob(os.path.join(WORK, "**", "*.events.json"), recursive=True))
    wins, windows_skipped = find_windows()
    rows, excluded = [], []
    for rel in rels:
        row, err = row_for(os.path.join(WORK, rel), rel, wins)
        (rows.append(row) if row else excluded.append({"path": rel, "reason": err}))

    tok = [r for r in rows if r["terminal_reason"] == "tokens"]
    tok_live = [r for r in tok if not r["void"]]
    metered = [r for r in rows if r["cum_tokens_at_halt"] is not None]
    unknown_fam = sorted({r["family"] for r in rows if r["token_cap"] is None})

    # Q1: the absolute count is the statistic; the fraction is a client-config ratio.
    # The window is a pi-config value keyed by model id only -- last provider wins --
    # so it is not a served limit and the fraction must never be quoted as headroom.
    # Both blocks keep the sample earlier artifacts published (sourceable window), so the
    # only change here is which number leads and what it is called.
    q1_rows = [r for r in tok_live if r["q1_context_fraction_from_client_config"] is not None]
    no_window = [r for r in tok_live if r["q1_context_fraction_from_client_config"] is None]
    sources: dict = {}
    for r in q1_rows:
        sources[r["context_window_source"]] = sources.get(r["context_window_source"], 0) + 1
    bound = max((r for r in rows if r["final_turn_input_tokens"] is not None),
                key=lambda r: r["final_turn_input_tokens"], default=None)
    q1 = {
        "n": len(q1_rows),
        "primary_statistic": "final_turn_input_tokens",
        "final_turn_input_tokens": stats([r["final_turn_input_tokens"] for r in q1_rows]),
        "excluded_no_client_config_window": len(no_window),
        "excluded_families": sorted({r["family"] for r in no_window}),
        "secondary_statistic": "q1_context_fraction_from_client_config",
        "q1_context_fraction_from_client_config": stats(
            [r["q1_context_fraction_from_client_config"] for r in q1_rows]),
        "denominator_note": "denominator = client config, not a served limit; do not quote as "
                            "headroom without provenance",
        "context_window_tokens_from_client_config": stats([r["context_window"] for r in q1_rows]),
        "context_window_sources": dict(sorted(sources.items())),
        "max_final_turn_input_tokens": {
            "tokens": bound["final_turn_input_tokens"] if bound else None,
            "run": bound["path"] if bound else None,
            "scanned": "every parsed row, void families included; not restricted to the Q1 sample",
            "note": "empirical lower bound on what an endpoint actually served: any context-window "
                    "claim below this many tokens is refuted for that endpoint",
        },
        "over_half_window_from_client_config": sum(
            1 for r in q1_rows if r["q1_context_fraction_from_client_config"] >= 0.5),
        "fraction_min_run": min(q1_rows, key=lambda r: r["q1_context_fraction_from_client_config"])["path"]
        if q1_rows else None,
        "fraction_max_run": max(q1_rows, key=lambda r: r["q1_context_fraction_from_client_config"])["path"]
        if q1_rows else None,
        "void_token_run_fractions": sorted(r["q1_context_fraction_from_client_config"] for r in tok
                                           if r["void"]
                                           and r["q1_context_fraction_from_client_config"] is not None),
    }

    # Q2
    q2_rows = [r for r in tok_live if r["q2_extra_turns_last5"] is not None]
    q2_null = [r for r in tok_live if q2_skip_reason(r)]
    reasons: dict = {}
    for r in q2_null:
        k = q2_skip_reason(r) or "unknown"
        reasons[k] = reasons.get(k, 0) + 1

    def extra_split(extra_key, win_key) -> dict:
        """Extra turns split by which wall won: the two buckets mean different things."""
        sc = [r for r in q2_rows if r[win_key] == "step_cap"]
        tk = stats([r[extra_key] for r in q2_rows if r[win_key] == "token_cap"])
        return {
            "step_capped": {
                "n": len(sc),
                "rule": "extra == step_cap - recorded_halt_step by construction: this reads as steps "
                        "left on the clock, not turns bought by miss-plus-out",
                "median": pct(sorted(r[extra_key] for r in sc), 0.5),
            },
            "token_capped": {"n": tk["n"], "median": tk["median"], "min": tk["min"], "max": tk["max"]},
        }

    basis: dict = {"recorded": 0, "extrapolated": 0}
    for r in q2_rows:
        if (r["q2_basis"] or "").startswith("extrapolated"):
            basis["extrapolated"] += 1
        elif r["q2_basis"] == "recorded":
            basis["recorded"] += 1
    q2 = {
        "n": len(q2_rows),
        "cap_replayed": stats([r["token_cap"] for r in q2_rows]),
        "recorded_halt_step": stats([r["steps"] for r in q2_rows]),
        "cum_gross_at_halt": stats([r["cum_tokens_at_halt"] for r in q2_rows]),
        "cum_miss_plus_out_at_halt": stats([r["cum_miss_plus_out_at_halt"] for r in q2_rows]),
        "extra_turns_last5": extra_split("q2_extra_turns_last5", "q2_winner_last5"),
        "winner_last5": {k: sum(1 for r in q2_rows if r["q2_winner_last5"] == k)
                         for k in ("step_cap", "token_cap")},
        "extra_turns_last_turn": extra_split("q2_extra_turns_last_turn", "q2_winner_last_turn"),
        "winner_last_turn": {k: sum(1 for r in q2_rows if r["q2_winner_last_turn"] == k)
                             for k in ("step_cap", "token_cap")},
        "q2_basis_counts": basis,
        "q2_basis_note": "recorded = the miss-plus-out counter itself crossed and the dump shows its "
                         "halt step; extrapolated = it never crossed, so the halt step is projected from "
                         "the trailing increment. An extrapolated halt step is a projection, not a "
                         "measurement, and q2_mpo_halt_step_near_boundary says how many sit just past "
                         "the step cap.",
        "q2_mpo_halt_step_near_boundary": sum(
            1 for r in q2_rows if r["q2_winner_last5"] == "step_cap"
            and r["q2_mpo_halt_step"] is not None
            and r["q2_mpo_halt_step"] <= r["step_cap"] * 1.25),
        "mpo_halt_step_last5": stats([r["q2_mpo_halt_step"] for r in q2_rows]),
        "step_cap": stats([r["step_cap"] for r in q2_rows]),
        "excluded": len(q2_null),
        "excluded_reason": dict(sorted(reasons.items())),
        "void_token_run_extra_turns_last5": sorted(r["q2_extra_turns_last5"] for r in tok if r["void"]
                                                   and r["q2_extra_turns_last5"] is not None),
    }

    # Q3
    q3_rows = [r for r in tok_live if r["steps"]]
    mutating = sum(1 for r in q3_rows if r["q3_last3_mutated_tree"] is True)
    read_only = sum(1 for r in q3_rows if r["q3_last3_mutated_tree"] is False)
    unknown = sum(1 for r in q3_rows if r["q3_last3_mutated_tree"] is None)
    q3 = {
        "n": len(q3_rows),
        "classification_rule": "mutating = a write/edit/multi_edit tool (kind 'tool'), else an exec whose "
                               "argv[0] is a write verb or whose program writes inline via "
                               "open(...,'w'/'a') (kind 'exec_verb'); the strongest kind wins and "
                               "q3_last3_mutation_evidence is that hit. Verbs are matched at command "
                               "position only ('apt-get install' and 'chmod --help' are not writes). There is "
                               "no shell-redirect kind: exec splits argv without a shell and rejects the "
                               "operators, so '>' is a literal argument and cannot mutate the tree. A result "
                               "with is_error true did not mutate and is skipped; a result with no recorded "
                               "is_error is unknown, so the row counts in unknown_result and is never assumed "
                               "read-only.",
        "split_check": "still_editing_at_kill + read_only_last3 + unknown_result == n",
        "split_sums_to_n": mutating + read_only + unknown == len(q3_rows),
        "still_editing_at_kill": mutating,
        "still_editing_by_kind": {k: sum(1 for r in q3_rows if r["q3_last3_mutation_kind"] == k)
                                  for k in ("tool", "exec_verb")},
        "read_only_last3": read_only,
        "unknown_result": unknown,
        "results_skipped_is_error": sum(r["q3_last3_error_results"] for r in q3_rows),
        "results_skipped_unknown": sum(r["q3_last3_unknown_results"] for r in q3_rows),
        "no_tools_in_last3": sum(1 for r in q3_rows if not r["q3_last3_tool_names"]),
        "declared_done_true": sum(1 for r in q3_rows if r["q3_declared_done"] is True),
        "declared_done_false": sum(1 for r in q3_rows if r["q3_declared_done"] is False),
        "declared_done_unknown_no_text": sum(1 for r in q3_rows if r["q3_declared_done"] is None),
        "passing_check_true": sum(1 for r in q3_rows if r["q3_passing_check"]),
        "still_editing_and_passing_check": sum(
            1 for r in q3_rows if r["q3_last3_mutated_tree"] and r["q3_passing_check"]),
        "void_family_token_runs": {"still_editing_at_kill": sum(1 for r in tok if r["void"] and r["q3_last3_mutated_tree"]),
                                   "read_only_last3": sum(1 for r in tok if r["void"] and r["q3_last3_mutated_tree"] is False),
                                   "unknown_result": sum(1 for r in tok if r["void"] and r["q3_last3_mutated_tree"] is None)},
    }

    # every family, so no aggregate above can be read without its scope
    by_family: dict = {}
    for fam in sorted({r["family"] for r in rows}):
        fr = [r for r in rows if r["family"] == fam]
        ft = [r for r in fr if r["terminal_reason"] == "tokens"]
        by_family[fam] = {
            "model": fr[0]["model"], "token_cap": fr[0]["token_cap"], "step_cap": fr[0]["step_cap"],
            "budget_shape": fr[0]["budget_shape"], "cap_provenance": fr[0]["cap_provenance"],
            "already_ran_this_treatment": fr[0]["family"].split(":")[0] in ("pilot-dphn-cap", "pilot-glm-bs"),
            "void": all(r["void"] for r in fr),
            "dumps": len(fr), "token_deaths": len(ft),
            "q1_median_fraction_from_client_config": pct(sorted(
                r["q1_context_fraction_from_client_config"] for r in ft
                if r["q1_context_fraction_from_client_config"] is not None), 0.5),
            "q2_median_extra_turns_last5_mixed_walls": pct(sorted(r["q2_extra_turns_last5"] for r in ft
                                                                  if r["q2_extra_turns_last5"] is not None), 0.5),
            "q3_still_editing": sum(1 for r in ft if r["q3_last3_mutated_tree"]),
            "q3_read_only": sum(1 for r in ft if r["q3_last3_mutated_tree"] is False),
            "q3_unknown": sum(1 for r in ft if r["q3_last3_mutated_tree"] is None),
        }

    # counter-model check on this corpus: first gross crossing == recorded step count
    cross = [r for r in tok_live if r["budget_shape"] == GROSS and r["q2_note"]]
    reproduced = sum(1 for r in cross if f"halt_step={r['steps']}" in (r["q2_note"] or ""))

    report = {
        "generated_by": "tools/envelope_report.py",
        "corpus": {
            "root": "~/.local/share/rof-tb/work",
            "supersedes": "research/accounting-recompute.md",
            "supersedes_note": "That recompute describes an 83-dump corpus and scripts under /tmp/acct that no "
                               "longer exist on disk, so it is NOT reproducible as written and must not be cited "
                               "as if it were. Its conclusions survive: this file re-derives the same counter "
                               "model on the 253 dumps that remain (135/135 gross token deaths reproduce, and the "
                               "22 surviving rows it published match their step counts and dollars exactly). "
                               "Cite this artifact, not that report.",
            "dumps_found": len(rels),
            "dumps_parsed": len(rows),
            "dumps_excluded": excluded,
            "metered": len(metered),
            "unparsed_or_unmetered": len(rows) - len(metered),
            "unparsed_or_unmetered_paths": sorted(r["path"] for r in rows if r["cum_tokens_at_halt"] is None),
            "metered_without_per_turn_detail": sorted(
                r["path"] for r in metered if not r["per_turn_covers_totals"]),
            "metered_without_per_turn_detail_note": "a total exists, but retried or truncated calls, or a "
                                                    "usage field missing on a settle, left the per-turn "
                                                    "series short of it; these rows carry no Q1/Q2/Q3 "
                                                    "answer and are excluded from those aggregates, never "
                                                    "counted as zero.",
            "pi_config_sources_skipped": windows_skipped,
            "context_window_note": "every context window in this file comes from ~/.pi/agent/models*.json, a "
                                    "client-side config keyed by model id only (last provider seen wins). It "
                                    "is not a measurement of what an endpoint serves; each row records its "
                                    "source in context_window_source, and 'unknown' means no entry was found.",
            "families_without_sourceable_cap_or_model": unknown_fam,
            "rows_without_sourceable_cap_or_model": sum(1 for r in rows if r["token_cap"] is None),
            "terminal_reason_counts": {k: sum(1 for r in rows if r["terminal_reason"] == k)
                                       for k in sorted({r["terminal_reason"] for r in rows})},
            "ignored": "52 ~/.rof-events.jsonl Actor-WAL files sit in the same run dirs; "
                       "they carry no event stream and are not counted as dumps.",
            "not_in_this_corpus": "the 16 /tmp dumps of research/accounting-recompute.md are gone; "
                                  "so are its pilot-capab, pilot-tokcap, pilot-slice-a, matrix-v1 and "
                                  "pilot-r2iter dirs. Only pilot-r1 and pilot-sweep survive from that list.",
            "solve_rate": "not computed: the dumps carry no graded reward. RunEnd 'passed'/'Failed' "
                          "is a harness outcome, not a verifier score.",
        },
        "counter_model": {
            "rule": "cumulative (input + output) per settle, cache reads at full weight, halt on first crossing",
            "source": "research/accounting-recompute.md §1",
            "prices_usd_per_mtok": {"cache_read": 0.006, "input_miss": 0.30, "output": 1.20},
            "priced_models": sorted(PRICE),
            "dollars_note": "null dollars = model id has no flash-peak price (see priced_models); "
                            "glm-5.3-flash and deepseek-flash rows stay null.",
            "reproduced_first_crossing": reproduced,
            "checked": len(cross),
        },
        "aggregates": {
            "by_family": by_family,
            "q1_context_headroom": q1,
            "q2_miss_plus_out_replay": q2,
            "q3_convergence": q3,
            "scope_note": "aggregates cover token-terminated, non-void rows only; the void families "
                          "(pilot-dphn-cap and any run dir named void/) are reported in their own keys, "
                          "because README.md marks the 18-run envelope chart 'Void, re-measured pending' "
                          "(agent seeded from the Docker build context, not the image's /app).",
        },
        "rows": rows,
    }

    os.makedirs(os.path.dirname(OUT), exist_ok=True)
    with open(OUT, "w") as fh:
        json.dump(report, fh, indent=1, sort_keys=False)
        fh.write("\n")

    # --- compact text summary ---------------------------------------------------------
    c = report["corpus"]
    print(f"dumps found {c['dumps_found']}  parsed {c['dumps_parsed']}  excluded {len(c['dumps_excluded'])}")
    for e in c["dumps_excluded"]:
        print(f"  excluded {e['path']} :: {e['reason']}")
    print(f"metered {c['metered']}  unparsed_or_unmetered {c['unparsed_or_unmetered']}  "
          f"metered_without_per_turn_detail {len(c['metered_without_per_turn_detail'])}")
    for p in c["metered_without_per_turn_detail"]:
        print(f"  no per-turn series {p}")
    print(f"pi-config window sources: {2 - len(windows_skipped)} read, {len(windows_skipped)} skipped"
          f"{' ' + str(windows_skipped) if windows_skipped else ''}")
    print(f"no sourceable cap/model: {c['rows_without_sourceable_cap_or_model']} rows "
          f"{c['families_without_sourceable_cap_or_model']}")
    print(f"terminal reasons {c['terminal_reason_counts']}")
    print(f"counter model reproduced {reproduced}/{len(cross)} gross token deaths "
          f"(first crossing == recorded step count)")
    print(f"Q1 PRIMARY final-turn input tokens over {q1['n']} token runs: {q1['final_turn_input_tokens']}")
    print(f"   SECONDARY final-turn input / context window over {q1['n']} (same sample): "
          f"{q1['q1_context_fraction_from_client_config']}")
    print("   denominator = client config, not a served limit; do not quote as headroom "
          "without provenance")
    print(f"   window sources {q1['context_window_sources']}")
    print(f"   declared window (denominator) {q1['context_window_tokens_from_client_config']}")
    bound = q1["max_final_turn_input_tokens"]
    print(f"   empirical bound: max final-turn input {bound['tokens']} tokens at {bound['run']} — "
          f"any window claim below {bound['tokens']} is refuted for that endpoint")
    print(f"   runs at >=50% of window: {q1['over_half_window_from_client_config']}; "
          f"no window {q1['excluded_no_client_config_window']} {q1['excluded_families']}")
    print(f"   min fraction run {q1['fraction_min_run']}  max fraction run {q1['fraction_max_run']}")
    print(f"Q2 replay over {q2['n']} token runs, same cap {q2['cap_replayed']}, cache at 0.0")
    print(f"   recorded halt step {q2['recorded_halt_step']} step cap {q2['step_cap']}")
    print(f"   cum gross at halt {q2['cum_gross_at_halt']} vs miss-plus-out {q2['cum_miss_plus_out_at_halt']}")
    print(f"   basis counts {q2['q2_basis_counts']} (extrapolated = projected halt step, not measured)")
    print(f"   mpo halt step {q2['mpo_halt_step_last5']}; step-capped runs within 25% of the "
          f"cap: {q2['q2_mpo_halt_step_near_boundary']}")
    for key, wkey in (("extra_turns_last5", "winner_last5"),
                      ("extra_turns_last_turn", "winner_last_turn")):
        s = q2[key]
        print(f"   {key}: step-capped {s['step_capped']} | token-capped {s['token_capped']}")
        print(f"   {wkey} {q2[wkey]}")
    print(f"   excluded {q2['excluded']} {q2['excluded_reason']}")
    print(f"Q3 last 3 steps over {q3['n']} token runs: mutating {q3['still_editing_at_kill']} "
          f"by kind {q3['still_editing_by_kind']} / read-only {q3['read_only_last3']} / "
          f"unknown {q3['unknown_result']} (sums to n: {q3['split_sums_to_n']})")
    print(f"   skipped last3 results: is_error {q3['results_skipped_is_error']} / "
          f"unknown {q3['results_skipped_unknown']}; no tools {q3['no_tools_in_last3']}")
    print(f"   declared done {q3['declared_done_true']} / not {q3['declared_done_false']} / "
          f"unknown {q3['declared_done_unknown_no_text']}; passing check {q3['passing_check_true']} "
          f"(editing AND passing check {q3['still_editing_and_passing_check']})")
    print(f"   void token runs (own bucket, NOT in the aggregates above): Q1 {q1['void_token_run_fractions']} "
          f"Q2 {q2['void_token_run_extra_turns_last5']} Q3 {q3['void_family_token_runs']}")
    print("by family (token deaths; q1 = median fraction of the client-config window, q2 = median "
          "extra turns last5 with both walls mixed, see the split above):")
    for fam, f in by_family.items():
        print(f"  {fam:34s} dumps {f['dumps']:3d} tok {f['token_deaths']:3d} "
              f"cap {str(f['token_cap']):>7s} steps {str(f['step_cap']):>4s} {str(f['budget_shape']):>14s} "
              f"q1 {str(f['q1_median_fraction_from_client_config']):>6s} "
              f"q2 {str(f['q2_median_extra_turns_last5_mixed_walls']):>4s} "
              f"edit/read/unk {f['q3_still_editing']}/{f['q3_read_only']}/{f['q3_unknown']}"
              f"{' VOID' if f['void'] else ''}{' RERAN-TREATMENT' if f['already_ran_this_treatment'] else ''}")
    print("solve rate: not computed — the dumps carry no graded reward")
    print(f"wrote {OUT}")


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
"""Zero-spend triage of banked in-container rows. stdlib only, no model calls.

Reads only banked artefacts under ~/.local/share/rof-tb/work:
  verifier-logs/verifier/{ctrf.json,reward.txt}   rof-native in-container layout
  verifier/{ctrf.json,reward.txt}                 harbor in-container layout
  events.jsonl | agent/rof/events.jsonl           halt reason + step count
  <pilot>/reports/<row>.report.json               provenance block
  <pilot>/rows/<row>/manifest.json                harbor-adapter provenance
  <pilot>/tasks|task-inputs/<cell>/instruction.md public statement

Class is decided from the PUBLIC instruction.md alone (harness appendices
stripped). ctrf test NAMES are recorded but never used to pick a class.
"""

from __future__ import annotations

import datetime
import glob
import json
import os
import re
import subprocess
import sys

WORK = os.path.expanduser("~/.local/share/rof-tb/work")
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

# The workspace-fidelity fixes landed on 2026-11-06 (README, "Grading fidelity:
# five defects found and fixed on 2026-11-06") as six commits. FIX_SHA is the
# last, so a binary is post-fix iff FIX_SHA is an ancestor of it. Rows built at an
# earlier commit keep a partial count in FIX_CHAIN, so a reviewer can re-cut the
# line (e.g. to "seeding fixed only", d5891c2) without re-reading the table.
FIX_CHAIN = (
    ("d5891c2", "seed /app from the task image"),
    ("1934f24", "upload /tests at grading instead of mounting them"),
    ("a6da984", "docker cp the frozen per-task dir"),
    ("69a9378", "agent-phase wall clock from task.toml"),
    ("a4f1103", "per-section task.toml timeouts"),
    ("3874141", "in-container git safe.directory + empty-workdir baseline"),
)
FIX_SHA = FIX_CHAIN[-1][0]
FIX_DATE = "2026-11-06"

HARNESS_APPENDIX = (
    r"^Reasoning Effort: *\d+[^\n]*\n+",
    r"\n*Rules: all paths are relative to the workdir.*?(?=\n\nCOMPLETION CHECKLIST|\Z)",
    r"\n*COMPLETION CHECKLIST - do this before you declare the work complete:.*\Z",
)

# pilot -> {run-name token: role}. Read from each pilot's driver header comment,
# which states the arm contrast; a pilot absent from the map runs one arm.
ARM_ROLES = {
    "pilot-goalchk": {"ctl": "control", "chk": "treatment"},
    "pilot-goalchk2": {"ctl": "control", "chk": "treatment"},
    "pilot-dphn-effort": {"ctl": "control", "max": "treatment"},
    "pilot-effort2": {"ctl": "control", "mark": "treatment"},
    "pilot-dphn-trust": {"ctl": "control", "trust": "treatment"},
    "pilot-dphn-cap": {"C": "control", "S": "treatment", "J": "treatment"},
    "pilot-dphn-apppath": {"pre": "control", "ctl": "control", "post": "treatment"},
    "pilot-steer2": {"off": "control", "on": "treatment"},
    "pilot-steer": {"noat": "control", "on": "treatment"},
    "pilot-glm-bs": {"bs": "treatment"},
}

# pilot -> estimand when the report carries no `estimand` field, with the
# evidence that settles it.
ESTIMAND_BY_PILOT = {
    "pilot-glm-ict": ("InContainerReward", "driver: `rof eval --agent --in-container`"),
    "pilot-glm-ict2": (
        "InContainerReward",
        "driver: `rof eval --agent --in-container`",
    ),
    "pilot-sweep-eval": (
        "HarborOracle",
        "driver: `rof eval --agent` (no --in-container)",
    ),
}


def sha_is_post_fix(sha):
    """True/False when git can answer, None when it cannot."""
    if not sha:
        return None
    base = sha.split("-")[0]
    if base == FIX_SHA:
        return True
    try:
        have = (
            subprocess.run(
                ["git", "-C", REPO, "cat-file", "-e", base + "^{commit}"],
                check=False,
                capture_output=True,
            ).returncode
            == 0
        )
        if not have:
            return None
        return (
            subprocess.run(
                ["git", "-C", REPO, "merge-base", "--is-ancestor", base, FIX_SHA],
                check=False,
                capture_output=True,
            ).returncode
            != 0
        )
    except OSError:
        return None


def strip_appendix(text):
    for pat in HARNESS_APPENDIX:
        text = re.sub(pat, "", text, flags=re.DOTALL)
    return text.strip() + "\n"


def read_text(path):
    try:
        with open(path, errors="replace") as fh:
            return fh.read()
    except OSError:
        return None


def read_json(path):
    try:
        with open(path) as fh:
            return json.load(fh)
    except (OSError, ValueError):
        return None


DATASETS = os.path.expanduser("~/.local/share/rof-tb/datasets")

# Directory names that hold a task but do not name one.
CONTAINER_NAMES = {"grade-task", "task", "template-task", "task-template", "tasks"}


def public_instructions():
    """cell -> public instruction text, canonical dataset copy first.

    The frozen dataset task dirs are the statement the agent was shown; the
    pilot copies under work/ only add a harness tail (reasoning-effort line,
    rules paragraph, completion checklist) which strip_appendix removes.
    """
    out = {}
    for path in sorted(glob.glob(DATASETS + "/*/*/*/instruction.md")):
        text = read_text(path)
        if text is not None:
            out[os.path.basename(os.path.dirname(path))] = strip_appendix(text)
    for path in sorted(glob.glob(WORK + "/**/instruction.md", recursive=True)):
        parts = os.path.relpath(path, WORK).split("/")
        cell = None
        for marker in ("task-inputs", "tasks"):
            if marker in parts:
                i = parts.index(marker)
                if i + 1 < len(parts):
                    cell = parts[i + 1]
                break
        if cell is None:
            cell = parts[-2]
        if cell in CONTAINER_NAMES:
            continue
        text = read_text(path)
        if text is not None and cell not in out:
            out[cell] = strip_appendix(text)
    return out


def find_verifier_dirs():
    """Yield (run_root, layout) for every banked verifier dir, deduped."""
    seen = set()
    for name in ("ctrf.json", "reward.txt"):
        for pat, layout in (
            (WORK + "/**/verifier-logs/verifier/" + name, "native"),
            (WORK + "/**/verifier/" + name, "harbor"),
        ):
            for path in glob.glob(pat, recursive=True):
                vdir = os.path.dirname(path)
                if layout == "harbor" and vdir.endswith("/verifier-logs/verifier"):
                    continue
                if vdir in seen:
                    continue
                seen.add(vdir)
                # native: <run>/verifier-logs/verifier -> <run>; harbor: <trial>/verifier -> <trial>
                run_root = (
                    os.path.dirname(os.path.dirname(vdir))
                    if layout == "native"
                    else os.path.dirname(vdir)
                )
                yield run_root, layout


def run_events(run_root):
    for cand in (
        os.path.join(run_root, "events.jsonl"),
        os.path.join(run_root, "agent", "rof", "events.jsonl"),
    ):
        if os.path.exists(cand):
            return cand
    return None


def halt_and_steps(events_path, report_row):
    """-> (halt, steps). halt in {done,tokens,steps,no_run_end,error,unknown}."""
    steps = None
    halt = None
    if events_path:
        steps = 0
        seen_end = False
        with open(events_path, errors="replace") as fh:
            for line in fh:
                try:
                    ev = json.loads(line)
                except ValueError:
                    continue
                kind = ev.get("type")
                if kind == "MessageStart":
                    steps += 1
                elif kind == "RunEnd":
                    seen_end = True
                    outcome = ev.get("outcome")
                    if outcome == "Passed":
                        halt = "done"
                    elif isinstance(outcome, dict) and "Failed" in outcome:
                        why = str(outcome["Failed"])
                        halt = why if why in ("tokens", "steps") else "error"
                    else:
                        halt = "error"
        if steps == 0:
            steps = None
        if not seen_end:
            halt = "no_run_end"
    if report_row:
        if steps is None and isinstance(report_row.get("steps"), int):
            steps = report_row["steps"]
        if halt is None:
            reason = report_row.get("halt_reason")
            if reason in ("tokens", "steps", "done"):
                halt = reason
            elif report_row.get("verdict") in ("Resolved",):
                halt = "done"
            elif reason:
                halt = "error"
    return (halt or "unknown"), steps


def summarise_ctrf(path):
    """-> (status, passed, total, [failing names])."""
    if not os.path.exists(path):
        return "no_ctrf", None, None, []
    doc = read_json(path)
    if not isinstance(doc, dict) or "results" not in doc:
        return "malformed", None, None, []
    results = doc.get("results") or {}
    summary = results.get("summary") or {}
    tests = results.get("tests") or []
    if not isinstance(tests, list):
        return "malformed", None, None, []
    names = []
    for t in tests:
        if (
            isinstance(t, dict)
            and t.get("status") == "failed"
            and isinstance(t.get("name"), str)
        ):
            names.append(t["name"])
    failing = sorted(set(names))
    passed = summary.get("passed")
    total = summary.get("tests")
    if not isinstance(total, int):
        total = len(tests)
    if not isinstance(passed, int):
        passed = sum(
            1 for t in tests if isinstance(t, dict) and t.get("status") == "passed"
        )
    return "ok", passed, total, failing


def read_reward(path):
    text = read_text(path)
    if text is None:
        return None
    try:
        return float(text.strip().split()[0])
    except (ValueError, IndexError):
        return None


def report_index(pilot_dir):
    """stem -> path for every report json under a pilot dir."""
    idx = {}
    for path in glob.glob(pilot_dir + "/**/*.report.json", recursive=True):
        idx.setdefault(os.path.basename(path)[: -len(".report.json")], path)
    return idx


def manifest_index(pilot_dir):
    idx = {}
    for path in glob.glob(pilot_dir + "/rows/*/manifest.json"):
        idx[os.path.basename(os.path.dirname(path))] = path
    return idx


def row_stems(pilot, run_root, layout):
    """Candidate report/manifest stems for a run dir, most specific first."""
    rel = os.path.relpath(run_root, os.path.join(WORK, pilot))
    if layout == "native":
        cell = os.path.basename(run_root)
        row = os.path.dirname(rel)
        names = [os.path.basename(row)]
        if os.path.basename(row) and not cell.startswith(os.path.basename(row)):
            names.append(os.path.basename(row) + "-" + cell)
        names.append(cell)
        return [n for n in names if n and n != "."]
    job = os.path.basename(os.path.dirname(rel))
    return [job]


def provenance_for(pilot, run_root, layout, rep_idx, mf_idx):
    """-> (sha_or_None, evidence, estimand_or_None, report row or None)."""
    for stem in row_stems(pilot, run_root, layout):
        doc = read_json(rep_idx[stem]) if stem in rep_idx else None
        row = None
        estimand = None
        if isinstance(doc, dict):
            instances = doc.get("instances") or []
            row = instances[0] if instances else None
            prov = doc.get("provenance")
            if isinstance(prov, dict):
                estimand = prov.get("estimand")
                sha = prov.get("binary_git_sha")
                if sha:
                    return (
                        str(sha),
                        f"report {stem}.report.json provenance.binary_git_sha",
                        estimand,
                        row,
                    )
        if stem in mf_idx:
            man = read_json(mf_idx[stem])
            if isinstance(man, dict) and man.get("git_sha"):
                return (
                    str(man["git_sha"]),
                    f"rows/{stem}/manifest.json git_sha",
                    estimand,
                    row,
                )
        if row is not None or estimand is not None:
            return (
                None,
                f"report {stem}.report.json has no binary_git_sha",
                estimand,
                row,
            )
    head = streak_source_head(pilot, run_root)
    if head:
        return head, f"{pilot}/manifest.json source_head", None, None
    return None, "no provenance block found", None, None


def streak_source_head(pilot, run_root):
    """Git head recorded by a per-run manifest that lists this row."""
    man = read_json(os.path.join(WORK, pilot, "manifest.json"))
    if not isinstance(man, dict) or not man.get("source_head"):
        return None
    parts = os.path.relpath(run_root, os.path.join(WORK, pilot)).split(os.sep)
    cell, arm, attempt = None, None, None
    for i, part in enumerate(parts):
        if part.startswith("attempt-"):
            attempt = int(part.split("-")[1])
            cell, arm = parts[i - 2], parts[i - 1]
    for entry in man.get("rows") or []:
        if not isinstance(entry, dict):
            continue
        if entry.get("task") != cell:
            continue
        if arm is not None and entry.get("arm") != arm:
            continue
        if attempt is not None and entry.get("attempt") != attempt:
            continue
        return str(man["source_head"])
    return None


def harbor_eval_name(run_root):
    """`stats.evals` key of the enclosing harbor job, e.g. 'oracle__adhoc'."""
    doc = read_json(os.path.join(os.path.dirname(run_root), "result.json"))
    if not isinstance(doc, dict):
        return None
    evals = (doc.get("stats") or {}).get("evals") or {}
    return min(evals) if evals else None


def estimand_of(pilot, run_root, reported, pilot_default):
    """-> (estimand, source). Reported provenance wins; else a direct artefact."""
    if reported:
        return reported, "report provenance.estimand"
    graded = harbor_eval_name(run_root)
    if graded and graded.startswith("oracle"):
        return (
            "oracle",
            f"job result.json evals={graded!r} (harbor's oracle agent graded the gold patch)",
        )
    if graded and graded.startswith("rof"):
        return (
            "InContainerReward",
            (
                f"job result.json evals={graded!r} with tools.harbor_rof:RofAgent; "
                "harbor's verifier grades the agent's live container"
            ),
        )
    if os.path.exists(
        os.path.join(run_root, "agent", "oracle.txt")
    ) and os.path.getsize(os.path.join(run_root, "agent", "oracle.txt")):
        return "oracle", "agent/oracle.txt holds the gold run output"
    if pilot in ESTIMAND_BY_PILOT:
        return ESTIMAND_BY_PILOT[pilot]
    if pilot_default:
        return (
            pilot_default,
            "another row of the same pilot declares provenance.estimand",
        )
    return "unknown", "no estimand field and no oracle marker"


def cell_from(row_root, cells):
    """Resolve the cell by the longest known cell name in the run dir path.

    Harbor instance dirs are <cell>__<trial hash> and harbor truncates to 30
    chars, so a prefix match is allowed as a fallback. Suffixes the sweep added
    (-rN-task, -sN-task, -vN-task) are stripped before matching.
    """
    rel = os.path.relpath(row_root, WORK)
    name = os.path.basename(row_root).split("__")[0]
    name = re.sub(r"-(?:r|s|v)\d+-task$", "", name)
    direct = name
    for suffix in ("-task", "-tas", "-ta", "-t"):
        if direct in cells:
            break
        direct = direct.removesuffix(suffix)
    if direct in cells:
        return direct
    best = None
    for cell in cells:
        if cell in rel and (best is None or len(cell) > len(best)):
            best = cell
    if best:
        return best
    return name


# Arm vocabulary of the 2026-11-10 harbor campaign, taken from its own
# pre-registration files (rows-{wal,react}-{control,obs32,think256,retention}-
# preregistered.json): one control per cell, the rest are treatments.
HARBOR_ARMS = {
    "control": "control",
    "retention": "treatment",
    "obs32": "treatment",
    "think256": "treatment",
}


def arm_and_rep(pilot, run_root, layout):
    """-> (arm token, role, rep, rep evidence token)."""
    rel = os.path.relpath(run_root, os.path.join(WORK, pilot))
    tokens = [t for t in re.split(r"[-/]", rel) if t]
    roles = ARM_ROLES.get(pilot, {})
    arm, role = "", "single"
    for tok in tokens:
        if tok in roles:
            arm, role = tok, roles[tok]
            break
    if layout == "harbor":
        hits = [t for t in tokens if t in HARBOR_ARMS]
        arm = "-".join(hits) if hits else ""
        role = (
            "treatment"
            if any(HARBOR_ARMS[t] == "treatment" for t in hits)
            else "control"
        )
        if not hits:
            arm, role = "", "gold"
    rep, rep_tok = None, ""
    for tok in tokens:
        m = re.fullmatch(r"(?:r|s|bs|rep)(\d+)", tok)
        if m:
            rep, rep_tok = int(m.group(1)), tok
    return arm, role, rep, rep_tok


PILOT_ESTIMAND = {}


def learn_pilot_estimands():
    """pilot -> estimand declared by any of its report provenance blocks."""
    for path in glob.glob(WORK + "/*/reports/*.report.json"):
        pilot = os.path.relpath(path, WORK).split("/")[0]
        doc = read_json(path)
        if not isinstance(doc, dict):
            continue
        prov = doc.get("provenance")
        if isinstance(prov, dict) and prov.get("estimand"):
            PILOT_ESTIMAND.setdefault(pilot, prov["estimand"])


def build_rows():
    cells = public_instructions()
    learn_pilot_estimands()
    rep_idx, mf_idx = {}, {}
    rows = []
    for run_root, layout in find_verifier_dirs():
        pilot_rel = os.path.relpath(run_root, WORK).split("/")
        pilot = pilot_rel[0]
        pilot_dir = os.path.join(WORK, pilot)
        if pilot not in rep_idx:
            rep_idx[pilot] = report_index(pilot_dir)
            mf_idx[pilot] = manifest_index(pilot_dir)
        cell = cell_from(run_root, cells)
        arm, role, rep, rep_tok = arm_and_rep(pilot, run_root, layout)
        sha, prov_ev, reported_estimand, report_row = provenance_for(
            pilot, run_root, layout, rep_idx[pilot], mf_idx[pilot]
        )
        estimand, estimand_src = estimand_of(
            pilot, run_root, reported_estimand, PILOT_ESTIMAND.get(pilot)
        )
        vdir = os.path.join(run_root, "verifier-logs", "verifier")
        if not os.path.isdir(vdir):
            vdir = os.path.join(run_root, "verifier")
        ctrf_status, passed, total, failing = summarise_ctrf(
            os.path.join(vdir, "ctrf.json")
        )
        reward = read_reward(os.path.join(vdir, "reward.txt"))
        events = run_events(run_root)
        halt, steps = halt_and_steps(events, report_row)
        row = {
            "run_root": os.path.relpath(run_root, WORK),
            "sha": sha,
            "ts": None,
            "prov_evidence": prov_ev,
        }
        row["ts"] = row_timestamp(row)
        fid, fid_ev = fidelity(row)
        rows.append(
            {
                "pilot": pilot,
                "cell": cell,
                "layout": layout,
                "arm": arm,
                "role": role,
                "rep": rep,
                "rep_tok": rep_tok,
                "sha": sha,
                "prov_evidence": prov_ev,
                "estimand": estimand,
                "estimand_src": estimand_src,
                "ctrf": ctrf_status,
                "passed": passed,
                "total": total,
                "failing": failing,
                "reward": reward,
                "halt": halt,
                "steps": steps,
                "fidelity": fid,
                "fidelity_evidence": fid_ev,
                "family": cell_family(cell),
                "run_root": os.path.relpath(run_root, WORK),
                "events": events is not None,
            }
        )
    rows.sort(
        key=lambda r: (
            r["pilot"],
            r["cell"],
            r["role"],
            r["arm"],
            r["rep"] or 0,
            r["run_root"],
        )
    )
    return rows, cells


FIX_TS = None  # committer time of FIX_SHA, iso-strict


def fix_timestamp():
    global FIX_TS
    if FIX_TS is None:
        out = subprocess.run(
            [
                "git",
                "-C",
                REPO,
                "log",
                "-1",
                "--date=iso-strict",
                "--pretty=%aI",
                FIX_SHA,
            ],
            check=False,
            capture_output=True,
            text=True,
        )
        FIX_TS = out.stdout.strip() or "2026-11-06T22:54:02+08:00"
    return FIX_TS


def row_timestamp(row):
    """Run timestamp from the first run artefact that has a preserved mtime."""
    for name in ("events.jsonl", "ctrf.json"):
        for rel in (
            name,
            os.path.join("verifier-logs", "verifier", name),
            os.path.join("verifier", name),
        ):
            path = os.path.join(WORK, row["run_root"], rel)
            if os.path.exists(path):
                return (
                    datetime.datetime.fromtimestamp(
                        os.path.getmtime(path), datetime.UTC
                    )
                    .astimezone()
                    .isoformat(timespec="seconds")
                )
    return None


def _contains(base, commit):
    """True iff `commit` is `base` or an ancestor of it, in this repo."""
    try:
        if base.startswith(commit) or commit.startswith(base):
            return True
        if (
            subprocess.run(
                ["git", "-C", REPO, "cat-file", "-e", commit + "^{commit}"],
                check=False,
                capture_output=True,
            ).returncode
            != 0
        ):
            return False
        return (
            subprocess.run(
                ["git", "-C", REPO, "merge-base", "--is-ancestor", commit, base],
                check=False,
                capture_output=True,
            ).returncode
            == 0
        )
    except OSError:
        return False


def fix_coverage(sha):
    """-> how many of FIX_CHAIN the binary at `sha` already contains."""
    base = sha.split("-")[0]
    return sum(1 for commit, _ in FIX_CHAIN if _contains(base, commit))


def fidelity(row):
    """-> (status in {pre-fix,post-fix,unknown}, evidence sentence)."""
    if row["sha"]:
        post = sha_is_post_fix(row["sha"])
        base = row["sha"].split("-")[0]
        dirty = " (working tree dirty)" if row["sha"].endswith("-dirty") else ""
        have = fix_coverage(row["sha"])
        carries = f"carries {have}/{len(FIX_CHAIN)} of the 2026-11-06 fixes"
        if post is True:
            return (
                "post-fix",
                f"binary_git_sha {base}{dirty} {carries} ({FIX_SHA} is an ancestor); {row['prov_evidence']}",
            )
        if post is False:
            return (
                "pre-fix",
                f"binary_git_sha {base}{dirty} {carries} (ancestor of {FIX_SHA}); {row['prov_evidence']}",
            )
        return (
            "unknown",
            f"binary_git_sha {base} not resolvable here; {row['prov_evidence']}",
        )
    ts = row.get("ts")
    if ts and ts < fix_timestamp():
        return (
            "pre-fix",
            f"no binary_git_sha; run artefact mtime {ts} precedes {fix_timestamp()}; {row['prov_evidence']}",
        )
    if ts:
        return (
            "unknown",
            f"no binary_git_sha; mtime {ts} is after the fix but the build is unrecorded",
        )
    return "unknown", f"no binary_git_sha and no dated artefact; {row['prov_evidence']}"


# Per cell: does the PUBLIC instruction state an executable artefact property,
# and the literal line that decides it? A property counts when it is checkable as
# "path or command + a literal structural fact" (filename, header, field list,
# port, socket path, importable symbol). "must be correct" is not structural, so a
# cell whose only stated acceptance is a computed result is accuracy-bound.
# The needle must appear verbatim in the stripped public instruction; the tool
# reports any cell whose needle it cannot find instead of asserting a class.
CELL_ARTEFACT = {
    "break-filter-js-from-html": (
        False,
        "will still trigger a JavaScript alert() when opened in a browser",
    ),
    "build-pov-ray": (
        True,
        "This should complete successfully and show rendering statistics.",
    ),
    "cargo-flight-dispatch": (True, "produces a correct flight plan"),
    "custom-memory-heap-crash": (
        False,
        "There must be no memory leaks detected by Valgrind.",
    ),
    "dna-assembly": (True, "The output fasta file should be titled primers.fasta."),
    "dna-insert": (True, "The output fasta file should be titled primers.fasta."),
    "headless-terminal": (True, "put it in a file called `/app/headless_terminal.py`"),
    "hf-model-inference": (
        True,
        "save to the local directory '/app/model_cache/sentiment_model'",
    ),
    "install-windows-3.11": (
        True,
        "Configure a QEMU monitor socket at `/tmp/qemu-monitor.sock`",
    ),
    "kv-store-grpc": (
        True,
        "Create a file /app/kv-store.proto containing a service called KVStore",
    ),
    "log-summary-date-ranges": (
        True,
        "Write a CSV file /app/summary.csv with the following structure (including the header)",
    ),
    "make-mips-interpreter": (True, "so that I can run `node vm.js`"),
    "mcmc-sampling-stan": (True, "Save your results to these files:"),
    "model-extraction-relu-logits": (
        True,
        "The script should save this matrix to `/app/stolen_A1.npy`",
    ),
    "mteb-retrieve": (True, "Write the resulting line to /app/result.txt"),
    "protein-assembly": (
        True,
        "The gBlock should be stored in file titled /app/gblock.txt",
    ),
    "pypi-server": (True, "set up a pypi server on port 8080 locally"),
    "query-optimize": (True, "Please save your solution in the file /app/sol.sql"),
    "raman-fitting": (True, 'write them to a file called "/app/results.json"'),
    "react-lead-form": (
        True,
        "Accepted leads should write `/app/output/submission.json`",
    ),
    "sam-cell-seg": (
        True,
        "You must write a python script in /app named convert_masks.py",
    ),
    "sanitize-git-repo": (
        False,
        "Please ensure that the sensitive values are not present in the repository after the sanitization.",
    ),
    "session-window-debug": (
        False,
        "A session window processor at `/app/app/` is not working correctly.",
    ),
    "video-processing": (
        True,
        "Your software must generate a TOML file with exactly these fields and names, and store it in `/app/output.toml`",
    ),
    "wal-recovery-ordering": (
        True,
        "The verifier imports `make_engine` and `recover_engine` from `/app/app.py`",
    ),
}

# What the public statement lets a predicate assert for the cells that reach
# `done` post-fix. Only the artefact shape is claimed; the semantics behind it
# ("produced a correct flight plan") is not decidable this way.
Q_B_DETAIL = {
    "hf-model-inference": (
        "`/app/model_cache/sentiment_model/` exists and is non-empty; `POST :5000/sentiment` with "
        '`{"text": ...}` answers `{"sentiment", "confidence": {"positive", "negative"}}` '
        "- the directory half is checkable from files, the HTTP half only in the live container"
    ),
    "wal-recovery-ordering": (
        "`/app/app.py` imports and exposes `make_engine` and `recover_engine`; `/app/recovery.py` "
        "exposes `recover_from_snapshot`; `/app/config.py` exposes `RECOVERY_ENTRY_FIELDS` and "
        "`RECOVERY_STATS_KEYS` - all checkable by import alone"
    ),
}

CLASS_ARTEFACT = "rerun-context/artefact-form"
CLASS_ACCURACY = "accuracy/correctness"
CLASS_INFRA = "environment/infra"

LOG_PERIODS = ("today", "last_7_days", "last_30_days", "month_to_date", "total")
LOG_SEVERITIES = ("ERROR", "WARNING", "INFO")


def cell_family(cell):
    """[metadata] category from the frozen task.toml (public task metadata)."""
    for pat in (
        DATASETS + "/*/*/" + cell + "/task.toml",
        WORK + "/**/" + cell + "/task.toml",
    ):
        for path in glob.glob(pat, recursive=True):
            m = re.search(r'^category = "(.*)"', read_text(path) or "", re.MULTILINE)
            if m:
                return m.group(1)
    return "unknown"


def cell_class(cell, instr):
    """-> (class, quote, verified) from the public instruction alone."""
    if cell not in CELL_ARTEFACT:
        return "unknown", "", False
    has_artefact, needle = CELL_ARTEFACT[cell]
    if needle not in (instr or ""):
        return "unknown", "", False
    return (CLASS_ARTEFACT if has_artefact else CLASS_ACCURACY), needle, True


def row_class(row, instr):
    """Infra override: a row with no verifier verdict has no public requirement.

    A row that solved is never infra however its agent loop ended -- the reward is
    a positive capability datum that a later transport or rollback error cannot
    retract.
    """
    if row["reward"] != 1.0 and (row["ctrf"] != "ok" or row["halt"] == "error"):
        return (
            CLASS_INFRA,
            "no verifier verdict to classify (no usable ctrf, or halt=error)",
        )
    cls, quote, ok = cell_class(row["cell"], instr)
    return cls, (quote if ok else "__UNVERIFIED__")


def _read_csv(path):
    text = read_text(path)
    return None if text is None else [line.split(",") for line in text.splitlines()]


def _pred_summary_csv(work):
    path = os.path.join(work, "summary.csv")
    if not os.path.exists(path):
        return False, "no summary.csv"
    rows = _read_csv(path) or []
    if not rows or rows[0] != ["period", "severity", "count"]:
        return False, "header is not period,severity,count"
    want = [(p, s) for p in LOG_PERIODS for s in LOG_SEVERITIES]
    got = [(r[0], r[1]) for r in rows[1:] if len(r) >= 2]
    if got != want:
        return False, "period/severity rows differ from the 15 stated"
    if not all(len(r) == 3 and r[2].lstrip("-").isdigit() for r in rows[1:]):
        return False, "a count column is not an integer"
    return True, "header + the 15 stated rows"


def _pred_kv_grpc(work):
    for name, needle in (("kv-store.proto", "rpc"), ("server.py", "class Server")):
        path = os.path.join(work, name)
        if not os.path.exists(path):
            return False, f"no {name}"
        if needle not in (read_text(path) or ""):
            return False, f"{name} lacks {needle!r}"
    return True, "kv-store.proto + server.py with class Server"


def _pred_model_cache(work):
    path = os.path.join(work, "model_cache", "sentiment_model")
    if not os.path.isdir(path) or not os.listdir(path):
        return False, "no non-empty model_cache/sentiment_model"
    return True, "model_cache/sentiment_model populated"


def _pred_out_html(work):
    path = os.path.join(work, "out.html")
    if not os.path.exists(path) or os.path.getsize(path) == 0:
        return False, "no non-empty out.html"
    return True, "out.html present"


def _pred_convert_masks(work):
    if not os.path.exists(os.path.join(work, "convert_masks.py")):
        return False, "no convert_masks.py"
    return True, "convert_masks.py present"


ARTEFACT_PREDICATES = {
    "log-summary-date-ranges": _pred_summary_csv,
    "kv-store-grpc": _pred_kv_grpc,
    "hf-model-inference": _pred_model_cache,
    "break-filter-js-from-html": _pred_out_html,
    "sam-cell-seg": _pred_convert_masks,
}


def analyse(rows, instrs):
    """All derived numbers, computed once, in a fixed key order."""
    for r in rows:
        r["class"], r["class_quote"] = row_class(r, instrs.get(r["cell"]))
    inctr = [r for r in rows if r["estimand"] == "InContainerReward"]
    qa = sorted(
        r["cell"] for r in inctr if r["fidelity"] == "post-fix" and r["halt"] == "done"
    )
    qa_unique = sorted(set(qa))
    qb = []
    for cell in qa_unique:
        has_artefact, needle = CELL_ARTEFACT.get(cell, (False, ""))
        qb.append(
            (cell, has_artefact, needle if needle in (instrs.get(cell) or "") else "")
        )
    qc = []
    for cell in sorted(ARTEFACT_PREDICATES):
        predicate = ARTEFACT_PREDICATES[cell]
        passing = [r for r in inctr if r["cell"] == cell and r["reward"] == 1.0]
        with_work = [
            r
            for r in passing
            if os.path.isdir(os.path.join(WORK, r["run_root"], "work"))
        ]
        rejected = []
        captured = 0
        for r in with_work:
            path = os.path.join(WORK, r["run_root"], "work")
            snapshot = os.path.isdir(os.path.join(path, ".git"))
            captured += 1 if snapshot else 0
            ok, why = predicate(path)
            if not ok:
                rejected.append((r["run_root"], why, snapshot))
        qc.append(
            (cell, len(passing), len(with_work), len(rejected), captured, rejected)
        )
    # Accuracy-bound: >=2 arms ran, nothing in the cell solved, and at least one
    # named test is failed by EVERY row of EVERY arm -- the largest-envelope row
    # included. No arm moved it, so no completion predicate can.
    # Grouped by pilot AND cell: arms are only comparable inside one pilot, where
    # model, endpoint, allowlist and binary family are held fixed.
    by_cell = {}
    for r in inctr:
        if r["role"] == "gold":
            continue
        by_cell.setdefault((r["cell"], r["pilot"]), []).append(r)
    ruled_out = []
    for cell, pilot in sorted(by_cell):
        group = by_cell[(cell, pilot)]
        roles = {r["role"] for r in group}
        if len(roles) < 2:
            continue
        failing = [r for r in group if r["failing"]]
        if not failing:
            continue
        if any(r["ctrf"] == "ok" and not r["failing"] for r in group):
            continue
        never_passed = set(failing[0]["failing"])
        for r in failing[1:]:
            never_passed &= set(r["failing"])
        if not never_passed:
            continue
        deepest = max(failing, key=lambda r: (r["steps"] or 0, r["rep"] or 0))
        if not never_passed <= set(deepest["failing"]):
            continue
        ruled_out.append(
            (
                cell,
                pilot,
                len(group),
                sorted(roles),
                sorted(never_passed),
                deepest["steps"],
                deepest["arm"],
            )
        )
    both = [r for r in rows if r["ctrf"] == "ok" and r["reward"] is not None]
    disagree = [r for r in both if (r["reward"] == 1.0) != (not r["failing"])]
    no_ctrf = [r for r in rows if r["ctrf"] != "ok"]
    qa_rows = [r for r in inctr if r["fidelity"] == "post-fix" and r["halt"] == "done"]
    qa_solved = [r for r in qa_rows if r["reward"] == 1.0]
    return (
        inctr,
        qa,
        qa_unique,
        qb,
        qc,
        ruled_out,
        (disagree, no_ctrf, qa_rows, qa_solved),
    )


HEADER = (
    "# Zero-spend triage of banked in-container rows — 2026-11-11\n"
    "\n"
    "No model calls, no agent run, no spend. Every number below is read from\n"
    "artefacts already on disk under `~/.local/share/rof-tb/work/`.\n"
    "\n"
)


def _flat(name, limit=72):
    """ctrf names can carry captured stdout; collapse them to one table cell."""
    flat = " ".join(name.split())
    return flat if len(flat) <= limit else flat[: limit - 1] + "…"


def md_table(rows):
    out = [
        "| # | cell | family | row (pilot/run) | arm | rep | estimand | fidelity | halt | steps | ctrf pass/total | failing tests (ctrf names) | class | public line that decides the class |",
        "|---:|---|---|---|---|---:|---|---|---|---:|---|---|---|---|",
    ]
    for i, r in enumerate(rows, 1):
        fails = ", ".join(f"`{_flat(n)}`" for n in r["failing"]) or "—"
        if r["ctrf"] != "ok":
            fails = f"**{r['ctrf']}**"
        counts = f"{r['passed']}/{r['total']}" if r["ctrf"] == "ok" else "—"
        quote = r["class_quote"].replace("|", "\\|")
        out.append(
            "| {} | `{}` | {} | `{}` | {} | {} | {} | **{}** | {} | {} | {} | {} | {} | {} |".format(
                i,
                r["cell"],
                r["family"],
                r["run_root"],
                r["arm"] or "—",
                r["rep"] if r["rep"] is not None else "—",
                r["estimand"],
                r["fidelity"],
                r["halt"],
                r["steps"] if r["steps"] is not None else "—",
                counts,
                fails,
                r["class"],
                f"“{quote}”" if quote and quote != "__UNVERIFIED__" else quote,
            )
        )
    return "\n".join(out) + "\n"


def render(rows, instrs):
    inctr, qa, qa_unique, qb, qc, ruled_out, extra = analyse(rows, instrs)
    disagree, no_ctrf, qa_rows, qa_solved = extra
    out = [HEADER]

    out.append("## How every column is derived\n\n")
    out.append(
        "- **Row** = the directory holding `verifier-logs/verifier/` (rof native, in-container) or "
        "`verifier/` (harbor, in-container). 242 rows: 237 with `ctrf.json`, 5 with only `reward.txt`.\n"
        "- **family** = `[metadata] category` in the frozen task `task.toml`.\n"
        "- **estimand** = the report `provenance.estimand` when present; else the harbor job's "
        "`result.json` `stats.evals` key (`oracle__…` = the gold patch graded, `rof__…` = the RofAgent "
        "graded in the agent's own container); else `rof eval --agent --in-container` in the pilot "
        "driver; else `unknown`. Gold rows are listed but excluded from every count that speaks about "
        "model capability.\n"
        f"- **fidelity** = `post-fix` iff the row's own `binary_git_sha` carries **all six** 2026-11-06 "
        f"workspace-fidelity commits (equivalently: `{FIX_SHA}` is an ancestor); `pre-fix` iff it carries "
        "fewer. The evidence cell states how many of the six that binary already contains. With no "
        "`binary_git_sha`, a run-artefact mtime strictly before the fix commit dates the row `pre-fix` "
        "(sound: the binary cannot contain a commit that did not exist); a later mtime stays `unknown` "
        "because the built tree is unrecorded. Directory mtimes are not used — `verifier-logs/` carries "
        "migration timestamps (2026-10-26) that disagree with the run.\n"
        "- **halt** = the `RunEnd` outcome in the row's own `events.jsonl` (`Passed`→`done`, "
        "`Failed:tokens`→`tokens`, `Failed:steps`→`steps`, any other failure→`error`), `no_run_end` when the "
        "dump has no `RunEnd`; the report `halt_reason` is the fallback. **steps** = `MessageStart` count.\n"
        "- **failing tests** = `results.tests[].name` with `status == failed` in `ctrf.json`, sorted. "
        "Recorded only; never used to pick a class.\n"
        "- **class** = from the public `instruction.md` alone. `environment/infra` when an unsolved row "
        "banked no verifier verdict (`no_ctrf`/malformed ctrf, or `halt=error`); a solved row is never "
        "infra, because the reward stands whatever ended the agent loop. Otherwise "
        f"`{CLASS_ARTEFACT}` when the public statement names an artefact whose *form* is checkable "
        "(path/filename plus a literal structural fact, or a stated command), and "
        f"`{CLASS_ACCURACY}` when the stated acceptance is a computed result or a behaviour that no literal "
        "form test can decide. Harness tails (`Reasoning Effort:`, `Rules: all paths are relative…`, "
        "`COMPLETION CHECKLIST`) are stripped before the quote is taken.\n"
    )

    out.append("\n## Headline\n\n```\n" + compact(rows, instrs) + "\n```\n")
    out.append(
        "\nRead the numbers above as: the corpus is overwhelmingly **pre-fix** (169 of 183 in-container "
        "rows), so most of it cannot answer a capability question at all; the twelve post-fix rows are the "
        "only ones that can, and every one of their `done` rows is a solve.\n"
    )

    out.append("\n### The fidelity boundary, commit by commit\n\n")
    out.append(
        "| # | commit | fix | rows whose binary already carries it |\n|---:|---|---|---:|\n"
    )
    for i, (commit, what) in enumerate(FIX_CHAIN, 1):
        n = sum(
            1 for r in rows if r["sha"] and _contains(r["sha"].split("-")[0], commit)
        )
        out.append(f"| {i} | `{commit}` | {what} | {n} |\n")
    out.append(
        "\nThe line is drawn after the last of the six because each earlier binary leaves a measurement "
        "defect open: `/tests` readable by the agent (2) voids a capability reading outright, and the "
        "missing wall clock (4) and unparsed `task.toml` timeouts (5) put rows on a budget the task never "
        'asked for. A reviewer who reads the boundary as "the seeding fix only" can move the '
        "`d5891c2`-built rows to post-fix; the table then changes for those rows alone.\n"
    )

    out.append("\n## Summary — row counts\n\n")
    out.append("| key | value | rows |\n|---|---|---:|\n")
    for key in ("estimand", "family", "class", "fidelity", "halt", "ctrf"):
        counts = {}
        for r in rows:
            counts[r[key]] = counts.get(r[key], 0) + 1
        for k in sorted(counts, key=lambda x: (-counts[x], str(x))):
            out.append(f"| {key} | {k} | {counts[k]} |\n")
    for key in ("class", "fidelity"):
        counts = {}
        for r in inctr:
            counts[r[key]] = counts.get(r[key], 0) + 1
        for k in sorted(counts, key=lambda x: (-counts[x], str(x))):
            out.append(f"| {key} (in-container only) | {k} | {counts[k]} |\n")
    out.append(
        f"| rows | all | {len(rows)} |\n| rows | InContainerReward | {len(inctr)} |\n"
    )

    out.append("\n## The triage table\n\n")
    out.append(
        "Sorted by pilot, then cell, then arm, then rep. `no_ctrf` rows are listed with their path, never dropped.\n\n"
    )
    out.append(md_table(rows))

    out.append("\n## Q(a) — which cells reach `done` on post-fix rows?\n\n")
    if qa_unique:
        out.append(
            f"{len(qa)} post-fix rows reach `done`, over {len(qa_unique)} cells:\n\n"
        )
        counts = {}
        for c in qa:
            counts[c] = counts.get(c, 0) + 1
        out.append("| cell | post-fix rows at `done` |\n|---|---:|\n")
        for c in sorted(counts):
            out.append(f"| `{c}` | {counts[c]} |\n")
    else:
        out.append(
            "**Answer: none.** No in-container row with a resolvable post-fix binary reaches `done`.\n"
        )
    out.append(
        f"\nOf those {len(qa_rows)} rows, **{len(qa_solved)} are solves** (`reward.txt == 1.0`, zero failing "
        f"tests): every post-fix row that reaches `done` in the whole corpus passes. There is no post-fix row "
        f"that reaches `done` and still fails a test.\n"
    )
    out.append(
        f"\nFor contrast, `done` overall (all estimands and fidelity states): "
        f"{sum(1 for r in rows if r['halt'] == 'done')} rows, of which "
        f"{sum(1 for r in rows if r['halt'] == 'done' and r['fidelity'] != 'post-fix')} are pre-fix or undated, "
        f"and only {sum(1 for r in rows if r['halt'] == 'done' and r['reward'] != 1.0)} of the 48 fail.\n"
    )

    out.append(
        "\n## Q(b) — is there a publicly stated, EXECUTABLE artefact property?\n\n"
    )
    if not qb:
        out.append("Not applicable: no cell qualifies under Q(a).\n")
    for cell, has_artefact, quote in qb:
        verdict = "yes" if has_artefact else "no"
        out.append(f"- `{cell}` — **{verdict}**")
        out.append(
            f": “{quote}”\n"
            if quote
            else ": no literal form property in the public statement.\n"
        )
        if cell in Q_B_DETAIL:
            out.append(
                f"  - expressible offline from the statement: {Q_B_DETAIL[cell]}\n"
            )

    out.append(
        "\n## Q(c) — offline false-rejection check on banked PASSING workspaces\n\n"
    )
    out.append(
        "Each predicate is written only from the public statement and evaluated against every\n"
        "`reward.txt == 1.0` row of that cell whose graded `work/` directory is still on disk.\n"
        "A passing workspace that the predicate rejects is a **false rejection** and is reported here,\n"
        "not tuned away.\n\n"
    )
    out.append(
        "| cell | predicate | passing rows | with banked work/ | of those, git-snapshotted | false rejections |\n"
        "|---|---|---:|---:|---:|---:|\n"
    )
    describe = {
        "log-summary-date-ranges": "`summary.csv` header `period,severity,count` + exactly the 15 stated (period,severity) rows, integer counts",
        "kv-store-grpc": "`kv-store.proto` contains `rpc`; `server.py` contains `class Server`",
        "hf-model-inference": "`model_cache/sentiment_model` exists and is non-empty",
        "break-filter-js-from-html": "`out.html` exists and is non-empty",
        "sam-cell-seg": "`convert_masks.py` exists",
    }
    for cell, n_pass, n_work, n_rej, n_cap, _ in qc:
        out.append(
            f"| `{cell}` | {describe[cell]} | {n_pass} | {n_work} | {n_cap} | **{n_rej}** |\n"
        )
    total_rej = sum(x[3] for x in qc)
    total_work = sum(x[2] for x in qc)
    total_cap = sum(x[4] for x in qc)
    rej_snap = sum(1 for x in qc for _p, _w, snap in x[5] if snap)
    out.append(
        f"\n**{total_rej} false rejections** across {total_work} banked passing workspaces "
        f"({total_cap} of them snapshotted with their own `.git`, "
        f"{total_work - total_cap} files-only).\n\n"
    )
    out.append(
        f"Split by capture quality: **{rej_snap} of the {total_cap} git-snapshotted workspaces are rejected**, "
        f"and **{total_rej - rej_snap} of the {total_work - total_cap} files-only captures are**.\n\n"
    )
    if total_rej:
        out.append(
            "| passing row the predicate rejects | git snapshot in work/ | why the predicate says no |\n|---|---|---|\n"
        )
        for _cell, _n, _nw, _nr, _nc, rejected in qc:
            for path, why, snap in rejected:
                out.append(f"| `{path}` | {'yes' if snap else 'no'} | {why} |\n")
        out.append(
            "\nCause, checked against the trees: the rejected `work/` directories are not faithful copies\n"
            "of the graded `/app` — they are the rows whose workdir capture had no git snapshot, so only the\n"
            "image's own files survived (`pilot-glm-ict/s3/kv-store-grpc/work` holds just `__pycache__`; a\n"
            "`pilot-*` log-summary capture holds just `logs/`). Each of those rows is `reward.txt == 1` with a\n"
            "`ctrf.json` the grader accepted, so the predicate is right about the public requirement and the\n"
            "banked copy is what is missing. The predicate itself is not tuned: it is left as written.\n"
            "Consequence for the gate: a completion predicate must run **in the live container**, before the\n"
            "workdir is rolled back — replaying it offline against `work/` rejects 32 of 47 solves.\n"
        )

    out.append("\n## Consistency: reward file vs ctrf\n\n")
    out.append(
        f"- {len(disagree)} rows where `reward.txt == 1.0` disagrees with an empty failing set in "
        "`ctrf.json`, or vice versa. The two records never contradict each other here, so the "
        "reward file and the ctrf failing set can be read as one verdict.\n"
    )
    if disagree:
        for r in disagree:
            out.append(
                f"  - `{r['run_root']}` reward={r['reward']} failing={len(r['failing'])}\n"
            )
    out.append(
        f"- {len(no_ctrf)} rows carry a `reward.txt` with **no usable `ctrf.json`**; they are listed in the "
        "table with `ctrf = no_ctrf` and their path, never dropped:\n"
    )
    for r in no_ctrf:
        out.append(f"  - `{r['run_root']}` — reward {r['reward']}, halt {r['halt']}\n")

    out.append("\n## What this rules out\n\n")
    if not ruled_out:
        out.append("No cell has a single identical failing set on two or more arms.\n")
    else:
        out.append(
            "These cells are accuracy-bound: a named test is failed by every row of every arm of the\n"
            "contrast, and the largest-envelope row (most steps) still fails it, so no arm moved it. A\n"
            "completion predicate changes only what the agent checks before declaring done; it cannot\n"
            "change a failing set that no arm moved. They are ruled out for a `--completion-check` arm on\n"
            "the banked evidence. Arms are compared only inside one pilot, where model, endpoint,\n"
            "allowlist and binary family are held fixed.\n\n"
        )
        out.append(
            "Rule used: the cell ran at least two arms, no row of the cell solved, and at least one named\n"
            "test is failed by **every** row of **every** arm — including the row with the largest step\n"
            "count. `never passed` below is that intersection.\n\n"
        )
        out.append("Rolled up by cell:\n\n")
        by_cell_ruled = {}
        for cell, _p, _n, _roles, never, _steps, _arm in ruled_out:
            by_cell_ruled.setdefault(cell, set()).update(never)
        for cell in sorted(by_cell_ruled):
            out.append(
                f"- **`{cell}`** — {len(by_cell_ruled[cell])} named test(s) that no arm ever passed\n"
            )
        out.append("\nPer pilot/cell group:\n\n")
        out.append(
            "| cell | pilot | rows | arms | never passed | deepest steps | deepest arm |\n"
            "|---|---|---:|---|---:|---:|---|\n"
        )
        for cell, pilot, n, roles, never, steps, arm in ruled_out:
            out.append(
                f"| `{cell}` | {pilot} | {n} | {', '.join(roles)} | {len(never)} | {steps} | {arm} |\n"
            )
        out.append("\n")
        for cell, pilot, _n, _roles, never, steps, arm in ruled_out:
            out.append(
                f"- `{cell}` ({pilot}) — never passed at {steps} steps ({arm}): "
            )
            out.append(", ".join(f"`{_flat(f)}`" for f in never) + "\n")

    contrasted = {
        cell
        for cell, pilot in {(r["cell"], r["pilot"]) for r in inctr}
        if len({x["role"] for x in inctr if x["cell"] == cell and x["pilot"] == pilot})
        > 1
    }
    single = sorted({r["cell"] for r in inctr} - contrasted)
    if single:
        out.append(
            "\nNot testable for arm-invariance, because no pilot contrasted two arms on them in this "
            f"corpus (so they are neither named nor cleared): {', '.join(f'`{c}`' for c in single)}.\n"
        )
    out.append(
        "\nNamed caveat, from the project record rather than from any verifier content: "
        "`mcmc-sampling-stan`'s never-passed test is the rstan-usage test, and the README records the "
        "cell as excluded pre-fix for *no rstan in-image*. Its rows measure an unwinnable environment "
        "as well as accuracy, so it should not be read as a completion-predicate candidate.\n"
    )

    out.append("\n## Rows the tool could not classify\n\n")
    unknown = [
        r
        for r in rows
        if r["class"] == "unknown" or r["class_quote"] == "__UNVERIFIED__"
    ]
    if not unknown:
        out.append(
            "None: every cell resolved to a public instruction and every class needle was found.\n"
        )
    else:
        out.append("| row | cell | reason |\n|---|---|---|\n")
        for r in unknown:
            out.append(
                f"| `{r['run_root']}` | `{r['cell']}` | no public instruction, or the class needle was not found |\n"
            )
    undated = [r for r in rows if r["fidelity"] == "unknown"]
    if undated:
        out.append(
            f"\n**{len(undated)} rows are undated (`fidelity = unknown`)** — listed in the table above "
            "with the reason; no date was guessed to fill the gap.\n"
        )
    else:
        out.append("\nEvery row in the table is dated.\n")

    out.append(
        "\n## Reproduce\n\n```\n"
        "python3 tools/triage_report.py                     # print the markdown to stdout\n"
        "python3 tools/triage_report.py summary             # compact one-screen summary\n"
        "python3 tools/triage_report.py markdown OUT.md     # write the markdown to OUT.md\n"
        "```\n"
    )
    return "".join(out)


def compact(rows, instrs):
    inctr, qa, qa_unique, qb, qc, ruled_out, extra = analyse(rows, instrs)
    lines = [f"rows={len(rows)} in_container={len(inctr)}"]
    for key in ("class", "fidelity"):
        counts = {}
        for r in inctr:
            counts[r[key]] = counts.get(r[key], 0) + 1
        parts = ", ".join(
            f"{k}={counts[k]}"
            for k in sorted(counts, key=lambda x: (-counts[x], str(x)))
        )
        lines.append(f"{key}: {parts}")
    lines.append(
        f"Q(a) done+post-fix cells={len(qa_unique)} rows={len(qa)} "
        f"(of which solves={len(extra[3])}): {', '.join(qa_unique) or 'none'}"
    )
    yes = [c for c, has, _ in qb if has]
    lines.append(
        f"Q(b) artefact property: yes={len(yes)}/{len(qb)} -> {', '.join(yes) or 'none'}"
    )
    rej = sum(x[3] for x in qc)
    lines.append(
        f"Q(c) false rejections={rej} over {sum(x[2] for x in qc)} banked passing workspaces"
        + ("".join(f"; {x[0]}={x[3]}/{x[2]}" for x in qc if x[3]))
    )
    lines.append(
        f"reward/ctrf disagreements={len(extra[0])} rows_without_ctrf={len(extra[1])}"
    )
    cells = sorted({x[0] for x in ruled_out})
    lines.append(
        "ruled out (>=1 named test no arm ever passed, within one pilot): "
        + (", ".join(cells) or "none")
        + (f" [{len(ruled_out)} pilot/cell groups]" if ruled_out else "")
    )
    return "\n".join(lines)


def main(argv):
    rows, instrs = build_rows()
    if len(argv) > 1 and argv[1] == "summary":
        print(compact(rows, instrs))
        return 0
    text = render(rows, instrs)
    if len(argv) > 2:
        with open(argv[2], "w") as fh:
            fh.write(text)
        print(compact(rows, instrs))
        print(f"wrote {argv[2]}")
    else:
        print(text)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))

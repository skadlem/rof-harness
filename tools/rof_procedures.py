"""Prepare or explicitly run offline, contrastive procedure distillation.

The corpus shape is ``{"pairs":[{"task_id":"...","success":{"repo_id":"...","path":"..."},"failure":{...}}]}``.
Each pair must describe the same task in the same repository. Trace excerpts
are capped at 2,500 bytes; provenance hashes cover each complete bounded trace.
The script caps corpora at 256 KiB, source traces at 1 MiB each, pairs at six,
prompts at 36,000 characters, proposals at 32, and retained event streams at 2 MiB.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time

MAX_CORPUS_BYTES = 256 * 1024
MAX_TRACE_BYTES = 1024 * 1024
MAX_TRACE_SNIPPET_BYTES = 2500
MAX_PAIRS = 6
MAX_PROPOSALS = 32
MAX_PROMPT_CHARS = 36_000
PROTECTED_PART = re.compile(r"(?i)(^|[-_.])(grader|graders|reward|rewards|ctrf|test_outputs|verifier|test-stdout)([-_.]|$)")
SECRET_PATTERNS = (
    re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----"),
    re.compile(r"(?i)\b(?:sk-[A-Za-z0-9_-]{16,}|AKIA[A-Z0-9]{16}|gh[pousr]_[A-Za-z0-9]{20,})\b"),
    re.compile(r"(?i)\b(?:api[_-]?key|password|secret|access[_-]?token)\s*[:=]\s*['\"]?[^\s'\"]{8,}"),
)
GRADER_MARKERS = re.compile(r"(?i)\b(?:grader|graders|hidden tests?|reward|rewards|ctrf|test_outputs)\b")


def _protected(path):
    if any(PROTECTED_PART.search(part) for part in Path(path).parts):
        raise ValueError(f"protected path refused: {path}")


def _no_secret(text, label):
    if any(pattern.search(text) for pattern in SECRET_PATTERNS):
        raise ValueError(f"secret-like content refused in {label}")


def _read_json(path, cap):
    path = Path(path).expanduser().resolve()
    _protected(path)
    if not path.is_file() or path.stat().st_size > cap:
        raise ValueError(f"{path} exceeds {cap} bytes")
    with path.open("rb") as source:
        data = source.read(cap + 1)
    if len(data) > cap:
        raise ValueError(f"{path} exceeds {cap} bytes")
    try:
        return path, data, json.loads(data)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ValueError(f"invalid JSON in {path}: {exc}") from exc


def _file_digest(path):
    digest = hashlib.sha256()
    size = 0
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(64 * 1024), b""):
            size += len(chunk)
            digest.update(chunk)
    return {"bytes": size, "sha256": digest.hexdigest()}


def build_prompt(corpus_path):
    corpus_path, corpus_bytes, corpus = _read_json(corpus_path, MAX_CORPUS_BYTES)
    if not isinstance(corpus, dict) or set(corpus) != {"pairs"} or not isinstance(corpus["pairs"], list):
        raise ValueError("corpus must contain only a pairs array")
    pairs = corpus["pairs"]
    if not 1 <= len(pairs) <= MAX_PAIRS:
        raise ValueError(f"corpus needs 1..{MAX_PAIRS} paired traces")

    traces = []
    examples = []
    repo_ids = set()
    for number, pair in enumerate(pairs, 1):
        if not isinstance(pair, dict) or set(pair) != {"task_id", "success", "failure"}:
            raise ValueError(f"pair {number} needs task_id, success, and failure traces")
        task_id = pair["task_id"]
        if not isinstance(task_id, str) or not task_id.strip() or len(task_id) > 128:
            raise ValueError(f"pair {number} has invalid task_id")
        example = []
        for outcome in ("success", "failure"):
            item = pair[outcome]
            if not isinstance(item, dict) or set(item) != {"repo_id", "path"}:
                raise ValueError(f"pair {number} {outcome} needs repo_id and path")
            repo_id = item["repo_id"]
            if not isinstance(repo_id, str) or not repo_id.strip() or len(repo_id) > 128:
                raise ValueError(f"pair {number} has invalid {outcome} repo_id")
            if not isinstance(item["path"], str) or not item["path"].strip():
                raise ValueError(f"pair {number} has invalid {outcome} path")
            trace_path = (corpus_path.parent / item["path"]).resolve()
            _protected(trace_path)
            if not trace_path.is_file() or trace_path.stat().st_size > MAX_TRACE_BYTES:
                raise ValueError(f"trace exceeds {MAX_TRACE_BYTES} bytes: {trace_path}")
            with trace_path.open("rb") as source:
                raw = source.read(MAX_TRACE_BYTES + 1)
            if len(raw) > MAX_TRACE_BYTES:
                raise ValueError(f"trace exceeds {MAX_TRACE_BYTES} bytes: {trace_path}")
            try:
                text = raw.decode("utf-8")
            except UnicodeDecodeError as exc:
                raise ValueError(f"trace is not UTF-8: {trace_path}") from exc
            _no_secret(text, str(trace_path))
            if GRADER_MARKERS.search(text):
                raise ValueError(f"grader-like content refused in {trace_path}")
            snippet = raw[:MAX_TRACE_SNIPPET_BYTES].decode("utf-8", errors="ignore")
            if len(raw) > MAX_TRACE_SNIPPET_BYTES:
                snippet += "\n[trace excerpt truncated; source hash covers complete trace]"
            repo_ids.add(repo_id)
            record = {"task_id": task_id, "repo_id": repo_id, "path": str(trace_path), "sha256": hashlib.sha256(raw).hexdigest()}
            traces.append(record)
            example.append(f"{outcome.upper()} trace excerpt (task {task_id}; repository {repo_id}; source {trace_path}):\n{snippet.strip()}")
        if pair["success"]["repo_id"] != pair["failure"]["repo_id"]:
            raise ValueError(f"pair {number} success and failure must share a repository ID")
        examples.append(f"PAIR {number}\n" + "\n\n".join(example))

    prompt = (
        "Distill only reusable development procedures supported by contrast within these matched success and failure traces. "
        "Do not claim measured efficacy. Return a JSON array with at most 32 objects. Each object must have exactly these fields: "
        "id, cue, instruction, source_repositories. source_repositories must contain only repository IDs shown below. "
        "Keep each cue specific and each instruction actionable and concise. Exclude secrets, benchmark answers, and task-specific patches. "
        "Every proposal is UNVALIDATED and requires cross-repository held-out evaluation before promotion. "
        "Use the write tool to create a file named procedures.json in your working directory containing the JSON array; then finish. "
        "Do not rely on final-message JSON because the runner collects the file.\n\n"
        + "\n\n".join(examples)
    )
    if len(prompt) > MAX_PROMPT_CHARS:
        raise ValueError(f"distillation prompt exceeds {MAX_PROMPT_CHARS} characters")
    return {
        "prompt": prompt,
        "provenance": {
            "corpus_path": str(corpus_path),
            "corpus_sha256": hashlib.sha256(corpus_bytes).hexdigest(),
            "traces": traces,
            "source_repositories": sorted(repo_ids),
            "validation": "unvalidated",
            "limits": {
                "corpus_bytes": MAX_CORPUS_BYTES,
                "source_trace_bytes": MAX_TRACE_BYTES,
                "trace_excerpt_bytes": MAX_TRACE_SNIPPET_BYTES,
                "pairs": MAX_PAIRS,
                "prompt_characters": MAX_PROMPT_CHARS,
                "proposals": MAX_PROPOSALS,
                "events_bytes": 2 * 1024 * 1024,
            },
        },
    }


def _validate_proposals(path, repo_ids):
    try:
        proposals = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ValueError(f"rof did not produce valid procedures.json: {exc}") from exc
    if not isinstance(proposals, list) or not 1 <= len(proposals) <= MAX_PROPOSALS:
        raise ValueError(f"procedures.json must contain 1..{MAX_PROPOSALS} proposals")
    seen = set()
    for index, proposal in enumerate(proposals, 1):
        keys = {"id", "cue", "instruction", "source_repositories"}
        if not isinstance(proposal, dict) or set(proposal) != keys:
            raise ValueError(f"proposal {index} must have id, cue, instruction, source_repositories only")
        for key, cap in (("id", 80), ("cue", 512), ("instruction", 2000)):
            value = proposal[key]
            if not isinstance(value, str) or not value.strip() or len(value) > cap:
                raise ValueError(f"proposal {index} has invalid {key}")
            _no_secret(value, f"proposal {index} {key}")
        if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", proposal["id"]):
            raise ValueError(f"proposal {index} id must be a simple filename-safe identifier")
        if proposal["id"] in seen:
            raise ValueError(f"duplicate proposal id: {proposal['id']}")
        seen.add(proposal["id"])
        sources = proposal["source_repositories"]
        if not isinstance(sources, list) or not sources or any(not isinstance(repo, str) or repo not in repo_ids for repo in sources):
            raise ValueError(f"proposal {index} cites an unknown source repository")
    return proposals


def execute(corpus_path, output_dir, *, binary="rof", model=None, endpoint=None, budget_tokens=None):
    prepared = build_prompt(corpus_path)
    if not model or not model.strip():
        raise ValueError("--model is required for --execute")
    if not isinstance(budget_tokens, int) or not 1 <= budget_tokens <= 1_000_000:
        raise ValueError("--budget-tokens must be in 1..1000000")
    endpoint = endpoint or os.environ.get("OPENAI_BASE_URL")
    if not endpoint or not endpoint.strip():
        raise ValueError("--endpoint or OPENAI_BASE_URL is required for --execute")
    output_dir = Path(output_dir).expanduser().resolve()
    if output_dir.exists():
        raise ValueError(f"output directory must not exist: {output_dir}")

    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="rof-procedure-distill-") as temp:
        work = Path(temp) / "work"
        work.mkdir()
        events_path = Path(temp) / "events.jsonl"
        command = [str(binary), "run", "--goal", prepared["prompt"], "--workdir", str(work),
                   "--model", model, "--endpoint", endpoint, "--budget-tokens", str(budget_tokens),
                   "--dump-events", str(events_path), "--no-log"]
        stdout_path, stderr_path = Path(temp) / "stdout", Path(temp) / "stderr"
        with stdout_path.open("wb") as stdout, stderr_path.open("wb") as stderr:
            result = subprocess.run(command, stdout=stdout, stderr=stderr, check=False)
        stdout_metadata = _file_digest(stdout_path)
        stderr_metadata = _file_digest(stderr_path)
        if result.returncode:
            raise RuntimeError(f"rof exited {result.returncode}; stderr sha256={stderr_metadata['sha256']}")
        proposals = _validate_proposals(work / "procedures.json", set(prepared["provenance"]["source_repositories"]))
        event_bytes = events_path.read_bytes() if events_path.exists() else b""
        if len(event_bytes) > 2 * 1024 * 1024:
            raise ValueError("rof event stream exceeds 2 MiB")
        usage_totals = {"input_tokens": 0, "output_tokens": 0, "cache_read_tokens": 0}
        try:
            events = [json.loads(line) for line in event_bytes.splitlines() if line.strip()]
        except json.JSONDecodeError as exc:
            raise ValueError(f"rof emitted invalid event JSON: {exc}") from exc
        for event in events:
            usage = event.get("usage") if event.get("type") == "MessageEnd" else None
            if isinstance(usage, dict):
                for key in usage_totals:
                    usage_totals[key] += int(usage.get(key, 0) or 0)
        run_metadata = {
            "model": model,
            "budget_tokens": budget_tokens,
            "endpoint_sha256": hashlib.sha256(endpoint.encode()).hexdigest(),
            "returncode": result.returncode,
            "elapsed_seconds": round(time.monotonic() - started, 3),
            "stdout_bytes": stdout_metadata["bytes"],
            "stdout_sha256": stdout_metadata["sha256"],
            "stderr_bytes": stderr_metadata["bytes"],
            "stderr_sha256": stderr_metadata["sha256"],
            "events_sha256": hashlib.sha256(event_bytes).hexdigest(),
            "usage_totals": usage_totals,
        }

    output_dir.mkdir(parents=True)
    for index, proposal in enumerate(proposals, 1):
        (output_dir / f"procedure-{index:03}.json").write_text(json.dumps(proposal, indent=2) + "\n", encoding="utf-8")
    manifest = dict(prepared["provenance"], validation="unvalidated", proposal_count=len(proposals), cross_repository_held_out_required=True)
    manifest["run"] = run_metadata
    (output_dir / "provenance.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    (output_dir / "events.jsonl").write_bytes(event_bytes)
    return {"status": "executed", "output_dir": str(output_dir), "proposal_count": len(proposals), "validation": "unvalidated"}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--corpus", required=True, help="JSON paired-trace corpus")
    parser.add_argument("--execute", action="store_true", help="explicitly launch rof within the supplied token budget")
    parser.add_argument("--output-dir", help="new directory for procedure JSON files and provenance")
    parser.add_argument("--binary", default="rof")
    parser.add_argument("--model")
    parser.add_argument("--endpoint", help="overrides OPENAI_BASE_URL")
    parser.add_argument("--budget-tokens", type=int)
    args = parser.parse_args(argv)
    if not args.execute:
        print(json.dumps(build_prompt(args.corpus), ensure_ascii=False, indent=2))
        return 0
    if not args.output_dir:
        parser.error("--output-dir is required with --execute")
    print(json.dumps(execute(args.corpus, args.output_dir, binary=args.binary, model=args.model,
                             endpoint=args.endpoint, budget_tokens=args.budget_tokens), indent=2))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

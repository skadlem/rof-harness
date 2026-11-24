# No-evidence defaults census — output cuts lose failure verdicts, the 8,000 cap wastes reasoning, rollback rarely saves work; time, spend and repeat caps never bind (2026-11-24)

Zero model calls. An audit found 38 harness choices with no evidence behind
them; this note tests the ones on the benchmark path against logged runs and
nine other harnesses. Raw reports, their scripts and the inventory are in
`target/no-evidence-census-20261124/`; the audit page is
https://claude.ai/artifact/7SUFD4vh2TaUC4DskWZeAG.

**Scope:** 286 to 294 `events.jsonl` dumps and 39 to 47 WALs under
`~/.local/share/rof-tb/` and `target/`, dated 2026-10-08 to 2026-11-23: 13,573
tool calls and 10,771 model replies. Most come from Slice A and earlier slices on
the legacy `rof eval` path; 241 ran with the allowlist and 45 with
`--exec-policy any`, and only 12 are Harbor adapter runs, so the rates below are
not Harbor rates.

## Measured

| Choice | Result |
|---|---|
| 8,000-char head cut on tool output | 178 exec results cut (0 test). 16 had failed and lost their `[exit N]` marker: 21% of the 76 exit markers emitted. No cut result kept an exit or stderr verdict. |
| 300 s command timeout, no partial output | 28 timeouts in 22 runs, which had used a median 46% of their token budget; 5 re-ran the identical command. |
| 8,000 first output cap | 166 of 10,771 replies (1.5%) exceeded 8,000 billed output tokens. The one opened (`slice-a-any/dna-assembly`) had 8,000 reasoning tokens and empty content: the first attempt was cut off and re-sent. No `finish_reason` is logged per attempt, so the count is inferred from billed usage. |
| Whole-batch rollback | 1,825 of 10,336 batches (17.7%): 18.7% allowlist, 11.8% `any`; 10.9% and 11.8% without allowlist-denial batches. Only 18 destroyed a successful write or edit in the same batch. WAL `Attempt` proxy matched 27 of 27. |
| Cleared child environment | No missing-variable symptom (JAVA_HOME, GOPATH, PYTHONPATH, LD_LIBRARY_PATH, NODE_PATH); under `docker exec` the child sees the image's ENV. 130 `executable file not found` results were mostly binaries absent from the image. |
| Secret-file deny globs | 0 denials. |
| Edit failures | 11 of 195: 5 root-owned (before the fix), 4 no match, 1 ambiguous, 1 missing argument; 0 size-cap or non-UTF-8. |
| How runs end | 191 token halts, 4 step, 6 spend or HTTP 402, 65 Done, 3 Cancelled, 6 no RunEnd; 0 wallclock, same-action, action or max-tokens halts. |
| Grace step | 4 of 191 token halts; none produced a Done. |
| Same-action halt | Never fired. |
| Wall clock | Longest run 1,518 s against the 3,600 s limit; slowest request about 388 s against the 600 s timeout. |
| $4 spend cap | Costliest run $0.417 at peak rates. |
| 200-path file map | Cut in 6 of 35 real workdirs (up to 593 paths); models used `find` and `ls` instead. |

## Other harnesses (source read at pinned commits)

Five of nine cap command output at 10k to 30k characters and keep head plus tail
(mini-swe-agent, Terminus-2, OpenHands SDK, Codex, Claude Code for failed
results); Codex prints the exit code first, and most return partial output on a
timeout. None sets `max_tokens` by default. None of those read rolls back on a
failed call. Only OpenHands has a repeat detector; only aider puts a repo map in
the prompt. Commits, files and lines are in `harness-defaults.md`.

## Decisions

Built opt-in or as correctness fixes, and unmeasured on tasks:

1. Exit status first, head and tail kept, and partial output on timeout.
2. A 32,768 first output cap, through the existing `--max-tokens` flag.
3. Non-zero exits no longer roll back a batch, and the notice appears only when
   something was reverted.
4. The exec and test descriptions match the exec policy.
5. Adapter fixes: crash exits count as infra failures, usage is withheld when
   unknown, and the binary is identified.

Unchanged and each needing a paired run before any default change: the
same-action halt, $4 cap, deny globs, retry counts, edit caps, grace step and
file map.

# Envelope phases: what to do with the 2026-11-11 architecture review

**Status:** proposed, unstarted. Extends — does not replace —
[`2026-11-11-capability-phases.md`](2026-11-11-capability-phases.md); that plan's
phases 0–4 stay as recorded and phase 5 stays stopped.

**Input:** [`docs/reviews/architecture-2026-11-11-1032.md`](../../reviews/architecture-2026-11-11-1032.md)
(verdict FLAWED BUT FIXABLE) and the brief it reviewed,
[`docs/architecture/design.md`](../../architecture/design.md).

**Why a new plan rather than new checkboxes.** The existing plan sequences
*mechanisms* and is blocked on spend. It has no slot for the question "is the
measured low-solve pattern produced by the budget envelope rather than the loop?"
That question is now pre-emptive: until it is answered, every mechanism arm in
phase 5 is measured inside a censored regime (review FINDING 3). So this plan
sits **before** the remaining phase-5 arms, not beside them.

**What the existing plan already settled, and must not be redone:**
phase 4A fixed the compaction capacity units (`context_capacity_tokens` /
`output_reserve_tokens`; landed, `crates/agent-loop/src/request.rs:445-448`).
The review's FINDING 5 is stale; a brief that still asserts the old wiring is the
defect to correct, not the code.

## Ordering

```text
P0  guard + integrity      (zero spend, code)      ─┐
P1  envelope evidence      (zero spend, offline)   ─┴─ both may run in parallel
        │
        ▼
P2  paired arm             (spend-gated, one writer, blocked on owner decisions)
        │
        ▼
P3  the existing phase-5 mechanism arms, re-based on P2's outcome
        │
        ▼
P4  existing phase 6, unchanged
```

---

## P0 — make loosening safe, and make grading attributable

Zero model calls. Both items are in `crates/rof/`, so **one owner runs them in
sequence** — not two children on the same crate.

### P0.1 Fail closed when a spend cap cannot be enforced

Review FINDING 4. Today `inert_spend_cap_warning`
(`crates/rof/src/cli.rs:980-990`) prints a warning and continues; with an
unpriced model id `price_usd` returns `None`, `record_spend_cents` is skipped,
and the dollar cap silently never trips. Every cap experiment in P2 relies on
that guard being real.

- Change the warning to a startup refusal when `max_spend_cents` is set and the
  model has no price-book entry, before any provider request.
- Keep the existing no-cap path unchanged: an unset spend cap must still run.
- Acceptance: `--model deepseek-ai/DeepSeek-V4.1-Flash` with a spend cap exits
  non-zero with no provider call; `deepseek-v4.1-flash` with the same cap starts
  normally. Both are assertions, not observations — extend
  `unpriced_models_warn_that_the_spend_cap_is_inert` in `cli.rs`.

### P0.2 No agent-started process may outlive the agent phase

Review FINDING 6. `docker_exec_wrap` is `kill_on_drop(true)`, which kills the
*client*; the in-container child survives (documented at
`crates/tools-std/src/runner.rs:96-99`). Grading then runs in the same live
container, so an orphan can write under `/app` after the tests are copied in.

- One `docker exec` immediately before the `docker cp` of the frozen tests in
  `crates/rof/src/eval_agent.rs` (~line 680): kill every process except PID 1.
  No shell tool, no new crate, no new dependency.
- Log the kill (count, or 0) to the run's events so the artifact records whether
  anything was drained.
- Acceptance: a test that backgrounds a writer inside the container, lets the
  agent phase end, and asserts no write lands after grading begins. If a
  background writer cannot be simulated cheaply, assert the drain command is
  issued and its non-zero exit is a recorded infra event — and say so in the
  record rather than implying the scenario was replayed.

**Finish gate:** `cargo test --locked -p rof`, `cargo clippy --locked -p rof
--all-targets -- -D warnings`, `cargo fmt -p rof -- --check`.

**P0 outcome (2026-11-11): shipped after review.** First review verdict
`ship-after-fixes`, one blocking defect: `agent_args` hardcoded
`allow_inert_spend_cap: false`, so the eval flag was recorded true in the report
while every instance refused to start. Fixed and mutation-proved (reverting it
fails `flags_parse_and_fill_agent_args`). Also fixed from that review: the eval
leg now aborts the campaign on `FailureKind::Input` (exit 2) instead of emitting
a slice of `InfraFailure` rows at exit 0; the outcome message prints for
`Halted`/`Cancelled` too; the boundary probe is bounded (30s), cannot hash empty
input into a clean-looking digest, neutralises embedded newlines, and excludes
its own shell. Second review: **all-fixed**, with one residual now closed (a
parse-level eval test for the flag, not just a hand-set field).

**Deviation found while implementing P0.1 (record, do not re-derive):** 20
driver invocations under `~/.local/share/rof-tb/work/` pass the vLLM gateway id
`deepseek-ai/DeepSeek-V4.1-Flash`, which `provider-openai/src/pricing.rs`
deliberately leaves unpriced ("rate anchor never invented; keyed by model id
with no endpoint dimension"). A bare refusal therefore breaks a working path.
The price book is NOT extended — that would fabricate a rate for an endpoint the
table does not model. Instead the refusal gets an explicit opt-out
(`--allow-inert-spend-cap`), default off, which restores the previous
warn-and-continue behaviour and still prints the warning once. Fail-closed stays
the default; choosing otherwise is a recorded, visible decision.

---

## P1 — answer the envelope question without spending

Zero model calls, `tools/` only, disjoint from P0. This is the cheapest possible
test of the review's CRITICAL finding, and it may kill P2 before it costs
anything.

Corpus: the 99 event dumps and 97 workdirs already used by
`research/accounting-recompute.md`. Reuse that validated counter model (90/99
exact) rather than rebuilding it.

Per run, emit one row: terminal reason, assistant-turn count, **input tokens and
cache-read tokens on the final settled turn**, cumulative counters at the kill,
and cumulative dollars.

Then answer three questions in the same artifact:

1. **How far from the context limit was the kill?** Final-turn input size against
   the endpoint's declared context window for the model actually used. If runs
   died at 30–40% of the window, the envelope is the cap, not context.
2. **What would `miss-plus-out` have bought on the identical traces?** Replay the
   counter shape: how many further turns does the same 300k allowance fund at the
   observed per-turn miss+out cost, and how many of those runs had a declared
   `Done` or a passing check already in hand.
3. **Was anything converging?** Per run, did the last three turns change files or
   only re-read, and did the test tally move. A run that was flailing at the kill
   is evidence *against* the envelope hypothesis and must be reported as such.

- Acceptance: a committed TSV/JSON artifact plus a five-line summary, both
  regenerated by one script, with the same honesty rule the existing recompute
  used: unknown usage stays unknown, never zero.
- Do **not** re-run any paid arm to fill a gap. If a dump lacks usage, it is a
  named exclusion.
- Explicit non-goal: this does not establish a solve-rate gain. It establishes
  whether the +15-turns counterfactual survives contact with per-turn cost.

**Finish gate:** the script runs from a clean checkout and reproduces the
committed artifact byte-for-byte.

---

## P1 result (2026-11-11) — reviewed and corrected

`tools/envelope_report.py` + `docs/experiments/2026-11-11-envelope.json`.
Deterministic (two runs byte-identical), corpus-relative paths (0 absolute),
guard on a corrupt `models*.json`. Root verified one row end to end from the raw
dump (24 steps / 306,583 cumulative / 11,933 final-turn input).

**Review verdict: trust-with-caveats.** It corrected the brief's own arithmetic
(7.6%/15.2% was computed on all 249 token rows, not the artifact's n=135; the
right figures are 8.5%/16.9%) and forced four changes now in the artifact:

- **Q1 (context headroom).** Every window comes from a client config keyed by
  model id only; 116 of 135 rows take theirs from a reseller entry that does not
  match the endpoint the driver hit. Rows now carry `context_window_source`
  (19/20/80/16 + 24 unknown) and the absolute tokens are the primary statistic —
  `{n:135, min:5170, p10:8185, median:11099, p90:16199, max:37665}`. The
  fraction is secondary and labelled "not a served limit". One empirical bound
  exists: `pilot-dphn-cap/S-r2-video-processing` **served a 95,236-token prompt**,
  which refutes any window claim below ~96k for that endpoint. **Do not quote a
  headroom percentage without its source.**
- **Q2 (miss-plus-out).** Split by winner; it was a single median before, which
  read as "turns bought" when on 105 of 135 rows it equals `60 - halt_step` by
  construction. Token-capped rows: n=30, median +16, max +29. Step-capped rows:
  n=105, median 16 **steps left on the clock**. And the counter crosses the cap
  on **0 of 135** traces (`q2_basis_counts {recorded:0, extrapolated:135}`), with
  20 rows within 25% of the step boundary.
- **Q3 (convergence).** Now skips `is_error` results and has no shell-redirect
  kind at all (`exec` splits argv without a shell, so `>` is a literal argument
  and cannot mutate). **81 -> 77 still-mutating**, 67 read-only, 0 unknown, out
  of 144: evidence for the envelope hypothesis on a bare majority, not a verdict.
- **Corpus drift.** 253 dumps, not the 83 `research/accounting-recompute.md`
  describes; `matrix-v1`, `pilot-capab`, `pilot-tokcap`, `pilot-slice-a`,
  `pilot-r2iter` and `/tmp/acct` are gone. That recompute is **not reproducible
  as written** — only its conclusions are, and P1 re-derives the counter model on
  what survives (135/135 gross token deaths reproduce; the 22 surviving doc rows
  match its published steps and dollars exactly).

## P2 — triage-gated (current state, 2026-11-11)

**No paid arm is authorised yet.** Two review passes (`docs/reviews/decision-2026-11-11-1134.md`,
`docs/reviews/decision-2026-11-11-1152.md`) and the arms themselves closed both candidates:

- **Envelope axis: triple-null.** miss-plus-out alone 0/4 (1.42x cost); the joint arm 0/4 cell-reps
  with the guardrail at 1.79x/3.05x and its registered KILL triggered; raising steps at a fixed token
  cap bought 0/10 -> 2/10 and then 7/10 died on the token cap. The 2026 evidence says budget and
  information SUBSTITUTE rather than complement (BRA Table 5: 21/36 interactions negative, none
  positive), which explains all three.
- **Checklist lever: killed on generalization.** Arm 1 converted mcmc 5/6 -> 6/6 (the project's only
  capability-positive row); **Arm 2, 16 rows on three further cells, moved no named test and triggered
  its own KILL** — "rerun-context specific, no further checklist arms run". The doc reads the surviving
  gap on those cells as capability-bound at this model tier, not prompt-bound.

**The gate is now a zero-cost triage**, in flight as `docs/experiments/2026-11-11-triage.md`: classify
every banked in-container row by fidelity status (pre/post 2026-11-06), halt reason, failing test set
and failure class — with the class justified from the PUBLIC `instruction.md` only, never from test
files or verifier stderr. Spend reopens only if that table names a cell that (a) reaches `done`,
(b) fails on a publicly stated executable artefact property, and (c) has a stable control. Then and
only then: one `--completion-check` arm there, pre-registered, on a metered endpoint.

**Known against reopening:** the README's own post-fix table (`On the faithful pipeline, 2026-11-06`)
already excludes mcmc ("no rstan in-image") — Arm 1's only positive cell cannot be re-measured
faithfully — while heap sits 0/3 on its floor, video 2/5 on its floor and query wall-clocks at 12 steps.

Ledger: balance **$3.12** (owner, 2026-11-11), approved for use, triage-first. The earlier $2.82 figure
was a campaign ceiling and $2.519179098 its consumed upper bound; neither is reset or reused.

## P2 — the paired arm (spend-gated; the only paid step)

Adapter support already exists (`tools/harbor_rof.py:35,149` forwards
`budget_shape`). No adapter work.

**DECIDED 2026-11-11 (owner-authorized review, `docs/reviews/decision-2026-11-11-1134.md`):
B, amended — not the review's original `miss-plus-out` vs `gross` pair, and not the
2026-practice band.** Sequence is now fixed:

0. **Fidelity probe (first paid step).** One control-envelope run on one cell, confirming
   the verifier actually sees `/app` after the workspace-fidelity fix. ~$0.03-0.11. If it
   does not, bank the remainder: an unprobed re-run repeats the void at ~$1.5.
1. Treatment = **600k miss-plus-out / 120 steps / 240 actions** ("candidate amendment 3"
   from `research/dphn-cap-ab1.md`), with its own post-fix control — NOT the void 18's
   rows. 3 cells x 2 reps, cells chosen where a row token-halted with steps unspent.
2. Report halt reason per rep; gross tokens per run is the only real guard; spend cap
   $2.00. A `done` halt with unspent cap is reported as "envelope not binding", never as
   treatment failure.

Why not the alternatives: a 3M/250-step arm's $3 per-instance guard cannot fire on the
unpriced vLLM endpoint and would be 96% of the total budget; option C (spend on a
semantic-deliverable lever) was rejected because scoping a lever to a named failing
verifier test feeds hidden-verifier content into runtime decisions.

Two things remain the owner's call, not an implementer's: superseding `dphn-cap`'s
registered KILL rule, and the README "Void, re-measured pending" marking.

Original text, kept for context: one treatment `budget_shape = miss-plus-out` against `gross`, everything else
frozen. **P1 changes this**: with steps held at 60 the step cap wins 105/135, so
both arms must raise steps above the point where it binds (or the experiment
reports itself as step-bound). Changing shape and steps together is two
variables; the honest options are (a) shape-only and accept that most rows are
step-bound, or (b) a pre-registered step cap chosen from the P1 table so it
cannot bind, documented as the envelope envelope. Decide before spending.
Rules that make it attributable:

- Both arms pass explicit budget flags, so `budget_for` yields the same 3600s
  wall (review FINDING 7). Never compare a flagged arm against a preset.
- The dollar cap is the binding guard in both arms; P0.1 must be merged first.
- Identical tasks, snapshot digests, model, endpoint, steps, actions.
- Rows carry `budget_shape` and must never pool with the 99 pre-recipe dumps.
- Report first terminal reason and native reward per row; a `Done` or a verifier
  exit code is never a solve.
- Abort rule: if the summed per-run spend caps exceed the authorised remainder,
  the experiment does not launch.

**Blocked on owner decisions** (these are authority, not engineering):
the ledger remainder ($0.300820902 against a $2.82 ceiling; the $4 figure is a
per-instance envelope, not the campaign budget), which control recipe is current
(300k/60/120 or 1,200,000/120/240), and which tasks are shared with a rival.

**Acceptance:** the envelope hypothesis is *supported* only if token-terminal
runs fall **and** solves rise in the paired comparison. Otherwise record
`refuted` and go to P3's model-class test, not to a bigger cap.

---

## P3 — existing phase-5 mechanism arms, re-based

Unchanged recipes, but their **order** changes: they are scheduled after P2, and
a mechanism arm is only worth running on tasks where the control is off the
floor. The review's FINDING 3 is the justification — mechanism ablations inside a
censored regime cannot move or falsify the capability estimand.

Carry over from the existing plan, untouched: one change at a time, charge every
attempt, preserve rescue/harm, no hidden-verifier feedback into runtime
decisions, no default promotion on one pilot.

Phase 3's single unchecked item (compare rendering hysteresis independently of
restoration) stays deferred: the wave-1 evidence already shows the causal subset
is 3.1% of view calls, so it has no gate to pass yet.

## P4 — existing phase 6, unchanged

Held-out confirmation, admission rule, documentation in the same change. No edits
proposed here.

---

## Subagent deployment

Two children in wave 1, and that is the honest maximum: the repo contract is one
exclusive writer per crate, and P0 is single-crate by construction.

| Wave | Child | Exclusive paths | Parallel with |
| --- | --- | --- | --- |
| 1 | `rof` guard + integrity | `crates/rof/` | P1 child |
| 1 | envelope analysis | `tools/` only | P0 child |
| 1 | — | root: `docs/` (drift fixes below) | both |
| 2 | independent diff review via a **subagent** on the session model (`deepseek/deepseek-flash:max`) per owner instruction 2026-11-11 — never the `solution_review` tool, which is reserved for full reviews the owner requests explicitly | none | root runs the gates |
| 3 | none | `crates/rof/` (root is sole campaign writer) | — |

Do not spawn a second child into `crates/rof/`, and do not let the analysis child
touch a crate. Each task brief carries: starting commit, allowed paths, the
input/output contract, the default behaviour that must not change, the failing
test to write first, the three gate commands, and stop conditions.

## Documentation drift to fix in wave 1 (root, small)

- `docs/research-next-steps.md` — the "separate context capacity" row already
  reads as fixed (done 2026-11-11).
- `docs/architecture/design.md` — corrected: stale compaction claim, $4-vs-$2.82
  distinction, stale "only one shared task" line.
- `docs/architecture.md` — check its "guarantee limits" section against P0.2 once
  the drain lands; it currently says an in-container descendant may outlive a
  killed `docker exec` client.
- This plan referenced from `docs/research-next-steps.md` and the phase plan's
  status line.

## What this plan refuses

- Re-architecting the loop, adding a planner, candidate racing, or parallel tool
  batches to lift the solve count.
- Flipping `MissPlusOut` to default before a paired native-reward result exists.
- Raising default caps to make a pilot look better.
- Spending the remaining ledger on mechanism arms before P2 answers the envelope
  question.
- Re-running paid cells to fill a hole that P1 can answer offline.

---

## Blocker found 2026-11-11 — container egress to github.com (root cause, diagnosed)

The Slice A oracle pre-flight cannot pass on this host, and the cause is routing, not the
tasks. Two independent oracle runs on `break-filter-js-from-html` both died the same way:
`astral.sh/uv/0.9.5/install.sh` redirects to
`github.com/astral-sh/uv/releases/...` → `curl: (35) Recv failure: Connection reset by peer`
→ `uvx: command not found` → reward 0. **18 of the 21 Slice A graders bootstrap uv this
way**, so a campaign today would produce network infras, not capability.

Measured, not inferred:
- Both the host and a container resolve `github.com` to `20.205.243.166`.
- The **host reaches it (200) and downloaded the full 21,370,871-byte tarball**.
- A container reaches `140.82.121.4` and `140.82.113.4` (200) but gets `000` on
  `20.205.243.166`. Everything else works from a container: PyPI 200, npm 200,
  crates.io 403 (reachable), `api.deepseek.com` 401 (reachable), deb.debian.org 200.
- `ip rule` shows `31269: not from all fwmark 0xcb6b lookup 52075`, and table 52075 is a
  single `default dev laptop` — a full-tunnel default. Host traffic carries fwmark
  `0xcb6b`, skips the tunnel and goes direct; container traffic is unmarked, enters the
  tunnel, and that tunnel cannot serve this GitHub edge.

Fix (needs root; this session has no `CAP_NET_ADMIN` and no passwordless sudo) — send one
destination back to the table the host's own working path uses:

```
sudo ip rule add to 20.205.243.0/24 lookup main pref 31000
```

Verify (`200` means unblocked):

```
docker run --rm debian:bookworm-slim sh -c \
  'apt-get -qq update && apt-get -qq install -y curl >/dev/null && \
   curl -sI https://github.com -o /dev/null -w "%{http_code}\n"'
```

Undo: `sudo ip rule del to 20.205.243.0/24 lookup main pref 31000`. `ip rule` additions do
not persist across reboot, which makes this safe to try. A no-root workaround does **_not**
exist here: harbor exposes no `--add-host`, container `/etc/hosts` and the Docker daemon
config are root-owned, and the task images are not to be rebuilt (that would change their
digests and void comparability).

Only 3 of 21 tasks are clean today (`headless-terminal`, `hf-model-inference`,
`kv-store-grpc`), and two of those already solve on the faithful pipeline, so running that
subset would be saturated and worthless.

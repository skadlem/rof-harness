# Decisions

Continuation: the
[phased capability plan](superpowers/plans/2026-11-11-capability-phases.md) records
paper-driven additions, ownership and evidence gates. Phases 0–4 are implemented and
locally verified; efficacy/default admission stay conditional. The
[implementation record](phase-0-2-implementation.md) documents contracts, including
rejecting composition of completion predicates with the review stage until their
shared repair allowance is specified. Historical entries keep their original
experiment scope; current outcomes and defaults are in the
[pilot](current-benchmark-pilot.md) and [handoff](research-next-steps.md).

Distilled from code comments and tests. Chosen-to-be-validated means no measured
anchor, as the code says.

- **Deliverable is the full uncut patch.** `rof` prints
  `snapshot::patch_since_start_full()` on stdout; the 8KiB/200-line evidence bound
  stays internal to the loop. A `main.rs` regression test pins it (the old
  `patch_since_start` cut stdout).
- **Strict vs Lenient eval grading.** Strict patch-apply by default;
  `--lenient-apply` records Lenient provenance. Strict grades are not comparable with
  earlier fuzz-lenient numbers.
- **WAL on by default, fail-closed.** `<workdir>/.rof-events.jsonl`; `--log-path`
  moves it, `--no-log` disables it and wins over the override.
- **`--compaction` is opt-in and unvalidated.** Absent the flag the run is
  byte-identical to the no-compaction default; a present flag requires explicit
  `--context-capacity` and `--output-reserve`. S-1 has not run; the flag exists for
  it only.
- **Fold knobs resolve at the edge.** `THINKING_KEEP` / `COLLAPSE_HYSTERESIS` fill in
  only when the matching flag is absent; a present-but-unparsable value is a usage
  error, never a silent default. The loop never reads the environment.
- **Workdir guard enforces exactly four refusals.** `/`, `$HOME`, the harness
  checkout, and a dirty repo without opt-in (exit 2) unless
  `--allow-dirty-workdir`; pre-existing dirt then appears in the reported patch, so
  it never leaks in silently. Disposability elsewhere is documented operator
  responsibility (threat model), not enforced.
- **Agent-leg eval budgets default to the frozen pilot recipe.** 300000 tokens / 60
  steps / 120 actions when budget flags are omitted (chosen: copied from the frozen
  pilot recipe in NEXT.md; validated when the agent-leg slice runs). Explicit flags
  win; `cli::budget_for` is the single resolver.
- **The goal rules tail gained a stdin clause after the 26-cell sweep.** Agents issued
  heredoc execs (`python3 - <<'EOF'`) — silent exit-0 no-ops under exec's null stdin —
  and built conclusions on them (56 execs, zero deliverable writes). The tail now
  states stdin is closed and points at the write-script-to-file fix; banked sweep
  cells ran the old tail (arm marker).
- **Second provider wired: GLM-5.3-flash at Z.ai list rates.** Cache-read $0.03 /
  input $0.15 / output $0.50 per 1M (chosen: Z.ai list rates effective 2026-09-10,
  corroborated by three independent pricing pages; validated against the first invoice
  — a proxy may mark them up). `--keep-thinking` on `rof run` and `rof eval` drops the
  DeepSeek thinking-off body for providers that reject the fragment or when reasoning
  is wanted; default body byte-identical. Only compaction-summary requests
  (`Thinking::Off`) and the truncation ladder's first rung carry it; ordinary requests
  use `Thinking::Auto` and send none. Unpriced models report null cost, never zero.
- **In-container eval is a separate mode with a separate estimand.**
  `rof eval --agent --in-container` starts the task container first (workdir
  bind-mounted at /app, cpus/memory 2/4096m — chosen: caps observed in the task specs;
  upgrade path: parse [environment] from task.toml per task), wraps tool policy with
  `docker exec <container>` so exec/test calls land inside (allowlist and shell-op
  guard still see the unwrapped argv; the wrap applies at spawn only), and grades by
  execing /tests/test.sh in the SAME live container — tests `docker cp`-uploaded at
  grading, never mounted for the agent phase (2026-11-06 below). The env-mutation
  class (agent-installed imports, a live server) becomes winnable. Verdict provenance
  is the task-written reward file (`/logs/verifier/reward.txt`, mounted writable at
  `<home>/verifier-logs`), exit code as no-artifact fallback — a NEW estimand, never
  mixed with harbor result.json rows. MEASURED 2026-10-12 (26-cell arm audit): every
  TB2.1 slice test.sh ends with `echo 1|0 > /logs/verifier/reward.txt`, so it ALWAYS
  exits 0 — exit-code-only grading read workless runs as Resolved (three
  seed-identical-workdir rows "Resolved" at reward 0; the banked 8/8 was 2/8 by
  reward; file-artifact widening read **4/18 solved** (`break-filter-js-from-html`,
  `hf-model-inference`, `log-summary-date-ranges`, `sam-cell-seg`, 2026-10-12). A
  present reward file decides; junk withholds as Ambiguous, never falling
  back to the lying exit code; absence keeps the exit-code fallback, so
  exit-propagating tasks stay byte-identical. The file is trusted because the instance
  home is fresh per cell with exactly one grading. Review correction (2026-11-03): a
  reused output dir could retain a stale reward — the verifier dir is cleared before
  each grade, and infra failure takes precedence. Ceiling: timeout/cancel kill the
  docker-exec client; the in-container process may outlive it (upgrade path: docker
  kill or exec-session tracking). Root-owned in-container artifacts survive host-side
  `git clean` during batch rollback (measured across 70 banked in-container rows:
  snapshot-class halts in 5, of which 4 are this class — fail-soft in 3 where the row
  still grades, hard halt in 2, one voiding a run at step 3 on 2026-10-16 and one at
  the end-of-run patch digest `halt=snapshot` on `install-windows-3.11` on 2026-10-14;
  upgrade: chown before rollback/digest or digest via a root-readable copy).
- **The harbor CLI is the agent-leg grading seam.** `rof eval --agent` ports the bash
  pilot driver's mk_goal/verdict verbatim: job yaml (JSON) + `harbor run -c <job> -y
  -q`. Harbor spawn failure, grading timeout without result.json, workdir refusal, or
  no RunEnd in the events dump = InfraFailure; infra is never capability. Live-smoked
  2026-10-08 (swd, DeepSeek direct, deepseek-flash): current harbor writes
  `reward_stats.reward` as a `{value: [trials]}` histogram, not the pilots' scalar —
  the reader takes both shapes and fails closed otherwise (old bash drivers print the
  dict; fix their parse or use `rof eval --agent` first).
- **Exec children run niced, never capped.** Host children take nice 19 at spawn
  (descendants inherit): sweeps stop making the desktop unusable without changing wall
  times on an idle host (measured trigger: sweep-cell builds drove the 14G/12-core
  host to global OOM and desktop stalls on 2026-10-08; grading containers were already
  spec-capped — a docker scope peaked at 125M). Affinity/cgroup caps would change
  measurements and are the operator's isolation job; memory uncapped by design.
- **Exit codes.** 0 done; 2 usage/parse/workdir refusal; 3 run failure (halted,
  cancelled, provider error — Provider keeps the historical 3); 4 missing credentials;
  5 log failure; 6 snapshot failure; 7 input failure; 8 unmet completion predicate.
- **Bets are feature-flagged.** `--bets` opts in; without it the batch commits as
  before. Bet A (proof gating) needs Bet B's claim field, so ablation order is B then
  A; Bet C (candidate racing) stays flagged and unbuilt. Read-only batches carry no
  hunks and commit.
- **Presets.** UnattendedBatch 20/12 measured; LongTask 40/25
  chosen-to-be-validated. Context-pinned 200k token floor chosen-to-validate
  (pinned-prefix floor ~4-5k tok/req x 20-30 requests plus reasoning completions);
  explicit flags always win.
- **Explicit budget flags own the whole budget.** Any of `--budget-steps` /
  `--budget-actions` / `--budget-tokens` lifts the UnattendedBatch 900s wall to the
  LongTask 3600s allowance: the wall guards 20-step default batches, never a flagged
  recipe (measured 2026-10-08: v0 sweep cells ran 207–816s, all bound on the token
  cap; one fresh 60-step/300k cell wall-halted at 21 steps, 24% of its token budget —
  300s exec timeouts burn the 900s wall in three builds). Flag-less runs keep the
  preset wall.
- **Dump semantics.** `--dump-events PATH` replaces any previous dump at PATH (one
  run, one dump), truncating at start and appending per event.
- **Reports self-describe: the provenance block is part of the report contract
  (2026-10-13).** `RunReport.provenance` carries the binary's short git sha (embedded
  at build; "unknown" when git is unavailable, never fabricated — a pinned test fails
  if it goes silently absent), the strict `Estimand` enum — `Ungraded` (plain eval: no
  grading path, rows typically `ErrorNoReport`), `HarborOracle` (`--agent`, harbor
  `result.json`), `InContainerReward` (`--agent --in-container`, reward file with
  exit-code fallback) — wired at the shared report tail both legs use, the agent-leg
  budget recipe (explicit flags over frozen pilot defaults; `Ungraded` never applies
  it), the token-cap shape, and the apply mode. Pre-block reports (no key)
  deserialize as `provenance: null`. Labels were verified against the actual grading
  seams by fresh review (2026-10-13), including that the always-zero-exit misgrade
  class of 2026-10-12 is not reintroduced.
- **Budget-shape is a separate estimand, OFF by default (2026-10-13).**
  `--budget-shape gross|miss-plus-out` (one spelling source shared by `run` and
  `eval`). `gross` = input+output, bit-identical to the frozen recipe by construction
  (the default arm selects the literal pre-change expression). `miss-plus-out` charges
  `(input - cache_read) + output`, saturating, at the single metering site — cached
  prompt re-reads stop spending the cap (measured 2026-10-08 forensics: 90–98% of the
  300k cap is context re-reads). Usage totals and dollars stay gross: only the cap
  moves. `cache_read ⊆ input` is a property of the OpenAI-shaped wire (the adapter maps
  `cached_tokens` inside `prompt_tokens`, the same miss share pricing charges), not a
  provider-core invariant — an adapter reporting cache reads outside `input` must not
  use `miss-plus-out`. A miss-plus-out arm never shares a table with a gross arm; the
  shape is a required provenance field (a block without it fails loudly rather than
  reading as `Gross`). Unvalidated; first run is owner-gated spend.
- **Rivals-grid comparability rules (grid defined 2026-10-13).** A measured inventory
  of stored data (204 cells) defines legal tables: per-estimand and per-arm only, never
  mixing harbor-graded rows with in-container reward-file rows, never mixing arms
  (different binaries, models, or recipes). Cross-harness cost is NOT computable —
  rival per-task logs carry zero usage fields (verified 2026-10-13) — so cost stays
  within-rof, and pass-rate deltas at 3 reps over 2 shared tasks sit below the noise
  floor (Wilson ±28–37pp). Recount of the 26-cell in-container arm: 6 Resolved / 19
  Unresolved / 1 infra, $0.76 — an earlier 6/17/1 note summed to 24, an arithmetic
  slip. The recorded-exit verdict layer is dead on this slice.
- **Budget-shape arm 1 measured (2026-10-14): gate failed, knob stays OFF.** Pass rule
  was more solves at ≤1.3× dollars-per-solved vs banked gross rows. On the 4 near-miss
  cells (glm-5.3-flash, frozen recipe, in-container, matched config, n=1/cell/arm):
  solves 0/4 vs 0/4, tests flat (video 2p both) to down (mcmc 2p→1p), arm cost 1.42×
  ($0.1693 vs $0.1196). Mechanism real — both previously token-dead cells migrated to
  the step cap and consumed 1.60×/1.70× the gross arm's input (466k/493k, cache reads
  246k/207k) — but extra working context did not convert to capability at this model
  tier. At step death the miss+out charge stood at 84.6% and 99.8% of the 300k cap, so
  the recipe binds both caps nearly together. The shape is a required provenance
  field, so miss-plus-out rows never pool silently with gross rows.

## Not inferable from code

- Numeric anchors for the LongTask and context-pinned defaults above (no measurements
  in-tree).
- When Strict-by-default eval grading becomes the permanent baseline.
- Whether Bet C will be built, and on what timeline.
- **Compaction premise refuted before spend (2026-10-16).** The landed
  `--compaction FRAC` trigger compared a *per-request* prompt estimate against
  `frac x token budget`, while the frozen recipe's token budget is *cumulative billed*
  — different units. Over 1064 settled requests in the banked arms, per-request prompts
  top out at 18.0k tokens (median 2-3k), so the registered 0.7 x 300k = 210k trigger
  cannot fire, and the 20k verbatim-keep budget exceeds an entire prompt so the cut
  point is never valid either. The validation arm is inert (treatment vs itself); the
  knob stays OFF and S-1 returns to design, keyed on spent-vs-cap rather than prompt
  size.
  > Now: trigger is `frac` x *usable context capacity* (window minus output reserve)
  > from explicit `--context-capacity` / `--output-reserve`
  > (`crates/context/src/compact.rs:105`, `crates/agent-loop/src/request.rs:488`).
- **Prompt composition of the frozen recipe (2026-10-16, inferred from a reconstruction
  calibrated against measured per-request token counts).** Typical mid-run prompt:
  system + tools + goal ~1.1k tokens, ~21 tokens of wire scaffolding per history row
  (~2.1k at 50 rows), tool result bodies ~2.7 chars/token, older results stubbed to one
  line, no cumulative reasoning echo in the pre-trim arms (the "echo bill" belongs to
  the pinned-context runs). Largest *uncollapsed* component: assistant tool-call
  arguments — the request fold stubs old results but never old call arguments, so every
  patch body is re-sent on every later request.
- **Second experiment endpoint wired (2026-10-16).** A vLLM-fronted gateway
  (`lighthouse.dphn.ai/run/text-v/v1`) serves `deepseek-ai/DeepSeek-V4.1-Flash`. Live
  probes + a smoke run: tool calls execute,
  prefix-cache reads reported, thinking arrives under `reasoning` and the adapter reads
  it as a fallback (`reasoning_content` wins when both keys are present; two-field
  parse rather than a serde alias, so a body with both keys parses), and DeepSeek's
  thinking-off knob is accepted and ignored. The endpoint is unpriced — cost on only
  some routes, so a partial sum is not meterable and dollars stay `null`; token/step
  caps are the guards and the roadmap's unpriced-model gate is open.
- **Envelope arm (pre-registered 2026-10-16, amended before the second launch, closed
  2026-10-16/18): the kill rule fired.** Arms: control = frozen recipe (300k gross / 60
  / 120); shape = 300k miss-plus-out at the same envelope; joint = miss-plus-out plus a
  doubled step/action envelope. Two clean in-container headroom cells (test fractions
  2/5 and 2/6 on the reference model) plus one already-solved regression cell, two reps
  each, in-container reward estimand. Pass: per-cell test counts up in the joint arm for
  two of three cells, at least one cell solved, tokens per test passed within 1.3x of
  control. Kill: neither shape nor joint raises test counts. Amendment: the original
  cell set came from host-side partial-credit rows and the in-container table shows the
  first cell at 0/6 for every model measured in that mode, so it has no delta; one
  wasted run, cells fixed, arms/reps/gates unchanged. Result: the envelope moved walls,
  not capability — shape turned one cell's token-death into a step-death, joint reached
  87 of 120 steps (1.85x steps, 3.0x gross input), yet every arm on that cell failed the
  identical verifier test (the missing deliverable is semantic, not budget), and the
  other headroom cell's single gain was an import-check test. Gross tokens per test
  passed 1.85x and 2.35x against the 1.3x rule. No further envelope arms; the parked
  near-miss top-ups stay parked, their prior was envelope-shaped.
- **In-container exec policy (landed 2026-10-16, arm closed 2026-10-18): mechanism
  confirmed, capability unconverted.** `--exec-policy allowlist|any` on `rof run` and
  `rof eval` (default `allowlist`, the strict `--allow-cmd` prefix list). `any` skips
  the list for callers whose process boundary is elsewhere: in-container eval wraps
  every exec as `docker exec <container>`, so the container is the boundary and the list
  only removes capability — on 53 banked in-container rows, 83% hit at least one denial
  and the denied set includes the toolchains the tasks' own tests require. The value is
  a required provenance field, so relaxed rows never pool with strict rows; the
  shell-syntax guard (a correctness teaching error) still applies under `any`, and the
  threat model states that `any` removes even the hygiene the list provides.
  Tokens/steps remain the guards on an unpriced endpoint. Gates: tools-std 60/0, eval
  31/0, rof 56/0, clippy `-D warnings` and fmt clean. Arm `dphn-trust` (pre-registered, queued,
  same model/endpoint/binary/recipe/estimand; cells whose zero is a denial artifact —
  make-mips-interpreter, custom memory-heap-crash, query-optimize — plus one solved
  regression cell; strict list vs trust x 2 reps; pass: treatment passes a test the
  cells never passed under the strict list or solves one, within 1.3x control tokens per
  test passed; kill: every treated cell stays at zero tests) removed the friction
  completely (allowlist denials 3-9 per row to 0, same tokens) and converted nothing:
  the same named verifier tests fail in every arm at the same steps, and the solved
  regression cell stays solved in both arms. Those zeros are capability/semantic at this
  model tier, so published in-container solve numbers need no policy re-label and the
  knob stays mechanism-validated / capability-unconverted. Next capability question
  there is goal/feedback shaped (the same named requirements fail every time), not
  tooling or budget.
- **Completion-checklist lever: rerun-context specific, not general (2026-10-18;
  re-entered 2026-11-11 from the untracked research copy).** One generic
  completion-checklist block appended to the task goal — list every explicit
  requirement, verify each in place, re-execute each deliverable from a working
  directory other than the one it was developed in, then declare done — converted a
  cell the control never solved (0 of 6 control rows) into a 6/6 solve at identical
  token spend and fewer tokens per test passed in every pair; path-robustness code
  appears 11-12 times in checklist scripts versus 0-1 in control. Arm 1
  (`mcmc-sampling-stan`) went 5/6 -> 6/6 SOLVED, the project's first
  capability-positive row. Arm 2 applied the byte-identical checklist to three
  requirement-shaped cells plus the
  regression cell (16/16 rows): no named test flipped, flat tokens, registered KILL
  triggered. So it converts a deliverable-verification defect (a script that works where
  it was written but not where an evaluator runs it) and nothing for accuracy or
  correctness gaps. Claim kept small: direction measured, effect size unresolved (one
  converted rep on one cell), and the cell is bimodal on whether the required R package
  survives to grading, which sinks rows in both arms equally. Delivery-form constraint:
  the banked arms used a task copy whose `instruction.md` carried the checklist; the
  phase plan requires original instructions and graders unchanged, so promotion needs
  harness-side goal text. [measured: research/goal-checklist-ab1.md, both arms;
  local-only — see the provenance note below]
- **File-tool `/app` path parity fixed (2026-10-18).** Task statements and the goal text
  spell the workspace `/app/<file>`, but host-side tools root at the host workdir, so
  the largest denial class across 96 in-container rows was agents refused the very files
  they were told to edit (`view:/app/Dockerfile` and friends). The resolver now strips a
  leading `/app/` before joining the root — strictly additive, absolute `/app/...` was
  always refused — while traversal out of the root, secrets deny-globs and the
  git-substrate refusal still apply. A two-binary A/B (12 rows): denial class gone, no
  test moved on the failing cells, the solved regression cell reached the same verdict
  in about half the steps and tokens. Container-only paths (`/tests`) stay refused:
  serving them would let the agent read the verifier's tests, a fairness decision rather
  than a bug fix.
- **Session banked (2026-10-18): five arms, one capability lever.** 68 graded
  in-container cells on one model/endpoint/recipe, all reward-file graded and
  provenance-self-describing: envelope axis dead (walls move, capability doesn't,
  1.79-3.05x tokens per test passed); allowlist friction removed with no capability
  effect; `/app` parity fixed (correctness; ~half the steps and tokens on the solved
  regression cell); checklist capability-positive but scoped. Remaining failures are
  semantic and capability-bound at this model tier.
- **Reasoning effort is a request knob, not a constant (2026-10-19).** The harness sent
  no effort level, so every arm ran the endpoint default. A $0 gateway probe on one
  prompt (and a harness rerun at fixed `--max-tokens`) measured `reasoning_effort` is
  honored — `none/low/high/xhigh/max` = 0/722/1060/1149/2976 reasoning tokens raw, 236
  → 568 in `rof run` at equal caps — while `thinking:{type:enabled|disabled}` moves
  nothing and the api.deepseek.com probe showed no effect. That does not establish
  current API support: the official documentation checked 2026-11-09 explicitly
  supports low/high/max with high as default. So it is a per-endpoint property on
  `EndpointProfile` (like the thinking-off fragment), set per run with
  `--reasoning-effort` on `run` and `eval`, never a global default, recorded in report
  provenance so an effort arm never pools with frozen-recipe rows. Default stays "no
  level sent" (byte-identical body). Why the first probe read flat: reasoning length is
  bounded by `max_tokens`, so a small per-request ceiling clips the effect; the overflow
  ladder raises the ceiling on a truncated attempt, which is why the knob is safe at
  default.
- **Unpriced model + armed `$` cap: warn, then REFUSE (2026-10-19, superseded
  2026-11-11).** The cap only binds when the price book prices the model; an id with no
  rate anchor (the vLLM gateway's `deepseek-ai/DeepSeek-V4.1-Flash`, any future
  provider) reported no cost, so `max_spend_cents` could never trip while the run looked
  guarded. First fix (2026-10-19): a run-start stderr warning naming it once per
  process — which left every `rof run` with a dead $4 cap, because the `UnattendedBatch`
  preset arms `max_spend_cents` for all of them. Now:
  `cli::budget_for`/`spend_guard_error` return `Err` when a cap is armed and
  `prices_model(&args.model)` is false, and `main` exits **7** (input/config) before any
  provider request; the eval leg aborts the campaign with exit 2 instead of emitting a
  slice of `InfraFailure` rows. No price is invented for the vLLM gateway id: 20 driver
  invocations pass it deliberately unpriced, so the escape is an explicit
  `--allow-inert-spend-cap` (run and eval), default off, restoring warn-once behaviour
  and recorded in `RunProvenance.run_config` so opted-out rows cannot pool with guarded
  ones. [measured: `cargo test -p rof` 88 green, re-measured 89 green + mutation proof
  (reverting the eval-leg forwarding fails `flags_parse_and_fill_agent_args`), live
  probe exit 7 with no provider call, priced run reaches the provider]
- **Reasoning effort: measured, and it is not a capability lever (2026-10-19).** 18
  in-container rows, one binary, one flag apart (control vs `--reasoning-effort max`):
  no named verifier test flipped that the control did not already pass — query-optimize
  went 1/6,1/6 to 0/6,0/6, video failed the same three tests in all four rows, the arm's
  only solve was a control row. The manipulation under-delivered: median reasoning
  tokens rose only 1.23x against a pre-registered 1.5x, per-pair ratios 0.42x to 2.4x
  (the raw probe's 497 → 2976 tokens came from a short ungrounded prompt), so on real
  tool-loop turns this endpoint's default is already near the task's natural
  reasoning length and the flag reallocates rather than adds. Tokens per test-passed
  1.54x (against a 1.3x rule). Kill rule triggered: no further effort variants. The
  failures on this slice therefore survive more budget, steps, a full toolchain,
  `/app`-correct paths, a stricter goal and more reasoning — capability-bound at this
  model tier; the next question is a different model or a smaller slice.
- **Native effort channel (`dphn-effort2`): kill rule triggered with a *delivered*
  treatment (2026-10-19).** 18 rows, same binary, the arms differing only by the vendor
  line `Reasoning Effort: 100 (range 1-100, …)` prepended to a task copy's instruction.
  One named-test flip in 9 treated rows (`test_jump_analyzer_imports`, video r1, 3/5) —
  1 of 3 reps, so the registered ≥2-of-3 rule fails, and that test has flipped as
  variance before. The mechanism under-delivered too: median reasoning tokens
  6,558 → 8,283 = 1.26x against a ≥1.5x rule, although the same marker measures 4.0x on
  an ungrounded prompt, because rows die on the token cap and reasoning length is set by
  the task, not the marker. Guardrail 1.26x tokens per test-passed with *fewer* tests
  passed, and the solved regression cell got slower (17-23 steps / 71-107k control vs
  26-38 / 129-270k treatment). Caveat: mcmc is rstan-bimodal and all three treatment
  rows hit the env-failure class, so that cell is uninformative again. Portable finding:
  in-loop reasoning effort is only weakly steerable, which is what both channels
  measured.
- **Literature sweep and the correction it forces (2026-10-19).** Four researcher lanes
  read the Aug–Oct 2026 literature against this repo's measured record (local report
  `lit-sweep-2026-10-19.md`, lane briefs beside it). (1) **Deterministic state-triggered
  gating** is a real lever at our model class: LivePlan (arXiv:2608.06701) runs a
  rule-based monitor over trajectory state and can block the next action; the
  monitor-only variant — no advisor model, 0 s monitor cost — moves 21.76→25.00 /
  13.17→18.94 / 52.50→54.17 % on 213–234 instances, while the full method (+12.33 pp
  and up) needs an advisor *stronger* than the executor, which this project does not
  have. (2) **Reward-free harness search works at our exact model family** — SelfSearch
  (arXiv:2609.37968): 65.2→73.0 on Terminal-Bench 2.1 with a deepseek-v4-flash executor
  at matched execution limits and 12% lower cost per task, search cost $4.03 — but it
  needs an agent-editable substrate and 20 search episodes, a re-architecture rather
  than a knob. (3) **Two levers are foreclosed by measurement**: an agent writing its
  own tests *hurts* at a fixed model (ExecCritic, 61.2→57.3 %, patch and test errors
  correlate), and single prompt-instruction changes are null on Terminal-Bench 2.1 at
  13× our n (−1.50…+2.25 pp, arXiv:2610.22289). Google's Harness² (arXiv:2610.14224),
  reviewed at primary-source level (local reports `harness2-teardown.md`,
  `harness-steals-2026h2.md`), leaves the deferred search gate closed: no
  attempts-matched ablation, cost floor 4 executor rollouts + 3 improver sessions per
  task against a 1.3× tokens-per-solved rule, wins are per-task workspace-fact
  injection over 34–100 tasks × 3 streams with several deltas inside 1 SD, and it needs
  a mounted editable harness surface this binary lacks. **Correction to our own
  record:** "every harness-side dial that does not need a different model has been
  measured" holds only for *passive* dials; the state-triggered class was tested once
  (`VerifyHold`, 2026-10-06) and no-shipped on two cells that could not show the effect
  — a container-only task where local verification was structurally impossible and a
  grading instrument later voided as window-flaky. The roadmap now says "passive";
  `dphn-gate` is pre-registered as the arm testing enforcement-vs-suggestion where a
  named verifier test is a deliverable requirement.
- **What a 21-task slice can and cannot say (2026-10-19).** Binding rules from the
  eval-methodology lane. At n=21 the paired minimum detectable effect (80 % power,
  α=0.05) is **40.8 pp at one rep, 32.2 at two, 28.8 at three**, never below **20.4 pp**
  — an arm is not justified by a smaller expected solve-rate delta, and repetition is
  not a power lever (task coverage is; at equal budget, task-covering designs cut median
  estimation MSE 87 %). The honest headline for a single-attempt harness claim is
  **pass^1 with a paired, task-clustered CI**; pass@k is an order statistic (and on
  stateful agents a replay artifact), and applying it with unit-test counts as `n`
  inflates scores by 0.85–0.97 absolute. **Named-test counts are ledgers, never
  capability percentages**: one pilot reads 0.80 macro test-pass against 0.20 task
  resolution, and 19.7 % of previously passing patches are semantically wrong. A
  one-test delta at one rep is not a capability unit — a named test that flips in more
  than one rep with a causal deliverable change is. Verifier and environment variance is
  larger than the effects we chase (a reward-function defect moved success 94.3 % →
  65.5 % on replay; tool description text alone moves trajectories −5.3…−9.2 pp), so
  pre-flight classification and a known-answer control belong in provenance. Applied
  here: `dphn-gate` needs the named test to flip in at least two of three treated reps.
- **There is a cheaper-than-incumbent candidate model after all (2026-10-19).** The "no
  stronger model available" premise that parked the ceiling read is wrong on price and
  unproven on capability. Pricing pages read 2026-10-19: `qwen3.8-flash` $0.15 in /
  $0.016 cached / $0.47 out (open weights `Qwen3.8-Flash-Next`, 125B/6B active, 1M ctx,
  vendor SWE-bench Pro 62.5 vs DeepSeek-V4-Flash 56.0); `glm-5.3-flash` $0.15 / $0.03 /
  $0.50 (MIT weights, honours `reasoning_effort`); `MiniMax-M3` $0.30 / $1.20;
  `kimi-k2.6` $0.16 / $0.95 / $4.00. Every published benchmark row for these is a
  **vendor** claim, no independent Terminal-Bench 2.1 row exists for any candidate, and
  GLM's vendor 84.3 coexists with this repo's own 6/26 on a different harness and
  envelope — vendor numbers rank candidates, they do not predict our solve rate.
  Cached-token reporting is verified only for the incumbent, so cache accounting must be
  re-probed per model. Planned validation: $0 pre-flight then 12 treatment rows on the
  four semantic-failure cells (controls banked), ~$1 of tokens, switch rule ≥1 cell
  solved in ≥2 of 3 reps at ≤1.3× tokens-per-solved. Blocked on an Alibaba Model Studio
  credential, which this repo does not hold.
- **Reasoning effort is a prompt line, and our first arm measured the wrong channel
  (2026-10-19).** The vendor's MIT Rust toolkit
  (`github.com/deepseek-ai/deepseek-recipe`) renders effort *into the prompt*:
  `Reasoning Effort: {score} (range 1-100, the higher the value, the more thorough the
  reasoning)` prepended to the first message when thinking mode is on, low=50,
  absent/high/xhigh=75, max=100. Through the same gateway on one prompt: **576**
  reasoning tokens plain, **1,038** with the JSON field `reasoning_effort:"max"` (what
  the first arm sent), **2,327** with the vendor marker. One in-loop probe row on
  video-processing gave **19,495** reasoning tokens against 4,816-7,055 in every control
  row (2.8-4.0x) at the same 2/5 test floor. So `dphn-effort`'s mechanism failure
  (median 1.23x) was a **channel artifact**, its capability half is inconclusive for
  effort, and the axis is re-tested by `dphn-effort2` with the vendor line. Two
  consequences. (i) The model premise is settled the other way: `deepseek-v4.1-flash`
  is frontier-tier and its 1017 snapshot (2026-10-17) is a large step up (vendor card:
  Terminal-Bench 4.0 42.3 vs 31.2 for the prior snapshot, against 37.9 GLM-5.3 and 39.9
  GPT-5.6 Sol; DeepSWE 77.0 vs 74.2), so model swapping is retired (shortlist stays in
  local report `models-2026h2-shortlist.md`) and the failures this harness sees are its
  own to fix. (ii) Wire fidelity is now a work item, not an
  inference: a vendor Rust encoder exists for exactly the surface `provider-openai`
  hand-rolls, and it corroborates our `reasoning_content` belt (always present in
  thinking mode, empty allowed).
- **The initiative nudge layer is ablatable, and the "state-triggered gate" was already
  shipped (2026-10-19).** The `VerifyHold` declare gate is an ancestor of the current
  branch, so it ran in every in-container arm — it emits no event, so dumps never showed
  it, but the WAL records the hold text: it fires **6/6, 3/4, 4/4, 3/4** on the solved
  log-summary cell and **0/6, 0/4, 0/4, 0/4** on mcmc, video, custom-memory-heap-crash
  and make-mips. A declare-keyed gate needs a declare, and every headroom row dies on
  the token cap before issuing one, so that shape is structurally unreachable on the
  cells we fail — the pre-registered "add the gate" arm would have been
  gate-versus-gate. Reachable instead: read-only starvation — cmhc rows run 55-68 steps
  with 23-33 reads and 0-1 writes, query-optimize 9-20 reads with 0-7 writes, while the
  solved cell never exceeds a 2-read streak — and that nudge also already ships ("0
  edits so far after N actions. Stop reading. Apply your first edit NOW."). So the live
  question became a **deletion test**, and `--steer on|off` exists to run it (`off`
  keeps the workflow contract and the rollback notice, isolating the nudges from the
  contract; the value is recorded in report provenance so arms never pool; the shipped
  default was on, byte-identical, and is now `off` — see `dphn-steer2`, 2026-11-06).
  Registered arm `dphn-steer` (paused after 1/18 rows; frozen binary
  `pilot-steer/rof-steer`, md5 `eedca26f`, build `ca2c164`, 17 missing rows; later
  superseded 2026-11-06 by workspace seeding fix): on/off x 3 reps over cmhc,
  query-optimize and log-summary; passes for steering only if `on` flips a named test
  `off` never flips, in at least two of three reps, within 1.3x tokens per test-passed;
  if `off` is never worse the default flips off and the nudges are flagged measured-dead
  rather than deleted (they remain the substrate for any future gate).
- **Reliability fixes (`32f6b36`, 2026-11-03) and steering feasibility pilot
  (2026-11-05, voided 2026-11-06).** Replaying historical event logs against old and
  new `32f6b36` parser sources (`fix-plan.md`) showed 74 argument differences in 2,609
  exec calls across 86 completed logs (no verdict change established; recorded output was
  already condensed). A local scripted-provider check on real containers graded a
  synthetic known-answer task Resolved, a failing task Unresolved, and a reused dir with
  planted stale success Unresolved. In the 2026-11-05 `--streak-steer` feasibility pilot
  (`steering-experiment.md`), an initial HTTP 403/1010 Cloudflare block from the proxy's
  Python user agent was fixed by forwarding the harness user agent; the proxy refuses the
  next request immediately after any unknown usage. One eligible heap control finished
  (Unresolved, 0/6, 286,477 input and 19,368 output tokens) before stopping on unknown
  usage; the $10 campaign stopped without an eligible result ($4.83727350 accounted or
  reserved across five unknown-usage reservations). The earlier ledger retained
  $3.80367360 for three 403/1010 attempts plus a full reservation for another
  unknown-usage attempt, $4.30645020 accounted or reserved before this campaign; all
  reservations carry into any newly registered campaign.
- **In-container runs grade the image's `/app`, never the Docker build context
  (2026-11-06, measured).** `fresh_workdir` seeded every workdir from
  `<task>/environment`, the build context; harbor mounts the image's `/app`. On the five
  cells the arms used, that seed differs from the real workspace on four: heap
  `environment/` = {Dockerfile, patches/, program/} vs image `/app` = {main.cpp,
  user.cpp, user.h} (its verifier md5-checks and compiles `/app/main.cpp`, so every row
  was a structurally impossible 0/6); mcmc {Dockerfile, task-deps/} vs {data.csv};
  query-optimize {Dockerfile, task-deps/} vs {my-sql-query.sql, oewn.sqlite};
  log-summary {Dockerfile, log_generator_deterministic.py} vs {logs/, 164 files, 6 MB}.
  Only video-processing coincided. A banked paid row confirms it in-run: the model's
  first `ls -la` in `/app` returned `Dockerfile patches program`. Decision: in-container
  workspaces reset from the image (`cp -a /app/.`) and chowned to the host uid so
  host-side `edit`/`write` still work; host mode keeps `environment/` because it has no
  image. Validated with zero model calls — heap: 6 tests collected, pristine 0/6 reward
  0, gold solve 6/6 reward 1; log-summary: 2 tests, pristine 0/2, gold 2/2; both
  verifiers run (the earlier `uv` failure was transient network: `apt-get update` and
  the astral download both work).
  CONSEQUENCE: `dphn-effort`, `dphn-effort2` and the banked `dphn-steer` row measured a
  different task on those cells; their paired arms stay internally valid but no
  benchmark-task capability claim survives, and the read-starvation pathology behind the
  streak trigger is partly this bug. The registered streak campaign is dead under its
  own verifier-integrity stop rule and needs re-registration after a
  corrected-workspace baseline.
- **`/tests` is uploaded at grading, never mounted for the agent phase (2026-11-06,
  measured against harbor).** The container mounted the task's tests read-only at
  `/tests` for the whole run, and a baseline row opened `/tests/test_outputs.py` before
  starting work — the grader was an input. The installed harbor source is explicit:
  `Trial._agent_env_mounts` binds only the verifier `/logs`, the agent dir and the
  artifacts, and `Verifier.verify()` uploads `tests_dir` at grade time. Decision:
  `env_container_run_args` no longer mounts `/tests` (unit-pinned) and
  `copy_tests_into_container` uploads the *frozen snapshot's* tests via `docker cp`
  immediately before `bash /tests/test.sh`, so identity stays the snapshot's, not the
  mutable source's. Rehearsed without model calls: no `/tests` at run start, pristine
  workdir 5/6 reward 0, gold solution 6/6 reward 1. CONSEQUENCE: the three
  `dphn-seedfix-base` rows had the grader visible; outcome unchanged (0/3 resolved, all
  at the 5/6 floor) but no row run before this fix is a leakage-free capability reading.
- **cmhc's floor is 5/6, so named-test counts alone are unreadable there (2026-11-06,
  measured).** An image-seeded pristine workdir with no agent work scores 5/6 reward 0:
  `test_protected_files_not_modified`, `test_program_compiles_debug/_release`,
  `test_debug_build_runs_without_crash` and `test_no_memory_leaks_with_valgrind` pass on
  the shipped sources, and only `test_release_build_runs_without_crash` — the task's bug
  — fails. Gold `solve.sh` reaches 6/6 reward 1. So the corrected 3-row baseline
  (`dphn-seedfix-base`, $0.32: Unresolved 5/6 x3, 286-293k tokens, halt=tokens, 41-44
  steps, 14-19 reads / 0-3 writes, zero nudges) shows the model did not solve it and did
  not break the floor; it is not a near-miss (the old 0/6 was below floor — the seed
  failed tests the untouched sources pass). Rule: report pass^1 and the floor delta on
  cmhc, never a named-test count as progress.
- **Pipeline re-established (`dphn-seedfix-base2`, 2026-11-06, 3 rows, $0.14).**
  `log-summary-date-ranges` scored 3/3 Resolved, 2/2 tests (124s/112s/69s, 14-32 steps,
  72-226k tokens, 1-5 reads / 2-9 writes), zero `/tests` mentions, ctrf in all three —
  pipeline check, not capability. Spend $0.5786 across 6 banked rows + 3 upload-failure
  + 1 provider-404 under `pilot-seedfix/void/`.
- **The initiative nudge layer ships off: measured dead, and a 2.64x tax (2026-11-06,
  `dphn-steer2`, binary `a6da984`, md5 `b2f696dc`, 12 rows, $0.89).** Ablation on the
  corrected pipeline (image-seeded `/app`, tests uploaded only at grading), two cells
  with measured baselines, 3 reps per arm, arms differing by `--steer on|off` only,
  matched order r1 on→off, r2 off→on, r3 on→off. Capability identical — cmhc 0/3 both
  arms, every row at the 5/6 floor, `test_release_build_runs_without_crash` never passed;
  log-summary 3/3 both arms at 2/2; no named test differs in any matched pair. Cost on
  the solved cell: 175,657 vs 66,620 gross tokens per solved task (2.64x), median 22 vs
  11 steps (2x), 155s vs 56s (2.7x wall); cmhc token-capped both arms (0.98x). Mechanism
  verified from the WALs: zero nudge text in all 6 `off` rows; `on` fired in all 6
  (log-summary declare hold 2/1/2 + wrap-up 1x per rep; cmhc wrap-up 1x, plus one half
  directive in r3 — the half directive still needs 60 actions, which cmhc reaches only
  at the margin). Decision: `steer` defaults to false on both CLI surfaces (`rof run`,
  `rof eval`); `--steer on` re-enables the declare hold, the action directives and the
  wrap-up; the workflow contract and the rollback notice are untouched; the nudge code
  stays as substrate for any future state-triggered gate. Limit: 2 cells x 3 reps is a
  deletion check on those two tasks, not a capability estimate, and the declare hold
  firing 6/6 on solved rows shows it is active where it was supposed to help and still
  bought nothing.
- **Wire-fidelity diff against vendor `deepseek-recipe` @ `935817c6`; one claim refuted
  by our own data, three recorded as measured, nothing changed (2026-11-06, zero model
  calls).** Report: `wire-fidelity-2026-11-06.md` (local-only; untracked workspace
  report); the vendor is a serving-side prompt renderer, not an HTTP client, so the
  layers only partly overlap. (1) *effort is a prompt line* — already measured on both
  channels (`dphn-effort` JSON field 1.23x, `dphn-effort2` vendor marker 1.26x against
  the 1.5x mechanism rule); the vendor template `Reasoning Effort: {1-100}` matches the
  marker we sent, confirmation not new work. (2) *cache-token field mismatch*
  (`prompt_cache_hit_tokens` vs our `prompt_tokens_details.cached_tokens`) — **refuted
  by banked events**: the endpoint emits the OpenAI shape and we parse it, with non-zero
  `cache_read_tokens` in real rows (1,216 / 2,624 / … on a single cmhc row), so the cost
  guardrail is not pricing every input token at the miss rate and `--budget-shape
  miss-plus-out` is usable. (3) *`finish_reason=length` becomes a `BodyFail` and re-runs
  with a doubled `max_tokens`* — deliberate, bounded (one thinking-off rung, then a
  32,768 ceiling), and the double billing IS accounted (`retry::merge_usage` sums
  attempts); frequency across every banked event log: 3 rows total (2 of today's, 1 old
  GLM), both of today's still graded (one Resolved, one at the 5/6 floor), so no
  evidence of capability loss and no change without an arm. (4) Runner-up, latent not
  live: a plain 400/422 falls through the fail-open `classify_message` table and would be
  retried although DeepSeek documents both as "modify your request"; zero 400/422 rows
  exist in the banked set (only 402 and 504 halt codes), so it stays recorded with its
  one-line fix, not implemented.
  > Now (implemented 2026-11-12): plain 400 and 422 classify as `ErrorClass::Fatal` in
  > `crates/provider-core/src/errors.rs` and are no longer retried.
- **The agent phase is now bounded by the task's `[agent] timeout_sec`, like harbor
  (2026-11-06, third in-container fidelity gap).** `inst.timeout_secs` bounded only the
  verifier and the harbor grade run; the agent leg had no wall clock, so it ran until
  token/step caps fired. Measured in the first map row: `mcmc-sampling-stan` was still
  working at ~2,100s where harbor's 900s agent timeout would have ended it, and older
  banked mcmc rows reached 971s — more generous than the benchmark, not comparable with
  a harbor run. Fix: `execute_with_deadline` (the same cancel token Ctrl-C uses, so the
  loop still ends `Outcome::Cancelled`, emits `RunEnd` and leaves a gradeable workdir —
  a dropped future would look like an infra failure), wired from the agent leg with
  `Instant::now() + inst.timeout_secs`, and `halt_reason` records `agent wall timeout
  (task.toml agent.timeout_sec=Ns)` so a timeout row is never read as a model failure.
  Test: an expired deadline cancels and still emits `RunEnd`; a future deadline is
  inert. Rows with walls under their cell's limit are unaffected (heap 240-501s,
  log-summary 56-195s against 900s); the one mcmc row that ran unbounded is kept as
  void.
- **Timeouts were never read from `task.toml`: every cell silently used the 1800s
  default (2026-11-06, measured).** `slices::load_tb_slice` looked for
  `timeout`/`timeout_secs`/`time_limit`, but task.toml says `timeout_sec`, in BOTH
  `[verifier]` and `[agent]` with different values per task: video-processing 3600/3600,
  query-optimize 1800 verifier vs 900 agent, log-summary 900/900, mcmc and heap
  1800/1800. So both the verifier budget and the new agent wall clock were the 1800s
  default — too tight for video (would cut a legitimate 3600s agent phase), too loose
  for query-optimize (900). Fix: section-aware `section_int` reads `[verifier]
  timeout_sec` into `Instance.timeout_secs` and `[agent] timeout_sec` into the new
  `Instance.agent_timeout_secs` (serde default keeps old JSON readable); the agent
  deadline and its halt reason use the agent value. Test pins a task.toml whose two
  sections disagree. CONSEQUENCE: earlier rows ran under 1800s for both phases; every
  banked wall is far under 900s except the mcmc row this exposed, so no reading changes.
- **`mcmc-sampling-stan` is a blocked cell, not a headroom cell (2026-11-06,
  measured).** Floor scan: pristine image scores 2/6 reward 0
  (`test_posterior_alpha_estimation`, `test_posterior_beta_estimation` pass on the
  shipped sources) and its reference `solution/solve.sh` FAILS in-image — `Error in
  library(rstan): there is no package called 'rstan'`, because the install needs
  sudo/apt plus a source build of rstan 2.32.7 and silently produces nothing. A map row
  then burned 291,445/13,467 tokens over 1,743s (49 steps, halt=tokens) and its verifier
  exec HUNG for the full 2,100s timeout with no ctrf. Harbour's own agent envelope is
  1800s, so install plus sampling cannot fit. Decision: drop mcmc from cell selection;
  the old "rstan-env class — uninformative" readings are now explained rather than
  mysterious.
- **`--exec-policy any` re-tested on the corrected pipeline: still mechanism-only
  (2026-11-06, `dphn-env1`, 3 heap rows, $0.31).** All three Unresolved 5/6 with
  `test_release_build_runs_without_crash` failing (40/46/39 steps, 286-298k tokens,
  408-561s), identical to their allowlist controls. Mechanism proven in the trace: with
  `any` the model ran `which valgrind` and `gdb -batch -ex run -ex bt ./release` — tools
  the census shows the allowlist had been denying — and recorded zero denials. It had
  the task's own debugging toolchain and still did not fix the crash, so denials are
  friction, not a capability ceiling. The October version of this verdict is only now
  trustworthy: it ran before the workspace-seed fix, when heap's floor was 0/6 and no
  test could move.
- **Two in-container bugs found by the first paid row on a fresh 0/N cell (2026-11-06,
  `dphn-solve1`, kv-store-grpc).** (1) *Git refused the workdir*: the seeding fix chowns
  `/app` to the host uid (so host-side `edit`/`write` can write it) while the container
  runs as root, and git rejects a repo owned by another user — measured `fatal: detected
  dubious ownership in repository at '/app'`, same text already in three banked rows
  (`pilot-steer2/on-r1-custom-memory-heap-crash`, two `make-mips-interpreter` rows).
  Fix: after the container starts, a best-effort `git config --system --add
  safe.directory /app` inside it (pinned argv; images without git are a no-op). (2) *An
  EMPTY workdir had no baseline*: kv-store-grpc's image ships an empty `/app`, so
  `ensure()` had nothing to commit, HEAD stayed unborn, and every row carried
  `halt_reason: snapshot rollback: … ensure() has not recorded a start HEAD` — including
  rows that RESOLVED 7/7. Fix: `commit_all` creates an empty baseline commit when the
  tree is empty, and `rollback` skips the pathspec restore when the baseline has no
  tracked paths (`clean` is the whole rollback there). Tests: an empty workdir yields an
  empty patch and a real patch once the agent creates files, and rollback clears it.
- **Cell portfolio from the zero-cost floor scan: 7 of 9 candidate cells trustworthy, 2
  of 9 with defective reference solutions (2026-11-06).** Every measurement is
  pristine-reward 0 with ctrf produced (no verifier failures), so a pass is real signal
  rather than a partial-credit floor: dna-assembly 0/1 → gold 1/1; kv-store-grpc 0/7 →
  7/7; raman-fitting 0/3 → 3/3; model-extraction-relu-logits 0/1 → 1/1;
  hf-model-inference 0/4 → 4/4 (verifier 24-28s, fastest cell measured);
  break-filter-js-from-html 0/1 → 1/1; make-mips-interpreter 0/3 → 3/3. EXCLUDED for
  broken gold: `protein-assembly` gold 0/1 (its DNAChisel solve is stochastic, no
  gblock.txt) and `sanitize-git-repo` gold 2/3 (its own solution trips
  `test_no_other_files_changed`), plus `mcmc-sampling-stan` (no rstan in-image). Gold on
  service tasks: `hf-model-inference`'s deliverable is a running Flask server, so gold
  scores 4/4 only when solve.sh runs INSIDE the graded container — a throwaway container
  takes the server down and grades 1/4, exactly the class in-container mode exists for.
- **Corrected-pipeline capability map, first complete version (2026-11-06).** All on the
  faithful pipeline (image-seeded `/app`, tests uploaded at grading, per-task
  agent/verifier budgets, git-safe workdir, in-container package and service
  persistence), shipped defaults, 300k/60/120: log-summary-date-ranges **3/3 solved**
  (ceiling), kv-store-grpc **3/3 solved** (0/7 floor, 0.01/row), hf-model-inference
  **2/3** (r1 wall-timeout at 900s, r2/r3 solved in 309-352s — path variance, not a
  capability gap), custom-memory-heap-crash **0/3** at its 5/6 floor with the crux test
  failing even when `--exec-policy any` let the model run `gdb`/`valgrind`,
  video-processing **0/1** at its 2/5 floor, query-optimize **0/1** (wall-clocked at its
  900s agent budget after 12 steps). Excluded: mcmc-sampling-stan (no rstan in-image),
  protein-assembly and sanitize-git-repo (defective gold). So this harness solves
  ordinary mid-difficulty tasks at this model tier and not the three legacy "headroom"
  cells — a statement about the cells, not a missing harness lever.
- **The budget/envelope axis is closed on measured evidence (2026-11-11).** Three arms,
  all null or void: miss-plus-out alone 0/4 solves at 1.42x cost (`budget-shape-ab1`);
  the joint arm (miss-plus-out + 120/240 steps, same 300k value) 0/4 cell-reps, guardrail
  1.79x/3.05x, its registered KILL triggered, its deepest row halted `done` at 113 steps
  with 73,508 of the cap unspent (`dphn-cap-ab1`, since marked VOID for a
  workspace-fidelity defect); raising steps at a fixed token cap bought 0/10 -> 2/10,
  then 7/10 died on the token cap. A 253-dump replay shows the killing cap is cumulative
  across ~27-40 requests of ~11k tokens, i.e. not a context rule, and the 2026 evidence
  says budget and information SUBSTITUTE rather than complement (BRA Table 5: 21/36
  interactions negative, none positive). A decision review chose a re-run arm; it was
  superseded the same day once the checklist result entered the brief. [measured:
  docs/experiments/2026-11-11-envelope.json,
  docs/experiments/2026-11-11-lit-semantic-deliverable.md,
  docs/reviews/decision-2026-11-11-1134.md]
- **A `--completion-check` predicate has no reachable trigger on this corpus
  (2026-11-11).** A zero-cost triage of 242 banked rows (183 in-container): of the 12
  post-fix rows, every one reaching `done` already passes, and no post-fix row both
  declares done and fails — failing rows die on the token cap mid-work (raman 0/3 at 30
  steps, session-window-debug 4/9 at 107, wal 1/2 at 81). Cells a predicate could target
  are accuracy-bound: custom-memory-heap-crash (6 named tests never passed by any arm),
  query-optimize (5), video-processing (3), session-window-debug (3),
  make-mips-interpreter (3), mcmc (1), each including its deepest-envelope row. The
  headline "32 false rejections" is a capture artifact, not predicate error: 0 of 14
  git-snapshotted passing workspaces are rejected against 32 of 33 files-only captures.
  [measured: docs/experiments/2026-11-11-triage.md; failure class justified from public
  `instruction.md` only, never from test files or verifier stderr]
- **Spend stays banked; the binding constraint moved from the harness to the evidence
  base (2026-11-11).** Five levers are closed (envelope x3, checklist, predicate), so the
  next dollar cannot buy a capability answer on this slice. Rival per-task rows exist on
  disk for exactly 2 tasks (session-window-debug, layout-config-recreation2) x 2
  harnesses x 3 reps; only 12 of 183 in-container rows are post-fix. A same-model
  comparison needs task count, not another internal arm. Balance $3.12, a fresh top-up —
  not a remainder, not a reset of the earlier $2.519179098 upper bound. [measured: triage
  header, rival tree inventory; chosen: triage-first spend order]
- **`RunProvenance` records how the workdir was seeded (2026-11-11).** 169 of 183 banked
  in-container rows were unusable for any capability question: the agent was seeded from
  `<task>/environment` — the Docker build context — rather than the image's `/app` the
  verifier grades, and nothing in the report said which. `workdir_seed` now records
  `ImageApp` or `BuildContext`; optional with a serde default, so every pre-field report
  reads as unknown, never as faithful, never pools with one.
- **Container egress to `github.com` was blocked by routing, not by the tasks
  (2026-11-11).** Two oracle pre-flights on `break-filter-js-from-html` scored 0 with
  `curl: (35) Recv failure` on the uv tarball and `uvx: command not found`; 18 of 21
  Slice A graders bootstrap uv from `astral.sh` -> `github.com`. The host downloaded the
  same 21,370,871-byte file fine. Cause: `ip rule 31269: not from all fwmark 0xcb6b
  lookup 52075`, where table 52075 is a full-tunnel `default dev laptop`; host traffic is
  marked and goes direct, container traffic is unmarked and enters a tunnel that cannot
  serve `20.205.243.166`, while other GitHub edges (`140.82.121.4`) work. One root rule
  restored parity: `ip rule add to 20.205.243.0/24 lookup main pref 31000`; a container
  then fetched the tarball byte-identically. This also explains the historical "verifier
  network errors at grading" rows on install-windows and mteb. [measured: two oracle
  runs, `ip rule`/route inspection, in-container host matrix]

**Provenance note (2026-11-11).** `research/` is excluded via `.git/info/exclude`, so
`research/DECISIONS.md` and the arm reports it cites never reach this repository. The
strategy-level decisions above are therefore repeated here, in the tracked log, with the
artifact paths that *are* tracked. A session reading only `research/DECISIONS.md` is
missing the reviewable record, and vice versa — both exist on purpose, but only this one
survives a clone.

**Scope reversal (2026-11-12, owner request).** MCP and an interactive surface were out of
scope. The owner asked for a minimal core of the product surface: four opt-in pieces, all
default off, none benchmark-validated — `rof chat` (line loop, not a TUI), `--resume LOG`,
`--stream on` (provider SSE, display only), `--mcp` (one stdio server per flag). Product
surface, not capability bets: no benchmark row uses them, none is a promotion candidate on
solve rate. Still out: full-screen TUI, MCP HTTP transport/auth/resources, session picker,
cross-run memory, planner/MAS. [chosen: owner directive; validated only by
scripted-provider and scripted-server tests]

**Scope reversal (2026-11-13, owner request).** "No planner/MAS scaffolding" stood in
`AGENTS.md`, and planner/MAS was listed as still out above. The owner confirmed a
configurable agent topology: roles and `spawn`/`message` edges in a JSON file (`--agents
FILE`), one runtime with `spawn` and `wait` tools, budget carved from the parent. Stages:
(1) tree at depth 1 with read-only children, (2) deeper tree with one worktree per writer,
(3) peer `message` edges. Default off; nothing benchmark-validated, no baseline row uses
it. JSON rather than TOML because `serde_json` is already a dependency. The `AGENTS.md`
rule still applies elsewhere: no planner, no multi-agent code outside this flag. [chosen:
owner directive; stage 1 validated only by scripted-provider tests]

- **Uncapped cumulative tokens, USD spend cap CLI/Harbor wiring, omitted `max_tokens`, and DeepSeek 384K output ceiling (2026-11-25).**
  Across 11 coding agents/harnesses verified from primary sources (Claude Code, Codex CLI,
  OpenCode, Pi, Goose, mini-SWE-agent, SWE-agent, OpenHands SDK, Terminus-2, Hermes Agent,
  Aider/Roo Code), **0 of 11** enforce a hard cumulative gross token cap by default; 7 of 11
  omit `max_tokens` on OpenAI-compatible endpoints; and DeepSeek's live docs (`api-docs.deepseek.com`,
  2026-11-25) specify a 1M context window, 64K default thinking output when `max_tokens` is
  omitted, and **384K (`393_216`)** max output tokens. In our 48-run TB4.1.0 CPU screen
  ([2026-11-25-tb41-option-screen](experiments/2026-11-25-tb41-option-screen.md)), 47/48 runs
  halted on the 300k gross token cap at 25–33 turns ($0.038 live USD/run), 1/48 halted on
  `MAX_TOKENS_CEILING = 32_768` truncation under `--keep-thinking`, and default `5/2`
  prefix-mutating retention averaged only 42.0% DeepSeek cache hit rate (dropping from 69–79%
  on turns 2–3 to 12–15% on turns 4–6). Implemented without changing defaults:
  (1) `--budget-tokens 0` / Harbor `budget_tokens: 0` uncaps cumulative tokens and omits the
  token fragment in `budget_line()`; (2) `--budget-spend-cents N` / Harbor `budget_usd` sets
  the USD spend cap (`0` disables it); (3) `--max-tokens 0|omit` / Harbor `max_tokens: 0|None`
  omits `"max_tokens"` on the wire; (4) `MAX_TOKENS_CEILING` raised from `32_768` to
  `393_216` (`OMITTED_FIRST_RAISE = 131_072` when omitted `max_tokens` truncates). Skipped
  a continuation loop above 393k: max observed reply across 342 runs is 61,518 tokens, and
  retrying past 384k risks ~$0.47/reply runaway loops. [measured: 48-run screen + 342-run
  corpus; verified-from-source: 11 harnesses + DeepSeek docs; defaults unchanged until
  benchmarked]
- **SSE per-chunk idle timeout, recovered-attempt `usage_unknown` accounting, and opt-in Harbor streaming (2026-11-25).**
  In [2026-11-25-tb41-uncapped-calib](experiments/2026-11-25-tb41-uncapped-calib.md),
  `append_1000` turn 5 (`dt = 1,727 s`, `attempts = 4`) hit `reqwest`'s whole-request 600 s
  timeout twice while DeepSeek generated >64K reasoning tokens, returning `usage: None` and
  silently retrying for `+$0.30` unrecorded server spend (`$0.88` live vs `$0.58` recorded).
  Fixed without changing defaults: (1) `OpenAiCompat::new` drops the client-wide `.timeout(...)`
  so SSE streaming (`complete_streaming`) bounds only header-wait and per-chunk idle gaps via
  `tokio::time::timeout(self.request_timeout, ...)` (non-streaming keeps per-request `.timeout(...)`);
  (2) `provider_core::Response.usage_unknown` (default `false`) propagates dropped/timed-out
  in-flight retry attempts so `agent-loop` emits `provider-usage-unknown` and clears `cost_usd`
  on both recovered and failed timeouts/cut streams; (3) Harbor `RofOptions.stream: bool = False`
  forwards `--stream on` when opt-in enabled. [measured: TB4.1 calibration trace; verified by
  scripted-server idle-timeout and agent-loop tests]

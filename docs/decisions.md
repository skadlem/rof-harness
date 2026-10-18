# Decisions

Distilled from the code comments and tests that state them. Anything
here marked chosen-to-be-validated lacks a measured anchor; the code
says so explicitly.

- **Deliverable is the full uncut patch.** `rof` prints
  `snapshot::patch_since_start_full()` on stdout; the 8KiB/200-line
  evidence bound stays internal to the loop. A regression test in
  `main.rs` pins this (the old `patch_since_start` cut stdout).
- **Strict vs Lenient eval grading.** `rof eval` grades with Strict
  patch-apply by default; `--lenient-apply` records Lenient provenance
  in the report. Strict grades are not comparable with earlier
  fuzz-lenient numbers.
- **WAL is on by default, fail-closed.** The durable event log lives at
  `<workdir>/.rof-events.jsonl`; `--log-path` moves it, `--no-log`
  disables it and wins over the override.
- **`--compaction` is opt-in and unvalidated.** Absent the flag, the run
  stays byte-identical to the no-compaction default. The S-1 compaction
  experiment has not run; the flag exists for it only.
- **Fold knobs resolve at the edge.** `THINKING_KEEP` /
  `COLLAPSE_HYSTERESIS` fill in only when the matching flag is absent;
  a present-but-unparsable value is a usage error, never a silent
  default. The loop never reads the environment.
- **Workdir guard is refused-by-default.** `/`, `$HOME`, the harness
  checkout, and repos with uncommitted changes are refused (exit 2)
  unless `--allow-dirty-workdir` is passed; pre-existing dirt then
  appears in the reported patch, so dirt never leaks in silently.
- **Agent-leg eval budgets default to the frozen pilot recipe.** `rof eval
  --agent` fills 300000 tokens / 60 steps / 120 actions when the user
  omits the budget flags (chosen: copied from the frozen pilot recipe in
  NEXT.md; validated when the agent-leg slice runs). Explicit flags win,
  and `cli::budget_for` stays the single resolver.
- **The goal rules tail gained a stdin clause after the 26-cell sweep.**
  Agents issued heredoc execs (`python3 - <<'EOF'`) — silent exit-0 no-ops
  under exec's null stdin — and built conclusions on them (56 execs, zero
  deliverable writes). The tail now states stdin is closed and points at
  the write-script-to-file fix; the banked sweep cells ran the old tail
  (arm marker).
- **Second provider wired: GLM-5.3-flash priced at Z.ai list rates.**
  The price book keys `glm-5.3-flash` at cache-read $0.03 / input $0.15 /
  output $0.50 per 1M (chosen: Z.ai list rates effective 2026-09-10,
  corroborated by three independent pricing pages; validated against the
  first invoice — a proxy may mark them up). `--keep-thinking` on both
  `rof run` and `rof eval` builds the request profile without the
  DeepSeek-specific thinking-off body, for providers that reject the
  fragment or when reasoning is wanted; the default body stays
  byte-identical. Precision on when the default sends the fragment: only
  compaction-summary requests (`Thinking::Off`) and the truncation ladder's
  first rung carry it — ordinary requests are built with `Thinking::Auto`
  and send no thinking fragment. Unpriced models still report null cost, never zero.
- **In-container eval is a separate mode with a separate estimand.**
  `rof eval --agent --in-container` starts the task environment container
  before the agent run (workdir bind-mounted at /app, tests read-only at
  /tests, cpus/memory capped at 2/4096m — chosen: the caps observed in the
  task specs; upgrade path: parse [environment] from task.toml per task),
  wraps the run's tool policy with `docker exec <container>` so exec/test
  calls land inside (the allowlist and shell-op guard still see the
  unwrapped argv; the wrap applies at spawn only), and grades by execing
  /tests/test.sh in the SAME live container. The env-mutation task class
  (installed-by-the-agent imports, a live server) becomes winnable.
  Verdict provenance in this mode is the task-written reward file
  (`/logs/verifier/reward.txt`, mounted writable at `<home>/verifier-logs`)
  with the verifier exit code as the no-artifact fallback — a NEW
  estimand, never mixed with the harbor result.json reward rows.
  MEASURED 2026-10-12 (26-cell arm audit): every TB2.1 slice test.sh
  ends with `echo 1|0 > /logs/verifier/reward.txt`, so the script ALWAYS
  exits 0 — exit-code-only grading read workless runs as Resolved (three
  seed-identical-workdir rows "Resolved" at reward 0; the banked 8/8 was
  2/8 by reward). The reward file decides whenever it exists; a junk
  reward withholds as Ambiguous, never falling back to the lying exit
  code; absence keeps the exit-code fallback so exit-propagating tasks
  stay byte-identical. A present reward file is trusted because the
  instance home is fresh per cell with exactly one grading (no staleness
  window). Ceiling: timeout/cancel kill the docker-exec client; the
  in-container process may outlive it (upgrade path: docker kill or
  exec-session tracking).
- **The harbor CLI is the agent-leg grading seam.** `rof eval --agent`
  ports the bash pilot driver's mk_goal/verdict verbatim: job yaml (JSON)
  plus `harbor run -c <job> -y -q`; harbor spawn failure, grading timeout
  without result.json, workdir refusal, or a run with no RunEnd in the
  events dump is InfraFailure — infra is never capability. Live-smoked
  2026-10-08 end-to-end (swd, DeepSeek direct, deepseek-flash): current
  harbor writes `reward_stats.reward` as a `{value: [trials]}` histogram,
  not the pilots' scalar — the Rust reader takes both shapes and fails
  closed on anything else (the old bash drivers' reward lines print the
  dict on current harbor; fix their parse or use `rof eval --agent`
  before resuming them).
- **The workdir guard enforces exactly the four refusals.** Root, `$HOME`,
  the harness checkout, and dirty-without-opt-in; disposability of every
  other directory is documented operator responsibility (threat model),
  not an enforced property.
- **Exec children run niced, never capped.** Every agent-spawned host
  child takes nice 19 at spawn (descendants inherit): sweeps stop
  making the desktop unusable during builds without changing task wall
  times on an idle host (measured trigger: sweep-cell builds drove the
  14G/12-core host to global OOM and desktop stalls on 2026-10-08; the
  grading containers were already spec-capped — a docker scope peaked at
  125M). Affinity/cgroup caps would change measurements and are the
  operator's isolation job; memory stays uncapped by design.
- **Exit codes.** 0 done; 2 usage/parse/workdir refusal; 3 run failure
  (halted, cancelled, provider error — Provider keeps the historical 3);
  4 missing credentials; 5 log failure; 6 snapshot failure; 7 input
  failure.
- **Bets are feature-flagged.** `--bets` opts in; without it the batch
  commits as before. Bet A (proof gating) needs Bet B's claim field, so
  ablation order is B then A; Bet C (candidate racing) stays flagged and
  unbuilt. Read-only batches carry no hunks and commit.
- **Presets.** UnattendedBatch 20/12 is measured; LongTask 40/25 is
  chosen-to-be-validated. The context-pinned 200k token floor is
  chosen-to-validate (pinned-prefix floor ~4-5k tok/req x 20-30 requests
  plus reasoning completions); explicit flags always win.
- **Explicit budget flags own the whole budget.** Any of
  `--budget-steps` / `--budget-actions` / `--budget-tokens` lifts the
  UnattendedBatch 900s wall to the LongTask 3600s allowance: the wall is
  the measured guard for 20-step default batches, never part of a flagged
  recipe (measured 2026-10-08: v0 sweep cells ran 207–816s and all bound
  on the token cap; one fresh 60-step/300k cell wall-halted at 21 steps,
  24% of its token budget — 300s exec timeouts burn the 900s wall in
  three builds). Flag-less runs keep the preset wall.
- **Dump semantics.** `--dump-events PATH` replaces any previous dump at
  PATH (one run, one dump), truncating at start and appending per event.
- **Reports self-describe: the provenance block is part of the report
  contract (2026-10-13).** `RunReport.provenance` carries the binary's
  short git sha (embedded at build time; "unknown" when git is
  unavailable, never fabricated — a pinned test fails if it goes
  silently absent), the strict `Estimand` enum — `Ungraded` (plain
  eval: no grading path runs, rows are typically `ErrorNoReport`),
  `HarborOracle` (`--agent`, harbor `result.json`), `InContainerReward`
  (`--agent --in-container`, task-written reward file with exit-code
  fallback) — wired once at the shared report tail both legs use, the
  agent-leg budget recipe (explicit flags over the frozen pilot
  defaults; an `Ungraded` run never applies it), the token-cap shape,
  and the apply mode. Reports written before the block existed (no
  key) still deserialize as `provenance: null`. The estimand labels
  were verified against the actual grading seams by fresh review
  (2026-10-13), including that the always-zero-exit misgrade class of
  2026-10-12 is not reintroduced.
- **Budget-shape is a separate estimand, OFF by default (2026-10-13).**
  `--budget-shape gross|miss-plus-out` (one spelling source shared by
  the `run` and `eval` tables). `gross` = input+output, bit-identical
  to the frozen recipe by construction (the default arm selects the
  literal pre-change expression). `miss-plus-out` charges
  `(input - cache_read) + output`, saturating, at the single metering
  site — so provider-cached prompt re-reads stop spending the token
  cap (measured 2026-10-08 forensics: 90–98% of the 300k cap is
  context re-reads). Reported usage totals and dollars stay gross:
  only the cap moves. `cache_read ⊆ input` is a property of the
  OpenAI-shaped wire (the adapter maps `cached_tokens` inside
  `prompt_tokens`, the same miss share pricing charges), not a
  documented provider-core invariant — an adapter that reports cache
  reads outside `input` must not use `miss-plus-out`. A miss-plus-out
  arm never shares a table with a gross arm; the shape is a required
  field on every provenance block (a block without it fails loudly
  rather than silently reading as `Gross`). The arm is unvalidated;
  the first run is owner-gated spend.
- **Rivals-grid comparability rules (grid defined 2026-10-13).** A
  measured inventory of the stored data (204 cells) defines which
  tables are legal: per-estimand and per-arm only, never mixing
  harbor-graded rows with in-container reward-file rows, never mixing
  arms (different binaries, models, or recipes). Cross-harness cost is
  NOT computable — rival per-task logs carry zero usage fields
  (verified 2026-10-13) — so the cost axis stays within-rof, and
  pass-rate deltas at 3 reps over 2 shared tasks sit below the noise
  floor (Wilson ±28–37pp). Recount of the 26-cell in-container arm
  from its summary artifact: 6 Resolved / 19 Unresolved / 1 infra,
  $0.76 — an earlier note reading 6/17/1 summed to 24 and was an
  arithmetic slip. The recorded-exit verdict layer is dead on this
  slice; the reward-verified layer supersedes it everywhere.
- **Budget-shape arm 1 measured (2026-10-14): gate failed, knob stays
  OFF.** The pre-registered pass rule was more solves at ≤1.3×
  dollars-per-solved vs the banked gross rows. Measured on the 4
  near-miss cells (glm-5.3-flash, frozen recipe, in-container,
  matched config, n=1/cell/arm): solves 0/4 vs 0/4, tests flat
  (video 2p both) to down (mcmc 2p→1p), arm cost 1.42× ($0.1693 vs
  $0.1196). The mechanism is real — both previously token-dead
  cells migrated to the step cap and consumed 1.60×/1.70× the gross
  arm's input (466k/493k, cache reads 246k/207k) — but the extra
  working context did not convert to capability at this model tier.
  Structural read: at step death the miss+out charge stood at 84.6%
  and 99.8% of the 300k cap, so the recipe binds both caps nearly
  together; raising either alone just moves the wall. The shape is
  a required provenance field, so miss-plus-out rows can never be
  silently pooled with gross rows.

## Not inferable from code

- The numeric anchors for the LongTask and context-pinned defaults above
  (no measurements in-tree).
- When Strict-by-default eval grading becomes the permanent baseline.
- Whether Bet C will be built, and on what timeline.
- **Compaction premise refuted before spend (2026-10-16).** The landed
  `--compaction FRAC` trigger compares a *per-request* prompt estimate
  against `frac x token budget`, while the frozen recipe's token budget is
  *cumulative billed tokens* — different units. Measured over 1064 settled
  requests in the banked arms, per-request prompts top out at 18.0k tokens
  (median 2-3k), so the registered 0.7 x 300k = 210k trigger cannot fire,
  and the 20k verbatim-keep budget exceeds an entire prompt so the cut
  point is never valid either. The registered validation arm is inert (it
  would compare a treatment arm against itself); the knob stays OFF by
  default and S-1 returns to design, keyed on spent-vs-cap rather than
  prompt size.
- **Prompt composition of the frozen recipe (2026-10-16, inferred from a
  reconstruction calibrated against measured per-request token counts).**
  Of a typical mid-run prompt: system + tools + goal ~1.1k tokens, ~21
  tokens of wire scaffolding per history row (~2.1k at 50 rows), tool
  result bodies at ~2.7 chars/token, older tool results stubbed to one
  line, and no cumulative reasoning echo in the pre-trim arms (so the
  "echo bill" figure belongs to the pinned-context runs, not the slice
  arms). The largest *uncollapsed* component is assistant tool-call
  arguments: the request fold stubs old results but never old call
  arguments, so every patch body is re-sent on every later request.
- **Second experiment endpoint wired (2026-10-16).** A vLLM-fronted
  gateway now serves `deepseek-ai/DeepSeek-V4.1-Flash` for experiments.
  Live probes + a smoke agent run: tool calls execute, prefix-cache reads
  are reported, thinking arrives under `reasoning` and the adapter reads
  it as a fallback (`reasoning_content` still wins when both keys are
  present; two-field parse rather than a serde alias so a body carrying
  both keys parses), and DeepSeek's thinking-off knob is accepted and
  ignored there. The endpoint is unpriced for this harness — it reports a
  cost on only some routes, so a partial sum is not meterable and dollars
  stay `null`; the token/step caps are the guards and the unpriced-model
  gate in the roadmap is now open.
- **Envelope arm pre-registered (2026-10-16), amended before its second
  launch.** Three matched arms over two clean in-container headroom cells
  (test fractions 2/5 and 2/6 on the reference model) plus one
  already-solved regression cell, two reps each, in-container reward
  estimand: control = frozen recipe (300k gross / 60 / 120); shape =
  300k miss-plus-out at the same envelope; joint = miss-plus-out plus a
  doubled step/action envelope. Pass rules: per-cell test counts up in
  the joint arm for two of three cells, at least one cell solved, and
  internal tokens per test passed within 1.3x of control. Kill rule: if
  neither the shape nor the joint arm raises test counts, the envelope
  axis is dead for this workload and further spend goes to
  capability-side levers. Amendment: the original cell set was picked
  from host-side partial-credit rows; the in-container table shows the
  first cell at 0/6 for every model measured in that mode, so it has no
  delta to show. One wasted run, cells fixed, arms/reps/gates unchanged.
- **In-container exec policy landed (2026-10-16).** `--exec-policy
  allowlist|any` on `rof run` and `rof eval` (default `allowlist`, the
  strict `--allow-cmd` prefix list; the previous behavior). `any` skips
  the list, for callers whose process boundary is elsewhere: in-container
  eval wraps every exec as `docker exec <container>`, so the container is
  the boundary and the allowlist only removes capability — measured on 53
  banked in-container rows, 83% hit at least one denial and the denied
  set includes the toolchains the tasks' own tests require. The value is
  a required provenance field, so relaxed rows can never pool with strict
  rows; the shell-syntax guard (a correctness teaching error) still
  applies under `any`, and the threat model now states that `any` removes
  even the hygiene the list provides. Tokens/steps remain the guards on an
  unpriced endpoint. Gates: tools-std 60/0, eval 31/0, rof 56/0, clippy
  `-D warnings` and fmt clean.
- **Pre-registered arm for that policy (`dphn-trust`, queued).** Same
  model/endpoint/binary/recipe/estimand as the envelope arm; cells chosen
  because their zero is a denial artifact (make-mips-interpreter, custom
  memory-heap-crash, query-optimize) plus one already-solved regression
  cell; two arms (strict list vs trust) x 2 reps. Pass: the treatment
  passes a test those cells never passed under the strict list, or solves
  one, within 1.3x the control's tokens per test passed. Kill: every
  treated cell stays at zero tests, i.e. the zeros are capability rather
  than policy.
- **Envelope arm closed (2026-10-16/18): the kill rule fired.** Control
  (frozen recipe) vs miss-plus-out accounting at the same cap vs the same
  plus a doubled step/action envelope, on two headroom cells and one
  already-solved regression cell, in-container reward estimand. The
  envelope moved walls and not capability: the shape converted one cell's
  token-death into a step-death and the joint arm then reached 87 of 120
  steps (1.85x steps, 3.0x gross input), yet every arm on that cell failed
  the identical verifier test — the missing deliverable is semantic, not
  budget — while the other headroom cell's single test gain was an
  import-check test. Test counts per cell were flat where it mattered and
  gross tokens per test passed came out at 1.85x and 2.35x against the
  1.3x rule. No further envelope arms; the parked near-miss top-ups stay
  parked because their prior was envelope-shaped.
- **In-container exec policy arm closed (2026-10-18): mechanism
  confirmed, capability unconverted.** `--exec-policy any` removed the
  command-list friction completely (allowlist denials 3-9 per row to 0,
  same tokens), and converted nothing on the three cells whose zeros
  looked like denial artefacts: the same named verifier tests fail in
  every arm at the same steps, and the solved regression cell stays
  solved in both arms. The zeros on that slice are capability/semantic at
  this model tier, so the published in-container solve numbers do not need
  a policy re-label and the knob stays flagged mechanism-validated /
  capability-unconverted. Next capability question there is goal/feedback
  shaped (the same named requirements fail every time), not tooling or
  budget.
- **Completion-checklist goal lever: first capability-positive result
  (2026-10-18).** One generic completion-checklist block appended to the
  task goal — list every explicit requirement, verify each in place,
  re-execute each deliverable from a working directory other than the one
  it was developed in, then declare done — converted a cell the control
  had never solved (0 of 6 control rows) into a 6/6 solve, at identical
  token spend and fewer tokens per test passed in every pair. The
  mechanism shows in the artifacts: path-robustness code appears 11-12
  times in the checklist scripts versus 0-1 in the control scripts. The
  claim is deliberately small: direction measured, effect size unresolved
  (one converted rep on one cell), and the cell is bimodal on whether the
  required R package survives to grading, which sinks rows in both arms
  equally. Next step is generalization to the other requirement-shaped
  cells, not more reps of this one.
- **Completion-checklist lever scoped (2026-10-18, second arm, 16 rows).**
  The same generic checklist that converted a deliverable-verification
  defect into a solve (5/6 -> 6/6, path-robustness code 11-12x in the
  checklist deliverables versus 0-1x in control) changed nothing on three
  cells that fail on accuracy or correctness: identical failing test sets
  in both arms, flat token spend, and the regression cell untouched. The
  lever is rerun-context specific; the kill rule fired and no further
  checklist arms run. The remaining failures on that slice are semantic and
  capability-bound at this model tier, so the next honest experiment is a
  stronger-model ceiling read, not another prompt variant.
- **File-tool `/app` path parity fixed (2026-10-18).** Task statements and
  the harness's own goal text spell the workspace `/app/<file>`, but the
  host-side file tools root at the host workdir, so the largest denial class
  across 96 in-container rows was agents being refused the very files they
  were told to edit (`view:/app/Dockerfile` and friends). The resolver now
  strips a leading `/app/` before joining the root — strictly additive,
  since absolute `/app/...` was always refused — while traversal out of the
  root, secrets deny-globs and the git-substrate refusal still apply. A
  two-binary A/B (12 rows) shows the denial class gone, no test moved on the
  cells that fail, and the already-solved regression cell reaching the same
  verdict in about half the steps and tokens. Container-only paths
  (`/tests`) stay refused: serving them would mean letting the agent read
  the verifier's tests, which is a fairness decision rather than a bug fix.

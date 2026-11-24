# Harness capability implementation

**Historical implementation record:** this document covers the twelve proposals
scheduled on 2026-11-07. Their bounded implementations shipped as described below.
The current [phased capability plan](superpowers/plans/2026-11-11-capability-phases.md)
adds the paper-audit work; phases 0–4 are implemented and locally verified.
Current history, completion-predicate and checkpoint contracts are in the
[phase 3–5 record](phase-3-5-implementation.md); the earlier contracts below keep
their original date and scope.

Chosen implementation plan, 2026-11-07. The owner approved implementing the twelve
proposals with GPT-6 Luna workers. These are experimental mechanisms, not measured
capability gains; existing defaults remain the control. Implementation needs no
benchmark model spend: all tests use local processes or scripted providers.

## Ownership and frozen interfaces

Workers own disjoint crates. The integration owner maintains this document and
README/architecture/roadmap. No worker edits research or commits another's files.

1. `agent-loop`: proposals 1, 2, 3 and 10. Add to `RunConfig`:
   `preserve_failed_checks: bool` (false), `minimal_workflow: bool` (false),
   `observation_keep: usize` (5), `stall_threshold: usize` (0, disabled).
   Existing public traits and methods stay unchanged. A failed check remains
   failed for verification/proof; preserving its patch never counts as proof.
   Executed check failures stay distinct from tool dispatch/policy/mutation
   errors. A successful `multi_edit` counts as an edit for existing counters.
2. `tools-std`: proposals 4, 5, 6, 7 and 8. Add
   `ExperimentalToolsConfig { output_dir: Option<PathBuf>, shell: bool,
   processes: bool, repo_lookup: bool, multi_edit: bool }` (Default/Clone/Debug)
   and `experimental_tools(policy: Arc<Policy>, config: ExperimentalToolsConfig)
   -> Result<Vec<Arc<dyn tool_core::Tool>>, String>`.
   Tools named `exec`/`test` may be replaced to archive output when configured;
   new names are `output_read`, `shell`, `process`, `repo_lookup`, `multi_edit`.
   Ordinary APIs and Policy fields stay compatible. Shell/process require
   `allow_any_cmd`; enabling an experiment never widens permissions. Artifact
   reads use run-owned handles, not arbitrary filesystem paths. Process sessions
   support start/poll/input/stop and cleanup, documenting pipe versus terminal
   semantics. Repository lookup starts with bounded literal symbol occurrences
   and test-file classification, not a semantic call-graph claim. Multi-edit
   validates all hunks before changing any file and preserves path restrictions.
3. `rof`: expose independent flags on both run and eval, default off, record all
   settings in eval provenance, register/grant only enabled tools, and wire the
   four loop fields. CLI: `--preserve-failed-checks on|off`,
   `--workflow legacy|minimal`, `--observation-keep N`, `--stall-threshold N`,
   `--archive-output on|off`, `--shell on|off`, `--processes on|off`,
   `--repo-lookup on|off`, `--multi-edit on|off`.

## Delivery sequence and gates

- Wave 1: the three lanes above in parallel, test-first. Regression cases: edit +
  failed check retains patch only in treatment; policy/mutation error still
  restores; context retention boundaries; unchanged repeated failure
  intervention; full output retrieval beyond preview; shell composition;
  interactive pipe lifecycle; lookup containment; all-or-nothing multi-file edit;
  CLI defaults, propagation and provenance. Scoped tests/clippy/fmt per crate.
- Wave 2: bounded independent verification (9), procedure retrieval (11) and
  alternative attempts (12) on the existing provider/tool interfaces, frozen
  after inspecting wave 1. No planner/MAS framework. Verification and selection
  cannot use hidden benchmark tests. Extra attempts share a total budget.
  Learning requires separate source trajectories and held-out repositories; a
  library loader alone does not establish learned transfer.
- Integration: scripted-provider end-to-end cases, cancellation and resource
  cleanup, default-control regression, documentation, then a concrete external
  comparison protocol. Publish implementation evidence separately from efficacy.

## Status

Both waves are implemented as bounded experiments. Measured local verification
on 2026-11-10: `agent-loop` 124 passing tests; `tools-std` 65 existing and 18
experimental; `rof` 80 unit tests and one build-provenance test; the candidate
helper 8; procedure distillation 3. Scoped clippy and formatting checks pass.
Stall-detector source evidence excludes known harness sidecars and the build
directory; it is not a semantic model of all external state. Final scripted
integration covers cumulative phase budgets/usage and excludes phase logs from
patches. No feature in this batch was benchmark-validated or promoted by these
local checks. Subsequent retention development runs produced one clean WAL task
pass and an interrupted React reward-1 artifact; see the
[current pilot record](current-benchmark-pilot.md). General gains and default
promotion remain unsupported.

Delivered scope deliberately separates mechanisms from larger research claims:
configurable observation retention precedes task-aware memory; literal lookup
precedes semantic call graphs; pipe sessions precede PTYs; procedure distillation
produces unvalidated candidates, not proven transferable skills; alternative
attempts are an explicit host helper, not automatic runtime search. These limits
are documented in `docs/harness-experiments.md`.

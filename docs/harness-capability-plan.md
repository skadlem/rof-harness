# Harness capability implementation

Chosen implementation plan, 2026-11-07. The owner approved implementing the twelve
proposals using GPT-6 Luna workers. These are experimental mechanisms, not measured
capability gains. Existing defaults remain the control. No benchmark model spend
is needed for implementation; all tests use local processes or scripted providers.

## Ownership and frozen interfaces

Workers own disjoint crates. The integration owner maintains this document and
README/architecture/roadmap. No worker edits research or commits another's files.

1. `agent-loop`: proposals 1, 2, 3 and 10. Add these fields to `RunConfig`:
   `preserve_failed_checks: bool` (false), `minimal_workflow: bool` (false),
   `observation_keep: usize` (5), `stall_threshold: usize` (0, disabled).
   Existing public traits and methods remain unchanged. A failed check remains
   failed for verification/proof; preserving its patch must never count as proof.
   Distinguish executed check failures from tool dispatch/policy/mutation errors.
   Recognize successful `multi_edit` as an edit for existing counters.
2. `tools-std`: proposals 4, 5, 6, 7 and 8. Add
   `ExperimentalToolsConfig { output_dir: Option<PathBuf>, shell: bool,
   processes: bool, repo_lookup: bool, multi_edit: bool }`, deriving Default/Clone/Debug.
   Add `experimental_tools(policy: Arc<Policy>, config: ExperimentalToolsConfig)
   -> Result<Vec<Arc<dyn tool_core::Tool>>, String>`.
   Tools named `exec`/`test` may be replaced to archive output when configured;
   new names are `output_read`, `shell`, `process`, `repo_lookup`, `multi_edit`.
   Ordinary APIs and Policy fields stay compatible. Shell/process require
   `allow_any_cmd`; no widening of permissions merely by enabling an experiment.
   Artifact reads use run-owned handles, not arbitrary filesystem paths.
   Process sessions support start/poll/input/stop and cleanup, with explicit
   documentation of pipe versus terminal semantics. Repository lookup starts
   with bounded literal symbol occurrences and test-file classification, not a
   claim of semantic call-graph accuracy. Multi-edit validates all hunks before
   changing any file and preserves path restrictions.
3. `rof`: expose independent flags on both run and eval, default off, record all
   settings in eval provenance, register/grant only enabled tools, and wire the
   four loop fields. CLI names: `--preserve-failed-checks on|off`,
   `--workflow legacy|minimal`, `--observation-keep N`, `--stall-threshold N`,
   `--archive-output on|off`, `--shell on|off`, `--processes on|off`,
   `--repo-lookup on|off`, `--multi-edit on|off`.

## Delivery sequence and gates

- Wave 1: implement the three lanes above in parallel, test-first. Regression
  cases: edit + failed check retains patch only in treatment; policy/mutation
  error still restores; context retention boundaries; unchanged repeated failure
  intervention; full output retrieval beyond preview; shell composition;
  interactive pipe lifecycle; lookup containment; all-or-nothing multi-file edit;
  CLI defaults, propagation and provenance. Run scoped tests/clippy/fmt per crate.
- Wave 2: bounded independent verification (9), procedure retrieval (11), and
  alternative attempts (12), using the existing provider/tool interfaces. Freeze
  these interfaces after inspecting the completed first wave. Do not introduce a
  planner/MAS framework. Verification and selection cannot use hidden benchmark
  tests. Extra attempts must share a total budget. Learning requires separate
  source trajectories and held-out repositories; shipping a library loader alone
  does not establish learned transfer.
- Integration: scripted-provider end-to-end cases, cancellation and resource
  cleanup, default-control regression, documentation, then a concrete external
  comparison protocol. Publish implementation evidence separately from efficacy.

## Status

Both waves are implemented as bounded experiments. Measured local verification
on 2026-11-10: `agent-loop` has 124 passing tests; `tools-std` has 65 existing and
18 experimental passing tests; `rof` has 80 unit tests and one build-provenance
test passing. The candidate helper has eight passing tests and procedure
distillation has three. Scoped clippy and formatting checks pass. Source evidence
for the stall detector excludes known harness sidecars and the build directory;
it is not a semantic model of all external state. Final scripted integration
covers cumulative phase budgets/usage and excludes phase logs from patches.
No new feature is benchmark-validated or promoted.

The delivered scope deliberately distinguishes mechanisms from larger research
claims: configurable observation retention precedes task-aware memory; literal
lookup precedes semantic call graphs; pipe sessions precede PTYs; procedure
distillation produces unvalidated candidates, not proven transferable skills;
alternative attempts are an explicit host helper, not automatic runtime search.
These limitations are documented in `docs/harness-experiments.md`.

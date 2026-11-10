# Official DeepSeek development pilot

Registered 2026-11-10 before inference calls. This is a small development
comparison, not a frontier-performance estimate.

**Status: superseded after the owner's benchmark-recency correction.** Do not
start further TB2.1 attempts. Verify the latest official benchmark release and
the linked harness leaderboard before choosing the next paid task. One TB2.1
Raman control completed: reward 0, 32 requests, 256,973 input tokens (101,501
cached), 55,984 output tokens, estimated off-peak cost $0.057215703 and peak
accounting upper bound $0.114431406. No usage reservations remain unresolved.
These figures are measured from that campaign's receipts, manifest and Harbor
reward. They are retained as a historical diagnostic, not current capability
evidence. The Git bootstrap failure cost zero model requests and is excluded.

## Budget and execution

The owner authorized a new ceiling of **$2.82** against the official DeepSeek
balance. Credentials live in the ignored, mode-0600 `.env.deepseek`; benchmark
containers receive a local proxy credential. Existing DPHN ledgers and results
belong to different accounts/campaigns and are not pooled here.

Use `deepseek-flash` at `https://api.deepseek.com`, identified by the provider as
DeepSeek-V4.1-Flash-1017. The owner supplied off-peak prices per million tokens:
cache hits $0.003, misses $0.15, output $0.60. Admission reserves the full
1,048,576-token context and requested output at peak rates; settlement uses a
peak upper bound with validated cache usage. Unknown usage retains its full
reservation and stops further requests. Report off-peak estimates separately
from the conservative ledger and from provider balance observations.

Pricing source: [DeepSeek pricing](https://api-docs.deepseek.com/quick_start/pricing/),
retrieved 2026-11-09 UTC. Peak rates are twice the off-peak rates. The documented
weekday peak intervals are 01:00–04:00 and 06:00–10:00 UTC.

## Frozen comparison

Run ROF through the installed Harbor runner, in independently reset
instances of the same task images, using the task's original instructions,
timeouts and reward verifier. Tests are introduced only by the verifier after
the agent phase. Record runner/package versions, task/input hashes, image IDs,
binary/runtime hashes, configuration, per-request usage, exceptions and reward.

Chosen development tasks, fixed before paid inference:

- `raman-fitting`
- `model-extraction-relu-logits`
- `make-mips-interpreter`
- `dna-assembly`

These provide numerical fitting, model reconstruction, systems programming and
scientific deliverables. Selection is a chosen development portfolio, not a
representative random sample. Exclude the previously identified broken-gold or
missing-dependency tasks. Other tasks remain outside this optimization slice.

Start with one control task to establish endpoint/runner compatibility,
then complete one control attempt per task if receipts and budget permit.
Count setup and provider failures
explicitly; no automatic paid retries and no reuse of successful workspaces.

Use the provider's normal thinking mode for both systems and record the actual
request settings. Keep the initial per-request output ceiling fixed. ROF's
chosen recipe is 300,000 gross tokens, 60 steps, 120 actions, max output 8,000,
`--exec-policy any`, with steering and all capability experiments off. Controls
and treatments share the campaign spending gate, recipe and task wall clock.

The 8,000-token ROF setting is its initial request limit; its existing truncation
recovery can increase that limit. Receipts record each actual requested limit,
thinking settings, sampling parameters and usage, including retries.

## Follow-up and interpretation

Inspect agent-visible traces from completed development attempts. Prefer one
mechanism with a concrete observed failure over a bundle of features. If a
failed executed check caused useful edits to be discarded, the first candidate
ablation is `--preserve-failed-checks on`, with every other setting fixed.
Register another mechanism before spending on it if the traces support it.
Keep unmeasured features off by default. Hidden verifier contents never enter
runtime reviews, learned procedures or candidate selection.

Owner amendment before paid inference: exclude Pi comparisons because of the
owner's concern about personal Pi configuration. This campaign compares ROF
controls with ROF treatments only. Pi setup and fake-provider rehearsals made
no DeepSeek inference calls and are not capability results.

Use published web results for external agent reference points, as requested by
the owner. Record benchmark version, model snapshot, harness and settings with
each published score. Full-suite web scores are contextual references; this
four-task development slice is not a directly comparable full-suite score.

Report per-task rewards, solved/attempted counts, infrastructure failures, token
usage and elapsed time. A one-attempt development pilot establishes feasibility
and failure cases; it cannot establish statistical superiority. The cost
guardrail is at most 1.3 times tokens per solved task when that ratio is defined.

Artifacts: `/home/madiyar/.local/share/rof-tb/work/deepseek-official-20261110/`.
The campaign driver and accounting checks are preserved there. Durable results
and reusable integration code belong in this repository.

## Published reference data

The [Maka nine-harness report](https://github.com/apache/maka/blob/279c2e8571c1ced5563e02d57814af5e6c50cacc/docs/eval/terminal-bench-2.1-deepseek-v4-flash-nine-arm.md)
provides [per-task CSV results](https://raw.githubusercontent.com/apache/maka/279c2e8571c1ced5563e02d57814af5e6c50cacc/docs/eval/terminal-bench-2.1-deepseek-v4-flash-nine-arm.csv).
Measured in that publication, restricted to our chosen task names:

| Task | Codex | Maka | DSH |
| --- | --- | --- | --- |
| raman-fitting | fail | fail | fail |
| model-extraction-relu-logits | fail | pass | pass |
| make-mips-interpreter | pass | fail | pass |
| dna-assembly | pass | pass | fail |
| Tasks solved | 2/4 | 2/4 | 2/4 |

The report uses TB2.1 revision `d49e28f1e4ddd13d289e85a5f312a66750951932`,
`deepseek-v4-flash`, max reasoning, native task deadlines and one selected
attempt per task. Its publication commit is dated 2026-09-01, before the 1017
upgrade. It does not establish a same-checkpoint comparison with this campaign.
DSH was a later follow-up and the report's infrastructure replacements further
limit causal interpretation. These are published observations, not ROF results.

The [Vals TB2.1 leaderboard](https://vals-ai.com/benchmarks/terminal-bench-2-1)
separately reports 74.53% for Terminus 2 with the September V4.1 Flash release
on the full suite. It is a historical full-suite reference, not a four-task or
1017 result. Source snapshots and SHA-256 hashes are preserved in the campaign's
`external-references/` directory.

# Steering feasibility experiment

Registered before new model calls. Status: **implemented behind an opt-in flag; pilot stopped with an incomplete pair**. The previous `dphn-steer` arm remains paused; its binary and the single banked row are diagnostic only and must not be pooled with this experiment.

## Evidence and limits

Measured 2026-11-03 from completed event logs, using the local `rof-steer-trace-check.py` scanner: with a streak defined as consecutive successful `view`/`search` results and reset on every other result, the two original allowlist-control heap rows had maximum streaks 10 and 21, both with a later model response. The two query-optimize controls had maxima 1 and 4; the two solved log-summary controls both had maxima 2. The paused steering heap row reached 6 with a later response. A later response is an opportunity to deliver a directive, not proof it would have been delivered. The first six reads in each original heap control addressed six different files: this evidence does not establish that reading was wasteful.

The threshold was selected after inspecting these historical traces. They have different configurations and binaries, so they do not validate its efficacy or its delivery on the new binary. The scanner and its raw output are local artifacts under `.local/share/rof-tb/work/analysis-grade-check-2026-11-03/`; they are not part of the repository.

## Pilot status, 2026-11-05

The first provider attempt returned three HTTP 403/1010 responses, with unknown usage; its full reservations remain carried forward. A restarted pilot produced one eligible heap control: Unresolved, 0/6 verifier tests, threshold crossed. The paired treatment crossed and was delivered but had an unknown-usage provider attempt and was stopped before grading. There is no completed pair and no efficacy conclusion. The accounted-plus-reserved ledger total was $4.30645020 of the $5 ceiling when stopped, including all four unknown-usage reservations under the stated assumptions. Its records are local under `.local/share/rof-tb/work/pilot-streak-run-20261104T213305Z-2825961/`; the current ledger is under `.local/share/rof-tb/work/pilot-streak-budget-carried-1l4ye9uh/`.

## Owner budget update, 2026-11-05

The owner authorized **$5 more**, raising the cumulative ceiling from $5 to **$10**, not resetting prior spend. Start a fresh matched campaign and carry the entire $4.30645020 already accounted or reserved, including all four unresolved provider attempts. The new campaign's ledger records those reservations separately; a new unknown-usage attempt stops that campaign. The old control and interrupted treatment remain diagnostic only and are not included in the fresh comparison. This budget amendment does not change the treatment, task selection, matched order, or evaluation rules.

The fresh campaign stopped with no eligible outcomes: both heap-control verifiers failed to produce CTRF output after the `uv` download failed. The retry also encountered an upstream HTTP 524 with unknown usage; the proxy denied subsequent requests. No treatment ran. The ledger now accounts for or reserves **$4.83727350 of $10**, including five unknown-usage reservations; this is not a provider billing receipt. Records are under `.local/share/rof-tb/work/pilot-streak-run-20261104T220645Z-2842978/` and `.local/share/rof-tb/work/pilot-streak-budget-plus5-1pmo22co/`.

Before another paid campaign, demonstrate that the actual task verifiers can run and produce valid output. Any new campaign must carry all accounted spend and unresolved reservations, and preserve or explicitly register any change to the verifier environment.

The guard assumes DeepSeek V4.1 Flash 1017 peak prices of $0.30 per million uncached input tokens and $1.20 per million output tokens ([DeepSeek price table](https://api-docs.deepseek.com/quick_start/pricing/)), as authorized by the owner for this endpoint. It is a conservative model-based ceiling, not a verified provider account quota; actual dphn pricing may differ.

## Treatment and control

Chosen to validate: one frozen new binary, the same model/endpoint, task snapshots, actual image IDs, execution policy, prompt settings and 300,000 gross-token / 60-step / 120-action recipe in both arms. Both keep existing `--steer on` behavior and `--exec-policy allowlist`. The fixed allowed command prefixes are:

```text
python3 python ls pwd find cat head tail grep wc sort cut sed awk tr diff
mkdir cp mv rm touch tar unzip gzip git make gcc g++ cc cargo rustc node
npm npx java javac R Rscript go cmake ffmpeg ffprobe xxd od strings file
timeout 7z qemu-system-i386 qemu-system-x86_64
```

Treatment alone adds a one-shot directive after six consecutive successful `view`/`search` results and before the first successful `edit`/`write`; failed reads and every other tool result reset the streak. The fixed directive is: "You have read six files or search results in a row without making a successful edit or write. Decide whether you have enough evidence to make a change; if so, make the smallest useful change and check it. Otherwise, state the specific missing evidence and obtain it."

The treatment is a new, separately controlled flag; it is off by default. Do not equate this intervention with the separate `--steer on` versus `off` deletion test. Both arms record counterfactual threshold crossings without changing control prompts. The treatment must record a unique trigger ID, the request carrying the directive, and whether that request returned a provider response. A crossing or a log append alone is not delivery. Successful writes here are tool outcomes, not proof that an edit survived later rollback.

Before model spend, scripted fixtures must verify threshold crossing, reset on failed/other tools, one-shot behavior, unchanged control request, a treatment-only directive, and no delivered marker when a budget or cancellation prevents the next request. Verify any instrumentation itself does not change provider-visible control requests or budget accounting.

## Feasibility pilot

Chosen: two tasks — custom-memory-heap-crash (reachability) and log-summary-date-ranges (previously solved regression control) — three repetitions per arm per task, 12 attempts total. Run each task/repetition pair in the same time window, in this fixed order: r1 control then treatment; r2 treatment then control; r3 control then treatment. Use a fresh output directory for every attempt. Record immutable task and image identities, binary hash, complete nonsecret config, endpoint, model, timestamps and arm assignment. Stop if a required identity is missing or differs within a pair, the endpoint/model changes, instrumentation fails, or a verifier integrity problem appears.

The recipe configures 3.6 million gross tokens across the initial 12 attempts, plus up to 3.6 million for one infrastructure retry per cell, before possible per-request/retry overshoot. The designated endpoint is unpriced in the harness. Provider failure, cancellation or missing usage must be reported as unknown rather than zero. The user authorized paid calls with a maximum total spend of $5 and instructed us to assume regular DeepSeek V4.1 Flash 1017 pricing. A local aggregate guard reserves for every HTTP attempt, using the configured 1,048,576-token context bound plus requested maximum output, all at DeepSeek's peak output price of $1.20 per million tokens. It debits actual gross tokens at that same conservative rate only when a valid usage report arrives; unknown attempts retain their full reservation and stop the pilot. This ceiling depends on the assumed rates, context bound, and truthful usage reports; it is not a verified dphn account quota.

Budget-accounting amendment, 2026-11-05, before restarting: the first pilot was stopped without an eligible result after three HTTP 403/1010 responses with unknown usage. Its full $3.80367360 reservation remains charged against the $5 ceiling. The new pilot uses a separate ledger carrying that entire amount, then reserves each new request at the assumed peak uncached-input rate of $0.30 per million and peak output rate of $1.20 per million, with no cache discount. Unknown usage in the new pilot still stops it and retains its full reservation. This changes only accounting, not the treatment or evaluation protocol. The original ledger remains intact.

Treatment-delivery feasibility requires the directive to be present in a subsequent provider request that returns a response in at least two of three eligible heap treatment attempts. Report every crossing, attempted delivery and response, including terminal-tail cases. If delivery fails, conclude the treatment was not reliably delivered; do not infer ineffectiveness.

Primary outcome is the task verifier's resolved/unresolved result per attempt, with paired task rows shown individually. Include all eligible outcomes regardless of whether the intervention fired; never select outcomes by observed delivery. Named-test counts, read streaks, retained edits and steps are diagnostic only, not independent task samples. These two selected tasks and three repetitions are a feasibility check, not a reliable population-level capability estimate.

Classify infrastructure failures using evidence established before reading the reward: container startup/inspection failure, provider transport, snapshot/log failure, verifier spawn/timeout, or missing required verifier output. Record every attempt and its reason; retry a preclassified infrastructure failure at most once in the same arm with a fresh directory, then report it as missing if the retry fails. Do not discard a valid failing verifier outcome. Ambiguous rewards remain ambiguous. Do not make a capability claim when a paired outcome is missing.

Report gross input+output tokens and tokens per solved task in each arm, including known usage from failed infrastructure attempts and retries. The ≤1.3× tokens-per-solved guardrail is evaluable only if both arms solve at least one task and all usage is known; otherwise report it as unassessable. A feasibility signal can justify a separately registered larger, held-out task-level paired study, but cannot by itself promote the treatment. Keep it off by default until that study establishes capability at the guardrail. Any change to the threshold, directive, recipe or exclusion rules is a new arm.

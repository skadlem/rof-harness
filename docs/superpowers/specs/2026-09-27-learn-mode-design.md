# Learn mode: bidirectional learning between the harness and the user

Date: 2026-09-27. Status: design agreed, slice A in progress. Relates to
`2026-09-27-rof-harness-design-report.md` (principle 10, dynamic skills) and
§5 (verification integrity) — this is a new product surface, not a
continuation of the P3 build order.

## 1. Goal

Two directions, and both are currently missing or half-present:

- **Agent ← user.** The harness learns how the user works and prefers, and
  adapts to it. Partly present: `~/.rof/LESSONS.md` + `AGENTS.md` load into
  the stable head (`src/context/memory.rs`), and the skills flow is already a
  gated write path (a proposal lands in `~/.rof/proposals/skills/` and a human
  applies it — never auto-write).
- **User ← agent.** The harness teaches the user how the project they are
  building actually works, as they go. Absent. The explorer already computes a
  `why` per file it considers important (`src/agents/explorer.rs`), which is a
  latent explanation surface doing nothing.

## 2. The rule everything else follows from

**The harness never records that the user understands something.**

`explained` is a fact about what *we did*. `understood` is a claim about the
user, so only the user may create it. There is no path — no prompt, no
heuristic, no judge — by which the harness moves a concept to `understood`.

This is not squeamishness. The evidence in the research brief says a single
LLM judgment flips 13.6% on identical re-runs, and "does this user know
this?" is exactly one such weak judgment with an asymmetric failure: assume too
little and the user is condescended to; assume too much and the one
explanation they needed never comes.

## 3. State machine

Per concept, three states:

| state | set by | meaning |
|---|---|---|
| `not_explained` | default | never addressed |
| `explained` | the harness, immediately after explaining | we explained it; we assume nothing beyond that |
| `understood` | **the user only** | confirmed |

What we know is **two assumption sets and nothing else**:

- `assumed_known` — entries in `explained` or `understood`.
- `assumed_unknown` — entries in `not_explained`.

There is deliberately no third bucket of "observed facts". We do not have one:
nothing in the harness observes competence. Entries are stored once with a
`state` field and the two sets are the **derived view**, so the sets cannot
drift apart as two independently-written lists would.

## 4. Store: `~/.rof/PROFILE.md`

Cross-repo, per the owner's decision. Machine-readable and human-editable in
one file: a fenced ```json block (parsed with `serde_json`, already a
dependency — no new dependency and no hand-rolled YAML) plus free-form notes
below it for the human side.

Entry shape:

```json
{"concept": "retry backoff", "state": "explained",
 "scope": "global", "evidence": "...", "first_mentioned": "..."}
```

- **`evidence` is mandatory on every entry.** An assumption with no cited
  prompt is an unreviewable guess, and this file is a set of guesses about a
  person. Evidence is what makes a wrong assumption correctable in one edit.
- **`scope`** exists because "cross-repo" cannot mean "load everything
  everywhere". A profile that accumulates *this repo's* auth layout and is then
  loaded into an unrelated repo leaks one project's internals into another's
  context. Entries are `global` (style and preferences, always loaded) or
  `repo:<name>` (project knowledge, loaded only when the workdir matches).
  A bare cross-repo profile is only safe with this split.
- Concepts are **free-text labels for slice A**. Known cost: `backoff` and
  `retry backoff` become two concepts forever. Merging/normalising labels is
  explicitly deferred to a later slice, not forgotten.

## 5. Teaching mechanism

The agent **names** the concept, so nothing has to guess. The implementer's
output gains one optional structured field:

```json
"introduces": [{"concept": "retry backoff", "because": "..."}]
```

Then, in order:

1. If the concept is in `assumed_unknown` → explain it, once, in the
   end-of-goal note; then move it to `explained`.
2. If it is already in `assumed_known` → **say nothing**. This is the
   anti-nag property, enforced by set membership rather than by willpower.
3. The end-of-goal note carries **one** concept, not a wall of text. A lesson
   the user cannot act on is a lesson they will learn to dismiss.

**Never mid-run.** The terminal is a work surface; an explanation between
rounds is how you train someone to ignore explanations. Teaching is on demand
(`/explain <thing>`) or at the end of a goal.

**Empty-start caveat, stated up front:** the profile begins empty, so "you are
demonstrably new to this" has no evidence base on day one and is a guess. The
default is silence, so a wrong guess costs nothing — which is the only reason
the guess is safe to make at all.

## 6. Reaching `understood` — three routes

All three ship. None is performed by the harness on its own initiative.

1. **Self-report** — `/got it` / `/still lost`. No judge, no inference.
2. **Probe** — `/probe [concept]` asks one concrete question about an
   `explained` concept, ideally about *this* repo ("where is the auth token
   validated?"). A right answer moves it to `understood`; a wrong or declined
   answer moves it **back** to `assumed_unknown`. This is the only route that
   can catch the harness being wrong, and it costs one cheap call the user
   explicitly asked for.
3. **Observed competence** (later slice) — the user does the thing correctly in
   a later session. Attractive and currently unreliable: it needs a competence
   signal the harness does not have. Shipped last, and only behind evidence
   that it does not fire on beginners.

`/still lost` is a first-class answer, not a failure: it is evidence, and it
is what stops the nagging.

## 7. Slices

- **A — the store (now).** `~/.rof/PROFILE.md` read/write, two derived sets,
  evidence required, scoped entries, loaded into the stable head beside
  `LESSONS.md` with only `global` always on, `/profile` to inspect and correct.
  No auto-write and no inference yet: every entry is created by an explicit
  user command. Proves the substrate and the loading budget.
- **B — explain.** The `introduces` field, the gate as list membership, the
  end-of-goal note, and the `not_explained` → `explained` transition.
- **C — probe.** `/probe` and the transition the user controls.
- **D — observed competence.** Only with evidence it is safe.

## 8. Non-goals

- No comprehension *tracking* beyond the three states. No "did that land?"
  metric, no per-concept confidence score.
- No auto-written judgments about the user. A wrong guess about a person,
  written silently, is permanent; written with evidence, it is one edit.
- No vector store and no inference engine. Membership is a set lookup; the
  store is a file a human reads.

## 9. Open, deliberately deferred

- Concept-label merging (`backoff` vs `retry backoff`).
- Whether `/explain` should read from the explorer's existing `why` lines or
  generate fresh prose.
- The competence signal for slice D — unnamed until it exists.

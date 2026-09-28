//! §7's second half: the run ASKS the research folder before it spends
//! (design report §7, build item 5's second half).
//!
//! The store landed first and nothing read it, so "retrieval before research"
//! was documented and inert. This module is the run's side of it, and it is
//! the same shape as [`teach`](super::teach): a pure decision function, one
//! entry point that loads the store, decides, acts, and degrades rather than
//! failing, and a trace line whenever it could not do what it was asked.
//!
//! # The property, and it is the whole design
//!
//! 1. The run derives ONE topic from its goal and asks
//!    [`Research::needs_research`] — a set lookup plus a commit comparison.
//!    There is no similarity, no ranking and no confidence anywhere in this
//!    file, and none is needed: a note is retrieved because a path says so.
//! 2. `Answered` (a FRESH note) means the answer goes into the stable head and
//!    **no model call is made**. That is the property, and it is the reason
//!    the lookup is wired at the shaping point rather than at the end of the
//!    run: a lookup after the spend cannot avoid it.
//! 3. `NotAnswered` — absent, or present and STALE — is the only state in
//!    which a call is bought, and it is ONE bounded call whose body the
//!    harness writes back verbatim through the store's one write path. Buy
//!    once, then retrieve: a bought answer that is not recorded is money the
//!    next run spends again.
//! 4. **Nothing usable writes nothing.** The body is the model's, or there is
//!    no note: a harness that filled the gap itself would be inventing research
//!    with a confident `because`, which is the exact failure the whole store
//!    exists to prevent.
//!
//! # Why the write-back is DEFERRED to the end of the run
//!
//! [`consult`] returns the note it wants recorded; the caller hands it to
//! [`Consulted::write_back`] after the run's last change-set read. Two
//! properties fall out of that one placement, and both are structural rather
//! than conventional:
//!
//! * **A note can never satisfy `expect_writes`.** An untracked `.rof/` entry
//!   IS a change git reports (`TreeDiff::changed()` counts it), so a note
//!   written before the rounds would, on its own, open a write-gated task. The
//!   write gate's own record is taken before the note exists, so the hazard
//!   this slice records as latent stops being reachable at all — no change to
//!   the gate, and no path filter to keep in step with one.
//! * **The pin is the commit the research was read against.** The research
//!   step runs at the shaping point, on a tree at one known commit. The note
//!   records THAT commit, not whatever the run's own baseline commits produced
//!   afterwards, so the note never claims to have been verified against a tree
//!   the model never saw. The frozen staleness rule is conservative on
//!   purpose; over-pinning to make a note look fresh would be the harness
//!   lying to its own store.
//!
//! The price is named rather than hidden: because the run's next `git baseline`
//! commits the (untracked) note, a run that committed work after the shaping
//! point has moved HEAD by the next consult, and the conservative rule calls
//! the note stale and re-buys. Re-verifying too often costs one call; serving
//! research about a tree that no longer exists costs an answer nothing in the
//! harness can detect.
//!
//! # The address
//!
//! A topic is a path segment, and a goal is prose, so the run derives one:
//! a readable slug of the goal plus a short hash of the whole goal. The hash
//! is not decoration — two goals that share a slug prefix must not share an
//! address, because a note served for the wrong goal is the failure the
//! store's "address by path, not by similarity" rule exists to prevent. It
//! also strips every separator, so a derived topic can never address the
//! `tests/` folder: a design run never reads research ABOUT a suite.

use crate::context::research::{self, Answer, Edit, Kind, Research, TreeState, MAX_TOPIC_CHARS};
use crate::eval::runner::fnv1a_hex;
use crate::llm::{ExecutorService, LlmReq};
use crate::obs::{TraceEvent, TraceSink};
use std::path::Path;

/// The heading the note rides under in the stable head, beside `[PROJECT
/// MEMORY]`. A section of its own, so a prompt reader can tell retrieved
/// knowledge from the model's own reasoning about the tree.
pub const HEAD_LABEL: &str = "[RESEARCH]";

/// Longest research body delivered into the head, in chars. The same bounded
/// head discipline the profile section uses, and deliberately smaller than
/// `AGENTS.md`'s own cap: this is knowledge the run RETRIEVED, and the
/// project conventions are what the model must not lose. The block is also
/// placed after them, so the bound is belt and braces rather than the only
/// thing standing between a long note and a displaced convention.
pub const HEAD_CAP: usize = 2000;

/// A phrase that appears in the research step's system prompt and nowhere
/// else, so a caller (or a test's fake model) can tell this call apart from
/// every other one a run makes.
pub const SYSTEM_MARKER: &str = "research note";

/// Chars of readable goal kept in a derived topic, and the length of the hash
/// that follows it. The two plus the joining `-` must stay under
/// [`MAX_TOPIC_CHARS`], which is checked rather than assumed.
const SLUG_CHARS: usize = 40;
const HASH_CHARS: usize = 8;
const ADDRESS_MAX: usize = SLUG_CHARS + 1 + HASH_CHARS;

/// Per-call output cap for the research step, and the env var that moves it.
/// Bounded because an unbounded completion is unbounded spend.
const MAX_TOKENS: usize = 1024;
const MAX_TOKENS_ENV: &str = "ROF_RESEARCH_MAX_TOKENS";

/// What one run's consultation decided, and the note it left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Consulted {
    /// The knob is off. Nothing was read, nothing bought, nothing written —
    /// and the head block is empty, so the run is byte-identical to one from
    /// before the feature existed.
    Off,
    /// A note answered the topic, and its body rides the head. `bought` is
    /// false when the note was already in the store and true when this call
    /// paid for it, which is also the only case [`Self::write_back`] records.
    Answered { answer: Answer, bought: bool },
    /// Nothing answered, and nothing usable came back. The store is untouched
    /// and the head is unchanged; `reason` says which of the two happened.
    Declined { topic: String, reason: String },
}

impl Consulted {
    /// The block the stable head carries, or `""` when nothing answered.
    ///
    /// Placed AFTER the project conventions by the caller, and head-capped
    /// here, so a retrieved note can never crowd `AGENTS.md` out of the head
    /// no matter how long it is. The answer and the commit it was verified
    /// against travel together: a note delivered without its pin is a claim
    /// nobody can check.
    pub fn head_block(&self) -> String {
        let Consulted::Answered { answer, .. } = self else {
            return String::new();
        };
        let body: String = answer.body.trim().chars().take(HEAD_CAP).collect();
        format!(
            "\n{HEAD_LABEL} {} (verified against {})\n{body}\n",
            answer.topic, answer.pinned
        )
    }

    /// Record the note this call bought, if it bought one.
    ///
    /// Called at the run's terminal boundary — after the last change-set read
    /// — for the reasons in the module doc. Idempotent in the only sense that
    /// matters: the store refuses a topic that already holds a claim, so a
    /// second call degrades to a refusal instead of writing a second note.
    pub fn write_back(&self, workdir: &Path, trace: &TraceSink) {
        let Consulted::Answered {
            answer,
            bought: true,
        } = self
        else {
            return;
        };
        // The pin is the commit the research read, reconstructed rather than
        // re-read: re-reading HEAD here would pin the note against the run's own
        // baseline commits, which the model never saw.
        let pinned = TreeState::at(answer.pinned.clone());
        let mut store = Research::load(workdir);
        if let Err(reason) = store
            .apply(
                Edit::Write {
                    topic: answer.topic.clone(),
                    body: answer.body.clone(),
                },
                &pinned,
            )
            .and_then(|_| {
                store
                    .save()
                    .map_err(|e| format!("the note was not written: {e}"))
            })
        {
            // A refusal is reported and nothing is forced. The store will not
            // hold a second claim about one topic, and re-pinning a stale body
            // without new research would be serving the thing this design
            // exists to refuse — so the bought answer is dropped, by name.
            emit(
                trace,
                &answer.topic,
                "not-recorded",
                &format!("{reason} — the answer was bought and not recorded"),
            );
        }
    }
}

/// The address this run looks up, derived from its goal.
///
/// A pure function of the goal, so a second run of the same goal asks the same
/// question — which is the entire reason a bought note is ever retrieved
/// again. `None` only when the goal has no characters a filename could carry,
/// and the caller reports that rather than guessing.
pub fn topic_for(goal: &str) -> Option<String> {
    let goal = goal.trim();
    if goal.is_empty() {
        return None;
    }
    let mut slug = String::new();
    for c in goal.chars() {
        // Everything that is not alphanumeric becomes a separator, so no
        // derived topic can contain `/`, `\`, `..` or a leading dot whatever
        // the goal said. A run of separators collapses to one.
        let mapped = if c.is_alphanumeric() {
            c.to_ascii_lowercase()
        } else {
            '-'
        };
        if mapped == '-' && slug.ends_with('-') {
            continue;
        }
        slug.push(mapped);
    }
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        return None;
    }
    let readable: String = slug.chars().take(SLUG_CHARS).collect();
    let hash: String = fnv1a_hex(goal.as_bytes())
        .chars()
        .take(HASH_CHARS)
        .collect();
    let topic = format!("{}-{hash}", readable.trim_end_matches('-'));
    debug_assert!(topic.chars().count() <= MAX_TOPIC_CHARS);
    debug_assert!(topic.chars().count() <= ADDRESS_MAX);
    Some(topic)
}

/// The note body a research reply carries, or `None` when it carries none.
///
/// Lenient in the same way the implementer's optional field is parsed, and
/// strict about the one thing that matters: a body that is absent, of the
/// wrong type, or blank yields NO note. The harness does not substitute
/// anything — not the goal, not a summary, not the retrieved snippets — so a
/// step that cannot answer leaves the store empty rather than full of the
/// harness's own prose wearing a commit pin.
pub fn parse_note_body(text: &str) -> Option<String> {
    let value = crate::llm::parse_lenient(text)?;
    let body = value.get("body")?.as_str()?.trim();
    if body.is_empty() {
        return None;
    }
    Some(body.to_string())
}

/// Consult the folder for `goal`, and buy exactly what nothing answered.
///
/// One `git rev-parse`, one read of the index, and AT MOST one model call.
/// The order is the point: the verdict is computed before any spend, so a
/// fresh note ends the step without a call being made at all.
pub async fn consult(
    goal: &str,
    workdir: &Path,
    executor: &ExecutorService,
    trace: &TraceSink,
) -> Consulted {
    let Some(topic) = topic_for(goal) else {
        let reason = "the goal carries no characters a note could be named after, so \
                      there is no address to ask about"
            .to_string();
        emit(trace, "", "declined", &reason);
        return Consulted::Declined {
            topic: String::new(),
            reason,
        };
    };
    let store = Research::load(workdir);
    // A store that could not be read is neither consulted nor rewritten. An
    // unreadable index and a repo that has done no research look identical,
    // and that difference is what decides whether research gets bought — so a
    // broken index must never read as "buy".
    if let Some(reason) = store.warning.clone() {
        emit(trace, &topic, "declined", &reason);
        return Consulted::Declined { topic, reason };
    }
    let tree = TreeState::read(workdir);
    match store.needs_research(&topic, &tree) {
        research::Verdict::Answered(answer) => {
            emit(
                trace,
                &topic,
                "retrieved",
                &format!("a fresh note at {} answered the topic", answer.path),
            );
            Consulted::Answered {
                answer,
                bought: false,
            }
        }
        research::Verdict::NotAnswered { reason, .. } => {
            buy(goal, &topic, &tree, &reason, executor, trace).await
        }
    }
}

/// The one call this step can make, and only because the verdict said so.
/// `why` is the computed verdict's own reason, carried into the trace so
/// "bought" and "bought because the pin had moved" stay different facts.
async fn buy(
    goal: &str,
    topic: &str,
    tree: &TreeState,
    why: &str,
    executor: &ExecutorService,
    trace: &TraceSink,
) -> Consulted {
    let declined = |reason: String| {
        emit(trace, topic, "declined", &reason);
        Consulted::Declined {
            topic: topic.to_string(),
            reason,
        }
    };
    let Some(head) = tree.head().map(|h| h.to_string()) else {
        return declined(
            "no commit could be read in this work root, so a note has nothing to pin".to_string(),
        );
    };
    let system = format!(
        "You write ONE {SYSTEM_MARKER} about a codebase: the question is the topic, the \
         answer is what you could read in the work root, and every claim names the file \
         it came from. Reply with JSON only: {{\"body\": \"<the note>\"}}. Rules: (1) if \
         you could not read enough to answer, return an empty body — an empty note is \
         accepted and nothing is written, a confident wrong note is not; (2) no \
         commentary, no markdown fence, no prose outside the JSON."
    );
    let prompt = format!("QUESTION:\n{goal}\n\nNOTE (JSON only):");
    crate::context::measure_turn(trace, "researcher", crate::context::TURN_CALL, &prompt);
    let response = match executor
        .complete(LlmReq {
            system,
            prompt,
            max_tokens: super::max_tokens_from_env(MAX_TOKENS_ENV, MAX_TOKENS),
            reasoning_off: false,
            reasoning_low: false,
            roomier: false,
            shrunk: false,
            thinking_off: true,
        })
        .await
    {
        Ok(r) => r,
        Err(e) => return declined(format!("the research call failed: {e}")),
    };
    trace.emit(TraceEvent::ModelCall {
        agent: "researcher".to_string(),
        model: executor.model.clone(),
        input_tokens: response.input_tokens,
        output_tokens: response.output_tokens,
        latency_ms: response.latency_ms,
        cost_usd: response.cost_usd,
        cached_input_tokens: response.cached_input_tokens,
        attempts: response.attempts,
    });
    let Some(body) = parse_note_body(&response.text) else {
        return declined(
            "the research step produced no usable note body (no `body` string, or a \
             blank one) — nothing was written, so the next run asks again rather \
             than trusting an invented answer"
                .to_string(),
        );
    };
    // The note the run will hold: the model's own words, at the address the
    // topic derives, pinned to the commit they were read against. It rides this
    // run's head immediately and is recorded at the run's terminal boundary.
    let answer = Answer {
        topic: topic.to_string(),
        kind: Kind::Design,
        path: match research::note_rel_path(Kind::Design, topic) {
            Ok(path) => path,
            Err(reason) => return declined(reason),
        },
        pinned: head.clone(),
        body,
    };
    emit(
        trace,
        topic,
        "bought",
        &format!("{why} — one bounded call, pinned at {head}"),
    );
    Consulted::Answered {
        answer,
        bought: true,
    }
}

/// One line per decision. Emitted by this step and by nothing else, so a
/// recorded run can say which way the folder went without re-deriving it.
fn emit(trace: &TraceSink, topic: &str, action: &str, reason: &str) {
    trace.emit(TraceEvent::ResearchStep {
        topic: topic.to_string(),
        action: action.to_string(),
        reason: reason.to_string(),
    });
}

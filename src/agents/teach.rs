//! Learn mode, slice B: the teaching mechanism.
//!
//! The harness EXPLAINING things. Three properties are structural here,
//! and each is a set lookup or an existing invariant rather than a
//! convention:
//!
//! - **The agent names the concept.** The implementer's structured output
//!   gains one optional `introduces: [{concept, because}]`. Nothing is
//!   inferred from a diff or guessed from a symbol: if the model does not
//!   name a concept, the harness teaches nothing, because a concept it
//!   guessed at is a concept it would also guess wrong about.
//! - **The gate is set membership.** [`gate`] consults
//!   [`Profile::assumed_known`] and nothing else — not a similarity score,
//!   not a confidence, not a judgement. A concept already in that set is
//!   excluded SILENTLY. This is the anti-nag property, and it is the
//!   reason the second run of a goal that names the same concept says
//!   nothing at all.
//! - **The harness writes no prose about the project.** The lesson text
//!   is the model's own `because` plus the concept label. The harness
//!   knows nothing about a retry policy it did not read, and a lesson
//!   that paraphrases one is a lesson that can be wrong.
//!
//! One concept per goal (spec §5.3). When an artifact names several, the
//! FIRST ADMITTED one in the model's declared order is taught and the rest
//! are named in the lesson as dropped. A wall of text is not a lesson.
//!
//! **The state write is tied to emitting the lesson.** A concept that
//! loses the pick, or whose store cannot be written, stays
//! `not_explained` — recording it would make the store assert we
//! explained something the user never saw, and the anti-nag gate would
//! then exclude it forever. It is taught on a later goal.
//!
//! **Nothing here produces `understood`.** That is the user's alone
//! (spec §2), and this module cannot even name the state: it records
//! through [`Profile::apply`] with [`Edit::AssumeKnown`], whose arm is
//! documented as recording what WE explained — an assumption, never a
//! confirmation. `/probe` (slice C) is where a user creates one.
//!
//! Teaching is at the end of a goal, never mid-round (spec §5.4), so
//! nothing here emits into the per-round event stream except the DEGRADED
//! case: an unreadable or unwritable store skips the lesson and records
//! why, because a silent skip is indistinguishable from "nothing to say".

use crate::context::profile::{self, Edit, Profile, Scope};
use crate::obs::{TraceEvent, TraceSink};
use serde_json::Value;
use std::path::Path;

/// One concept the model named, with the model's own reason for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Introduced {
    pub concept: String,
    pub because: String,
}

impl Introduced {
    pub fn new(concept: &str, because: &str) -> Self {
        Self {
            concept: concept.to_string(),
            because: because.to_string(),
        }
    }
}

/// The gate's decision for one goal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gated {
    /// Nothing to teach: no concept was introduced, or every one of them
    /// is already in `assumed_known`. The `concept` is "" — an excluded
    /// concept is excluded SILENTLY, and naming it here would leak the
    /// anti-nag decision back to the surface it is meant to protect.
    Silent,
    /// Teach this one. `dropped` are the other concepts the artifact
    /// named, which stay `not_explained` and are named in the lesson.
    Lesson {
        concept: String,
        because: String,
        text: String,
        dropped: Vec<String>,
    },
}

/// The concepts in an artifact's `introduces` array, in the model's
/// declared order.
///
/// A malformed value yields the concepts that ARE well formed, and never
/// an error: this is an OPTIONAL field on a model-authored artifact, and
/// a run that dies on a malformed optional field is a worse harness than
/// one that teaches less. The same discipline [`profile::load`] uses for
/// a hand-edited store — degrade, name the reason, keep the run.
///
/// A concept with no `because` is dropped, not defaulted. The store
/// refuses an entry with no evidence, and the harness does not invent
/// prose to stand in for the model's missing one: there is nothing here
/// to explain, and nothing to record.
pub fn parse_introduces(data: &Value) -> Vec<Introduced> {
    let Some(items) = data.get("introduces").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let concept = item.get("concept")?.as_str()?.trim();
            let because = item.get("because")?.as_str()?.trim();
            if concept.is_empty() || because.is_empty() {
                return None;
            }
            Some(Introduced::new(concept, because))
        })
        .collect()
}

/// The lesson as it rides on the run's result: the concept, the model's
/// own reason, the composed text, and what was dropped.
///
/// A plain object rather than a bare string so a consumer can key on the
/// concept without parsing prose, and so the `because` is carried
/// alongside the text that was composed from it.
pub fn lesson_value(concept: &str, because: &str, text: &str, dropped: &[String]) -> Value {
    serde_json::json!({
        "concept": concept,
        "because": because,
        "text": text,
        "dropped": dropped,
    })
}

/// The gate: the FIRST concept in `introduces`, in declared order, that
/// is not in `profile.assumed_known`.
///
/// Set membership, nothing else. A concept in either derived-known state
/// — `explained` or `understood` — is excluded, and a concept the store
/// has no entry for is admitted, because on day one the store is empty
/// and every concept is new. The whole anti-nag property is this one
/// `!known.contains(..)`.
pub fn gate(profile: &Profile, introduces: &[Introduced]) -> Gated {
    let known: Vec<&str> = profile
        .assumed_known()
        .iter()
        .map(|e| e.concept.as_str())
        .collect();
    // Admitted, in declared order. The first is taught; the rest are
    // named as dropped and stay unrecorded.
    let admitted: Vec<&Introduced> = introduces
        .iter()
        .filter(|i| !known.iter().any(|k| *k == i.concept))
        .collect();
    let Some(chosen) = admitted.first() else {
        return Gated::Silent;
    };
    let dropped: Vec<String> = admitted[1..].iter().map(|i| i.concept.clone()).collect();
    Gated::Lesson {
        concept: chosen.concept.clone(),
        because: chosen.because.clone(),
        text: compose(chosen, &dropped),
        dropped,
    }
}

/// The lesson text: the concept label, then the model's own `because`
/// verbatim, then what was dropped and why it was not taught this run.
///
/// Nothing here describes the project. The harness did not read the
/// retry policy; the model did, and its sentence is the explanation. The
/// only harness-authored words are the frame and the drop note, both of
/// which are about what the harness is doing rather than about the code.
fn compose(chosen: &Introduced, dropped: &[String]) -> String {
    let mut text = format!("{}: {}", chosen.concept, chosen.because);
    if !dropped.is_empty() {
        text.push_str(&format!(
            "\n(not explained this run, one concept per goal: {})",
            dropped.join(", ")
        ));
    }
    text
}

/// The reason this store cannot be consulted, or `None` when it can.
///
/// [`profile::load`] degrades a malformed file to an EMPTY store with a
/// `warning`, which is the right behaviour for a run (nothing to load, no
/// head section) and the WRONG input for this gate: an empty store
/// admits every concept, so a hand-edit typo would re-teach the user's
/// entire history. The caller checks this before gating.
pub fn unusable_store(profile: &Profile) -> Option<String> {
    profile.warning.clone()
}

/// Teach the goal's one lesson, and record it — or skip it, and say why.
///
/// The whole end-of-goal step, in the order the spec fixes (spec §5):
///
/// 1. the store must be readable, or there is nothing to gate against;
/// 2. the gate picks one concept by set membership;
/// 3. the lesson is composed from the model's own `because`;
/// 4. only THEN is the concept recorded and persisted — a lesson that
///    could not be recorded must not be shown, because the user would
///    have no way to tell it from a stored one, and we would nag again
///    next run having told them once.
///
/// Returns the lesson to attach to the run's result, or `None`.
pub fn teach(artifact: &Value, workdir: &Path, trace: &TraceSink) -> Option<Value> {
    let introduces = parse_introduces(artifact);
    if introduces.is_empty() {
        return None;
    }
    let mut profile = profile::load();
    if let Some(reason) = unusable_store(&profile) {
        // A store we could not read is never rewritten: the user's file
        // is the record, and a run that overwrites what it failed to
        // parse destroys the only copy.
        skipped(trace, "", &reason);
        return None;
    }
    let Gated::Lesson {
        concept,
        because,
        text,
        dropped,
    } = gate(&profile, &introduces)
    else {
        // The anti-nag gate. Silent by design — this is the property the
        // second run of a goal depends on.
        return None;
    };
    if let Err(reason) = record(&mut profile, &concept, &because, workdir) {
        skipped(trace, &concept, &reason);
        return None;
    }
    Some(lesson_value(&concept, &because, &text, &dropped))
}

/// Record the explanation: the entry is added if the store has never
/// heard of the concept, and moved to `explained` either way.
///
/// `explained`, never `understood`: [`Edit::AssumeKnown`] is the arm the
/// store documents as recording what WE explained, and it explicitly
/// refuses to step DOWN from a confirmation. There is no argument to this
/// function that could produce a claim about the user.
///
/// The evidence is the model's `because` — the cited prompt. A
/// synthesised evidence string would satisfy the mandatory-evidence rule
/// while defeating its purpose, which is that a wrong assumption is
/// correctable by one edit a reviewer can see.
fn record(
    profile: &mut Profile,
    concept: &str,
    because: &str,
    workdir: &Path,
) -> Result<(), String> {
    if !profile
        .entries
        .iter()
        .any(|e| e.concept.trim() == concept.trim())
    {
        profile
            .apply(Edit::Add {
                concept: concept.to_string(),
                // A concept the model named while working in THIS repo is
                // project knowledge, so it is repo-scoped: a cross-repo
                // profile that loaded one checkout's internals into an
                // unrelated repo is the leak the scope split exists to
                // stop (spec §4).
                scope: Scope::Repo(profile::repo_name(workdir)),
                evidence: because.to_string(),
            })
            .map_err(|e| format!("could not record {concept}: {e}"))?;
    }
    profile
        .apply(Edit::AssumeKnown(concept.to_string()))
        .map_err(|e| format!("could not record {concept}: {e}"))?;
    profile::save(profile).map_err(|e| format!("could not write the profile: {e}"))
}

/// Record why no lesson happened, so a skip is never silent.
fn skipped(trace: &TraceSink, concept: &str, reason: &str) {
    trace.emit(TraceEvent::LessonSkipped {
        concept: concept.to_string(),
        reason: reason.to_string(),
    });
}

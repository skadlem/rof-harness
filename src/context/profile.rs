//! The user-knowledge store: `~/.rof/PROFILE.md` (learn mode, slice A).
//!
//! One file, two audiences. A fenced ```json block holds the machine list —
//! parsed with `serde_json`, which is already a dependency, because a
//! hand-rolled YAML parser here would be a second source of truth for the
//! same bytes. Everything below the block is free-form human notes, kept
//! verbatim across machine writes so the file stays a document a person
//! reads, not a database only the harness can open.
//!
//! Three properties are structural, not conventions:
//!
//! - **One entry list, two derived sets.** `assumed_known` and
//!   `assumed_unknown` are computed from each entry's `state` on every
//!   read, so they cannot drift apart the way two independently written
//!   lists would.
//! - **Evidence is mandatory.** This file is a set of assumptions *about a
//!   person*. An assumption with no cited prompt is unreviewable, so an
//!   entry with empty evidence is refused, not stored.
//! - **`understood` is the user's alone.** The single write path is
//!   [`Profile::apply`], and the only arm that constructs
//!   [`State::Understood`] is the one `Edit::AssumeUnderstood` reaches —
//!   the user command `/profile assume-understood`. No agent turn, no
//!   heuristic and no inference calls it; see the design spec §2.
//!
//! Nothing here infers. Every entry in this file was created by an explicit
//! user command, and a malformed file degrades to an empty store with a
//! surfaced reason rather than taking a run down.
//!
//! `ROF_PROFILE` overrides the path, the same idiom `tui::auth` uses for
//! `ROF_CREDENTIALS`, so a test points it at a scratch file and a real
//! `~/.rof/PROFILE.md` is never touched. The override moves the FILE; it
//! never changes what may go in it.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Environment override for the store's path, so a test never reads or
/// writes a real `~/.rof/PROFILE.md`.
pub const PATH_ENV: &str = "ROF_PROFILE";

/// The profile's share of the stable head, in chars, against
/// `memory::CAP` (4000) for project conventions and user lessons. The
/// profile is a THIRD section with its own, smaller budget rather than a
/// share of a shared one: a profile is appendable by hand with no cap of
/// its own, so the bound has to live where the head is built. Project
/// conventions lead the head, so a large profile can never displace them.
pub const HEAD_CAP: usize = 1200;

/// Where the store lives: `ROF_PROFILE`, else `~/.rof/PROFILE.md`.
pub fn store_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var(PATH_ENV)
        .ok()
        .filter(|p| !p.trim().is_empty())
    {
        return Some(PathBuf::from(path));
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .map(|h| PathBuf::from(h).join(".rof/PROFILE.md"))
}

/// The `repo:<name>` a workdir is known by: its basename.
///
/// The store is hand-edited and shared across checkouts, so a scope name
/// that changed when the repo moved or was cloned elsewhere would silently
/// stop matching. A basename is also what the user calls the project out
/// loud, so `repo:rof-harness` is a name they can type. The known cost is
/// two unrelated checkouts sharing a basename (a fork named the same as
/// the original); those users edit one line.
pub fn repo_name(workdir: &Path) -> String {
    workdir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// What we know about a concept. `understood` is a claim about the user and
/// only a user command may create it (design spec §2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    NotExplained,
    Explained,
    Understood,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::NotExplained => "not_explained",
            State::Explained => "explained",
            State::Understood => "understood",
        }
    }
}

/// Where an entry applies. `Repo(name)` loads only when the workdir's
/// [`repo_name`] matches, so one project's internals never leak into an
/// unrelated repo's context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    Global,
    Repo(String),
}

impl Scope {
    /// `global`, or `repo:<name>`. Anything else is refused here, at the
    /// closed parser, so a scope that can never match is never stored.
    pub fn parse(s: &str) -> Option<Scope> {
        let s = s.trim();
        if s == "global" {
            return Some(Scope::Global);
        }
        s.strip_prefix("repo:")
            .map(|name| name.trim())
            .filter(|name| !name.is_empty())
            .map(|name| Scope::Repo(name.to_string()))
    }

    pub fn as_str(&self) -> String {
        match self {
            Scope::Global => "global".to_string(),
            Scope::Repo(name) => format!("repo:{name}"),
        }
    }
}

impl Serialize for Scope {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.as_str())
    }
}

impl<'de> Deserialize<'de> for Scope {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Scope, D::Error> {
        let raw = String::deserialize(d)?;
        Scope::parse(&raw).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "scope must be `global` or `repo:<name>` (got {raw})"
            ))
        })
    }
}

/// One assumption about the user. All five fields are stored, so a
/// hand-edited row that drops one is a degradation rather than a silent
/// default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub concept: String,
    pub state: State,
    pub scope: Scope,
    /// Mandatory. A guess with nothing behind it is not reviewable, so an
    /// entry without evidence is refused by [`Profile::apply`].
    pub evidence: String,
    /// When the entry first appeared, ISO-8601 UTC (`YYYY-MM-DD` at
    /// minimum). Human bookkeeping, never read back for logic.
    pub first_mentioned: String,
}

/// What the user asked the store to do. Every variant is reachable only
/// from a `/profile` subcommand: the enum IS the command surface, so a new
/// writer has to be added here on purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    Add {
        concept: String,
        scope: Scope,
        evidence: String,
    },
    /// The harness explained it (or the user says it landed): `explained`.
    /// An assumption, not a confirmation.
    AssumeKnown(String),
    /// Back to `not_explained` — a first-class answer, not a failure.
    AssumeUnknown(String),
    /// The ONLY route to `understood` in this slice, and it is the user's.
    AssumeUnderstood(String),
    /// Remove an entry entirely; the user correcting a wrong assumption.
    Forget(String),
}

/// The whole store: the machine list, the human notes below it, and a
/// reason the machine list could not be read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Profile {
    pub entries: Vec<Entry>,
    /// Free-form text below the block, preserved verbatim across writes.
    pub notes: String,
    /// `Some(reason)` when the file existed but could not be parsed. A
    /// degraded store is an empty store, never a failed run.
    pub warning: Option<String>,
}

impl Profile {
    /// `explained` or `understood`. DERIVED from the states on every read,
    /// never stored as a second list.
    pub fn assumed_known(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| matches!(e.state, State::Explained | State::Understood))
            .collect()
    }

    /// `not_explained`. The other derived half of the same partition.
    pub fn assumed_unknown(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|e| e.state == State::NotExplained)
            .collect()
    }

    /// The entries that apply to `workdir`: every `global` entry, plus the
    /// `repo:<name>` entries whose name is this workdir's. The scope filter
    /// is a view, not a deletion — the store keeps everything.
    pub fn in_scope(&self, workdir: &Path) -> Vec<&Entry> {
        let here = repo_name(workdir);
        self.entries
            .iter()
            .filter(|e| match &e.scope {
                Scope::Global => true,
                Scope::Repo(name) => *name == here,
            })
            .collect()
    }

    /// The section body for the stable head, head-truncated to
    /// [`HEAD_CAP`]. Empty when nothing is in scope, so a run without a
    /// profile is byte-identical to a run before profiles existed.
    pub fn head_section(&self, workdir: &Path) -> String {
        let mut out = String::new();
        for entry in self.in_scope(workdir) {
            out.push_str(&format!(
                "- {} [{}] ({})\n",
                entry.concept,
                entry.state.name(),
                entry.scope.as_str()
            ));
        }
        out.chars().take(HEAD_CAP).collect()
    }

    /// The store's ONE mutating entry point. Every caller is a user
    /// command, and this is where a refused edit is refused.
    ///
    /// Returns the line to show the user, or the reason it was refused.
    pub fn apply(&mut self, edit: Edit) -> Result<String, String> {
        match edit {
            Edit::Add {
                concept,
                scope,
                evidence,
            } => {
                let concept = concept.trim().to_string();
                if concept.is_empty() {
                    return Err("add needs a concept".to_string());
                }
                // Evidence is mandatory, and the refusal names the field:
                // this file is a set of guesses about a person, and a guess
                // with nothing behind it cannot be corrected in one edit.
                if evidence.trim().is_empty() {
                    return Err(format!(
                        "refused: /profile add needs evidence — {concept} would be an unreviewable assumption"
                    ));
                }
                if self.entry(&concept).is_some() {
                    return Err(format!("{concept} is already recorded; edit it in place"));
                }
                let first_mentioned = today();
                self.entries.push(Entry {
                    concept: concept.clone(),
                    // An added entry is not explained yet. Nothing may
                    // create a non-default state as a side effect of
                    // recording that a concept exists.
                    state: State::NotExplained,
                    scope,
                    evidence,
                    first_mentioned: first_mentioned.clone(),
                });
                Ok(format!(
                    "profile: added {concept} (not_explained, {first_mentioned})"
                ))
            }
            Edit::AssumeKnown(concept) => {
                let entry = self
                    .entry_mut(&concept)
                    .ok_or_else(|| format!("no entry for {concept}"))?;
                // Never a step DOWN from a confirmation: this records what
                // WE explained, which is an assumption and cannot un-know
                // something the user said they know.
                if !matches!(entry.state, State::Understood) {
                    entry.state = State::Explained;
                }
                Ok(format!("profile: {concept} → {}", entry.state.name()))
            }
            Edit::AssumeUnknown(concept) => {
                let entry = self
                    .entry_mut(&concept)
                    .ok_or_else(|| format!("no entry for {concept}"))?;
                entry.state = State::NotExplained;
                Ok(format!("profile: {concept} → not_explained"))
            }
            Edit::AssumeUnderstood(concept) => {
                let entry = self
                    .entry_mut(&concept)
                    .ok_or_else(|| format!("no entry for {concept}"))?;
                // The single construction of `understood` in the crate, and
                // it is reachable only because the user typed the command.
                entry.state = State::Understood;
                Ok(format!("profile: {concept} → understood (your call)"))
            }
            Edit::Forget(concept) => {
                let before = self.entries.len();
                self.entries.retain(|e| e.concept != concept.trim());
                if self.entries.len() == before {
                    return Err(format!("no entry for {concept}"));
                }
                Ok(format!("profile: forgot {concept}"))
            }
        }
    }

    fn entry(&self, concept: &str) -> Option<&Entry> {
        let concept = concept.trim();
        self.entries.iter().find(|e| e.concept == concept)
    }

    fn entry_mut(&mut self, concept: &str) -> Option<&mut Entry> {
        let concept = concept.trim().to_string();
        self.entries.iter_mut().find(|e| e.concept == concept)
    }
}

/// Read the store. A missing file is an EMPTY store with no warning: day
/// one of learn mode is a file that does not exist, and that is not an
/// error. A file that exists but cannot be read degrades to an empty store
/// WITH the reason, because a silent empty profile would look exactly like
/// a user who has taught us nothing.
pub fn load() -> Profile {
    let Some(path) = store_path() else {
        return Profile::default();
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Profile::default();
    };
    let Some((block, notes)) = split_block(&text) else {
        return Profile {
            entries: Vec::new(),
            notes: text,
            warning: Some(format!(
                "{} has no ```json block, so the store is empty; the notes below it were kept",
                display_path(&path)
            )),
        };
    };
    match serde_json::from_str::<Vec<Entry>>(&block) {
        Ok(entries) => Profile {
            entries,
            notes,
            warning: None,
        },
        Err(e) => Profile {
            entries: Vec::new(),
            notes,
            warning: Some(format!(
                "{} json block could not be read ({e}); the store is empty until it is fixed",
                display_path(&path)
            )),
        },
    }
}

/// Write the store, keeping the human notes and everything outside the
/// block. Atomic: a temp file in the same directory, then a rename, so a
/// reader never sees half a file and a failed write leaves the old one.
pub fn save(profile: &Profile) -> std::io::Result<()> {
    let Some(path) = store_path() else {
        return Ok(());
    };
    let body = render_file(profile);
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "PROFILE.md".to_string());
    let temp = parent.join(format!("{file_name}.tmp"));
    let written = (|| {
        std::fs::write(&temp, body.as_bytes())?;
        std::fs::rename(&temp, &path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// The file as it is written: the machine block first, then the notes,
/// verbatim. No heading is inserted, because a round trip must be
/// idempotent — anything this function adds to the notes comes back as
/// notes on the next read, and a header a human never wrote is the kind
/// of thing that accumulates.
pub fn render_file(profile: &Profile) -> String {
    let entries = serde_json::to_string_pretty(&profile.entries).unwrap_or_else(|_| "[]".into());
    let mut out = format!("# Profile\n\n```json\n{entries}\n```\n");
    if !profile.notes.trim().is_empty() {
        out.push('\n');
        out.push_str(profile.notes.trim_end());
        out.push('\n');
    }
    out
}

/// Split the file into the first fenced ```json block's body and
/// everything after it (the human notes). Content before the block is
/// dropped, so the writer's own header is not echoed back.
fn split_block(text: &str) -> Option<(String, String)> {
    let start = text.find("```json")? + "```json".len();
    let rest = &text[start..];
    let end = rest.find("```")?;
    let notes = rest[end + "```".len()..].to_string();
    Some((rest[..end].to_string(), notes))
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

/// Today, as `YYYY-MM-DD` UTC, computed from the epoch so no date crate is
/// needed. Bookkeeping only.
fn today() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let days = (secs / 86_400) as i64;
    // days-from-civil, the standard inverse (Howard Hinnant's algorithm):
    // shift the epoch to 0000-03-01 so leap days land at the end.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

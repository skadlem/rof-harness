//! Per-repo research: `.rof/research/index.md` plus one note per topic
//! (design report §7, build item 5).
//!
//! # The layout: a path-addressed mirror, not a vector store
//!
//! ```text
//! <work root>/.rof/research/index.md      one line per note: topic → path → pinned commit
//! <work root>/.rof/research/<topic>.md    one note per QUESTION, not per session
//! <work root>/.rof/research/tests/<topic>.md   research ABOUT a test suite
//! ```
//!
//! The work root IS the repo boundary, so there is no `<repo>` segment to
//! keep in sync with a name the user could change by moving the checkout —
//! the design report's `.rof/research/<repo>/` says the same thing one level
//! deeper and adds a name that can go stale on its own.
//!
//! **Why not embeddings.** Three reasons, in order of weight.
//!
//! 1. *Retrieval must be decidable without a judgement.* A note is retrieved
//!    because a path says so, or it is not retrieved. A nearest-neighbour
//!    score is a threshold someone tuned, and the first time it is wrong the
//!    harness serves a note about the wrong thing and believes it — which is
//!    the exact failure "do I need new research?" exists to prevent.
//! 2. *Code-adjacent knowledge has a known address.* "what does the backoff
//!    do" is a question about a file. When the answer is a path, the address
//!    is the path, and the strong external pattern is a deterministic mirror
//!    plus full-text search with git as the validator — not a similarity
//!    index over prose.
//! 3. *Cost and failure mode.* An index is a second store that can disagree
//!    with the tree, drift from it, and cost money and latency to query. This
//!    folder is markdown in the repo: reviewable in a diff, editable by hand,
//!    and gone the moment the user deletes it.
//!
//! # A note PINS a commit, and staleness is COMPUTED
//!
//! "Do I actually need new research?" is not a judgement anyone makes. A note
//! records the commit it was verified against, and freshness is a comparison:
//!
//! - **FRESH** ⟺ the note's pin equals the tree's current HEAD, *and* the
//!   note file still carries that same pin.
//! - **STALE** otherwise, always with a computed reason naming the pin and
//!   what the tree says now.
//!
//! This is deliberately the CONSERVATIVE rule: ANY movement of HEAD marks a
//! note stale, whether or not the movement touched what the note describes.
//! Re-verifying too often costs one command; a permissive rule that serves
//! research describing a tree that no longer exists costs a wrong answer that
//! nothing in the harness can detect. The precise rule —
//! `git diff <pinned>..HEAD -- <the note's paths>` — is a strict SUBSET of
//! this one, so anything this rule flags stale may be fine, and nothing it
//! calls fresh can be out of date. It is deferred because the plumbing for it
//! does not exist in this slice: `engine::tree` exposes a change set against
//! the run's baseline, not a diff between two arbitrary commits, and this
//! module must not run its own `git diff` (one substrate, one read).
//!
//! A tree whose HEAD cannot be read is `TreeState::unknown()`, which is
//! STALE. "Could not confirm" is never reported as "confirmed".
//!
//! # What is deliberately NOT here
//!
//! Nothing in this slice consults the folder. There is no orchestrator read,
//! no model call, and no prompt text: the store, the staleness computation
//! and the user commands ship, and wiring the run to ask the store before it
//! buys research is a separate change (design report §9 item 5's second half).
//! Until that lands, a run does zero research of any kind and this module is
//! reachable only from a `/research` line the user typed.
//!
//! # Safety
//!
//! The folder lives INSIDE the work root, so a topic is a path segment and
//! is refused unless it survives as one: no separator, no `..`, no leading
//! dot, nothing long. The address in the index is re-derived from the topic
//! on every read, so a hand-edited `path` field cannot point the store at a
//! file outside the folder.
//!
//! A note is a working-tree change: `engine::tree`'s change set names it, and
//! once committed, a re-verification is a modification of a committed file.
//! For a DESIGN note that is ordinary repository content. For a note under
//! `tests/` it is not: `is_test_shaped` matches on the `tests` path segment
//! and `TreeDiff::protected_oracle` exempts only UNTRACKED test-shaped paths,
//! so a re-verified test note is read by the write gate as oracle tampering
//! and can reject an otherwise passing review. Kept as a known hazard rather
//! than worked around here, because the fix belongs in the oracle's path
//! filter (`.rof/` is harness bookkeeping, not suite content) and that file
//! is outside this slice.
//!
//! A missing folder is an empty store, not a failure. A folder that exists
//! but cannot be read degrades to an empty store WITH the reason, because an
//! unreadable index and a user who has done no research look identical, and
//! that difference decides whether research gets bought.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

/// The folder, relative to the WORK ROOT. Research is per-repo and the work
/// root is the repo boundary, so the folder carries no repo name of its own.
pub const DIR: &str = ".rof/research";

/// The index file inside [`DIR`].
pub const INDEX_FILE: &str = "index.md";

/// The sub-directory for research ABOUT a test suite (principle 9). A
/// passing suite is not evidence that a design is correct, and the two are
/// never allowed to meet in one lookup.
pub const TESTS_SEGMENT: &str = "tests";

/// The body file's extension.
pub const BODY_EXT: &str = ".md";

/// The fence tag for the index's machine block. The block is one JSON value
/// per LINE, so the index really is one line per note and a hand edit is a
/// one-line edit.
pub const BLOCK: &str = "json";

/// The one machine-written line in a note file. Everything ABOVE it belongs
/// to the harness; everything BELOW it is the user's text and is copied
/// verbatim by every machine write.
pub const HEADER_PREFIX: &str = "<!-- rof:research ";
/// Closing half of the pin line.
pub const HEADER_SUFFIX: &str = " -->";

/// Overrides the work root the `/research` commands act on, the same idiom
/// `context::profile::PATH_ENV` uses. It moves the ROOT, never the rules: a
/// test points it at a scratch repo, and a note written there is still
/// refused if it would leave.
pub const ROOT_ENV: &str = "ROF_RESEARCH_ROOT";

/// Longest topic accepted, in chars. A topic is a filename; a megabyte-long
/// one is a paste accident, not a concept.
const MAX_TOPIC_CHARS: usize = 64;

/// The work root the commands act on: `ROF_RESEARCH_ROOT`, else the process
/// working directory. `None` only when neither can be had.
pub fn work_root() -> Option<PathBuf> {
    if let Some(path) = std::env::var(ROOT_ENV)
        .ok()
        .filter(|p| !p.trim().is_empty())
    {
        return Some(PathBuf::from(path));
    }
    std::env::current_dir().ok()
}

/// Which kind of question a note answers. The ONLY difference is the
/// directory it lives in, and that difference is the whole separation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// About the design: how something works and why.
    Design,
    /// ABOUT a test suite: what the suite asserts, not what is true.
    Tests,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Design => "design",
            Kind::Tests => "tests",
        }
    }

    /// The directory under [`DIR`]: empty for a design note, `tests/` for a
    /// note about a suite.
    pub fn segment(self) -> &'static str {
        match self {
            Kind::Design => "",
            Kind::Tests => "tests/",
        }
    }

    /// A topic spelled `tests/<name>` addresses the tests folder; anything
    /// else is a design topic. Splitting in ONE place is what makes the two
    /// kinds unable to meet: no lookup reaches a test note unless the caller
    /// spelled `tests/`, and a design topic can never be one.
    pub fn split_topic(topic: &str) -> (Kind, &str) {
        let topic = topic.trim();
        match topic.strip_prefix("tests/") {
            Some(rest) if !rest.trim().is_empty() => (Kind::Tests, rest.trim()),
            _ => (Kind::Design, topic),
        }
    }
}

/// The work-root-relative address of a note, e.g.
/// `.rof/research/retry backoff.md`. A FUNCTION of the topic: there is no
/// table to keep in sync and no way to steer one by hand.
pub fn note_rel_path(kind: Kind, name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(format!(
            "refused: a note needs a topic — it lives at <work root>/{DIR}/<topic>{BODY_EXT}"
        ));
    }
    // A topic is ONE path segment. Anything that could climb out of the
    // folder, or that a filesystem would read as something other than a name,
    // is refused here rather than resolved and then checked.
    for bad in ["/", "\\", "\n", "\r", "\0"] {
        if name.contains(bad) {
            return Err(escaped(name));
        }
    }
    if name.starts_with('.') {
        return Err(escaped(name));
    }
    if name.chars().count() > MAX_TOPIC_CHARS {
        return Err(format!(
            "refused: `{name}` is longer than {MAX_TOPIC_CHARS} characters — a topic is a filename"
        ));
    }
    let path = format!("{DIR}/{}{name}{BODY_EXT}", kind.segment());
    // Belt and braces, so the rule is checked by the filesystem's own idea of
    // a path rather than by this function's string arithmetic alone.
    let rel = Path::new(&path);
    let inside = !rel.is_absolute()
        && rel.starts_with(DIR)
        && rel
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)));
    if !inside {
        return Err(escaped(name));
    }
    Ok(path)
}

fn escaped(name: &str) -> String {
    format!(
        "refused: `{name}` is not a topic — a note lives at <work root>/{DIR}/[tests/]<topic>{BODY_EXT} and cannot leave that folder"
    )
}

/// The tree facts staleness is computed from, supplied by the caller so the
/// comparison is a pure function and a render path never spawns anything.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeState {
    head: Option<String>,
}

impl TreeState {
    /// The tree at a known commit. An empty commit is [`Self::unknown`],
    /// because "pinned to nothing" is not a claim anything can check.
    pub fn at(commit: impl Into<String>) -> TreeState {
        let commit = commit.into();
        let commit = commit.trim().to_string();
        TreeState {
            head: (!commit.is_empty()).then_some(commit),
        }
    }

    /// No commit could be read. Every note is stale against it.
    pub fn unknown() -> TreeState {
        TreeState { head: None }
    }

    pub fn head(&self) -> Option<&str> {
        self.head.as_deref()
    }

    /// Read HEAD from a work root with one `git rev-parse`, and nothing more.
    ///
    /// NOT a draw path: this spawns a process, so it belongs to an explicit
    /// user command (`/research list|write|verify`) and never to a render.
    /// When the run is wired to consult the folder, the caller will pass the
    /// engine's own commit here instead and this reader can be dropped — a
    /// second reader of "what is HEAD" that could disagree with the write
    /// gate's is not something to keep twice.
    pub fn read(work_root: &Path) -> TreeState {
        let read = Command::new("git")
            .arg("-C")
            .arg(work_root)
            .args(["rev-parse", "--verify", "HEAD"])
            .output();
        match read {
            Ok(out) if out.status.success() => {
                TreeState::at(String::from_utf8_lossy(&out.stdout).into_owned())
            }
            // Not a repo, no git, unborn HEAD: all the same answer here, and
            // all of them degrade to "cannot confirm" rather than to fresh.
            _ => TreeState::unknown(),
        }
    }
}

/// Why a note is not fresh. Every variant is computed from a value in hand;
/// none of them is a judgement about whether the note still reads true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleReason {
    /// The tree has moved off the pinned commit. Any movement, not only a
    /// movement that touched the note's subject — see the module doc.
    HeadMoved { pinned: String, head: String },
    /// No commit could be read, so the pin cannot be confirmed.
    HeadUnknown { pinned: String },
    /// The body is gone, unreadable, or carries a different pin than the
    /// index claims. A pin nobody can read is not a pin.
    NoteUnreadable { detail: String },
}

/// Computed freshness of one note. `Fresh` is the ONLY state in which a note
/// may answer a question.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Freshness {
    Fresh {
        pinned: String,
    },
    Stale(StaleReason),
    /// No note at this address. Not staleness — absence — and never fresh.
    Missing {
        topic: String,
    },
}

impl Freshness {
    pub fn is_fresh(&self) -> bool {
        matches!(self, Freshness::Fresh { .. })
    }

    /// The one line a user is shown, naming the pin and the way out.
    pub fn reason_line(&self) -> String {
        match self {
            Freshness::Fresh { pinned } => format!("verified against {pinned}"),
            Freshness::Missing { topic } => format!("no note for `{topic}`"),
            Freshness::Stale(why) => why.line(),
        }
    }
}

impl StaleReason {
    pub fn line(&self) -> String {
        match self {
            StaleReason::HeadMoved { pinned, head } => format!(
                "the tree moved off the pinned commit {pinned} (HEAD is now {head}) — re-verify it with `/research verify <topic>`"
            ),
            StaleReason::HeadUnknown { pinned } => format!(
                "no commit could be read here, so the pin {pinned} cannot be confirmed — re-verify it with `/research verify <topic>`"
            ),
            StaleReason::NoteUnreadable { detail } => format!(
                "the note could not be read ({detail}) — re-verify it with `/research verify <topic>` or write it again"
            ),
        }
    }
}

/// One index line: the topic, where it lives, and the commit it was verified
/// against. No body: the body is the user's file, not an index field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// The address key, `tests/<name>` included, so the kind is visible in
    /// the index without reading the path.
    pub topic: String,
    pub kind: Kind,
    /// Work-root-relative, always re-derived from the topic on read.
    pub path: String,
    /// The commit this note was verified against. Never empty.
    pub pinned: String,
}

/// A note that answered a question, with the body it answered it with, so the
/// answer and its evidence travel together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub topic: String,
    pub kind: Kind,
    pub path: String,
    pub pinned: String,
    pub body: String,
}

/// The verdict of "do I need new research for this topic?". Two states and
/// no third: a fresh note answered, or nothing did. There is no confidence,
/// no partial answer and no similarity — the state that is not `Answered` is
/// the state in which new research is bought.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Answered(Answer),
    NotAnswered { topic: String, reason: String },
}

impl Verdict {
    pub fn is_answered(&self) -> bool {
        matches!(self, Verdict::Answered(_))
    }

    /// The note that answered, or `None` — which is the whole of the
    /// "not answered" side, including a note that exists but is stale.
    pub fn answering_note(&self) -> Option<&Answer> {
        match self {
            Verdict::Answered(answer) => Some(answer),
            Verdict::NotAnswered { .. } => None,
        }
    }

    /// Why nothing answered. Always says something, so "not answered" is
    /// never a bare no.
    pub fn reason(&self) -> &str {
        match self {
            Verdict::Answered(_) => "",
            Verdict::NotAnswered { reason, .. } => reason,
        }
    }
}

/// What the user asked the store to do. Every variant is reachable only from
/// a `/research` subcommand, exactly as `profile::Edit` is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Edit {
    /// A new note. `body` is the user's text; the harness writes no research
    /// content of its own, and a blank body is refused rather than filled in.
    Write { topic: String, body: String },
    /// Re-pin an existing note at the tree's current commit. A
    /// re-verification, not a new claim: the topic and the body are the ones
    /// already on disk.
    Verify { topic: String },
    /// Drop a note and its body file.
    Forget { topic: String },
}

/// A body file the next [`Research::save`] writes, staged rather than
/// written by `apply`, so a refused edit can never leave half a note behind.
/// The pin travels with it: the file's own pin line is written from the same
/// value the index line carries, so the two cannot drift.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Staged {
    Body { note: Note, body: String },
    Delete { path: String },
}

/// The whole store: the index's notes, the free-form text below its block,
/// and the reason the index could not be read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Research {
    /// The work root these notes are relative to.
    pub root: PathBuf,
    pub notes: Vec<Note>,
    /// Free-form text below the index's block, preserved verbatim.
    pub notes_text: String,
    /// `Some(reason)` when the index existed but could not be read, or a
    /// line of it could not. A degraded store is a smaller store, never a
    /// failed run.
    pub warning: Option<String>,
    staged: Vec<Staged>,
}

impl Research {
    /// The store with nothing in it, for a work root with no folder.
    pub fn empty(root: &Path) -> Research {
        Research {
            root: root.to_path_buf(),
            ..Research::default()
        }
    }

    /// The note at a topic, `tests/<name>` reaching the tests folder.
    pub fn note(&self, topic: &str) -> Option<&Note> {
        let topic = topic.trim();
        self.notes.iter().find(|n| n.topic == topic)
    }

    fn note_mut(&mut self, topic: &str) -> Option<&mut Note> {
        let topic = topic.trim().to_string();
        self.notes.iter_mut().find(|n| n.topic == topic)
    }

    /// Freshness of the note at a topic, computed. A topic with no note is
    /// `Missing`, which is not fresh.
    pub fn freshness(&self, topic: &str, tree: &TreeState) -> Freshness {
        let topic = topic.trim().to_string();
        let Some(note) = self.note(&topic) else {
            return Freshness::Missing { topic };
        };
        self.note_freshness(note, tree)
    }

    /// The retrieval decision, as a function (design spec §7: "retrieval
    /// before fetch"). The ONLY path by which a note reaches a question in
    /// this slice, and it is a total function of the index and the tree: no
    /// model, no ranking, no judgement.
    pub fn needs_research(&self, topic: &str, tree: &TreeState) -> Verdict {
        let topic = topic.trim().to_string();
        let Some(note) = self.note(&topic) else {
            let reason = match &self.warning {
                // An index that could not be read must never read as "this
                // user has no research", because that difference is what
                // decides whether research gets bought.
                Some(warning) => format!(
                    "no readable note for `{topic}` — and the index itself is degraded: {warning}"
                ),
                None => format!(
                    "no note for `{topic}` — nothing in {DIR} covers it, so this is where new research is bought"
                ),
            };
            return Verdict::NotAnswered { topic, reason };
        };
        let note = note.clone();
        match self.note_freshness(&note, tree) {
            Freshness::Fresh { .. } => match self.read_body(&note) {
                Ok(body) => Verdict::Answered(Answer {
                    topic: note.topic,
                    kind: note.kind,
                    path: note.path,
                    pinned: note.pinned,
                    body,
                }),
                Err(detail) => Verdict::NotAnswered {
                    topic: topic.clone(),
                    reason: format!(
                        "`{topic}` cannot be read ({detail}) — new research is what is needed"
                    ),
                },
            },
            Freshness::Missing { .. } => Verdict::NotAnswered {
                topic: topic.clone(),
                reason: format!("no note for `{topic}`"),
            },
            Freshness::Stale(why) => Verdict::NotAnswered {
                topic: topic.clone(),
                reason: format!(
                    "`{topic}` is stale: {} — new research is what is needed",
                    why.line()
                ),
            },
        }
    }

    /// The freshness rule, in one place, in the order it refuses: the body
    /// must exist and carry the pin the index claims, and the tree must be
    /// readable and still AT that pin.
    fn note_freshness(&self, note: &Note, tree: &TreeState) -> Freshness {
        let text = match std::fs::read_to_string(self.root.join(&note.path)) {
            Ok(text) => text,
            Err(e) => {
                return Freshness::Stale(StaleReason::NoteUnreadable {
                    detail: format!("{}: {e}", note.path),
                })
            }
        };
        let (kind, pinned, _) = match split_note(&text) {
            Ok(parsed) => parsed,
            Err(detail) => return Freshness::Stale(StaleReason::NoteUnreadable { detail }),
        };
        if pinned != note.pinned || kind != note.kind {
            return Freshness::Stale(StaleReason::NoteUnreadable {
                detail: format!(
                    "{} pins {}/{} but the index says {}/{}",
                    note.path,
                    kind.name(),
                    pinned,
                    note.kind.name(),
                    note.pinned
                ),
            });
        }
        match tree.head() {
            None => Freshness::Stale(StaleReason::HeadUnknown {
                pinned: note.pinned.clone(),
            }),
            Some(head) if head == note.pinned => Freshness::Fresh {
                pinned: note.pinned.clone(),
            },
            Some(head) => Freshness::Stale(StaleReason::HeadMoved {
                pinned: note.pinned.clone(),
                head: head.to_string(),
            }),
        }
    }

    /// The user's own text, verbatim.
    fn read_body(&self, note: &Note) -> Result<String, String> {
        let text = std::fs::read_to_string(self.root.join(&note.path))
            .map_err(|e| format!("{}: {e}", note.path))?;
        split_note(&text).map(|(_, _, body)| body)
    }

    /// The store's ONE mutating entry point. Every caller is a user command,
    /// and this is where a refused edit is refused. No write happens here:
    /// bodies are staged for [`Research::save`], so a refusal leaves the
    /// folder exactly as it was.
    pub fn apply(&mut self, edit: Edit, tree: &TreeState) -> Result<String, String> {
        let Some(head) = tree.head().map(|h| h.to_string()) else {
            return Err(
                "refused: no commit could be read in this work root, so a note has nothing to \
                 pin — a note records the tree it was verified against"
                    .to_string(),
            );
        };
        match edit {
            Edit::Write { topic, body } => {
                let topic = topic.trim().to_string();
                let (kind, name) = Kind::split_topic(&topic);
                let path = note_rel_path(kind, name)?;
                if self.note(&topic).is_some() {
                    return Err(format!(
                        "refused: {topic} is already recorded at {path} — `/research verify {topic}` re-pins what is there, and the harness will not hold a second claim about one topic"
                    ));
                }
                if body.trim().is_empty() {
                    return Err(format!(
                        "refused: {topic} needs the body you supply — the harness writes no research content of its own"
                    ));
                }
                let note = Note {
                    topic: topic.clone(),
                    kind,
                    path: path.clone(),
                    pinned: head.clone(),
                };
                self.notes.push(note.clone());
                self.staged.push(Staged::Body { note, body });
                Ok(format!(
                    "research: wrote {topic} at {path}, pinned to {head}"
                ))
            }
            Edit::Verify { topic } => {
                let topic = topic.trim().to_string();
                let Some(note) = self.note(&topic) else {
                    return Err(format!(
                        "refused: no note for {topic} — `/research write {topic} -- <body>` creates one"
                    ));
                };
                let (path, kind) = (note.path.clone(), note.kind);
                // The body is read back, not reconstructed, so a user's hand
                // edit is what gets re-pinned. An unreadable note is refused
                // rather than overwritten with something the harness wrote.
                let current = std::fs::read_to_string(self.root.join(&path))
                    .map_err(|e| format!("refused: {path} could not be read ({e})"))?;
                let (file_kind, _, body) = split_note(&current)
                    .map_err(|detail| format!("refused: {path} has no usable pin ({detail})"))?;
                if file_kind != kind {
                    return Err(format!(
                        "refused: {path} is a {} note but the index says {}",
                        file_kind.name(),
                        kind.name()
                    ));
                }
                let note = self
                    .note_mut(&topic)
                    .expect("the note was just found and the topic matches");
                let before = note.pinned.clone();
                note.pinned = head.clone();
                let repinned = note.clone();
                self.staged.push(Staged::Body {
                    note: repinned,
                    body,
                });
                Ok(format!(
                    "research: {topic} re-verified — {before} → {head} (a re-verification, not a new claim)"
                ))
            }
            Edit::Forget { topic } => {
                let topic = topic.trim().to_string();
                let before = self.notes.len();
                let Some(note) = self.note(&topic).cloned() else {
                    return Err(format!("refused: no note for {topic}"));
                };
                self.notes.retain(|n| n.topic != topic);
                debug_assert_eq!(before - 1, self.notes.len());
                self.staged.push(Staged::Delete {
                    path: note.path.clone(),
                });
                Ok(format!("research: forgot {topic} ({} removed)", note.path))
            }
        }
    }

    /// Write the staged bodies and the index. Bodies first: an index that
    /// points at a file that is not there yet is a store that lies about
    /// being complete, and an unreferenced file is merely ignored.
    pub fn save(&mut self) -> std::io::Result<()> {
        for staged in std::mem::take(&mut self.staged) {
            match staged {
                Staged::Body { note, body } => {
                    write_atomic(&self.root.join(&note.path), &render_note(&note, &body))?
                }
                Staged::Delete { path } => match std::fs::remove_file(self.root.join(&path)) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e),
                },
            }
        }
        let index = self.root.join(DIR).join(INDEX_FILE);
        write_atomic(&index, &render_index(self))
    }

    /// Read the store from a work root. A missing folder is an EMPTY store
    /// with no warning: day one is a repo that has done no research, and
    /// that is not an error. An index that exists but cannot be read
    /// degrades to a smaller store WITH the reason.
    pub fn load(work_root: &Path) -> Research {
        let mut store = Research::empty(work_root);
        let path = work_root.join(DIR).join(INDEX_FILE);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return store;
        };
        let Some((block, notes_text)) = split_index(&text) else {
            store.notes_text = text;
            store.warning = Some(format!(
                "{DIR}/{INDEX_FILE} has no ```{BLOCK} block, so the store is empty; the notes below it were kept"
            ));
            return store;
        };
        store.notes_text = notes_text;
        let mut broken: Vec<String> = Vec::new();
        for (at, line) in block.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str::<Note>(line) {
                Ok(note) => match adopt(&note) {
                    Ok(fixed) => {
                        if let Some(complaint) = fixed.complaint {
                            broken.push(format!("line {}: {complaint}", at + 1));
                        }
                        store.notes.push(fixed.note);
                    }
                    Err(complaint) => broken.push(format!("line {}: {complaint}", at + 1)),
                },
                Err(e) => broken.push(format!("line {}: {e}", at + 1)),
            }
        }
        if !broken.is_empty() {
            store.warning = Some(format!(
                "{DIR}/{INDEX_FILE}: {} line(s) could not be read ({}); the rest of the index loaded",
                broken.len(),
                broken.join("; ")
            ));
        }
        store
    }
}

struct Adopted {
    note: Note,
    /// `Some` when the line could not be taken at face value.
    complaint: Option<String>,
}

/// Re-derive a line's address from its topic, so a hand-edited or
/// copied-from-elsewhere `path` cannot point the store at a file outside the
/// folder, and refuse a line whose topic is not a topic at all.
fn adopt(note: &Note) -> Result<Adopted, String> {
    let (kind, name) = Kind::split_topic(&note.topic);
    let path = note_rel_path(kind, name)?;
    if note.pinned.trim().is_empty() {
        return Err(format!(
            "`{}` pins no commit, so it is not a note",
            note.topic
        ));
    }
    let complaint = (path != note.path || kind != note.kind).then(|| {
        format!(
            "`{}` claimed {} but its address is {path}",
            note.topic, note.path
        )
    });
    Ok(Adopted {
        note: Note {
            topic: note.topic.trim().to_string(),
            kind,
            path,
            pinned: note.pinned.trim().to_string(),
        },
        complaint,
    })
}

/// The index as it is written: the machine block, one note per line, then
/// the free-form text below it, verbatim. No heading is inserted above the
/// block, because a round trip must be idempotent — anything this adds
/// comes back on the next read.
pub fn render_index(store: &Research) -> String {
    let block = store
        .notes
        .iter()
        .map(|n| serde_json::to_string(n).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\n");
    let mut out = format!("# Research index\n\n```{BLOCK}\n{block}\n```\n");
    if !store.notes_text.trim().is_empty() {
        out.push('\n');
        out.push_str(store.notes_text.trim_end());
        out.push('\n');
    }
    out
}

/// A note file as it is written: the topic heading, the ONE pin line, then
/// the user's body EXACTLY as supplied, including the blank line that
/// separates it from the pin. Everything above the pin is the harness's and
/// everything below it is the user's, which is what makes a machine write
/// non-destructive to a hand-edited note.
pub fn render_note(note: &Note, body: &str) -> String {
    format!(
        "# {}\n\n{HEADER_PREFIX}kind={} pinned={}{HEADER_SUFFIX}\n{body}",
        Kind::split_topic(&note.topic).1.trim(),
        note.kind.name(),
        note.pinned
    )
}

/// Split a note file into its pin line and everything after it: the heading
/// and the pin belong to the harness, and the rest is the user's.
fn split_note(text: &str) -> Result<(Kind, String, String), String> {
    let mut offset = 0usize;
    let mut header = None;
    for line in text.split_inclusive('\n') {
        offset += line.len();
        if line.trim_start().starts_with(HEADER_PREFIX) {
            header = Some(line.trim().to_string());
            break;
        }
    }
    let header = header.ok_or_else(|| {
        format!(
            "no `{}` pin line, so the harness never verified it",
            HEADER_PREFIX.trim()
        )
    })?;
    let rest = header
        .trim()
        .strip_prefix(HEADER_PREFIX)
        .and_then(|r| r.strip_suffix(HEADER_SUFFIX))
        .ok_or_else(|| format!("`{header}` is not a pin line"))?;
    // Split from the right: the commit is a hash and the topic is not part
    // of the line, so the two `=` markers are unambiguous this way.
    let (kind_field, pinned) = rest
        .rsplit_once(" pinned=")
        .ok_or_else(|| format!("`{header}` has no pinned commit"))?;
    let kind_name = kind_field
        .trim()
        .strip_prefix("kind=")
        .ok_or_else(|| format!("`{header}` has no kind"))?;
    let kind = match kind_name.trim() {
        "design" => Kind::Design,
        "tests" => Kind::Tests,
        other => return Err(format!("`{header}` has an unknown kind `{other}`")),
    };
    Ok((kind, pinned.trim().to_string(), text[offset..].to_string()))
}

/// Split the index into the machine block's body and everything after it.
fn split_index(text: &str) -> Option<(String, String)> {
    let start = text.find(&format!("```{BLOCK}"))? + BLOCK.len() + 3;
    let rest = &text[start..];
    let end = rest.find("```")?;
    Some((rest[..end].to_string(), rest[end + 3..].to_string()))
}

/// A temp file in the same directory, then a rename: a reader never sees half
/// a note, and a failed write leaves the old one.
fn write_atomic(path: &Path, body: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| INDEX_FILE.to_string());
    let temp = parent.join(format!("{name}.tmp"));
    let written = (|| {
        std::fs::write(&temp, body.as_bytes())?;
        std::fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

//! SKILL.md as procedural memory: a directory of skills, each one a `SKILL.md`
//! with YAML-ish frontmatter plus optional support files.
//!
//! Progressive disclosure: the prompt carries `index()` (names + one-line
//! descriptions, byte-stable); a body is fetched (`view`) only when it is
//! needed — because the task names the skill, or because the model asks.
//!
//! Write policy defaults to `Propose`. An agent editing its own instructions
//! unattended is the one failure mode this module must not have: a proposal
//! lands in `<root>/../proposals/skills/<id>.json` and a human applies it with
//! `rof skills approve <id>`.
//!
//! Frontmatter is a deliberate minimal subset (`key: value`, `key: [a, b]`,
//! `- item` lists, folded `key: >`): the crate stays dependency-free until
//! frontmatter needs more than this, and `serde_yaml` is a one-line upgrade
//! the day it does.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Skill names are directory names: lowercase, hyphenated, no path games.
pub const MAX_NAME_LEN: usize = 64;
/// How much description the prompt index carries per skill.
pub const INDEX_DESC_CHARS: usize = 60;
/// How many skills the index lists before it starts counting the rest. The
/// index rides every prompt, so it is budgeted like everything else.
pub const INDEX_MAX: usize = 50;
/// Support-file reads are capped (a skill is a document, not a database).
const SUPPORT_FILE_CAP: usize = 64 * 1024;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum SkillError {
    #[error("skill not found: {0}")]
    NotFound(String),
    #[error("invalid skill: {0}")]
    Invalid(String),
    #[error("denied: {0}")]
    Denied(String),
    #[error("io: {0}")]
    Io(String),
}

impl From<std::io::Error> for SkillError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// The index entry: all the prompt ever sees until a body is asked for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// `<root>/<name>/SKILL.md`.
    pub path: PathBuf,
    /// Which root it came from: `user` (the writable one) or `repo`.
    pub source: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    pub meta: SkillMeta,
    pub body: String,
}

/// A support file: listed with its size, content only on request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillFile {
    pub rel: String,
    pub bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillView {
    pub meta: SkillMeta,
    /// `None` = the SKILL.md body; `Some(rel)` = that support file's text.
    #[serde(default)]
    pub file: Option<String>,
    pub body: String,
    pub files: Vec<SkillFile>,
}

/// Everything `skills.manage` can do. Internally tagged so the tool input is
/// the obvious JSON: `{"op": "create", "name": ..., "description": ..., ...}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum SkillOp {
    Create {
        name: String,
        description: String,
        #[serde(default)]
        body: String,
    },
    Patch {
        name: String,
        find: String,
        replace: String,
    },
    WriteFile {
        name: String,
        rel: String,
        content: String,
    },
    Delete {
        name: String,
    },
}

impl SkillOp {
    pub fn name(&self) -> &str {
        match self {
            SkillOp::Create { name, .. }
            | SkillOp::Patch { name, .. }
            | SkillOp::WriteFile { name, .. }
            | SkillOp::Delete { name } => name,
        }
    }
}

/// How a manage op lands. `Propose` is the default and the only safe one for
/// an unattended agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillPolicy {
    ReadOnly,
    #[default]
    Propose,
    Direct,
}

/// What happened to a manage op.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillChange {
    /// `proposed` | `applied` | `deleted`.
    pub outcome: String,
    pub name: String,
    /// Set when the outcome is a proposal.
    #[serde(default)]
    pub id: Option<String>,
    pub detail: String,
    #[serde(default)]
    pub bytes: u64,
}

/// A pending change, waiting for a human. Plain JSON on disk so it can be
/// reviewed in a terminal or committed to a repo like any other artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillProposal {
    pub id: String,
    pub op: SkillOp,
    /// Who proposed it (`implementer`, ...).
    pub agent: String,
    #[serde(default)]
    pub rationale: String,
    pub created_at: u64,
    /// `proposed` | `applied` | `rejected`.
    pub status: String,
}

impl SkillProposal {
    pub fn pending(&self) -> bool {
        self.status == "proposed"
    }
}

/// What a scan found, plus what it refused to guess about.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Scan {
    pub skills: Vec<SkillMeta>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SkillManager {
    root: PathBuf,
    extra_root: Option<PathBuf>,
    policy: SkillPolicy,
}

impl SkillManager {
    pub fn new(root: PathBuf, extra_root: Option<PathBuf>, policy: SkillPolicy) -> Self {
        Self {
            root,
            extra_root,
            policy,
        }
    }

    /// The default home: `~/.rof/skills` (relative fallback when `HOME` is
    /// unset, so a test or a container still gets a usable path).
    pub fn default_root() -> PathBuf {
        match std::env::var("HOME") {
            Ok(h) if !h.trim().is_empty() => PathBuf::from(h.trim()).join(".rof/skills"),
            _ => PathBuf::from(".rof/skills"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn policy(&self) -> SkillPolicy {
        self.policy
    }
    /// Where proposals wait: next to the skills root (`~/.rof/proposals/skills`).
    pub fn proposals_dir(&self) -> PathBuf {
        match self.root.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.join("proposals/skills"),
            _ => self.root.join(".proposals"),
        }
    }

    /// Both roots, in precedence order: the writable one first.
    fn roots(&self) -> Vec<(PathBuf, &'static str)> {
        let mut v = vec![(self.root.clone(), "user")];
        if let Some(x) = &self.extra_root {
            if x.is_dir() && *x != self.root {
                v.push((x.clone(), "repo"));
            }
        }
        v
    }

    pub fn list(&self) -> Vec<SkillMeta> {
        self.scan().skills
    }

    /// Read every `<root>/<name>/SKILL.md`. Directory order is filesystem
    /// order, so the result is sorted by name — the index rides the cached
    /// prompt prefix and must be byte-stable.
    pub fn scan(&self) -> Scan {
        let mut out: Vec<SkillMeta> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();
        for (root, source) in self.roots() {
            let Ok(rd) = std::fs::read_dir(&root) else {
                continue;
            };
            let mut dirs: Vec<PathBuf> = rd
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.path())
                .collect();
            dirs.sort();
            for dir in dirs {
                let name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if out.iter().any(|s| s.name == name) {
                    continue; // first root wins: a repo skill cannot shadow the user's
                }
                let md = dir.join("SKILL.md");
                if !md.is_file() {
                    warnings.push(format!("{name}: no SKILL.md in {}", dir.display()));
                    continue;
                }
                if !valid_name(&name) {
                    warnings.push(format!(
                        "{name}: invalid skill name (lowercase-hyphen, <= {MAX_NAME_LEN})"
                    ));
                    continue;
                }
                let text = match std::fs::read_to_string(&md) {
                    Ok(t) => t,
                    Err(e) => {
                        warnings.push(format!("{name}: unreadable SKILL.md: {e}"));
                        continue;
                    }
                };
                match parse_skill(&text) {
                    Ok((parsed, _)) => {
                        if let Some(n) = &parsed.name {
                            if n != &name {
                                warnings.push(format!(
                                    "{name}: frontmatter says name: {n} (the directory names the skill)"
                                ));
                            }
                        }
                        out.push(SkillMeta {
                            name,
                            description: parsed.description.unwrap_or_default(),
                            version: parsed.version,
                            tags: parsed.tags,
                            path: md,
                            source: source.to_string(),
                        });
                    }
                    Err(e) => warnings.push(format!("{name}: {e}")),
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Scan {
            skills: out,
            warnings,
        }
    }

    /// The prompt index: one `- name: description` line per skill, capped.
    /// Empty string when there are no skills — callers must not render an
    /// empty header into a prompt.
    pub fn index(&self, max: usize) -> String {
        let skills = self.list();
        if skills.is_empty() {
            return String::new();
        }
        let max = max.max(1);
        let mut out = String::new();
        for s in skills.iter().take(max) {
            out.push_str(&format!(
                "- {}: {}\n",
                s.name,
                one_line(&s.description, INDEX_DESC_CHARS)
            ));
        }
        if skills.len() > max {
            out.push_str(&format!("[...{} more]\n", skills.len() - max));
        }
        out
    }

    /// A skill's text, or one of its support files. `file` is a path relative
    /// to the skill directory.
    pub fn view(&self, name: &str, file: Option<&str>) -> Result<SkillView, SkillError> {
        let meta = self
            .list()
            .into_iter()
            .find(|s| s.name == name)
            .ok_or_else(|| SkillError::NotFound(name.to_string()))?;
        let dir = meta
            .path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let files = list_files(&dir);
        let (file, body) = match file {
            None => {
                let md = std::fs::read_to_string(&meta.path)?;
                (None, parse_skill(&md)?.1)
            }
            Some(rel) => {
                let p = safe_child(&dir, rel)?;
                let data = std::fs::read_to_string(&p)?;
                if data.len() > SUPPORT_FILE_CAP {
                    return Err(SkillError::Invalid(format!(
                        "{rel} is {} bytes (cap {SUPPORT_FILE_CAP})",
                        data.len()
                    )));
                }
                (Some(rel.to_string()), data)
            }
        };
        Ok(SkillView {
            meta,
            file,
            body,
            files,
        })
    }

    /// A manage op, under policy. `Propose` (the default) never touches the
    /// store: it validates the op and writes a proposal a human applies.
    pub fn manage(
        &self,
        op: SkillOp,
        agent: &str,
        rationale: &str,
    ) -> Result<SkillChange, SkillError> {
        match self.policy {
            SkillPolicy::ReadOnly => Err(SkillError::Denied(
                "skills policy is readonly (set skills.policy to propose or direct)".to_string(),
            )),
            SkillPolicy::Direct => self.apply(&op),
            SkillPolicy::Propose => {
                self.validate(&op)?;
                let id = new_proposal_id();
                let proposal = SkillProposal {
                    id: id.clone(),
                    op,
                    agent: agent.to_string(),
                    rationale: rationale.to_string(),
                    created_at: now_secs(),
                    status: "proposed".to_string(),
                };
                let dir = self.proposals_dir();
                std::fs::create_dir_all(&dir)?;
                std::fs::write(
                    dir.join(format!("{id}.json")),
                    serde_json::to_string_pretty(&proposal).unwrap_or_default(),
                )?;
                Ok(SkillChange {
                    outcome: "proposed".to_string(),
                    name: proposal.op.name().to_string(),
                    id: Some(id.clone()),
                    detail: format!("proposal {id} pending; apply with `rof skills approve {id}`"),
                    bytes: 0,
                })
            }
        }
    }

    /// Apply an op to the writable root. Used by `Direct` policy and by the
    /// approval path — the human's `approve` is what makes an op land.
    pub fn apply(&self, op: &SkillOp) -> Result<SkillChange, SkillError> {
        self.validate(op)?;
        match op {
            SkillOp::Create {
                name,
                description,
                body,
            } => {
                let dir = self.root.join(name);
                std::fs::create_dir_all(&dir)?;
                let text = render_skill(name, description, body);
                std::fs::write(dir.join("SKILL.md"), &text)?;
                Ok(SkillChange {
                    outcome: "applied".to_string(),
                    name: name.clone(),
                    id: None,
                    detail: format!("created {}", dir.join("SKILL.md").display()),
                    bytes: text.len() as u64,
                })
            }
            SkillOp::Patch {
                name,
                find,
                replace,
            } => {
                let dir = self.writable_dir(name)?;
                let md = dir.join("SKILL.md");
                let original = std::fs::read_to_string(&md)?;
                let (updated, start, end) = crate::tools::apply_hunk(&original, find, replace)
                    .map_err(SkillError::Invalid)?;
                std::fs::write(&md, &updated)?;
                Ok(SkillChange {
                    outcome: "applied".to_string(),
                    name: name.clone(),
                    id: None,
                    detail: format!("patched {} (bytes {start}..{end})", md.display()),
                    bytes: updated.len() as u64,
                })
            }
            SkillOp::WriteFile { name, rel, content } => {
                let dir = self.writable_dir(name)?;
                let p = safe_child(&dir, rel)?;
                if content.len() > SUPPORT_FILE_CAP * 4 {
                    return Err(SkillError::Invalid(format!(
                        "content over {}KB cap",
                        SUPPORT_FILE_CAP * 4 / 1024
                    )));
                }
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&p, content)?;
                Ok(SkillChange {
                    outcome: "applied".to_string(),
                    name: name.clone(),
                    id: None,
                    detail: format!("wrote {rel} ({} bytes)", content.len()),
                    bytes: content.len() as u64,
                })
            }
            SkillOp::Delete { name } => {
                let dir = self.writable_dir(name)?;
                std::fs::remove_dir_all(&dir)?;
                Ok(SkillChange {
                    outcome: "deleted".to_string(),
                    name: name.clone(),
                    id: None,
                    detail: format!("removed {}", dir.display()),
                    bytes: 0,
                })
            }
        }
    }

    /// Pending proposals, oldest first.
    pub fn proposals(&self) -> Vec<SkillProposal> {
        let mut out: Vec<SkillProposal> = Vec::new();
        let Ok(rd) = std::fs::read_dir(self.proposals_dir()) else {
            return out;
        };
        for e in rd.flatten() {
            let Ok(text) = std::fs::read_to_string(e.path()) else {
                continue;
            };
            if let Ok(p) = serde_json::from_str::<SkillProposal>(&text) {
                if p.pending() {
                    out.push(p);
                }
            }
        }
        out.sort_by_key(|p| (p.created_at, p.id.clone()));
        out
    }

    /// Load one proposal by id (an unambiguous prefix is accepted: the ids are
    /// long and a human types them).
    pub fn proposal(&self, id: &str) -> Result<SkillProposal, SkillError> {
        let dir = self.proposals_dir();
        let mut hits: Vec<PathBuf> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let stem = e
                    .path()
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                if stem == id || stem.starts_with(id) {
                    hits.push(e.path());
                }
            }
        }
        match hits.len() {
            0 => Err(SkillError::NotFound(format!("proposal {id}"))),
            1 => {
                let text = std::fs::read_to_string(&hits[0])?;
                serde_json::from_str::<SkillProposal>(&text)
                    .map_err(|e| SkillError::Invalid(format!("proposal {id}: {e}")))
            }
            n => Err(SkillError::Invalid(format!(
                "{id} matches {n} proposals; use more characters"
            ))),
        }
    }

    /// The human half of the proposal loop: apply a pending proposal.
    pub fn approve(&self, id: &str) -> Result<(SkillProposal, SkillChange), SkillError> {
        let mut p = self.proposal(id)?;
        if !p.pending() {
            return Err(SkillError::Invalid(format!(
                "proposal {} is already {}",
                p.id, p.status
            )));
        }
        let change = self.apply(&p.op)?;
        p.status = "applied".to_string();
        self.save_proposal(&p)?;
        Ok((p, change))
    }

    /// Refuse a proposal (it stays on disk as a record; it stops being pending).
    pub fn reject(&self, id: &str, why: &str) -> Result<SkillProposal, SkillError> {
        let mut p = self.proposal(id)?;
        if !p.pending() {
            return Err(SkillError::Invalid(format!(
                "proposal {} is already {}",
                p.id, p.status
            )));
        }
        p.status = if why.trim().is_empty() {
            "rejected".to_string()
        } else {
            format!("rejected: {}", why.trim())
        };
        self.save_proposal(&p)?;
        Ok(p)
    }

    fn save_proposal(&self, p: &SkillProposal) -> Result<(), SkillError> {
        let path = self.proposals_dir().join(format!("{}.json", p.id));
        std::fs::write(path, serde_json::to_string_pretty(p).unwrap_or_default())?;
        Ok(())
    }

    /// Cheap validation before a proposal file exists, so a nonsense op is
    /// refused where the agent can see why.
    fn validate(&self, op: &SkillOp) -> Result<(), SkillError> {
        match op {
            SkillOp::Create {
                name, description, ..
            } => {
                if !valid_name(name) {
                    return Err(SkillError::Invalid(format!(
                        "invalid name {name:?}: lowercase a-z, 0-9 and '-', <= {MAX_NAME_LEN} chars"
                    )));
                }
                if description.trim().is_empty() {
                    return Err(SkillError::Invalid(
                        "description is required: one sentence, self-contained, <= 60 chars"
                            .to_string(),
                    ));
                }
                if description.lines().count() > 1 {
                    return Err(SkillError::Invalid(
                        "description must be one line".to_string(),
                    ));
                }
                if self.list().iter().any(|s| &s.name == name) {
                    return Err(SkillError::Invalid(format!(
                        "skill {name} exists; use patch or write_file"
                    )));
                }
                Ok(())
            }
            SkillOp::Patch { name, find, .. } => {
                self.writable_dir(name)?;
                if find.is_empty() {
                    return Err(SkillError::Invalid("find is empty".to_string()));
                }
                Ok(())
            }
            SkillOp::WriteFile { name, rel, .. } => {
                let dir = self.writable_dir(name)?;
                safe_child(&dir, rel)?;
                Ok(())
            }
            SkillOp::Delete { name } => self.writable_dir(name).map(|_| ()),
        }
    }

    /// The skill's directory in the writable root. A skill that only exists in
    /// a read-only root (a repo's own `./skills/`) is refused: the harness does
    /// not edit a checked-in tree behind the user's back.
    fn writable_dir(&self, name: &str) -> Result<PathBuf, SkillError> {
        if !valid_name(name) {
            return Err(SkillError::Invalid(format!("invalid name {name:?}")));
        }
        let dir = self.root.join(name);
        if dir.join("SKILL.md").is_file() {
            return Ok(dir);
        }
        let in_extra = self
            .extra_root
            .as_ref()
            .map(|x| x.join(name).join("SKILL.md").is_file())
            .unwrap_or(false);
        if in_extra {
            return Err(SkillError::Denied(format!(
                "{name} lives in a read-only repo skills root; copy it to {} first",
                self.root.display()
            )));
        }
        Err(SkillError::NotFound(name.to_string()))
    }
}

/// Frontmatter fields a SKILL.md may carry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Frontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub version: Option<String>,
    pub tags: Vec<String>,
}

/// Split a SKILL.md into frontmatter and body. Errors when the frontmatter is
/// missing or has no description: a skill whose `when to use it` line is absent
/// is invisible to progressive disclosure, which is the whole mechanism.
pub fn parse_skill(md: &str) -> Result<(Frontmatter, String), SkillError> {
    let mut lines = md.lines().map(|l| l.trim_end_matches('\r'));
    match lines.next() {
        Some(l) if l.trim() == "---" => {}
        _ => {
            return Err(SkillError::Invalid(
                "missing frontmatter: SKILL.md must start with a `---` line".to_string(),
            ))
        }
    }
    let mut fm = Frontmatter::default();
    let mut body_start = None;
    let mut pending: Option<String> = None;
    let mut index = 1usize;
    for line in lines.by_ref() {
        index += 1;
        if line.trim() == "---" {
            body_start = Some(index);
            break;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        // Continuation of a folded scalar, or an item of a `- ` list.
        if indented || line.trim_start().starts_with("- ") {
            let t = line.trim().trim_start_matches("- ").trim().to_string();
            if t.is_empty() {
                continue;
            }
            match pending.as_deref() {
                Some("description") => {
                    let slot = fm.description.get_or_insert_with(String::new);
                    if !slot.is_empty() {
                        slot.push(' ');
                    }
                    slot.push_str(&t);
                }
                Some("tags") => fm.tags.push(t.trim_matches('"').to_string()),
                _ => {}
            }
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue; // a stray line is ignored: frontmatter is not a schema
        };
        let key = key.trim().to_lowercase();
        let value = value.trim();
        match key.as_str() {
            "name" => fm.name = Some(value.trim_matches('"').to_string()),
            "version" => fm.version = Some(value.trim_matches('"').to_string()),
            "description" => {
                if value == ">" || value == "|" {
                    pending = Some(key.clone());
                } else {
                    fm.description = Some(value.trim_matches('"').to_string());
                    pending = None;
                }
            }
            "tags" => {
                if value.is_empty() {
                    pending = Some(key.clone());
                } else {
                    fm.tags = parse_list(value);
                    pending = None;
                }
            }
            _ => {}
        }
    }
    let Some(_) = body_start else {
        return Err(SkillError::Invalid(
            "unterminated frontmatter: expected a closing `---` line".to_string(),
        ));
    };
    if fm.description.as_deref().unwrap_or("").trim().is_empty() {
        return Err(SkillError::Invalid(
            "frontmatter needs a non-empty `description:` (it is the index line)".to_string(),
        ));
    }
    let body = md
        .lines()
        .skip(body_start.unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    Ok((fm, body.trim_start_matches('\n').trim_end().to_string()))
}

fn parse_list(value: &str) -> Vec<String> {
    let v = value.trim();
    let inner = if v.starts_with('[') && v.ends_with(']') {
        &v[1..v.len() - 1]
    } else {
        v
    };
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn render_skill(name: &str, description: &str, body: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: {description}\n---\n\n{}\n",
        body.trim()
    )
}

/// Directory names are the skill names: lowercase a-z, 0-9, `-`, no leading or
/// trailing `-`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Does `text` name this skill? Exact name first (bounded by word characters),
/// else — for multi-word names — every word of the name has to appear. Same
/// spirit as "the goal names the file" in the retriever: the task says what it
/// wants by name, and the skill is delivered.
pub fn mentions(text: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let t = text.to_lowercase();
    let n = name.to_lowercase();
    if contains_word(&t, &n) {
        return true;
    }
    let words: Vec<&str> = n.split('-').filter(|w| w.len() >= 3).collect();
    n.split('-').count() >= 2 && words.len() >= 2 && words.iter().all(|w| contains_word(&t, w))
}

/// `needle` bounded by non-word characters on both sides (a word character is
/// alphanumeric or `-`).
fn contains_word(hay: &str, needle: &str) -> bool {
    let mut from = 0usize;
    while from < hay.len() {
        let Some(i) = hay[from..].find(needle) else {
            return false;
        };
        let p = from + i;
        let e = p + needle.len();
        let word = |b: u8| b.is_ascii_alphanumeric() || b == b'-';
        let before_ok = p == 0 || !word(hay.as_bytes()[p - 1]);
        let after_ok = e >= hay.len() || !word(hay.as_bytes()[e]);
        if before_ok && after_ok {
            return true;
        }
        from = p + 1;
        while from < hay.len() && !hay.is_char_boundary(from) {
            from += 1;
        }
    }
    false
}

/// One line, capped, for the index. A description that runs long is truncated
/// rather than allowed to cost every prompt.
fn one_line(s: &str, cap: usize) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    let mut out: String = line.chars().take(cap).collect();
    if line.chars().count() > cap {
        out.push('…');
    }
    out
}

/// Everything under a skill directory, relative paths, sorted.
fn list_files(dir: &Path) -> Vec<SkillFile> {
    let mut out = Vec::new();
    collect_files(dir, dir, &mut out, 0);
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    out
}

fn collect_files(base: &Path, dir: &Path, out: &mut Vec<SkillFile>, depth: usize) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let rel = p
            .strip_prefix(base)
            .map(|r| r.to_string_lossy().to_string())
            .unwrap_or_default();
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if is_dir {
            collect_files(base, &p, out, depth + 1);
        } else {
            let bytes = e.metadata().map(|m| m.len() as usize).unwrap_or(0);
            out.push(SkillFile { rel, bytes });
        }
    }
}

/// A path relative to a skill directory, refused if it could leave it.
fn safe_child(dir: &Path, rel: &str) -> Result<PathBuf, SkillError> {
    let rel = rel.trim();
    if rel.is_empty() {
        return Err(SkillError::Invalid("empty file path".to_string()));
    }
    let p = Path::new(rel);
    if p.is_absolute()
        || p.components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(SkillError::Denied(format!(
            "path escapes the skill dir: {rel}"
        )));
    }
    let joined = dir.join(p);
    if !crate::tools::under(dir, &joined) {
        return Err(SkillError::Denied(format!(
            "path escapes the skill dir: {rel}"
        )));
    }
    Ok(joined)
}

fn new_proposal_id() -> String {
    let secs = now_secs();
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("{secs}-{}", &u[..4])
}

pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_variants_and_body() {
        let md = "---\nname: add-doc-comment\ndescription: >\n  Add a doc comment\n  to an item.\nversion: 2\ntags: [rust, docs]\n---\n\n# Steps\n1. read the file\n";
        let (fm, body) = parse_skill(md).unwrap();
        assert_eq!(fm.name.as_deref(), Some("add-doc-comment"));
        assert_eq!(
            fm.description.as_deref(),
            Some("Add a doc comment to an item.")
        );
        assert_eq!(fm.version.as_deref(), Some("2"));
        assert_eq!(fm.tags, vec!["rust", "docs"]);
        assert_eq!(body, "# Steps\n1. read the file");
    }

    #[test]
    fn parses_inline_and_dash_list_tags() {
        let md = "---\nname: a-b\ndescription: one line\ntags:\n  - rust\n  - docs\n---\nbody\n";
        let (fm, body) = parse_skill(md).unwrap();
        assert_eq!(fm.tags, vec!["rust", "docs"]);
        assert_eq!(body, "body");
    }

    #[test]
    fn refuses_a_skill_without_a_usable_description() {
        // No frontmatter at all, no description, unterminated block: all three
        // would make the skill invisible to the index, which is the mechanism.
        for md in [
            "# just a doc\n",
            "---\nname: a-b\n---\nbody\n",
            "---\nname: a-b\ndescription: x\n",
        ] {
            assert!(parse_skill(md).is_err(), "must refuse: {md:?}");
        }
    }

    #[test]
    fn names_are_lowercase_hyphen_and_bounded() {
        for ok in ["a", "add-doc-comment", "skill-2", "x9"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in [
            "",
            "-x",
            "x-",
            "Upper",
            "with space",
            "with/slash",
            "with.dot",
            "../etc",
        ] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert!(!valid_name(&"a".repeat(MAX_NAME_LEN + 1)));
    }

    #[test]
    fn mentions_matches_the_name_or_its_words() {
        assert!(mentions("use the add-doc-comment skill", "add-doc-comment"));
        assert!(mentions(
            "Add a doc comment to ModelRouter, following the repo doc-comment style",
            "doc-comment-style"
        ));
        assert!(!mentions("add docs", "doc-comment-style"));
        assert!(!mentions("", "any-skill"));
        assert!(!mentions("the tracker is elsewhere", "track"));
    }

    #[test]
    fn index_is_sorted_capped_and_truncates_long_descriptions() {
        let root = std::env::temp_dir().join(format!("rof-skills-idx-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (n, d) in [
            ("b-second", "second"),
            ("a-first", &"long ".repeat(30) as &str),
        ] {
            let dir = root.join(n);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {n}\ndescription: {d}\n---\nbody\n"),
            )
            .unwrap();
        }
        let m = SkillManager::new(root.clone(), None, SkillPolicy::Propose);
        let idx = m.index(10);
        let lines: Vec<&str> = idx.lines().collect();
        assert_eq!(lines.len(), 2, "{idx}");
        assert!(lines[0].starts_with("- a-first: "), "{idx}");
        assert!(lines[0].len() < 90, "long description truncated: {idx}");
        assert!(lines[1].starts_with("- b-second: second"), "{idx}");
        assert_eq!(m.index(1).lines().count(), 2, "cap + the omitted count");
        assert!(m.index(1).contains("[...1 more]"));
        std::fs::remove_dir_all(&root).ok();
    }
}

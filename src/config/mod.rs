use crate::context::ContextPolicy;
use crate::skills::SkillPolicy;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct TokenBudgets {
    pub long_term: usize,
    pub mid_term: usize,
    pub short_term: usize,
}

impl Default for TokenBudgets {
    fn default() -> Self {
        Self {
            long_term: 2000,
            mid_term: 4000,
            short_term: 6000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutingConfig {
    pub context_model: String,
    pub executor_model: String,
    pub executor_fallback: Option<String>,
    /// Model serving the reviewer. `None` = the executor model, so the
    /// default is self-review and the slot changes nothing until set.
    pub verify_model: Option<String>,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            // Empty = follow the executor (single-model default). Explicit
            // configs keep whatever they state; `ROF_CTX_MODEL` still wins.
            context_model: String::new(),
            executor_model: "strong-model".to_string(),
            executor_fallback: None,
            verify_model: None,
        }
    }
}

/// Deny-by-default: a tool runs only if (agent, tool) is allowed AND the
/// target path (if any) sits under an allowed dir. The run root is always
/// anchored in (see `anchor_allowed_dirs`); extra dirs are honoured for live
/// runs. Eval task runs restrict to the task copy regardless — that
/// isolation is a security boundary, not configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PermissionPolicy {
    pub allowed_dirs: Vec<PathBuf>,
    /// BTreeMap (not HashMap): identical configs must dump byte-identically,
    /// or versioned configs cannot be diffed.
    pub agents_tools: BTreeMap<String, Vec<String>>,
    /// Exact command strings proc.run may execute. Empty = deny all (default).
    pub allowed_commands: Vec<String>,
    /// Prefix allowlist for proc.run (v4 shell freedom): a command is allowed
    /// when it equals a prefix or starts with `prefix + " "`. Empty = no prefixes.
    #[serde(default)]
    pub allowed_prefixes: Vec<String>,
    /// Hostnames http.get may reach. Empty = deny all (default).
    pub allowed_hosts: Vec<String>,
}

impl Default for PermissionPolicy {
    fn default() -> Self {
        let mut agents_tools = BTreeMap::new();
        // reviewer verifies by running allowlisted checks; it never writes
        // v4 explorer: read-only context gathering in an isolated pass.
        agents_tools.insert(
            "explorer".to_string(),
            vec![
                "fs.list".to_string(),
                "fs.read".to_string(),
                "skills.list".to_string(),
                "skills.view".to_string(),
            ],
        );
        agents_tools.insert(
            "reviewer".to_string(),
            vec![
                "fs.read".to_string(),
                "proc.run".to_string(),
                "skills.list".to_string(),
                "skills.view".to_string(),
            ],
        );
        // reviewer stays read-only; implementer alone may write, still path-gated
        agents_tools.insert(
            "implementer".to_string(),
            vec![
                "fs.list".to_string(),
                "fs.read".to_string(),
                "fs.write".to_string(),
                "fs.patch".to_string(),
                "http.get".to_string(),
                "skills.list".to_string(),
                "skills.view".to_string(),
                // The only agent that may change the skill store, and by
                // default that means "write a proposal", not "edit itself".
                "skills.manage".to_string(),
            ],
        );
        Self {
            allowed_dirs: Vec::new(),
            agents_tools,
            allowed_commands: Vec::new(),
            allowed_prefixes: Vec::new(),
            allowed_hosts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RetrievalConfig {
    pub max_snippets: usize,
    pub max_bytes_per_file: usize,
    pub max_total_chars: usize,
    pub extensions: Vec<String>,
    pub max_depth: usize,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            max_snippets: 5,
            max_bytes_per_file: 4000,
            max_total_chars: 12_000,
            extensions: ["rs", "md", "toml", "json", "yaml", "yml", "txt"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            max_depth: 4,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PricingConfig {
    /// USD per 1M uncached input tokens.
    pub input_per_mtok: f64,
    /// USD per 1M cache-hit input tokens.
    pub cached_input_per_mtok: f64,
    /// USD per 1M output tokens.
    pub output_per_mtok: f64,
}

impl Default for PricingConfig {
    fn default() -> Self {
        // DeepSeek off-peak (primary target): 0.15 / 0.003 / 0.60 USD per Mtok.
        Self {
            input_per_mtok: 0.15,
            cached_input_per_mtok: 0.003,
            output_per_mtok: 0.60,
        }
    }
}

/// SKILL.md store. `Propose` is the default on purpose: an agent editing the
/// instructions that govern it, unattended, is the failure mode this design
/// must not have.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SkillsConfig {
    /// Where skills live. None = `~/.rof/skills` (`ROF_SKILLS_ROOT` overrides).
    pub root: Option<PathBuf>,
    /// `readonly` | `propose` | `direct`.
    pub policy: SkillPolicy,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            root: None,
            policy: SkillPolicy::Propose,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub budgets: TokenBudgets,
    /// Stage 2 per-layer policy (budgets, strategy, summarize threshold).
    /// `None` = derive from `budgets`, so a config file that never mentions
    /// `context` keeps meaning exactly what it said. Present = this block
    /// decides, budgets included — one source per case, no silent override.
    #[serde(default)]
    pub context: Option<ContextPolicy>,
    pub routing: RoutingConfig,
    pub permissions: PermissionPolicy,
    pub retrieval: RetrievalConfig,
    pub skills: SkillsConfig,
    pub max_review_rounds: u32,
    /// Per-task token ceiling (input+output across all calls). 0 = unlimited.
    pub max_tokens_per_task: u64,
    /// USD prices for local cost estimation (providers rarely report spend).
    pub pricing: PricingConfig,
    /// Weight on cost in the utility score. 0 = report-only (lexicographic).
    pub cost_lambda: f64,

    /// "pipeline" runs Planner -> Implementer -> Reviewer; "direct" uses one executor.
    #[serde(default)]
    pub execution: String,
    /// Whether a goal must produce a file change to be considered passing.
    /// Suite tasks declare this per task; this is the run-mode default.
    pub expect_writes: bool,
    /// Max suite tasks running at once. 1 = strictly sequential.
    /// Each task gets its own scratch copy of the workdir, so parallel
    /// writers never share a tree. Overridden by `rof eval --jobs N`.
    #[serde(default = "one_job")]
    pub max_parallel_tasks: usize,
    /// Where per-task workdir copies are made. None = the system temp dir,
    /// which may be a small tmpfs — point this at disk for real suites
    /// (a task that runs cargo checks builds its own target/ there).
    #[serde(default)]
    pub task_root: Option<PathBuf>,
    /// Delete a task's workdir copy when the task finishes. A suite whose
    /// checks run a build leaves a full `target/` per task (~1 GB on a Rust
    /// repo), which is debug-only state: on by default, set false to keep a
    /// failing copy for inspection.
    #[serde(default = "yes")]
    pub clean_task_dirs: bool,
    /// Run the goal-quality pre-check before the run. Emits a
    /// `GoalQuality` trace event and, when set, a note in the implementer prompt.
    /// Off by default: it changes prompt content, which is an A/B'able change.
    #[serde(default)]
    pub goal_quality: bool,
    /// Buy one extra implementer round when a task fails at its round cap
    /// Off by default: it changes the number of
    /// rounds a live arm executes.
    #[serde(default)]
    pub auto_poke: bool,
    /// v4: isolated read-only exploration pass before implement (ROF_EXPLORER=yes). Off by default.
    #[serde(default)]
    pub explorer: bool,
    /// v4: independent task attempts, cheapest-pass wins (ROF_ATTEMPTS=N, 1..=5). Default 1 = current behavior.
    #[serde(default = "one_attempt")]
    pub attempts: usize,
    /// v4: outer verify guard after inner pass (ROF_VERIFY_GUARD=yes). Off by default.
    #[serde(default)]
    pub verify_guard: bool,
    /// Per-endpoint capability: the prompt ceiling and retry-ladder order.
    /// Defaults reproduce the shipped DeepSeek/vLLM behavior bit for bit.
    #[serde(default)]
    pub endpoint: crate::llm::profile::EndpointProfile,
    /// §6: a per-turn cap, in tokens, on the part of one agent turn that no
    /// layer budget sees — the file map, the goal-named files, the files a
    /// model asked for, and the skill bodies, all placed below the layers by
    /// the §4.1 assembler. The assembler's budget becomes
    /// `min(cap * 4, the endpoint's emission ceiling)`, so a cap is enforced
    /// by the windowing that already exists and a file that does not fit is
    /// narrowed rather than dropped.
    ///
    /// NOT a cap on the whole prompt: the three layers keep their own per-layer
    /// caps (`context`, or `budgets` when that block is absent), and the goal,
    /// the task statement and the evidence block live there. This number only
    /// governs the tail below them.
    ///
    /// **0 = off, which is the default, and that is deliberate.** Context
    /// discipline was measured before it was cut: an arm that lowers this
    /// before a run has recorded what it feeds the model has no baseline to
    /// compare against. The default leaves the assembler budget exactly where
    /// it was, so a run is byte-identical to one from before the knob existed.
    #[serde(default)]
    pub per_turn_context_cap: usize,
}

fn one_job() -> usize {
    1
}

fn one_attempt() -> usize {
    1
}

fn yes() -> bool {
    true
}

impl AppConfig {
    /// Load a JSON config (partial files are fine: every field falls back to
    /// `Default`, so a config only states what it overrides). JSON because
    /// serde_json is already in the tree — no new dependency for a format
    /// the suite loader already uses.
    pub fn load(path: &std::path::Path) -> anyhow::Result<Self> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("config {}: {e}", path.display()))?;
        let cfg: Self = serde_json::from_str(&text)
            .map_err(|e| anyhow::anyhow!("config {}: {e}", path.display()))?;
        Ok(cfg)
    }

    /// Canonical, diffable form. `rof config > a.json` produces a file that
    /// loads back to the same effective config, so configs can be versioned,
    /// diffed and A/B'd like any other artifact (the meta-harness premise).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Release gate preset: reps=3 (unless explicitly set otherwise),
    /// outer judge on, and an independent judge model required. Errors when
    /// no verify model is configured — a release gate without a judge is
    /// self-review with extra steps. Call after env overrides so the
    /// effective verify slot is what gets checked.
    pub fn apply_release_preset(&mut self, reps: &mut usize) -> Result<(), String> {
        let has_judge = self
            .routing
            .verify_model
            .as_deref()
            .map(|m| !m.trim().is_empty())
            .unwrap_or(false);
        if !has_judge {
            return Err(
                "release gate needs an independent judge: set verify_model or ROF_VERIFY_MODEL"
                    .to_string(),
            );
        }
        if *reps <= 1 {
            *reps = 3;
        }
        self.verify_guard = true;
        Ok(())
    }

    /// The effective per-layer context policy: the config's `context` block
    /// when it has one, otherwise derived from `budgets` — so a file that only
    /// states budgets (every config written before stage 2) behaves exactly as
    /// it did, and a file that states `context` is not silently overridden by
    /// the older field.
    pub fn context_policy(&self) -> ContextPolicy {
        self.context
            .unwrap_or_else(|| ContextPolicy::from(&self.budgets))
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            budgets: TokenBudgets::default(),
            context: None,
            routing: RoutingConfig::default(),
            permissions: PermissionPolicy::default(),
            retrieval: RetrievalConfig::default(),
            skills: SkillsConfig::default(),
            max_review_rounds: 2,
            // ~2.5x observed per-task usage: catches runaway loops, not normal work.
            max_tokens_per_task: 50_000,
            pricing: PricingConfig::default(),
            cost_lambda: 0.0,
            execution: "pipeline".to_string(),
            expect_writes: true,
            max_parallel_tasks: 1,
            task_root: None,
            clean_task_dirs: true,
            // Off by default: both change live behaviour (prompt content /
            // rounds executed), so they are opt-in A/B'able switches.
            goal_quality: false,
            auto_poke: false,
            explorer: false,
            attempts: 1,
            verify_guard: false,
            endpoint: crate::llm::profile::EndpointProfile::default(),
            // §6: measured, not cut. See the field's doc comment.
            per_turn_context_cap: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AppConfig;

    #[test]
    fn release_preset_requires_a_judge() {
        let mut cfg = AppConfig::default();
        let mut reps = 1;
        assert!(cfg.apply_release_preset(&mut reps).is_err());
        cfg.routing.verify_model = Some("judge-model".to_string());
        assert!(cfg.apply_release_preset(&mut reps).is_ok());
        assert_eq!(reps, 3);
        assert!(cfg.verify_guard);
    }

    #[test]
    fn explicit_reps_survive_release() {
        let mut cfg = AppConfig::default();
        cfg.routing.verify_model = Some("j".to_string());
        let mut reps = 5;
        cfg.apply_release_preset(&mut reps).unwrap();
        assert_eq!(reps, 5);
        assert!(cfg.verify_guard);
    }
}

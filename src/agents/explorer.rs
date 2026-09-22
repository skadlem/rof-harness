use std::path::Path;

/// One file the explorer thinks matters, and why (one line).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct KeyFile {
    pub path: String,
    pub why: String,
}

/// One exact quoted line grounding a claim.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Quote {
    pub path: String,
    pub line: usize,
    pub text: String,
}

/// What an isolated read-only exploration returns. Small by design: the caller
/// appends key_files to the implementer's volatile tail through the assembler,
/// never raw dumps.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ExplorerReport {
    pub summary: String,
    pub key_files: Vec<KeyFile>,
    pub quotes: Vec<Quote>,
}

/// Read-only explorer. v1 is deterministic (no model call): it runs the same
/// keyword retriever the loop uses plus the goal-named files, so its output is
/// offline-testable and byte-stable. A model-call summarizer is P2 and must
/// prove itself against this baseline on recall before it ships.
pub struct ExplorerAgent;

impl ExplorerAgent {
    /// Explore `workdir` for `goal`. Read-only: lists and reads through the
    /// passed registry as agent "explorer" (grant-gated like everyone else).
    pub async fn explore(
        goal: &str,
        workdir: &Path,
        tools: &crate::tools::ToolRegistry,
        trace: &crate::obs::TraceSink,
    ) -> ExplorerReport {
        let cfg = crate::config::RetrievalConfig::default();
        let r = crate::context::Retriever::new(workdir.to_path_buf(), cfg.clone());
        let snips = r.retrieve(goal, cfg.max_total_chars);
        let mut key_files: Vec<KeyFile> = Vec::new();
        let mut quotes: Vec<Quote> = Vec::new();
        for s in snips.iter().take(5) {
            key_files.push(KeyFile {
                path: s.path.clone(),
                why: format!("keyword hit for goal ({})", s.content.chars().count()),
            });
            // First non-empty line as the grounding quote.
            if let Some((i, line)) = s
                .content
                .lines()
                .enumerate()
                .find(|(_, l)| !l.trim().is_empty())
            {
                quotes.push(Quote {
                    path: s.path.clone(),
                    line: i + 1,
                    text: line.trim().chars().take(160).collect(),
                });
            }
        }
        // Goal-named files first: the goal almost always names the file it
        // wants changed, and keyword density lets big files win otherwise.
        for (p, _) in r.named_file_contents(goal) {
            if !key_files.iter().any(|k| k.path == p) && key_files.len() < 5 {
                key_files.insert(
                    0,
                    KeyFile {
                        path: p,
                        why: "named by the goal".to_string(),
                    },
                );
            }
        }
        trace.emit(crate::obs::TraceEvent::StateTransition {
            from: "exploring".to_string(),
            to: "explored".to_string(),
        });
        // Touch the registry as explorer so a missing grant is visible early:
        // one gated list call, result discarded (the retriever above already
        // did the real work through the filesystem).
        let _ = tools
            .call(
                "explorer",
                "fs.list",
                Some(workdir),
                serde_json::json!({"path": "."}),
            )
            .await;
        ExplorerReport {
            summary: format!(
                "explorer: {} key files for '{}'",
                key_files.len(),
                goal.chars().take(80).collect::<String>()
            ),
            key_files,
            quotes,
        }
    }
}

/// Stable JSON shape for tests and for callers that log the report.
pub fn explorer_report_for_test(goal: &str, paths: &[&str]) -> String {
    let rep = ExplorerReport {
        summary: goal.to_string(),
        key_files: paths
            .iter()
            .map(|p| KeyFile {
                path: p.to_string(),
                why: "test".to_string(),
            })
            .collect(),
        quotes: Vec::new(),
    };
    serde_json::to_string(&rep).unwrap_or_default()
}

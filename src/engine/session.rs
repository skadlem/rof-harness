use crate::context::CtxState;

/// One user goal execution. Holds the layered context others read/write.
pub struct Session {
    pub id: String,
    pub goal: String,
    pub ctx: CtxState,
    /// Allowlisted commands the reviewer runs as acceptance evidence.
    pub checks: Vec<String>,
    /// Whether this goal requires file changes (guards against passing empty work).
    pub expect_writes: bool,
    /// Per-task token override; None = config default.
    pub max_tokens: Option<u64>,
}

impl Session {
    pub fn new(goal: String) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            ctx: CtxState {
                long_term: "conventions: small diffs, cargo test must pass".to_string(),
                mid_term: String::new(),
                short_term: String::new(),
            },
            goal,
            checks: Vec::new(),
            expect_writes: true,
            max_tokens: None,
        }
    }

    pub fn with_checks(mut self, checks: Vec<String>) -> Self {
        self.checks = checks;
        self
    }

    pub fn with_token_limit(mut self, limit: Option<u64>) -> Self {
        self.max_tokens = limit;
        self
    }

    pub fn expecting_writes(mut self, yes: bool) -> Self {
        self.expect_writes = yes;
        self
    }
}

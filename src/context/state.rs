use serde::{Deserialize, Serialize};

/// Layered context. v1: plain strings with per-layer token budgets enforced
/// by ContextBuilder (chars/4 estimate). RAG/memory plugs in later as a
/// retriever feeding the mid/short layers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CtxState {
    pub long_term: String,
    pub mid_term: String,
    pub short_term: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CtxView {
    pub prompt: String,
    pub used_tokens: usize,
    pub truncated: bool,
}

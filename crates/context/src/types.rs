use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemKey {
    pub path: String,
    pub region: String,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Fidelity {
    Exact,
    Windowed {
        anchor: String,
        cap: usize,
    },
    /// Reserved, never constructed in prod: the `Exact|Drop` arm (`shape()`)
    /// treats it as Exact (fits-or-must_include). Only `tests` builds it.
    Drop,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextItem {
    pub key: ItemKey,
    pub fidelity: Fidelity,
    pub must_include: bool,
    pub text: String,
}

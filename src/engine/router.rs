use crate::config::RoutingConfig;

/// Which role needs a model: cheap context work vs strong execution.
///
/// Each role maps to a distinct model slot in `ModelRouter`, so callers
/// (and the wiring in `main.rs::setup`) never have to know which model id
/// ends up serving which phase:
///
/// * [`Role::Context`] -> `ModelRouter::context_model` (used by
///   [`crate::llm::ContextService`] for planning / retrieval / summarization).
/// * [`Role::Executor`] -> `ModelRouter::executor_model` (used by
///   [`crate::llm::ExecutorService`] for tool-using implementation work),
///   with `ModelRouter::fallback` applied only when configured.
#[derive(Debug, Clone, Copy)]
pub enum Role {
    Context,
    Executor,
}

/// Picks which model id serves context vs executor roles.
///
/// Resolution semantics (see [`ModelRouter::resolve`]):
///
/// * `Role::Context` always returns `context_model` and **never** falls
///   back. Context covers the cheap, high-frequency phases (planning,
///   retrieval, summarization); silently upgrading to a stronger model
///   there would change cost/latency characteristics without an opt-in.
/// * `Role::Executor` returns `executor_model` plus the configured
///   `fallback` (if any) as its secondary candidate. This is the strong
///   model doing real work, so having a fallback chain is desirable.
///
/// [`ModelRouter::from_config`] maps `RoutingConfig` fields to these slots:
/// `context_model`, `executor_model` and `executor_fallback` respectively.
/// Pure data + accessor today; routing policies plug in here later.
pub struct ModelRouter {
    pub context_model: String,
    pub executor_model: String,
    pub fallback: Option<String>,
}

impl ModelRouter {
    pub fn new(context_model: String, executor_model: String, fallback: Option<String>) -> Self {
        Self {
            context_model,
            executor_model,
            fallback,
        }
    }

    /// Build a router from [`RoutingConfig`], mapping:
    /// `cfg.context_model` -> context slot,
    /// `cfg.executor_model` -> executor slot,
    /// `cfg.executor_fallback` -> executor fallback (context never falls back).
    pub fn from_config(cfg: &RoutingConfig) -> Self {
        Self::new(
            cfg.context_model.clone(),
            cfg.executor_model.clone(),
            cfg.executor_fallback.clone(),
        )
    }

    /// (primary, fallback) for the requested `role`.
    ///
    /// * `Role::Context` -> `(context_model, None)`: no fallback by design.
    /// * `Role::Executor` -> `(executor_model, fallback)`: the configured
    ///   fallback chain, or `None` when none is configured.
    pub fn resolve(&self, role: Role) -> (&str, Option<&str>) {
        match role {
            Role::Context => (&self.context_model, None),
            Role::Executor => (&self.executor_model, self.fallback.as_deref()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_roles() {
        let r = ModelRouter::new("cheap".into(), "strong".into(), Some("fb".into()));
        assert_eq!(r.resolve(Role::Context), ("cheap", None));
        assert_eq!(r.resolve(Role::Executor), ("strong", Some("fb")));
    }

    /// With no configured fallback, Context still never falls back and
    /// Executor reports `None` as its secondary candidate.
    #[test]
    fn resolve_roles_no_fallback() {
        let r = ModelRouter::new("cheap".into(), "strong".into(), None);
        assert_eq!(r.resolve(Role::Context), ("cheap", None));
        assert_eq!(r.resolve(Role::Executor), ("strong", None));
    }
}

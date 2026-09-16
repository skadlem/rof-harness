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
/// * [`Role::Verify`] -> `ModelRouter::verify_model` when set, otherwise
///   the executor model. The reviewer defaults to self-review; a separate
///   model is opt-in, and the executor's fallback chain applies to it too.
#[derive(Debug, Clone, Copy)]
pub enum Role {
    Context,
    Executor,
    Verify,
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
/// `verify_model` maps to the verify slot (`None` resolves to the executor model).
/// Pure data + accessor today; routing policies plug in here later.
pub struct ModelRouter {
    pub context_model: String,
    pub executor_model: String,
    pub fallback: Option<String>,
    pub verify_model: Option<String>,
}

impl ModelRouter {
    pub fn new(
        context_model: String,
        executor_model: String,
        fallback: Option<String>,
        verify_model: Option<String>,
    ) -> Self {
        Self {
            context_model,
            executor_model,
            fallback,
            verify_model,
        }
    }

    /// Build a router from [`RoutingConfig`], mapping:
    /// `cfg.context_model` -> context slot,
    /// `cfg.executor_model` -> executor slot,
    /// `cfg.executor_fallback` -> executor fallback (context never falls back),
    /// `cfg.verify_model` -> verify slot (`None` resolves to the executor model).
    pub fn from_config(cfg: &RoutingConfig) -> Self {
        Self::new(
            cfg.context_model.clone(),
            cfg.executor_model.clone(),
            cfg.executor_fallback.clone(),
            cfg.verify_model.clone(),
        )
    }

    /// (primary, fallback) for the requested `role`.
    ///
    /// * `Role::Context` -> `(context_model, None)`: no fallback by design.
    /// * `Role::Executor` -> `(executor_model, fallback)`: the configured
    ///   fallback chain, or `None` when none is configured.
    /// * `Role::Verify` -> `(verify_model or executor_model, fallback)`:
    ///   an unset verify slot is self-review on the executor model.
    pub fn resolve(&self, role: Role) -> (&str, Option<&str>) {
        match role {
            Role::Context => (&self.context_model, None),
            Role::Executor => (&self.executor_model, self.fallback.as_deref()),
            Role::Verify => (
                self.verify_model.as_deref().unwrap_or(&self.executor_model),
                self.fallback.as_deref(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_roles() {
        let r = ModelRouter::new(
            "cheap".into(),
            "strong".into(),
            Some("fb".into()),
            Some("verifier".into()),
        );
        assert_eq!(r.resolve(Role::Context), ("cheap", None));
        assert_eq!(r.resolve(Role::Executor), ("strong", Some("fb")));
        assert_eq!(r.resolve(Role::Verify), ("verifier", Some("fb")));
    }

    /// With no configured fallback, Context still never falls back and
    /// Executor reports `None` as its secondary candidate.
    #[test]
    fn resolve_roles_no_fallback() {
        let r = ModelRouter::new("cheap".into(), "strong".into(), None, None);
        assert_eq!(r.resolve(Role::Context), ("cheap", None));
        assert_eq!(r.resolve(Role::Executor), ("strong", None));
    }

    /// An unset verify slot resolves to the executor model: the default is
    /// self-review, so the slot changes nothing until configured.
    #[test]
    fn verify_defaults_to_executor() {
        let r = ModelRouter::new("cheap".into(), "strong".into(), Some("fb".into()), None);
        assert_eq!(r.resolve(Role::Verify), ("strong", Some("fb")));
    }
}

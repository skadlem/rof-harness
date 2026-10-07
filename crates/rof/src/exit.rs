//! Process exit codes and the run-end event summary on stderr.

use agent_event::AgentEvent;
use agent_loop::{FailureKind, Outcome};

/// Startup credential-gate exit: 4, distinct from 2 (usage/parse) and 3
/// (run failure). Missing credentials are an auth-setup problem before any
/// spend — wrappers fix the env and retry instead of reading a run failure.
pub(crate) const EXIT_NO_CREDENTIALS: i32 = 4;

/// Run-outcome exit codes. 0/2/3 keep their historical meanings (done /
/// usage-parse / run failure); 4 is the startup credential gate. Each
/// [`FailureKind`] gets its own code so wrappers can react without parsing
/// stderr — except [`FailureKind::Provider`], which keeps the historical 3
/// as the most common run failure. 5/6/7 follow declaration order after the
/// taken codes.
pub(crate) fn exit_code(outcome: &Outcome) -> i32 {
    match outcome {
        Outcome::Done => 0,
        Outcome::Halted(_) => 3,
        Outcome::Cancelled => 3,
        Outcome::Failed { kind, .. } => match kind {
            FailureKind::Log => 5,
            FailureKind::Snapshot => 6,
            FailureKind::Provider => 3,
            FailureKind::Input => 7,
        },
    }
}

pub(crate) const NAMES: [&str; 11] = [
    "RunStart",
    "RunEnd",
    "TurnStart",
    "TurnEnd",
    "MessageStart",
    "MessageUpdate",
    "MessageEnd",
    "ToolStart",
    "ToolEnd",
    "Control",
    "Error",
];

pub(crate) fn summarize(events: &[AgentEvent]) -> String {
    let mut counts = [0usize; 11];
    for e in events {
        counts[match e {
            AgentEvent::RunStart { .. } => 0,
            AgentEvent::RunEnd { .. } => 1,
            AgentEvent::TurnStart { .. } => 2,
            AgentEvent::TurnEnd { .. } => 3,
            AgentEvent::MessageStart { .. } => 4,
            AgentEvent::MessageUpdate { .. } => 5,
            AgentEvent::MessageEnd { .. } => 6,
            AgentEvent::ToolStart { .. } => 7,
            AgentEvent::ToolEnd { .. } => 8,
            AgentEvent::Control(_) => 9,
            AgentEvent::Error { .. } => 10,
        }] += 1;
    }
    NAMES
        .iter()
        .zip(counts)
        .filter(|(_, n)| *n > 0)
        .map(|(name, n)| format!("{name}={n}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_and_summary_pinned() {
        assert_eq!(exit_code(&Outcome::Done), 0);
        for o in [
            Outcome::Halted("steps".into()),
            Outcome::Cancelled,
            Outcome::Failed {
                kind: FailureKind::Provider,
                message: "e".into(),
            },
        ] {
            assert_eq!(exit_code(&o), 3);
        }
        // Each FailureKind has its own code (Provider keeps the historic 3).
        for (kind, code) in [
            (FailureKind::Log, 5),
            (FailureKind::Snapshot, 6),
            (FailureKind::Input, 7),
        ] {
            assert_eq!(
                exit_code(&Outcome::Failed {
                    kind,
                    message: "e".into()
                }),
                code,
                "{kind:?}"
            );
        }
        let evs = vec![
            AgentEvent::RunStart {
                run_id: 0,
                goal: "g".into(),
            },
            AgentEvent::TurnStart { turn: 1 },
            AgentEvent::TurnStart { turn: 1 },
            AgentEvent::Error {
                error: agent_event::AgentError {
                    code: "E".into(),
                    message: "m".into(),
                },
            },
        ];
        assert_eq!(summarize(&evs), "RunStart=1 TurnStart=2 Error=1");
        assert_eq!(summarize(&[]), "");
    }
}

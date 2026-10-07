use crate::Claim;
use serde::{Deserialize, Serialize};

/// Bet A: proof-gated batch commit with proven-hunk savepoints.
/// Commit succeeds only on the verifier cascade; failure keeps the verified
/// prefix and rolls back only the failing suffix (savepoints, not all-or-nothing).
///
/// Aider `/undo` guard checklist (stolen — enforce before any partial rollback):
/// refuse if the commit was pushed, refuse dirty overlaps between agent and
/// user edits, refuse multi-parent merges, verify each file existed in the
/// prior tree before restoring. Plus codex-rewind's rule: never infer absence
/// from a missing path — guessing wrong destroys work never ours to remove.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Savepoint {
    pub kept_hunks: Vec<String>,
    pub reverted_hunks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CommitVerdict {
    Committed,
    Partial { savepoint: Savepoint },
    Aborted { reason: String },
}

/// Proof = the predicted verifier verdict, satisfied. Single-proof fast path.
pub fn gate_commit(claim: &Claim, proof_passed: bool) -> CommitVerdict {
    if proof_passed {
        CommitVerdict::Committed
    } else {
        CommitVerdict::Aborted {
            reason: format!("claim unsatisfied: {}", claim.predicted_verdict),
        }
    }
}

/// Keep the verified leading prefix, revert from the first failure on.
/// A later hunk that passed in isolation still reverts: it was verified
/// against uncommitted state, so it is not proven. Never all-or-nothing.
pub fn split_savepoint(hunks: &[(String, bool)]) -> Savepoint {
    let first_fail = hunks.iter().position(|(_, ok)| !ok).unwrap_or(hunks.len());
    Savepoint {
        kept_hunks: hunks[..first_fail].iter().map(|(h, _)| h.clone()).collect(),
        reverted_hunks: hunks[first_fail..].iter().map(|(h, _)| h.clone()).collect(),
    }
}

/// Batch commit: all proven → Committed; verified prefix exists → Partial with
/// the savepoint; nothing verified (empty batch or first hunk failed) → Aborted.
pub fn gate_batch_commit(claim: &Claim, hunks: &[(String, bool)]) -> CommitVerdict {
    if hunks.is_empty() {
        return CommitVerdict::Aborted {
            reason: "no hunks proven".to_string(),
        };
    }
    let savepoint = split_savepoint(hunks);
    if savepoint.reverted_hunks.is_empty() {
        CommitVerdict::Committed
    } else if savepoint.kept_hunks.is_empty() {
        CommitVerdict::Aborted {
            reason: format!(
                "claim unsatisfied at first hunk: {}",
                claim.predicted_verdict
            ),
        }
    } else {
        CommitVerdict::Partial { savepoint }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(predicted: &str) -> Claim {
        Claim {
            predicted_verdict: predicted.to_string(),
            on_mismatch: "revert last hunk, retry once".to_string(),
        }
    }

    #[test]
    fn gate_single_proof_commits_or_aborts() {
        let c = claim("cargo check passes");
        assert_eq!(gate_commit(&c, true), CommitVerdict::Committed);
        assert!(matches!(
            gate_commit(&c, false),
            CommitVerdict::Aborted { .. }
        ));
    }

    #[test]
    fn gate_batch_all_proven_commits() {
        let c = claim("all green");
        let hunks = vec![("h1".to_string(), true), ("h2".to_string(), true)];
        assert_eq!(gate_batch_commit(&c, &hunks), CommitVerdict::Committed);
    }

    #[test]
    fn gate_batch_partial_keeps_verified_prefix() {
        let c = claim("all green");
        // h4 passed in isolation but sits past the first failure: not proven.
        let hunks = vec![
            ("h1".to_string(), true),
            ("h2".to_string(), true),
            ("h3".to_string(), false),
            ("h4".to_string(), true),
        ];
        match gate_batch_commit(&c, &hunks) {
            CommitVerdict::Partial { savepoint } => {
                assert_eq!(savepoint.kept_hunks, vec!["h1", "h2"]);
                assert_eq!(savepoint.reverted_hunks, vec!["h3", "h4"]);
            }
            v => panic!("expected Partial, got {v:?}"),
        }
    }

    #[test]
    fn gate_batch_first_failure_aborts() {
        let c = claim("all green");
        let hunks = vec![("h1".to_string(), false), ("h2".to_string(), true)];
        assert!(matches!(
            gate_batch_commit(&c, &hunks),
            CommitVerdict::Aborted { .. }
        ));
        assert!(matches!(
            gate_batch_commit(&c, &[]),
            CommitVerdict::Aborted { .. }
        ));
    }
}

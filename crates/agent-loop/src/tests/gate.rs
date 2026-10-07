use crate::*;
use tokio_util::sync::CancellationToken;

#[test]
fn gate_transitions_idempotent() {
    let gate = EffectGate::new();
    assert_eq!(gate.status(), GateStatus::Open);
    assert!(gate.admit().is_ok());
    gate.begin_abort();
    assert_eq!(gate.status(), GateStatus::Aborting);
    assert!(matches!(gate.admit(), Err(GateError::AbortRequested)));
    gate.begin_abort(); // idempotent: still aborting, not closed
    assert_eq!(gate.status(), GateStatus::Aborting);
    let token = CancellationToken::new();
    gate.signal_abort(&token);
    assert!(token.is_cancelled());
    gate.signal_abort(&token); // idempotent propagate
    assert!(token.is_cancelled());
    gate.close("boom".into());
    assert_eq!(gate.status(), GateStatus::Closed);
    assert!(matches!(gate.admit(), Err(GateError::Closed(_))));
    gate.begin_abort(); // close wins over a late abort
    assert_eq!(gate.status(), GateStatus::Closed);
}

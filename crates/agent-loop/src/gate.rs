//! Two-phase effect gate for abort/close propagation.

use std::sync::Mutex;
#[cfg(test)]
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateStatus {
    Open,
    Aborting,
    Closed,
}

#[derive(Debug, Clone)]
pub enum GateError {
    AbortRequested,
    Closed(String),
}

/// Two-phase effect gate: begin_abort decides, signal_abort propagates (signal_abort is test-only today; prod cancels the token directly).
pub struct EffectGate {
    inner: Mutex<GateInner>,
}

struct GateInner {
    status: GateStatus,
    closed_err: String,
}

impl EffectGate {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(GateInner {
                status: GateStatus::Open,
                closed_err: String::new(),
            }),
        }
    }
    pub fn status(&self) -> GateStatus {
        self.inner.lock().unwrap().status
    }
    pub fn is_open(&self) -> bool {
        self.status() == GateStatus::Open
    }
    pub fn admit(&self) -> Result<(), GateError> {
        let gate = self.inner.lock().unwrap();
        match gate.status {
            GateStatus::Open => Ok(()),
            GateStatus::Aborting => Err(GateError::AbortRequested),
            GateStatus::Closed => Err(GateError::Closed(gate.closed_err.clone())),
        }
    }
    pub fn begin_abort(&self) {
        let mut gate = self.inner.lock().unwrap();
        if gate.status == GateStatus::Open {
            gate.status = GateStatus::Aborting;
        }
    }
    /// TEST-ONLY propagation helper: cancels `token` iff the gate is Aborting.
    /// Prod calls `begin_abort()` then cancels the turn token directly.
    #[cfg(test)]
    pub fn signal_abort(&self, token: &CancellationToken) {
        if self.status() == GateStatus::Aborting {
            token.cancel();
        }
    }
    pub fn close(&self, err: String) {
        let mut gate = self.inner.lock().unwrap();
        gate.status = GateStatus::Closed;
        gate.closed_err = err;
    }
}

impl Default for EffectGate {
    fn default() -> Self {
        Self::new()
    }
}

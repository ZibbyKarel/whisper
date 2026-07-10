//! Dictation session FSM and cancellation token (`ARCHITECTURE.md` §3.2):
//! `Idle -> Recording -> Transcribing -> Cleaning -> Delivering -> Idle`, with
//! `Cancelled` and `Failed(err)` exits from any in-flight stage.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::error::FsmError;

/// A stage failure that carries the best transcript text produced before the failure
/// (`ARCHITECTURE.md` §3.2: "a failed stage returns the best text so far — raw transcript
/// beats nothing").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailureInfo {
    pub error: String,
    pub partial_text: Option<String>,
}

/// Dictation session state (`ARCHITECTURE.md` §3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Recording,
    Transcribing,
    Cleaning,
    Delivering,
    Cancelled,
    Failed(FailureInfo),
}

/// Session-scoped cancellation flag, checked between pipeline stages
/// (`ARCHITECTURE.md` §3.2). Cheaply cloneable; all clones observe the same flag.
#[derive(Debug, Clone, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    pub fn new() -> Self {
        Self(Arc::new(AtomicBool::new(false)))
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// The session finite-state machine. Only transitions matching `ARCHITECTURE.md` §3.2 are
/// accepted; anything else returns [`FsmError`] rather than silently mutating state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFsm {
    state: SessionState,
}

impl Default for SessionFsm {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionFsm {
    pub fn new() -> Self {
        Self {
            state: SessionState::Idle,
        }
    }

    pub fn state(&self) -> &SessionState {
        &self.state
    }

    /// Attempt a transition; rejects any edge not in the FSM diagram.
    pub fn transition(&mut self, to: SessionState) -> Result<(), FsmError> {
        use SessionState::*;

        let allowed = matches!(
            (&self.state, &to),
            (Idle, Recording)
                | (Recording, Transcribing)
                | (Recording, Cancelled)
                | (Recording, Failed(_))
                | (Transcribing, Cleaning)
                | (Transcribing, Cancelled)
                | (Transcribing, Failed(_))
                | (Cleaning, Delivering)
                | (Cleaning, Cancelled)
                | (Cleaning, Failed(_))
                | (Delivering, Idle)
                | (Delivering, Cancelled)
                | (Delivering, Failed(_))
        );

        if allowed {
            self.state = to;
            Ok(())
        } else {
            Err(FsmError {
                from: format!("{:?}", self.state),
                attempted: format!("{to:?}"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_session_starts_idle() {
        let fsm = SessionFsm::new();
        assert_eq!(fsm.state(), &SessionState::Idle);
    }

    #[test]
    fn happy_path_transitions_are_accepted() {
        let mut fsm = SessionFsm::new();
        fsm.transition(SessionState::Recording).unwrap();
        fsm.transition(SessionState::Transcribing).unwrap();
        fsm.transition(SessionState::Cleaning).unwrap();
        fsm.transition(SessionState::Delivering).unwrap();
        fsm.transition(SessionState::Idle).unwrap();
        assert_eq!(fsm.state(), &SessionState::Idle);
    }

    #[test]
    fn skipping_stages_is_rejected() {
        let mut fsm = SessionFsm::new();
        let err = fsm.transition(SessionState::Delivering).unwrap_err();
        assert_eq!(err.from, "Idle");
        assert_eq!(err.attempted, "Delivering");
        // Rejected transition must not mutate state.
        assert_eq!(fsm.state(), &SessionState::Idle);
    }

    #[test]
    fn failed_carries_best_text_so_far() {
        let mut fsm = SessionFsm::new();
        fsm.transition(SessionState::Recording).unwrap();
        fsm.transition(SessionState::Transcribing).unwrap();
        let info = FailureInfo {
            error: "cleanup engine unavailable".to_string(),
            partial_text: Some("Příliš žluťoučký kůň".to_string()),
        };
        fsm.transition(SessionState::Cleaning).unwrap();
        fsm.transition(SessionState::Failed(info.clone())).unwrap();
        assert_eq!(fsm.state(), &SessionState::Failed(info));
    }

    #[test]
    fn cancellation_token_shares_state_across_clones() {
        let token = CancellationToken::new();
        let clone = token.clone();
        assert!(!token.is_cancelled());
        clone.cancel();
        assert!(token.is_cancelled());
    }
}

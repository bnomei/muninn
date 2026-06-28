//! Recording-runtime state machine shared by hotkeys, tray, and external control.
//!
//! [`AppState::on_event`] is the single transition table. Invalid event/state
//! pairs leave the state unchanged; processing and injecting ignore new
//! recording triggers until the pipeline finishes.

/// High-level capture and pipeline phase tracked by the runtime coordinator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    /// No active capture and no pipeline work in flight.
    Idle,
    /// Push-to-talk capture; release ends recording and enters processing.
    RecordingPushToTalk,
    /// Done-mode capture; a second done toggle ends recording and enters processing.
    RecordingDone,
    /// Transcription and refinement running after capture ends.
    Processing,
    /// Final text is being injected into the focused target.
    Injecting,
}

/// Input events that drive [`AppState`] transitions from hotkeys, tray, and
/// external control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEvent {
    /// Begin or continue push-to-talk capture.
    PttPressed,
    /// End push-to-talk capture and start processing.
    PttReleased,
    /// Toggle done-mode capture on or off.
    DoneTogglePressed,
    /// Discard the active capture without running the pipeline.
    CancelPressed,
    /// Pipeline finished; move to injection.
    ProcessingFinished,
    /// Injection finished; return to idle.
    InjectionFinished,
}

impl AppState {
    /// Apply an [`AppEvent`] and return the next state.
    ///
    /// Unhandled combinations are a no-op and return `self`. While processing
    /// or injecting, recording triggers are ignored until the pipeline completes.
    #[must_use]
    pub const fn on_event(self, event: AppEvent) -> Self {
        use AppEvent::*;
        use AppState::*;

        match (self, event) {
            (Idle, PttPressed) => RecordingPushToTalk,
            (RecordingPushToTalk, PttReleased) => Processing,
            (Idle, DoneTogglePressed) => RecordingDone,
            (RecordingDone, DoneTogglePressed) => Processing,
            (RecordingPushToTalk | RecordingDone, CancelPressed) => Idle,
            (Processing, ProcessingFinished) => Injecting,
            (Injecting, InjectionFinished) => Idle,
            (Processing | Injecting, PttPressed | PttReleased | DoneTogglePressed) => self,
            _ => self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AppEvent, AppState};

    #[test]
    fn idle_ptt_pressed_starts_push_to_talk_recording() {
        assert_eq!(
            AppState::Idle.on_event(AppEvent::PttPressed),
            AppState::RecordingPushToTalk
        );
    }

    #[test]
    fn push_to_talk_release_starts_processing() {
        assert_eq!(
            AppState::RecordingPushToTalk.on_event(AppEvent::PttReleased),
            AppState::Processing
        );
    }

    #[test]
    fn idle_done_toggle_starts_done_recording() {
        assert_eq!(
            AppState::Idle.on_event(AppEvent::DoneTogglePressed),
            AppState::RecordingDone
        );
    }

    #[test]
    fn recording_done_done_toggle_starts_processing() {
        assert_eq!(
            AppState::RecordingDone.on_event(AppEvent::DoneTogglePressed),
            AppState::Processing
        );
    }

    #[test]
    fn cancel_pressed_in_recording_states_returns_idle() {
        assert_eq!(
            AppState::RecordingPushToTalk.on_event(AppEvent::CancelPressed),
            AppState::Idle
        );
        assert_eq!(
            AppState::RecordingDone.on_event(AppEvent::CancelPressed),
            AppState::Idle
        );
    }

    #[test]
    fn processing_finished_transitions_to_injecting() {
        assert_eq!(
            AppState::Processing.on_event(AppEvent::ProcessingFinished),
            AppState::Injecting
        );
    }

    #[test]
    fn injection_finished_transitions_to_idle() {
        assert_eq!(
            AppState::Injecting.on_event(AppEvent::InjectionFinished),
            AppState::Idle
        );
    }

    #[test]
    fn processing_ignores_recording_triggers() {
        assert_eq!(
            AppState::Processing.on_event(AppEvent::PttPressed),
            AppState::Processing
        );
        assert_eq!(
            AppState::Processing.on_event(AppEvent::PttReleased),
            AppState::Processing
        );
        assert_eq!(
            AppState::Processing.on_event(AppEvent::DoneTogglePressed),
            AppState::Processing
        );
    }

    #[test]
    fn injecting_ignores_recording_triggers() {
        assert_eq!(
            AppState::Injecting.on_event(AppEvent::PttPressed),
            AppState::Injecting
        );
        assert_eq!(
            AppState::Injecting.on_event(AppEvent::PttReleased),
            AppState::Injecting
        );
        assert_eq!(
            AppState::Injecting.on_event(AppEvent::DoneTogglePressed),
            AppState::Injecting
        );
    }
}

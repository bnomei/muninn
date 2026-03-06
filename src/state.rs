#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppState {
    Idle,
    RecordingPushToTalk,
    RecordingDone,
    Processing,
    Injecting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEvent {
    PttPressed,
    PttReleased,
    DoneTogglePressed,
    CancelPressed,
    ProcessingFinished,
    InjectionFinished,
}

impl AppState {
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

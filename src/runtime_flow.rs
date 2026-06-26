//! Recording and injection state machine coordinator.
//!
//! Encapsulates [`AppState`] transitions with the indicator, audio recorder, and
//! text injector adapters. Called from the runtime worker Tokio runtime; does not
//! touch the tao event loop directly.

use std::time::Duration;

use crate::{
    AppEvent, AppState, AudioFrame, AudioRecorder, HotkeyAction, HotkeyEvent, HotkeyEventKind,
    IndicatorAdapter, IndicatorState, InjectionRoute, MacosAdapterResult, RecordedAudio,
    RecordingMode, TextInjector, TARGET_RUNTIME,
};
use tracing::warn;

/// Coordinates recording capture, indicator updates, and text injection.
#[derive(Debug)]
pub struct RuntimeFlowCoordinator<I, R, T>
where
    I: IndicatorAdapter,
    R: AudioRecorder,
    T: TextInjector,
{
    state: AppState,
    indicator: I,
    recorder: R,
    injector: T,
}

impl<I, R, T> RuntimeFlowCoordinator<I, R, T>
where
    I: IndicatorAdapter,
    R: AudioRecorder,
    T: TextInjector,
{
    /// Create a coordinator starting in [`AppState::Idle`].
    pub fn new(indicator: I, recorder: R, injector: T) -> Self {
        Self {
            state: AppState::Idle,
            indicator,
            recorder,
            injector,
        }
    }

    /// Current high-level runtime state.
    pub fn state(&self) -> AppState {
        self.state
    }

    /// Mutable access for callers that apply transitions outside this type.
    pub fn state_mut(&mut self) -> &mut AppState {
        &mut self.state
    }

    /// Mutable indicator adapter handle.
    pub fn indicator_mut(&mut self) -> &mut I {
        &mut self.indicator
    }

    /// Mutable recorder adapter handle.
    pub fn recorder_mut(&mut self) -> &mut R {
        &mut self.recorder
    }

    /// Text injector used after pipeline routing.
    pub fn injector(&self) -> &T {
        &self.injector
    }

    /// Borrow state, indicator, and injector together for post-recording processing.
    pub fn processing_parts(&mut self) -> (&mut AppState, &mut I, &T) {
        (&mut self.state, &mut self.indicator, &self.injector)
    }

    /// Initialize the indicator to idle appearance.
    pub async fn initialize(&mut self) -> MacosAdapterResult<()> {
        self.indicator.initialize().await
    }

    /// Begin push-to-talk recording. No-op when the state machine rejects [`AppEvent::PttPressed`].
    pub async fn start_push_to_talk(&mut self, glyph: Option<char>) -> MacosAdapterResult<bool> {
        self.start_push_to_talk_with_audio_sink(glyph, None).await
    }

    /// Begin push-to-talk recording with an optional live audio frame sink.
    pub async fn start_push_to_talk_with_audio_sink(
        &mut self,
        glyph: Option<char>,
        sink: Option<tokio::sync::mpsc::Sender<AudioFrame>>,
    ) -> MacosAdapterResult<bool> {
        self.start_recording(AppEvent::PttPressed, RecordingMode::PushToTalk, glyph, sink)
            .await
    }

    /// Begin done-mode recording. No-op when the state machine rejects [`AppEvent::DoneTogglePressed`].
    pub async fn start_done_mode(&mut self, glyph: Option<char>) -> MacosAdapterResult<bool> {
        self.start_done_mode_with_audio_sink(glyph, None).await
    }

    /// Begin done-mode recording with an optional live audio frame sink.
    pub async fn start_done_mode_with_audio_sink(
        &mut self,
        glyph: Option<char>,
        sink: Option<tokio::sync::mpsc::Sender<AudioFrame>>,
    ) -> MacosAdapterResult<bool> {
        self.start_recording(
            AppEvent::DoneTogglePressed,
            RecordingMode::DoneMode,
            glyph,
            sink,
        )
        .await
    }

    /// Stop push-to-talk capture and enter processing with the given initial indicator.
    ///
    /// Returns `None` when release is ignored. Resets to idle if stop fails.
    pub async fn finish_push_to_talk_for_processing(
        &mut self,
        initial_indicator: IndicatorState,
        glyph: Option<char>,
    ) -> MacosAdapterResult<Option<RecordedAudio>> {
        self.finish_recording_for_processing(AppEvent::PttReleased, initial_indicator, glyph)
            .await
    }

    /// Stop done-mode capture and enter processing with the given initial indicator.
    ///
    /// Returns `None` when the toggle is ignored. Resets to idle if stop fails.
    pub async fn finish_done_mode_for_processing(
        &mut self,
        initial_indicator: IndicatorState,
        glyph: Option<char>,
    ) -> MacosAdapterResult<Option<RecordedAudio>> {
        self.finish_recording_for_processing(AppEvent::DoneTogglePressed, initial_indicator, glyph)
            .await
    }

    /// Discard the active recording and flash the cancelled indicator briefly.
    ///
    /// No-op when cancel is not valid for the current state.
    pub async fn cancel_current_capture(
        &mut self,
        glyph: Option<char>,
        min_duration: Duration,
    ) -> MacosAdapterResult<bool> {
        let previous = self.state;
        let next = previous.on_event(AppEvent::CancelPressed);
        if next == previous {
            return Ok(false);
        }

        self.recorder.cancel_recording().await?;
        self.indicator
            .set_temporary_state_with_glyph(
                IndicatorState::Cancelled,
                glyph,
                min_duration,
                IndicatorState::Idle,
                None,
            )
            .await?;
        self.state = next;
        Ok(true)
    }

    /// Inject routed text after processing and drive output indicator timing.
    ///
    /// No-op unless the coordinator is in [`AppState::Processing`]. Resets the
    /// indicator to idle after injection failures.
    pub async fn complete_processing_with_route(
        &mut self,
        route: &InjectionRoute,
        glyph: Option<char>,
        output_indicator_min_duration: Duration,
    ) -> MacosAdapterResult<bool> {
        if self.state != AppState::Processing {
            return Ok(false);
        }

        self.indicator
            .set_state_with_glyph(IndicatorState::Pipeline, glyph)
            .await?;
        self.state = self.state.on_event(AppEvent::ProcessingFinished);
        let route_result = async {
            if let Some(text) = route.target.text() {
                self.indicator
                    .set_temporary_state_with_glyph(
                        IndicatorState::Output,
                        glyph,
                        output_indicator_min_duration,
                        IndicatorState::Idle,
                        None,
                    )
                    .await?;
                self.injector.inject_checked(text).await?;
            }
            Ok::<(), crate::MacosAdapterError>(())
        }
        .await;

        self.state = self.state.on_event(AppEvent::InjectionFinished);
        if let Err(error) = route_result {
            if matches!(self.indicator.state().await, Ok(IndicatorState::Pipeline)) {
                if let Err(reset_error) = self.indicator.set_state(IndicatorState::Idle).await {
                    warn!(
                        target: TARGET_RUNTIME,
                        error = %reset_error,
                        "failed to reset indicator after injection failure"
                    );
                }
            }
            return Err(error);
        }
        if matches!(self.indicator.state().await?, IndicatorState::Pipeline) {
            self.indicator.set_state(IndicatorState::Idle).await?;
        }
        Ok(true)
    }

    async fn start_recording(
        &mut self,
        event: AppEvent,
        mode: RecordingMode,
        glyph: Option<char>,
        sink: Option<tokio::sync::mpsc::Sender<AudioFrame>>,
    ) -> MacosAdapterResult<bool> {
        let previous = self.state;
        let next = previous.on_event(event);
        if next == previous {
            return Ok(false);
        }

        self.indicator
            .set_state_with_glyph(IndicatorState::Recording { mode }, glyph)
            .await?;
        let start_result = match sink {
            Some(sink) => {
                self.recorder
                    .start_recording_with_audio_sink(Some(sink))
                    .await
            }
            None => self.recorder.start_recording().await,
        };
        if let Err(error) = start_result {
            if let Err(reset_error) = self.indicator.set_state(IndicatorState::Idle).await {
                warn!(
                    target: TARGET_RUNTIME,
                    error = %reset_error,
                    "failed to reset indicator after recording start failure"
                );
            }
            return Err(error);
        }
        self.state = next;
        Ok(true)
    }

    async fn finish_recording_for_processing(
        &mut self,
        event: AppEvent,
        initial_indicator: IndicatorState,
        glyph: Option<char>,
    ) -> MacosAdapterResult<Option<RecordedAudio>> {
        let previous = self.state;
        let next = previous.on_event(event);
        if next == previous {
            return Ok(None);
        }

        self.indicator
            .set_state_with_glyph(initial_indicator, glyph)
            .await?;
        let recorded = match self.recorder.stop_recording().await {
            Ok(recorded) => recorded,
            Err(error) => {
                self.state = AppState::Idle;
                if let Err(reset_error) = self.indicator.set_state(IndicatorState::Idle).await {
                    warn!(
                        target: TARGET_RUNTIME,
                        error = %reset_error,
                        "failed to reset indicator after recording stop failure"
                    );
                }
                return Err(error);
            }
        };
        self.state = next;
        Ok(Some(recorded))
    }
}

/// Map a macOS hotkey edge to the corresponding [`AppEvent`].
///
/// Release events for toggle and cancel chords are ignored.
pub fn map_hotkey_event(event: HotkeyEvent) -> Option<AppEvent> {
    match (event.action, event.kind) {
        (HotkeyAction::PushToTalk, HotkeyEventKind::Pressed) => Some(AppEvent::PttPressed),
        (HotkeyAction::PushToTalk, HotkeyEventKind::Released) => Some(AppEvent::PttReleased),
        (HotkeyAction::DoneModeToggle, HotkeyEventKind::Pressed) => {
            Some(AppEvent::DoneTogglePressed)
        }
        (HotkeyAction::CancelCurrentCapture, HotkeyEventKind::Pressed) => {
            Some(AppEvent::CancelPressed)
        }
        (HotkeyAction::DoneModeToggle, HotkeyEventKind::Released)
        | (HotkeyAction::CancelCurrentCapture, HotkeyEventKind::Released) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        InjectionRouteReason, InjectionTarget, MacosAdapterError, MockAudioRecorder,
        MockIndicatorAdapter, MockTextInjector,
    };

    #[tokio::test(flavor = "current_thread")]
    async fn coordinator_runs_record_start_stop_and_route_injection() {
        let indicator = MockIndicatorAdapter::new();
        let recorder = MockAudioRecorder::new();
        let injector = MockTextInjector::new();
        let mut coordinator = RuntimeFlowCoordinator::new(indicator.clone(), recorder, injector);

        coordinator
            .initialize()
            .await
            .expect("initialize should work");
        assert!(coordinator
            .start_push_to_talk(Some('T'))
            .await
            .expect("start should work"));
        let recorded = coordinator
            .finish_push_to_talk_for_processing(IndicatorState::Transcribing, Some('T'))
            .await
            .expect("stop should work")
            .expect("recorded audio expected");
        assert_eq!(recorded.duration_ms, 1_000);
        assert_eq!(coordinator.state(), AppState::Processing);

        let route = InjectionRoute {
            target: InjectionTarget::TranscriptRawText("ship to sf".to_string()),
            reason: InjectionRouteReason::SelectedTranscriptRawText,
            pipeline_stop_reason: None,
        };
        assert!(coordinator
            .complete_processing_with_route(&route, Some('T'), Duration::from_millis(1))
            .await
            .expect("route completion should work"));
        assert_eq!(coordinator.state(), AppState::Idle);
        assert_eq!(
            indicator.state_history(),
            vec![
                IndicatorState::Recording {
                    mode: RecordingMode::PushToTalk
                },
                IndicatorState::Transcribing,
                IndicatorState::Pipeline,
                IndicatorState::Output,
                IndicatorState::Idle,
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coordinator_returns_to_idle_when_injection_fails() {
        let indicator = MockIndicatorAdapter::new();
        let recorder = MockAudioRecorder::new();
        let injector = MockTextInjector::new();
        injector.enqueue_inject_error(MacosAdapterError::operation_failed("injector", "boom"));
        let mut coordinator =
            RuntimeFlowCoordinator::new(indicator.clone(), recorder, injector.clone());

        coordinator
            .initialize()
            .await
            .expect("initialize should work");
        coordinator
            .start_push_to_talk(Some('T'))
            .await
            .expect("start should work");
        coordinator
            .finish_push_to_talk_for_processing(IndicatorState::Transcribing, Some('T'))
            .await
            .expect("stop should work")
            .expect("recorded audio expected");

        let route = InjectionRoute {
            target: InjectionTarget::TranscriptRawText("ship to sf".to_string()),
            reason: InjectionRouteReason::SelectedTranscriptRawText,
            pipeline_stop_reason: None,
        };
        let error = coordinator
            .complete_processing_with_route(&route, Some('T'), Duration::from_millis(1))
            .await
            .expect_err("injection failure should surface");

        assert_eq!(coordinator.state(), AppState::Idle);
        assert_eq!(injector.inject_calls(), 1);
        assert_eq!(
            indicator.state_history(),
            vec![
                IndicatorState::Recording {
                    mode: RecordingMode::PushToTalk
                },
                IndicatorState::Transcribing,
                IndicatorState::Pipeline,
                IndicatorState::Output,
                IndicatorState::Idle,
            ]
        );
        assert!(error.to_string().contains("boom"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn coordinator_returns_to_idle_when_recording_stop_fails() {
        let indicator = MockIndicatorAdapter::new();
        let recorder = MockAudioRecorder::new();
        recorder.enqueue_stop_result(Err(MacosAdapterError::operation_failed(
            "stop_recording",
            "recording exceeded max buffered duration (180s)",
        )));
        let injector = MockTextInjector::new();
        let mut coordinator =
            RuntimeFlowCoordinator::new(indicator.clone(), recorder.clone(), injector);

        coordinator
            .initialize()
            .await
            .expect("initialize should work");
        coordinator
            .start_push_to_talk(Some('T'))
            .await
            .expect("start should work");
        let error = coordinator
            .finish_push_to_talk_for_processing(IndicatorState::Transcribing, Some('T'))
            .await
            .expect_err("stop failure should surface");

        assert_eq!(coordinator.state(), AppState::Idle);
        assert!(!recorder.is_active());
        assert!(error.to_string().contains("recording exceeded"));
        assert_eq!(
            indicator.state_history(),
            vec![
                IndicatorState::Recording {
                    mode: RecordingMode::PushToTalk
                },
                IndicatorState::Transcribing,
                IndicatorState::Idle,
            ]
        );

        coordinator
            .start_push_to_talk(Some('T'))
            .await
            .expect("next start should work after failed stop");
        assert_eq!(
            coordinator.state(),
            AppState::RecordingPushToTalk,
            "failed stop must not leave runtime stuck in recording state"
        );
    }
}

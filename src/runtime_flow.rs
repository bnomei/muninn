use std::time::Duration;

use crate::{
    AppEvent, AppState, AudioRecorder, HotkeyAction, HotkeyEvent, HotkeyEventKind,
    IndicatorAdapter, IndicatorState, InjectionRoute, MacosAdapterResult, RecordedAudio,
    RecordingMode, TextInjector,
};

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
    pub fn new(indicator: I, recorder: R, injector: T) -> Self {
        Self {
            state: AppState::Idle,
            indicator,
            recorder,
            injector,
        }
    }

    pub fn state(&self) -> AppState {
        self.state
    }

    pub fn state_mut(&mut self) -> &mut AppState {
        &mut self.state
    }

    pub fn indicator_mut(&mut self) -> &mut I {
        &mut self.indicator
    }

    pub fn recorder_mut(&mut self) -> &mut R {
        &mut self.recorder
    }

    pub fn injector(&self) -> &T {
        &self.injector
    }

    pub fn processing_parts(&mut self) -> (&mut AppState, &mut I, &T) {
        (&mut self.state, &mut self.indicator, &self.injector)
    }

    pub async fn initialize(&mut self) -> MacosAdapterResult<()> {
        self.indicator.initialize().await
    }

    pub async fn start_push_to_talk(&mut self, glyph: Option<char>) -> MacosAdapterResult<bool> {
        self.start_recording(AppEvent::PttPressed, RecordingMode::PushToTalk, glyph)
            .await
    }

    pub async fn start_done_mode(&mut self, glyph: Option<char>) -> MacosAdapterResult<bool> {
        self.start_recording(AppEvent::DoneTogglePressed, RecordingMode::DoneMode, glyph)
            .await
    }

    pub async fn finish_push_to_talk_for_processing(
        &mut self,
        initial_indicator: IndicatorState,
        glyph: Option<char>,
    ) -> MacosAdapterResult<Option<RecordedAudio>> {
        self.finish_recording_for_processing(AppEvent::PttReleased, initial_indicator, glyph)
            .await
    }

    pub async fn finish_done_mode_for_processing(
        &mut self,
        initial_indicator: IndicatorState,
        glyph: Option<char>,
    ) -> MacosAdapterResult<Option<RecordedAudio>> {
        self.finish_recording_for_processing(AppEvent::DoneTogglePressed, initial_indicator, glyph)
            .await
    }

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
                let _ = self.indicator.set_state(IndicatorState::Idle).await;
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
    ) -> MacosAdapterResult<bool> {
        let previous = self.state;
        let next = previous.on_event(event);
        if next == previous {
            return Ok(false);
        }

        self.indicator
            .set_state_with_glyph(IndicatorState::Recording { mode }, glyph)
            .await?;
        if let Err(error) = self.recorder.start_recording().await {
            let _ = self.indicator.set_state(IndicatorState::Idle).await;
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
                let _ = self.indicator.set_state(IndicatorState::Idle).await;
                return Err(error);
            }
        };
        self.state = next;
        Ok(Some(recorded))
    }
}

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
}

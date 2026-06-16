use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use muninn::config::{
    OnErrorPolicy, PayloadFormat, PipelineConfig, PipelineStepConfig, StepIoMode,
};
use muninn::{
    map_hotkey_event, ActiveStreamingTranscription, AppConfig, AppEvent, AppState, AudioFrame,
    HotkeyAction, HotkeyEvent, HotkeyEventKind, HotkeyEventSource, InProcessStepError,
    InProcessStepExecutor, IndicatorState, InjectionRoute, InjectionRouteReason, InjectionTarget,
    MockAudioRecorder, MockHotkeyEventSource, MockIndicatorAdapter, MockTextInjector,
    MuninnEnvelopeV1, PipelineOutcome, PipelineRunner, RecordingMode, ResolvedUtteranceConfig,
    RuntimeFlowCoordinator, StepFailureKind, StreamingTranscriptOutcome,
    StreamingTranscriptionError, StreamingTranscriptionProvider,
    StreamingTranscriptionProviderEntry, StreamingTranscriptionSession,
    TranscriptionAttemptOutcome, TranscriptionProvider,
};

struct RuntimeTestRig {
    indicator: MockIndicatorAdapter,
    recorder: MockAudioRecorder,
    injector: MockTextInjector,
    coordinator: RuntimeFlowCoordinator<MockIndicatorAdapter, MockAudioRecorder, MockTextInjector>,
}

impl RuntimeTestRig {
    async fn new() -> Self {
        let indicator = MockIndicatorAdapter::new();
        let recorder = MockAudioRecorder::new();
        let injector = MockTextInjector::new();
        let mut coordinator =
            RuntimeFlowCoordinator::new(indicator.clone(), recorder.clone(), injector.clone());
        coordinator
            .initialize()
            .await
            .expect("indicator init should succeed");

        Self {
            indicator,
            recorder,
            injector,
            coordinator,
        }
    }

    async fn replay_hotkeys(&mut self, events: &[HotkeyEvent]) {
        let mut source = MockHotkeyEventSource::with_events(events.iter().copied());
        for _ in 0..events.len() {
            let event = source
                .next_event()
                .await
                .expect("hotkey event should exist");
            self.handle_hotkey(event).await;
        }
        assert_eq!(source.pending_events(), 0);
    }

    async fn handle_hotkey(&mut self, event: HotkeyEvent) {
        let Some(app_event) = map_hotkey_event(event) else {
            return;
        };

        match app_event {
            AppEvent::PttPressed => {
                let _ = self
                    .coordinator
                    .start_push_to_talk(None)
                    .await
                    .expect("ptt start should succeed");
            }
            AppEvent::PttReleased => {
                let _ = self
                    .coordinator
                    .finish_push_to_talk_for_processing(IndicatorState::Transcribing, None)
                    .await
                    .expect("ptt release should stop recorder");
            }
            AppEvent::DoneTogglePressed => {
                if self.coordinator.state() == AppState::Idle {
                    let _ = self
                        .coordinator
                        .start_done_mode(None)
                        .await
                        .expect("done-toggle start should succeed");
                } else {
                    let _ = self
                        .coordinator
                        .finish_done_mode_for_processing(IndicatorState::Transcribing, None)
                        .await
                        .expect("done-toggle second press should stop recorder");
                }
            }
            AppEvent::CancelPressed => {
                let _ = self
                    .coordinator
                    .cancel_current_capture(None, Duration::from_millis(1))
                    .await
                    .expect("cancel should clear recorder session");
            }
            AppEvent::ProcessingFinished | AppEvent::InjectionFinished => {}
        }
    }

    async fn complete_processing_with_route(&mut self, route: &InjectionRoute) {
        assert_eq!(self.coordinator.state(), AppState::Processing);
        assert!(self
            .coordinator
            .complete_processing_with_route(route, None, Duration::from_millis(1))
            .await
            .expect("route completion should succeed"));
    }
}

fn ptt_pressed() -> HotkeyEvent {
    HotkeyEvent::new(HotkeyAction::PushToTalk, HotkeyEventKind::Pressed)
}

fn ptt_released() -> HotkeyEvent {
    HotkeyEvent::new(HotkeyAction::PushToTalk, HotkeyEventKind::Released)
}

fn done_pressed() -> HotkeyEvent {
    HotkeyEvent::new(HotkeyAction::DoneModeToggle, HotkeyEventKind::Pressed)
}

fn cancel_pressed() -> HotkeyEvent {
    HotkeyEvent::new(HotkeyAction::CancelCurrentCapture, HotkeyEventKind::Pressed)
}

#[tokio::test(flavor = "current_thread")]
async fn ptt_flow_transitions_idle_recording_processing() {
    let mut rig = RuntimeTestRig::new().await;
    rig.replay_hotkeys(&[ptt_pressed(), ptt_released()]).await;

    assert_eq!(rig.coordinator.state(), AppState::Processing);
    assert_eq!(rig.recorder.start_calls(), 1);
    assert_eq!(rig.recorder.start_with_audio_sink_calls(), 0);
    assert_eq!(rig.recorder.stop_calls(), 1);
    assert_eq!(rig.recorder.cancel_calls(), 0);
    assert_eq!(
        rig.indicator.state_history(),
        vec![
            IndicatorState::Recording {
                mode: RecordingMode::PushToTalk
            },
            IndicatorState::Transcribing,
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn done_toggle_flow_transitions_idle_recording_processing() {
    let mut rig = RuntimeTestRig::new().await;
    rig.replay_hotkeys(&[done_pressed(), done_pressed()]).await;

    assert_eq!(rig.coordinator.state(), AppState::Processing);
    assert_eq!(rig.recorder.start_calls(), 1);
    assert_eq!(rig.recorder.start_with_audio_sink_calls(), 0);
    assert_eq!(rig.recorder.stop_calls(), 1);
    assert_eq!(rig.recorder.cancel_calls(), 0);
    assert_eq!(
        rig.indicator.state_history(),
        vec![
            IndicatorState::Recording {
                mode: RecordingMode::DoneMode
            },
            IndicatorState::Transcribing,
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn streaming_success_seeds_raw_text_before_downstream_pipeline() {
    let state = Arc::new(Mutex::new(FakeStreamingState::default()));
    let provider = FakeStreamingProvider {
        state: Arc::clone(&state),
        start_error: None,
        finish_result: Ok(StreamingTranscriptOutcome::produced(
            TranscriptionProvider::OpenAi,
            "hello from streaming",
        )),
    };
    let resolved = resolved_config(
        r#"
[transcription]
mode = "streaming"
providers = ["apple_speech", "openai"]
"#,
    );
    let active = ActiveStreamingTranscription::start_with_providers(
        &resolved,
        [StreamingTranscriptionProviderEntry::new(
            TranscriptionProvider::OpenAi,
            provider,
        )],
    )
    .await;
    let sink = active.sink().expect("streaming sink should be available");
    sink.send(AudioFrame {
        samples: vec![10, 20, 30],
        sample_rate_hz: 16_000,
        channels: 1,
    })
    .await
    .expect("audio frame should enqueue");
    drop(sink);

    let outcome = active.finish().await.expect("streaming should succeed");
    let mut envelope =
        MuninnEnvelopeV1::new("utt-stream", "2026-06-16T12:00:00Z").with_audio(None, 1000);
    outcome.apply_to_envelope(&mut envelope);
    let runner = PipelineRunner::with_in_process_step_executor(
        true,
        Arc::new(AssertRefineStep {
            expected_raw_text: "hello from streaming".to_string(),
            final_text: "Hello from streaming.".to_string(),
        }),
    );

    let outcome = runner.run(envelope, &single_step_pipeline("refine")).await;

    match outcome {
        PipelineOutcome::Completed { envelope, trace } => {
            assert_eq!(trace.len(), 1);
            assert_eq!(
                envelope.transcript.raw_text.as_deref(),
                Some("hello from streaming")
            );
            assert_eq!(
                envelope.output.final_text.as_deref(),
                Some("Hello from streaming.")
            );
        }
        other => panic!("pipeline should complete after streaming seed, got {other:?}"),
    }
    assert_eq!(state.lock().expect("fake state poisoned").frame_count, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn streaming_failure_falls_back_to_recorded_route_when_enabled() {
    let state = Arc::new(Mutex::new(FakeStreamingState::default()));
    let provider = FakeStreamingProvider {
        state: Arc::clone(&state),
        start_error: None,
        finish_result: Err(StreamingTranscriptionError::failed(
            TranscriptionProvider::Deepgram,
            "stream_closed",
            "stream closed before final transcript",
        )),
    };
    let resolved = resolved_config(
        r#"
[transcription]
mode = "streaming"
providers = ["deepgram"]
"#,
    );

    let active = ActiveStreamingTranscription::start_with_providers(
        &resolved,
        [StreamingTranscriptionProviderEntry::new(
            TranscriptionProvider::Deepgram,
            provider,
        )],
    )
    .await;

    assert!(active.finish().await.is_none());
    assert_eq!(state.lock().expect("fake state poisoned").start_calls, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn streaming_failure_without_fallback_records_error_without_inventing_text() {
    let state = Arc::new(Mutex::new(FakeStreamingState::default()));
    let provider = FakeStreamingProvider {
        state: Arc::clone(&state),
        start_error: None,
        finish_result: Err(StreamingTranscriptionError::failed(
            TranscriptionProvider::Google,
            "stream_closed",
            "stream closed before final transcript",
        )),
    };
    let resolved = resolved_config(
        r#"
[transcription]
mode = "streaming"
providers = ["google"]

[transcription.streaming]
fallback_to_recorded_on_error = false
"#,
    );

    let active = ActiveStreamingTranscription::start_with_providers(
        &resolved,
        [StreamingTranscriptionProviderEntry::new(
            TranscriptionProvider::Google,
            provider,
        )],
    )
    .await;
    let streaming_outcome = active
        .finish()
        .await
        .expect("streaming error outcome should be preserved");
    let mut envelope =
        MuninnEnvelopeV1::new("utt-stream-failed", "2026-06-16T12:05:00Z").with_audio(None, 1000);

    streaming_outcome.apply_to_envelope(&mut envelope);

    assert!(envelope.transcript.raw_text.is_none());
    assert_eq!(envelope.transcript.provider.as_deref(), Some("google"));
    assert_eq!(envelope.errors.len(), 1);
    let attempts = muninn::transcription_attempts(&envelope);
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].provider, TranscriptionProvider::Google);
    assert_eq!(
        attempts[0].outcome,
        TranscriptionAttemptOutcome::RequestFailed
    );
    assert_eq!(state.lock().expect("fake state poisoned").start_calls, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn streaming_cancel_stays_idle_without_processing_or_injection() {
    let state = Arc::new(Mutex::new(FakeStreamingState::default()));
    let provider = FakeStreamingProvider {
        state: Arc::clone(&state),
        start_error: None,
        finish_result: Ok(StreamingTranscriptOutcome::produced(
            TranscriptionProvider::OpenAi,
            "unused",
        )),
    };
    let resolved = resolved_config(
        r#"
[transcription]
mode = "streaming"
providers = ["openai"]
"#,
    );
    let active = ActiveStreamingTranscription::start_with_providers(
        &resolved,
        [StreamingTranscriptionProviderEntry::new(
            TranscriptionProvider::OpenAi,
            provider,
        )],
    )
    .await;
    let mut rig = RuntimeTestRig::new().await;

    rig.coordinator
        .start_push_to_talk_with_audio_sink(None, active.sink())
        .await
        .expect("recording should start with streaming sink");
    rig.coordinator
        .cancel_current_capture(None, Duration::from_millis(1))
        .await
        .expect("recording cancel should succeed");
    active.cancel().await;

    assert_eq!(rig.coordinator.state(), AppState::Idle);
    assert_eq!(rig.recorder.stop_calls(), 0);
    assert_eq!(rig.recorder.cancel_calls(), 1);
    assert_eq!(rig.recorder.audio_sink_start_history(), vec![true]);
    assert_eq!(rig.injector.inject_calls(), 0);
    assert_eq!(state.lock().expect("fake state poisoned").cancel_calls, 1);
}

#[tokio::test(flavor = "current_thread")]
async fn streaming_disabled_or_without_capable_provider_uses_recorded_start_path() {
    let state = Arc::new(Mutex::new(FakeStreamingState::default()));
    let provider = FakeStreamingProvider {
        state: Arc::clone(&state),
        start_error: None,
        finish_result: Ok(StreamingTranscriptOutcome::produced(
            TranscriptionProvider::OpenAi,
            "unused",
        )),
    };
    let recorded = resolved_config(
        r#"
[transcription]
mode = "recorded"
providers = ["openai"]
"#,
    );
    let active = ActiveStreamingTranscription::start_with_providers(
        &recorded,
        [StreamingTranscriptionProviderEntry::new(
            TranscriptionProvider::OpenAi,
            provider.clone(),
        )],
    )
    .await;
    assert!(active.sink().is_none());
    assert!(active.finish().await.is_none());

    let local_only = resolved_config(
        r#"
[transcription]
mode = "streaming"
providers = ["apple_speech", "whisper_cpp"]
"#,
    );
    let active = ActiveStreamingTranscription::start_with_providers(
        &local_only,
        [StreamingTranscriptionProviderEntry::new(
            TranscriptionProvider::OpenAi,
            provider,
        )],
    )
    .await;
    let local_sink = active.sink();
    assert!(local_sink.is_none());
    assert!(active.finish().await.is_none());
    assert_eq!(state.lock().expect("fake state poisoned").start_calls, 0);

    let mut rig = RuntimeTestRig::new().await;
    rig.coordinator
        .start_done_mode_with_audio_sink(None, local_sink)
        .await
        .expect("recorded start should still work");
    assert_eq!(rig.recorder.start_calls(), 1);
    assert_eq!(rig.recorder.start_with_audio_sink_calls(), 0);
}

#[derive(Clone)]
struct FakeStreamingProvider {
    state: Arc<Mutex<FakeStreamingState>>,
    start_error: Option<StreamingTranscriptionError>,
    finish_result: Result<StreamingTranscriptOutcome, StreamingTranscriptionError>,
}

#[derive(Default)]
struct FakeStreamingState {
    start_calls: usize,
    frame_count: usize,
    cancel_calls: usize,
}

#[async_trait]
impl StreamingTranscriptionProvider for FakeStreamingProvider {
    async fn start(
        &self,
        _resolved: &ResolvedUtteranceConfig,
    ) -> Result<Box<dyn StreamingTranscriptionSession>, StreamingTranscriptionError> {
        self.state.lock().expect("fake state poisoned").start_calls += 1;
        if let Some(error) = self.start_error.clone() {
            return Err(error);
        }
        Ok(Box::new(FakeStreamingSession {
            state: Arc::clone(&self.state),
            finish_result: self.finish_result.clone(),
        }))
    }
}

struct FakeStreamingSession {
    state: Arc<Mutex<FakeStreamingState>>,
    finish_result: Result<StreamingTranscriptOutcome, StreamingTranscriptionError>,
}

#[async_trait]
impl StreamingTranscriptionSession for FakeStreamingSession {
    async fn send_audio(&mut self, _frame: AudioFrame) -> Result<(), StreamingTranscriptionError> {
        self.state.lock().expect("fake state poisoned").frame_count += 1;
        Ok(())
    }

    async fn finish(
        self: Box<Self>,
    ) -> Result<StreamingTranscriptOutcome, StreamingTranscriptionError> {
        self.finish_result.clone()
    }

    async fn cancel(self: Box<Self>) {
        self.state.lock().expect("fake state poisoned").cancel_calls += 1;
    }
}

struct AssertRefineStep {
    expected_raw_text: String,
    final_text: String,
}

#[async_trait]
impl InProcessStepExecutor for AssertRefineStep {
    async fn try_execute(
        &self,
        step: &PipelineStepConfig,
        input: &MuninnEnvelopeV1,
    ) -> Option<Result<MuninnEnvelopeV1, InProcessStepError>> {
        if step.cmd != "refine" {
            return None;
        }
        if input.transcript.raw_text.as_deref() != Some(self.expected_raw_text.as_str()) {
            return Some(Err(InProcessStepError {
                kind: StepFailureKind::InvalidEnvelope,
                message: "missing streaming raw text".to_string(),
                stderr: String::new(),
                exit_status: None,
            }));
        }

        let mut output = input.clone();
        output.output.final_text = Some(self.final_text.clone());
        Some(Ok(output))
    }
}

fn resolved_config(transcription_toml: &str) -> ResolvedUtteranceConfig {
    let raw = format!(
        r#"
{transcription_toml}

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "refine"
cmd = "refine"
timeout_ms = 100
on_error = "continue"
"#
    );
    AppConfig::from_toml_str(&raw)
        .expect("config should parse")
        .resolve_effective_config(muninn::TargetContextSnapshot::default())
}

fn single_step_pipeline(cmd: &str) -> PipelineConfig {
    PipelineConfig {
        deadline_ms: 500,
        payload_format: PayloadFormat::JsonObject,
        steps: vec![PipelineStepConfig {
            id: cmd.to_string(),
            cmd: cmd.to_string(),
            args: Vec::new(),
            io_mode: StepIoMode::Auto,
            timeout_ms: 100,
            on_error: OnErrorPolicy::Continue,
        }],
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancel_returns_to_idle_from_each_recording_mode() {
    let mut ptt_rig = RuntimeTestRig::new().await;
    ptt_rig
        .replay_hotkeys(&[ptt_pressed(), cancel_pressed()])
        .await;
    assert_eq!(ptt_rig.coordinator.state(), AppState::Idle);
    assert_eq!(ptt_rig.recorder.start_calls(), 1);
    assert_eq!(ptt_rig.recorder.stop_calls(), 0);
    assert_eq!(ptt_rig.recorder.cancel_calls(), 1);
    assert_eq!(
        ptt_rig.indicator.state_history(),
        vec![
            IndicatorState::Recording {
                mode: RecordingMode::PushToTalk
            },
            IndicatorState::Cancelled,
            IndicatorState::Idle,
        ]
    );

    let mut done_rig = RuntimeTestRig::new().await;
    done_rig
        .replay_hotkeys(&[done_pressed(), cancel_pressed()])
        .await;
    assert_eq!(done_rig.coordinator.state(), AppState::Idle);
    assert_eq!(done_rig.recorder.start_calls(), 1);
    assert_eq!(done_rig.recorder.stop_calls(), 0);
    assert_eq!(done_rig.recorder.cancel_calls(), 1);
    assert_eq!(
        done_rig.indicator.state_history(),
        vec![
            IndicatorState::Recording {
                mode: RecordingMode::DoneMode
            },
            IndicatorState::Cancelled,
            IndicatorState::Idle,
        ]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn busy_states_ignore_new_record_triggers() {
    let mut rig = RuntimeTestRig::new().await;
    rig.replay_hotkeys(&[ptt_pressed(), ptt_released()]).await;
    assert_eq!(rig.coordinator.state(), AppState::Processing);

    let start_calls = rig.recorder.start_calls();
    let stop_calls = rig.recorder.stop_calls();
    let cancel_calls = rig.recorder.cancel_calls();

    rig.replay_hotkeys(&[done_pressed(), ptt_pressed(), ptt_released()])
        .await;
    assert_eq!(rig.coordinator.state(), AppState::Processing);
    assert_eq!(rig.recorder.start_calls(), start_calls);
    assert_eq!(rig.recorder.stop_calls(), stop_calls);
    assert_eq!(rig.recorder.cancel_calls(), cancel_calls);

    *rig.coordinator.state_mut() = AppState::Injecting;
    rig.replay_hotkeys(&[done_pressed(), ptt_pressed(), ptt_released()])
        .await;
    assert_eq!(rig.coordinator.state(), AppState::Injecting);
    assert_eq!(rig.recorder.start_calls(), start_calls);
    assert_eq!(rig.recorder.stop_calls(), stop_calls);
    assert_eq!(rig.recorder.cancel_calls(), cancel_calls);
}

#[tokio::test(flavor = "current_thread")]
async fn fallback_route_injects_transcript_raw_text() {
    let mut rig = RuntimeTestRig::new().await;
    rig.replay_hotkeys(&[ptt_pressed(), ptt_released()]).await;
    assert_eq!(rig.coordinator.state(), AppState::Processing);

    let route = InjectionRoute {
        target: InjectionTarget::TranscriptRawText("ship to sf".to_string()),
        reason: InjectionRouteReason::SelectedTranscriptRawText,
        pipeline_stop_reason: None,
    };
    rig.complete_processing_with_route(&route).await;

    assert_eq!(
        route.reason,
        InjectionRouteReason::SelectedTranscriptRawText
    );
    assert_eq!(route.target.text(), Some("ship to sf"));
    assert!(route.pipeline_stop_reason.is_none());
    assert_eq!(rig.injector.inject_calls(), 1);
    assert_eq!(rig.injector.injected_text(), vec!["ship to sf".to_string()]);
    assert_eq!(rig.coordinator.state(), AppState::Idle);
    assert_eq!(
        rig.indicator.state_history(),
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

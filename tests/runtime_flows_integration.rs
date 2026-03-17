use std::time::Duration;

use muninn::{
    map_hotkey_event, AppEvent, AppState, HotkeyAction, HotkeyEvent, HotkeyEventKind,
    HotkeyEventSource, IndicatorState, InjectionRoute, InjectionRouteReason, InjectionTarget,
    MockAudioRecorder, MockHotkeyEventSource, MockIndicatorAdapter, MockTextInjector,
    RecordingMode, RuntimeFlowCoordinator,
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

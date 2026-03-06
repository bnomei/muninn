use std::{
    future::Future,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
};

use muninn::{AppEvent, AppState, InjectionRoute, InjectionRouteReason, InjectionTarget};
use muninn::{
    AudioRecorder, HotkeyAction, HotkeyEvent, HotkeyEventKind, HotkeyEventSource, IndicatorAdapter,
    IndicatorState, MockAudioRecorder, MockHotkeyEventSource, MockIndicatorAdapter,
    MockTextInjector, RecordingMode, TextInjector,
};

struct NoopWaker;

impl Wake for NoopWaker {
    fn wake(self: Arc<Self>) {}
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker: Waker = Waker::from(Arc::new(NoopWaker));
    let mut context = Context::from_waker(&waker);
    let mut future = Pin::from(Box::new(future));

    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        std::thread::yield_now();
    }
}

struct RuntimeHarness {
    state: AppState,
    indicator: MockIndicatorAdapter,
    recorder: MockAudioRecorder,
    injector: MockTextInjector,
}

impl RuntimeHarness {
    fn new() -> Self {
        let mut indicator = MockIndicatorAdapter::new();
        block_on(indicator.initialize()).expect("indicator init should succeed");

        Self {
            state: AppState::Idle,
            indicator,
            recorder: MockAudioRecorder::new(),
            injector: MockTextInjector::new(),
        }
    }

    fn replay_hotkeys(&mut self, events: &[HotkeyEvent]) {
        let mut source = MockHotkeyEventSource::with_events(events.iter().copied());
        for _ in 0..events.len() {
            let event = block_on(source.next_event()).expect("hotkey event should exist");
            self.handle_hotkey(event);
        }
        assert_eq!(source.pending_events(), 0);
    }

    fn handle_hotkey(&mut self, event: HotkeyEvent) {
        if let Some(app_event) = map_hotkey_event(event) {
            self.apply_event(app_event);
        }
    }

    fn apply_event(&mut self, event: AppEvent) {
        use AppEvent::*;
        use AppState::*;

        let previous = self.state;
        let next = previous.on_event(event);
        if next == previous {
            return;
        }

        match (previous, event, next) {
            (Idle, PttPressed, RecordingPushToTalk) => {
                block_on(self.indicator.set_state(IndicatorState::Recording {
                    mode: RecordingMode::PushToTalk,
                }))
                .expect("indicator should update to recording push-to-talk");
                block_on(self.recorder.start_recording())
                    .expect("ptt start should activate recorder");
            }
            (RecordingPushToTalk, PttReleased, Processing) => {
                block_on(self.indicator.set_state(IndicatorState::Transcribing))
                    .expect("indicator should update to transcribing");
                block_on(self.recorder.stop_recording()).expect("ptt release should stop recorder");
            }
            (Idle, DoneTogglePressed, RecordingDone) => {
                block_on(self.indicator.set_state(IndicatorState::Recording {
                    mode: RecordingMode::DoneMode,
                }))
                .expect("indicator should update to recording done mode");
                block_on(self.recorder.start_recording())
                    .expect("done-toggle start should activate recorder");
            }
            (RecordingDone, DoneTogglePressed, Processing) => {
                block_on(self.indicator.set_state(IndicatorState::Transcribing))
                    .expect("indicator should update to transcribing");
                block_on(self.recorder.stop_recording())
                    .expect("done-toggle second press should stop recorder");
            }
            (RecordingPushToTalk | RecordingDone, CancelPressed, Idle) => {
                block_on(self.recorder.cancel_recording())
                    .expect("cancel should clear recorder session");
                block_on(self.indicator.set_state(IndicatorState::Idle))
                    .expect("indicator should return to idle");
            }
            (Processing, ProcessingFinished, Injecting) => {
                block_on(self.indicator.set_state(IndicatorState::Output))
                    .expect("indicator should update to output");
            }
            (Injecting, InjectionFinished, Idle) => {
                block_on(self.indicator.set_state(IndicatorState::Idle))
                    .expect("indicator should return to idle after injection");
            }
            _ => {}
        }

        self.state = next;
    }

    fn complete_processing_with_route(&mut self, route: &InjectionRoute) {
        assert_eq!(self.state, AppState::Processing);

        block_on(self.indicator.set_state(IndicatorState::Pipeline))
            .expect("indicator should update to pipeline");
        self.apply_event(AppEvent::ProcessingFinished);

        if let Some(text) = route.target.text() {
            block_on(self.injector.inject_checked(text))
                .expect("injection should forward non-empty route text");
        }

        self.apply_event(AppEvent::InjectionFinished);
    }
}

fn map_hotkey_event(event: HotkeyEvent) -> Option<AppEvent> {
    match (event.action, event.kind) {
        (HotkeyAction::PushToTalk, HotkeyEventKind::Pressed) => Some(AppEvent::PttPressed),
        (HotkeyAction::PushToTalk, HotkeyEventKind::Released) => Some(AppEvent::PttReleased),
        (HotkeyAction::DoneModeToggle, HotkeyEventKind::Pressed) => {
            Some(AppEvent::DoneTogglePressed)
        }
        (HotkeyAction::CancelCurrentCapture, HotkeyEventKind::Pressed) => {
            Some(AppEvent::CancelPressed)
        }
        _ => None,
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

#[test]
fn ptt_flow_transitions_idle_recording_processing() {
    let mut harness = RuntimeHarness::new();
    harness.replay_hotkeys(&[ptt_pressed(), ptt_released()]);

    assert_eq!(harness.state, AppState::Processing);
    assert_eq!(harness.recorder.start_calls(), 1);
    assert_eq!(harness.recorder.stop_calls(), 1);
    assert_eq!(harness.recorder.cancel_calls(), 0);
    assert_eq!(
        harness.indicator.state_history(),
        vec![
            IndicatorState::Recording {
                mode: RecordingMode::PushToTalk
            },
            IndicatorState::Transcribing,
        ]
    );
}

#[test]
fn done_toggle_flow_transitions_idle_recording_processing() {
    let mut harness = RuntimeHarness::new();
    harness.replay_hotkeys(&[done_pressed(), done_pressed()]);

    assert_eq!(harness.state, AppState::Processing);
    assert_eq!(harness.recorder.start_calls(), 1);
    assert_eq!(harness.recorder.stop_calls(), 1);
    assert_eq!(harness.recorder.cancel_calls(), 0);
    assert_eq!(
        harness.indicator.state_history(),
        vec![
            IndicatorState::Recording {
                mode: RecordingMode::DoneMode
            },
            IndicatorState::Transcribing,
        ]
    );
}

#[test]
fn cancel_returns_to_idle_from_each_recording_mode() {
    let mut ptt_harness = RuntimeHarness::new();
    ptt_harness.replay_hotkeys(&[ptt_pressed(), cancel_pressed()]);
    assert_eq!(ptt_harness.state, AppState::Idle);
    assert_eq!(ptt_harness.recorder.start_calls(), 1);
    assert_eq!(ptt_harness.recorder.stop_calls(), 0);
    assert_eq!(ptt_harness.recorder.cancel_calls(), 1);
    assert_eq!(
        ptt_harness.indicator.state_history(),
        vec![
            IndicatorState::Recording {
                mode: RecordingMode::PushToTalk
            },
            IndicatorState::Idle,
        ]
    );

    let mut done_harness = RuntimeHarness::new();
    done_harness.replay_hotkeys(&[done_pressed(), cancel_pressed()]);
    assert_eq!(done_harness.state, AppState::Idle);
    assert_eq!(done_harness.recorder.start_calls(), 1);
    assert_eq!(done_harness.recorder.stop_calls(), 0);
    assert_eq!(done_harness.recorder.cancel_calls(), 1);
    assert_eq!(
        done_harness.indicator.state_history(),
        vec![
            IndicatorState::Recording {
                mode: RecordingMode::DoneMode
            },
            IndicatorState::Idle,
        ]
    );
}

#[test]
fn busy_states_ignore_new_record_triggers() {
    let mut harness = RuntimeHarness::new();
    harness.replay_hotkeys(&[ptt_pressed(), ptt_released()]);
    assert_eq!(harness.state, AppState::Processing);

    let start_calls = harness.recorder.start_calls();
    let stop_calls = harness.recorder.stop_calls();
    let cancel_calls = harness.recorder.cancel_calls();

    harness.replay_hotkeys(&[done_pressed(), ptt_pressed(), ptt_released()]);
    assert_eq!(harness.state, AppState::Processing);
    assert_eq!(harness.recorder.start_calls(), start_calls);
    assert_eq!(harness.recorder.stop_calls(), stop_calls);
    assert_eq!(harness.recorder.cancel_calls(), cancel_calls);

    harness.apply_event(AppEvent::ProcessingFinished);
    assert_eq!(harness.state, AppState::Injecting);

    harness.replay_hotkeys(&[done_pressed(), ptt_pressed(), ptt_released()]);
    assert_eq!(harness.state, AppState::Injecting);
    assert_eq!(harness.recorder.start_calls(), start_calls);
    assert_eq!(harness.recorder.stop_calls(), stop_calls);
    assert_eq!(harness.recorder.cancel_calls(), cancel_calls);
}

#[test]
fn fallback_route_injects_transcript_raw_text() {
    let mut harness = RuntimeHarness::new();
    harness.replay_hotkeys(&[ptt_pressed(), ptt_released()]);
    assert_eq!(harness.state, AppState::Processing);

    let route = InjectionRoute {
        target: InjectionTarget::TranscriptRawText("ship to sf".to_string()),
        reason: InjectionRouteReason::SelectedTranscriptRawText,
        pipeline_stop_reason: None,
    };
    harness.complete_processing_with_route(&route);

    assert_eq!(
        route.reason,
        InjectionRouteReason::SelectedTranscriptRawText
    );
    assert_eq!(route.target.text(), Some("ship to sf"));
    assert!(route.pipeline_stop_reason.is_none());
    assert_eq!(harness.injector.inject_calls(), 1);
    assert_eq!(
        harness.injector.injected_text(),
        vec!["ship to sf".to_string()]
    );
    assert_eq!(harness.state, AppState::Idle);
    assert_eq!(
        harness.indicator.state_history(),
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

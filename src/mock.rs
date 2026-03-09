use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;

use crate::{
    AudioRecorder, HotkeyEvent, HotkeyEventSource, IndicatorAdapter, IndicatorState,
    MacosAdapterError, MacosAdapterResult, PermissionPreflightStatus, PermissionsAdapter,
    RecordedAudio, TextInjector,
};

#[derive(Debug, Clone)]
pub struct MockIndicatorAdapter {
    inner: Arc<Mutex<IndicatorStateInner>>,
}

#[derive(Debug)]
struct IndicatorStateInner {
    current_state: IndicatorState,
    initialize_calls: usize,
    state_history: Vec<IndicatorState>,
    initialize_error: Option<MacosAdapterError>,
    set_state_error_queue: VecDeque<MacosAdapterError>,
    state_error: Option<MacosAdapterError>,
}

impl Default for IndicatorStateInner {
    fn default() -> Self {
        Self {
            current_state: IndicatorState::Idle,
            initialize_calls: 0,
            state_history: Vec::new(),
            initialize_error: None,
            set_state_error_queue: VecDeque::new(),
            state_error: None,
        }
    }
}

impl Default for MockIndicatorAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl MockIndicatorAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(IndicatorStateInner::default())),
        }
    }

    pub fn set_initialize_error(&self, error: Option<MacosAdapterError>) {
        self.inner
            .lock()
            .expect("indicator mutex poisoned")
            .initialize_error = error;
    }

    pub fn enqueue_set_state_error(&self, error: MacosAdapterError) {
        self.inner
            .lock()
            .expect("indicator mutex poisoned")
            .set_state_error_queue
            .push_back(error);
    }

    pub fn set_state_error(&self, error: Option<MacosAdapterError>) {
        self.inner
            .lock()
            .expect("indicator mutex poisoned")
            .state_error = error;
    }

    #[must_use]
    pub fn initialize_calls(&self) -> usize {
        self.inner
            .lock()
            .expect("indicator mutex poisoned")
            .initialize_calls
    }

    #[must_use]
    pub fn state_history(&self) -> Vec<IndicatorState> {
        self.inner
            .lock()
            .expect("indicator mutex poisoned")
            .state_history
            .clone()
    }
}

#[async_trait]
impl IndicatorAdapter for MockIndicatorAdapter {
    async fn initialize(&mut self) -> MacosAdapterResult<()> {
        let mut inner = self.inner.lock().expect("indicator mutex poisoned");
        inner.initialize_calls += 1;
        match inner.initialize_error.clone() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    async fn set_state(&mut self, state: IndicatorState) -> MacosAdapterResult<()> {
        let mut inner = self.inner.lock().expect("indicator mutex poisoned");
        if let Some(error) = inner.set_state_error_queue.pop_front() {
            return Err(error);
        }

        inner.current_state = state;
        inner.state_history.push(state);
        Ok(())
    }

    async fn set_temporary_state(
        &mut self,
        state: IndicatorState,
        _min_duration: Duration,
        fallback_state: IndicatorState,
    ) -> MacosAdapterResult<()> {
        self.set_state(state).await?;
        self.set_state(fallback_state).await
    }

    async fn state(&self) -> MacosAdapterResult<IndicatorState> {
        let inner = self.inner.lock().expect("indicator mutex poisoned");
        match inner.state_error.clone() {
            Some(error) => Err(error),
            None => Ok(inner.current_state),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MockPermissionsAdapter {
    inner: Arc<Mutex<PermissionsInner>>,
}

#[derive(Debug)]
struct PermissionsInner {
    preflight_result: MacosAdapterResult<PermissionPreflightStatus>,
    preflight_calls: usize,
    request_microphone_result: MacosAdapterResult<bool>,
    request_microphone_calls: usize,
    request_input_monitoring_result: MacosAdapterResult<bool>,
    request_input_monitoring_calls: usize,
    request_accessibility_result: MacosAdapterResult<bool>,
    request_accessibility_calls: usize,
    preflight_results_after_request: VecDeque<MacosAdapterResult<PermissionPreflightStatus>>,
}

impl Default for PermissionsInner {
    fn default() -> Self {
        Self {
            preflight_result: Ok(PermissionPreflightStatus::default()),
            preflight_calls: 0,
            request_microphone_result: Ok(false),
            request_microphone_calls: 0,
            request_input_monitoring_result: Ok(false),
            request_input_monitoring_calls: 0,
            request_accessibility_result: Ok(false),
            request_accessibility_calls: 0,
            preflight_results_after_request: VecDeque::new(),
        }
    }
}

impl Default for MockPermissionsAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl MockPermissionsAdapter {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(PermissionsInner::default())),
        }
    }

    pub fn set_preflight_status(&self, status: PermissionPreflightStatus) {
        self.inner
            .lock()
            .expect("permissions mutex poisoned")
            .preflight_result = Ok(status);
    }

    pub fn set_preflight_error(&self, error: MacosAdapterError) {
        self.inner
            .lock()
            .expect("permissions mutex poisoned")
            .preflight_result = Err(error);
    }

    #[must_use]
    pub fn preflight_calls(&self) -> usize {
        self.inner
            .lock()
            .expect("permissions mutex poisoned")
            .preflight_calls
    }

    pub fn set_request_input_monitoring_result(&self, granted: bool) {
        self.inner
            .lock()
            .expect("permissions mutex poisoned")
            .request_input_monitoring_result = Ok(granted);
    }

    pub fn set_request_input_monitoring_error(&self, error: MacosAdapterError) {
        self.inner
            .lock()
            .expect("permissions mutex poisoned")
            .request_input_monitoring_result = Err(error);
    }

    pub fn set_request_microphone_result(&self, granted: bool) {
        self.inner
            .lock()
            .expect("permissions mutex poisoned")
            .request_microphone_result = Ok(granted);
    }

    pub fn set_request_microphone_error(&self, error: MacosAdapterError) {
        self.inner
            .lock()
            .expect("permissions mutex poisoned")
            .request_microphone_result = Err(error);
    }

    pub fn set_request_accessibility_result(&self, granted: bool) {
        self.inner
            .lock()
            .expect("permissions mutex poisoned")
            .request_accessibility_result = Ok(granted);
    }

    pub fn set_request_accessibility_error(&self, error: MacosAdapterError) {
        self.inner
            .lock()
            .expect("permissions mutex poisoned")
            .request_accessibility_result = Err(error);
    }

    pub fn set_post_request_preflight_status(&self, status: PermissionPreflightStatus) {
        self.inner
            .lock()
            .expect("permissions mutex poisoned")
            .preflight_results_after_request
            .push_back(Ok(status));
    }

    #[must_use]
    pub fn request_input_monitoring_calls(&self) -> usize {
        self.inner
            .lock()
            .expect("permissions mutex poisoned")
            .request_input_monitoring_calls
    }

    #[must_use]
    pub fn request_microphone_calls(&self) -> usize {
        self.inner
            .lock()
            .expect("permissions mutex poisoned")
            .request_microphone_calls
    }

    #[must_use]
    pub fn request_accessibility_calls(&self) -> usize {
        self.inner
            .lock()
            .expect("permissions mutex poisoned")
            .request_accessibility_calls
    }
}

#[async_trait]
impl PermissionsAdapter for MockPermissionsAdapter {
    async fn preflight(&self) -> MacosAdapterResult<PermissionPreflightStatus> {
        let mut inner = self.inner.lock().expect("permissions mutex poisoned");
        inner.preflight_calls += 1;
        inner.preflight_result.clone()
    }

    async fn request_microphone_access(&self) -> MacosAdapterResult<bool> {
        let mut inner = self.inner.lock().expect("permissions mutex poisoned");
        inner.request_microphone_calls += 1;
        let result = inner.request_microphone_result.clone();
        if result.is_ok() {
            if let Some(next_preflight) = inner.preflight_results_after_request.pop_front() {
                inner.preflight_result = next_preflight;
            }
        }
        result
    }

    async fn request_input_monitoring_access(&self) -> MacosAdapterResult<bool> {
        let mut inner = self.inner.lock().expect("permissions mutex poisoned");
        inner.request_input_monitoring_calls += 1;
        let result = inner.request_input_monitoring_result.clone();
        if result.is_ok() {
            if let Some(next_preflight) = inner.preflight_results_after_request.pop_front() {
                inner.preflight_result = next_preflight;
            }
        }
        result
    }

    async fn request_accessibility_access(&self) -> MacosAdapterResult<bool> {
        let mut inner = self.inner.lock().expect("permissions mutex poisoned");
        inner.request_accessibility_calls += 1;
        let result = inner.request_accessibility_result.clone();
        if result.is_ok() {
            if let Some(next_preflight) = inner.preflight_results_after_request.pop_front() {
                inner.preflight_result = next_preflight;
            }
        }
        result
    }
}

#[derive(Debug, Clone)]
pub struct MockHotkeyEventSource {
    inner: Arc<Mutex<HotkeyInner>>,
}

#[derive(Debug, Default)]
struct HotkeyInner {
    events: VecDeque<MacosAdapterResult<HotkeyEvent>>,
    next_event_calls: usize,
}

impl Default for MockHotkeyEventSource {
    fn default() -> Self {
        Self::new()
    }
}

impl MockHotkeyEventSource {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HotkeyInner::default())),
        }
    }

    #[must_use]
    pub fn with_events(events: impl IntoIterator<Item = HotkeyEvent>) -> Self {
        let source = Self::new();
        source.extend_events(events);
        source
    }

    pub fn push_event(&self, event: HotkeyEvent) {
        self.inner
            .lock()
            .expect("hotkey mutex poisoned")
            .events
            .push_back(Ok(event));
    }

    pub fn push_error(&self, error: MacosAdapterError) {
        self.inner
            .lock()
            .expect("hotkey mutex poisoned")
            .events
            .push_back(Err(error));
    }

    pub fn extend_events(&self, events: impl IntoIterator<Item = HotkeyEvent>) {
        let mut inner = self.inner.lock().expect("hotkey mutex poisoned");
        for event in events {
            inner.events.push_back(Ok(event));
        }
    }

    #[must_use]
    pub fn pending_events(&self) -> usize {
        self.inner
            .lock()
            .expect("hotkey mutex poisoned")
            .events
            .len()
    }

    #[must_use]
    pub fn next_event_calls(&self) -> usize {
        self.inner
            .lock()
            .expect("hotkey mutex poisoned")
            .next_event_calls
    }
}

#[async_trait]
impl HotkeyEventSource for MockHotkeyEventSource {
    async fn next_event(&mut self) -> MacosAdapterResult<HotkeyEvent> {
        let mut inner = self.inner.lock().expect("hotkey mutex poisoned");
        inner.next_event_calls += 1;

        match inner.events.pop_front() {
            Some(result) => result,
            None => Err(MacosAdapterError::HotkeyEventStreamClosed),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MockAudioRecorder {
    inner: Arc<Mutex<AudioRecorderInner>>,
}

#[derive(Debug)]
struct AudioRecorderInner {
    active: bool,
    start_calls: usize,
    stop_calls: usize,
    cancel_calls: usize,
    start_error_queue: VecDeque<MacosAdapterError>,
    stop_results: VecDeque<MacosAdapterResult<RecordedAudio>>,
    cancel_error_queue: VecDeque<MacosAdapterError>,
    default_stop_result: MacosAdapterResult<RecordedAudio>,
}

impl Default for AudioRecorderInner {
    fn default() -> Self {
        Self {
            active: false,
            start_calls: 0,
            stop_calls: 0,
            cancel_calls: 0,
            start_error_queue: VecDeque::new(),
            stop_results: VecDeque::new(),
            cancel_error_queue: VecDeque::new(),
            default_stop_result: Ok(RecordedAudio::new("mock-recording.wav", 1_000)),
        }
    }
}

impl Default for MockAudioRecorder {
    fn default() -> Self {
        Self::new()
    }
}

impl MockAudioRecorder {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(AudioRecorderInner::default())),
        }
    }

    pub fn enqueue_start_error(&self, error: MacosAdapterError) {
        self.inner
            .lock()
            .expect("audio recorder mutex poisoned")
            .start_error_queue
            .push_back(error);
    }

    pub fn enqueue_stop_result(&self, result: MacosAdapterResult<RecordedAudio>) {
        self.inner
            .lock()
            .expect("audio recorder mutex poisoned")
            .stop_results
            .push_back(result);
    }

    pub fn enqueue_cancel_error(&self, error: MacosAdapterError) {
        self.inner
            .lock()
            .expect("audio recorder mutex poisoned")
            .cancel_error_queue
            .push_back(error);
    }

    pub fn set_default_stop_result(&self, result: MacosAdapterResult<RecordedAudio>) {
        self.inner
            .lock()
            .expect("audio recorder mutex poisoned")
            .default_stop_result = result;
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.inner
            .lock()
            .expect("audio recorder mutex poisoned")
            .active
    }

    #[must_use]
    pub fn start_calls(&self) -> usize {
        self.inner
            .lock()
            .expect("audio recorder mutex poisoned")
            .start_calls
    }

    #[must_use]
    pub fn stop_calls(&self) -> usize {
        self.inner
            .lock()
            .expect("audio recorder mutex poisoned")
            .stop_calls
    }

    #[must_use]
    pub fn cancel_calls(&self) -> usize {
        self.inner
            .lock()
            .expect("audio recorder mutex poisoned")
            .cancel_calls
    }
}

#[async_trait(?Send)]
impl AudioRecorder for MockAudioRecorder {
    async fn start_recording(&mut self) -> MacosAdapterResult<()> {
        let mut inner = self.inner.lock().expect("audio recorder mutex poisoned");
        inner.start_calls += 1;

        if inner.active {
            return Err(MacosAdapterError::RecorderAlreadyActive);
        }

        if let Some(error) = inner.start_error_queue.pop_front() {
            return Err(error);
        }

        inner.active = true;
        Ok(())
    }

    async fn stop_recording(&mut self) -> MacosAdapterResult<RecordedAudio> {
        let mut inner = self.inner.lock().expect("audio recorder mutex poisoned");
        inner.stop_calls += 1;

        if !inner.active {
            return Err(MacosAdapterError::RecorderNotActive);
        }

        inner.active = false;
        if let Some(result) = inner.stop_results.pop_front() {
            return result;
        }

        inner.default_stop_result.clone()
    }

    async fn cancel_recording(&mut self) -> MacosAdapterResult<()> {
        let mut inner = self.inner.lock().expect("audio recorder mutex poisoned");
        inner.cancel_calls += 1;

        if !inner.active {
            return Err(MacosAdapterError::RecorderNotActive);
        }

        if let Some(error) = inner.cancel_error_queue.pop_front() {
            return Err(error);
        }

        inner.active = false;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct MockTextInjector {
    inner: Arc<Mutex<TextInjectorInner>>,
}

#[derive(Debug, Default)]
struct TextInjectorInner {
    payloads: Vec<String>,
    inject_error_queue: VecDeque<MacosAdapterError>,
    inject_calls: usize,
}

impl Default for MockTextInjector {
    fn default() -> Self {
        Self::new()
    }
}

impl MockTextInjector {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(TextInjectorInner::default())),
        }
    }

    pub fn enqueue_inject_error(&self, error: MacosAdapterError) {
        self.inner
            .lock()
            .expect("text injector mutex poisoned")
            .inject_error_queue
            .push_back(error);
    }

    #[must_use]
    pub fn injected_text(&self) -> Vec<String> {
        self.inner
            .lock()
            .expect("text injector mutex poisoned")
            .payloads
            .clone()
    }

    #[must_use]
    pub fn inject_calls(&self) -> usize {
        self.inner
            .lock()
            .expect("text injector mutex poisoned")
            .inject_calls
    }
}

#[async_trait]
impl TextInjector for MockTextInjector {
    async fn inject_unicode_text(&self, text: &str) -> MacosAdapterResult<()> {
        let mut inner = self.inner.lock().expect("text injector mutex poisoned");
        inner.inject_calls += 1;

        if let Some(error) = inner.inject_error_queue.pop_front() {
            return Err(error);
        }

        inner.payloads.push(text.to_owned());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::Future,
        pin::Pin,
        sync::Arc,
        task::{Context, Poll, Wake, Waker},
    };

    use crate::{
        AudioRecorder, HotkeyAction, HotkeyEvent, HotkeyEventKind, HotkeyEventSource,
        IndicatorAdapter, IndicatorState, MacosAdapterError, PermissionPreflightStatus,
        PermissionStatus, PermissionsAdapter, RecordedAudio, TextInjector,
    };

    use super::{
        MockAudioRecorder, MockHotkeyEventSource, MockIndicatorAdapter, MockPermissionsAdapter,
        MockTextInjector,
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

    #[test]
    fn indicator_tracks_state_history_and_supports_error_injection() {
        let mut indicator = MockIndicatorAdapter::new();

        block_on(indicator.initialize()).expect("mock initialize should succeed");
        block_on(indicator.set_state(IndicatorState::Transcribing))
            .expect("set state should succeed");
        assert_eq!(
            block_on(indicator.state()).expect("state read should succeed"),
            IndicatorState::Transcribing
        );
        assert_eq!(indicator.initialize_calls(), 1);
        assert_eq!(
            indicator.state_history(),
            vec![IndicatorState::Transcribing]
        );

        indicator
            .enqueue_set_state_error(MacosAdapterError::operation_failed("indicator", "expected"));
        let err = block_on(indicator.set_state(IndicatorState::Output))
            .expect_err("queued state error should be returned");
        assert_eq!(
            err,
            MacosAdapterError::operation_failed("indicator", "expected")
        );
    }

    #[test]
    fn permissions_adapter_returns_configured_status_and_error() {
        let adapter = MockPermissionsAdapter::new();
        let status = PermissionPreflightStatus {
            microphone: PermissionStatus::Granted,
            accessibility: PermissionStatus::Denied,
            input_monitoring: PermissionStatus::Granted,
        };

        adapter.set_preflight_status(status);
        assert_eq!(
            block_on(adapter.preflight()).expect("preflight status should be returned"),
            status
        );
        assert_eq!(adapter.preflight_calls(), 1);

        adapter.set_preflight_error(MacosAdapterError::operation_failed("permissions", "boom"));
        let err = block_on(adapter.preflight())
            .expect_err("configured permissions error should be returned");
        assert_eq!(
            err,
            MacosAdapterError::operation_failed("permissions", "boom")
        );
        assert_eq!(adapter.preflight_calls(), 2);

        let requested_status = PermissionPreflightStatus {
            microphone: PermissionStatus::Granted,
            accessibility: PermissionStatus::Denied,
            input_monitoring: PermissionStatus::Granted,
        };
        adapter.set_request_input_monitoring_result(true);
        adapter.set_post_request_preflight_status(requested_status);
        assert!(block_on(adapter.request_input_monitoring_access())
            .expect("request result should be returned"));
        assert_eq!(adapter.request_input_monitoring_calls(), 1);
        assert_eq!(
            block_on(adapter.preflight()).expect("post-request status should be returned"),
            requested_status
        );

        let accessibility_status = PermissionPreflightStatus {
            microphone: PermissionStatus::Granted,
            accessibility: PermissionStatus::Granted,
            input_monitoring: PermissionStatus::Granted,
        };
        adapter.set_request_accessibility_result(true);
        adapter.set_post_request_preflight_status(accessibility_status);
        assert!(block_on(adapter.request_accessibility_access())
            .expect("accessibility request result should be returned"));
        assert_eq!(adapter.request_accessibility_calls(), 1);
        assert_eq!(
            block_on(adapter.preflight())
                .expect("post-request accessibility status should be returned"),
            accessibility_status
        );
    }

    #[test]
    fn hotkey_source_drains_queue_then_closes() {
        let mut source = MockHotkeyEventSource::new();
        let press = HotkeyEvent::new(HotkeyAction::PushToTalk, HotkeyEventKind::Pressed);
        let release = HotkeyEvent::new(HotkeyAction::PushToTalk, HotkeyEventKind::Released);

        source.push_event(press);
        source.push_event(release);

        assert_eq!(
            block_on(source.next_event()).expect("first event should be available"),
            press
        );
        assert_eq!(
            block_on(source.next_event()).expect("second event should be available"),
            release
        );

        let err =
            block_on(source.next_event()).expect_err("empty queue should close deterministically");
        assert_eq!(err, MacosAdapterError::HotkeyEventStreamClosed);
        assert_eq!(source.next_event_calls(), 3);
    }

    #[test]
    fn audio_recorder_enforces_state_machine_and_custom_stop_outputs() {
        let mut recorder = MockAudioRecorder::new();
        recorder.enqueue_stop_result(Ok(RecordedAudio::new("first.wav", 111)));
        recorder.enqueue_stop_result(Ok(RecordedAudio::new("second.wav", 222)));

        assert_eq!(
            block_on(recorder.stop_recording()).expect_err("cannot stop before start"),
            MacosAdapterError::RecorderNotActive
        );

        block_on(recorder.start_recording()).expect("start should succeed");
        let first =
            block_on(recorder.stop_recording()).expect("first stop result should come from queue");
        assert_eq!(first, RecordedAudio::new("first.wav", 111));

        block_on(recorder.start_recording()).expect("second start should succeed");
        let second =
            block_on(recorder.stop_recording()).expect("second stop result should come from queue");
        assert_eq!(second, RecordedAudio::new("second.wav", 222));

        recorder.set_default_stop_result(Ok(RecordedAudio::new("default.wav", 333)));
        block_on(recorder.start_recording()).expect("third start should succeed");
        let default = block_on(recorder.stop_recording())
            .expect("default stop result should be returned once queue is empty");
        assert_eq!(default, RecordedAudio::new("default.wav", 333));

        assert_eq!(recorder.start_calls(), 3);
        assert_eq!(recorder.stop_calls(), 4);
        assert!(!recorder.is_active());
    }

    #[test]
    fn audio_recorder_cancel_requires_active_session() {
        let mut recorder = MockAudioRecorder::new();
        assert_eq!(
            block_on(recorder.cancel_recording()).expect_err("cancel before start should fail"),
            MacosAdapterError::RecorderNotActive
        );

        block_on(recorder.start_recording()).expect("start should succeed");
        recorder.enqueue_cancel_error(MacosAdapterError::operation_failed("cancel", "transient"));
        assert_eq!(
            block_on(recorder.cancel_recording())
                .expect_err("queued cancel error should be returned"),
            MacosAdapterError::operation_failed("cancel", "transient")
        );
        assert!(recorder.is_active());

        block_on(recorder.cancel_recording()).expect("second cancel should clear active session");
        assert!(!recorder.is_active());
        assert_eq!(recorder.cancel_calls(), 3);
    }

    #[test]
    fn text_injector_captures_payloads_and_supports_checked_path() {
        let injector = MockTextInjector::new();

        block_on(injector.inject_checked("hello"))
            .expect("inject_checked should forward non-empty payloads");
        block_on(injector.inject_unicode_text("world")).expect("direct inject should succeed");
        assert_eq!(
            injector.injected_text(),
            vec!["hello".to_string(), "world".to_string()]
        );
        assert_eq!(injector.inject_calls(), 2);

        let err = block_on(injector.inject_checked(""))
            .expect_err("inject_checked should reject empty payloads before delegating");
        assert_eq!(err, MacosAdapterError::EmptyInjectionText);
        assert_eq!(injector.inject_calls(), 2);
    }
}

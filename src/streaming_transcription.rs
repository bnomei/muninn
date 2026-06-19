use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::warn;

use crate::{
    append_transcription_attempt, AudioFrame, MuninnEnvelopeV1, ResolvedUtteranceConfig,
    TranscriptionAttempt, TranscriptionAttemptOutcome, TranscriptionProvider,
};

pub mod deepgram;
pub mod google;
pub mod openai;

const STREAMING_FRAME_QUEUE_CAPACITY: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub struct StreamingTranscriptOutcome {
    pub provider: TranscriptionProvider,
    pub raw_text: Option<String>,
    pub attempt: TranscriptionAttempt,
    pub errors: Vec<Value>,
}

impl StreamingTranscriptOutcome {
    #[must_use]
    pub fn produced(provider: TranscriptionProvider, raw_text: impl Into<String>) -> Self {
        Self {
            provider,
            raw_text: Some(raw_text.into()),
            attempt: TranscriptionAttempt::new(
                provider,
                TranscriptionAttemptOutcome::ProducedTranscript,
                "produced_transcript",
                "streaming transcription produced transcript text",
            ),
            errors: Vec::new(),
        }
    }

    #[must_use]
    pub fn empty(provider: TranscriptionProvider, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        Self {
            provider,
            raw_text: None,
            attempt: TranscriptionAttempt::new(
                provider,
                TranscriptionAttemptOutcome::EmptyTranscript,
                "empty_transcript_text",
                detail.clone(),
            ),
            errors: vec![streaming_error_value(
                provider,
                TranscriptionAttemptOutcome::EmptyTranscript,
                "empty_transcript_text",
                &detail,
            )],
        }
    }

    #[must_use]
    pub fn from_error(error: StreamingTranscriptionError) -> Self {
        let provider = error.provider();
        Self {
            provider,
            raw_text: None,
            attempt: error.to_attempt(),
            errors: vec![error.to_error_value()],
        }
    }

    pub fn apply_to_envelope(self, envelope: &mut MuninnEnvelopeV1) {
        envelope.transcript.provider = Some(self.provider.to_string());
        if let Some(raw_text) = self
            .raw_text
            .map(|raw_text| raw_text.trim().to_string())
            .filter(|raw_text| !raw_text.is_empty())
        {
            envelope.transcript.raw_text = Some(raw_text);
        }
        append_transcription_attempt(envelope, self.attempt);
        envelope.errors.extend(self.errors);
    }

    fn has_final_text(&self) -> bool {
        self.raw_text
            .as_deref()
            .map(str::trim)
            .is_some_and(|raw_text| !raw_text.is_empty())
    }
}

#[derive(Debug, Clone, Error, PartialEq)]
pub enum StreamingTranscriptionError {
    #[error("{provider} streaming transcription unavailable: {detail}")]
    Unavailable {
        provider: TranscriptionProvider,
        outcome: TranscriptionAttemptOutcome,
        code: String,
        detail: String,
    },
    #[error("{provider} streaming transcription failed: {detail}")]
    Failed {
        provider: TranscriptionProvider,
        code: String,
        detail: String,
    },
    #[error("{provider} streaming transcription was cancelled")]
    Cancelled { provider: TranscriptionProvider },
}

impl StreamingTranscriptionError {
    #[must_use]
    pub fn unavailable_runtime_capability(
        provider: TranscriptionProvider,
        code: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self::Unavailable {
            provider,
            outcome: TranscriptionAttemptOutcome::UnavailableRuntimeCapability,
            code: code.into(),
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn unavailable_credentials(
        provider: TranscriptionProvider,
        code: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self::Unavailable {
            provider,
            outcome: TranscriptionAttemptOutcome::UnavailableCredentials,
            code: code.into(),
            detail: detail.into(),
        }
    }

    #[must_use]
    pub fn failed(
        provider: TranscriptionProvider,
        code: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self::Failed {
            provider,
            code: code.into(),
            detail: detail.into(),
        }
    }

    #[must_use]
    pub const fn cancelled(provider: TranscriptionProvider) -> Self {
        Self::Cancelled { provider }
    }

    #[must_use]
    pub const fn provider(&self) -> TranscriptionProvider {
        match self {
            Self::Unavailable { provider, .. }
            | Self::Failed { provider, .. }
            | Self::Cancelled { provider } => *provider,
        }
    }

    #[must_use]
    pub fn to_attempt(&self) -> TranscriptionAttempt {
        match self {
            Self::Unavailable {
                provider,
                outcome,
                code,
                detail,
            } => TranscriptionAttempt::new(*provider, *outcome, code.clone(), detail.clone()),
            Self::Failed {
                provider,
                code,
                detail,
            } => TranscriptionAttempt::new(
                *provider,
                TranscriptionAttemptOutcome::RequestFailed,
                code.clone(),
                detail.clone(),
            ),
            Self::Cancelled { provider } => TranscriptionAttempt::new(
                *provider,
                TranscriptionAttemptOutcome::RequestFailed,
                "streaming_cancelled",
                "streaming transcription was cancelled",
            ),
        }
    }

    #[must_use]
    pub fn to_error_value(&self) -> Value {
        match self {
            Self::Unavailable {
                provider,
                outcome,
                code,
                detail,
            } => streaming_error_value(*provider, *outcome, code, detail),
            Self::Failed {
                provider,
                code,
                detail,
            } => streaming_error_value(
                *provider,
                TranscriptionAttemptOutcome::RequestFailed,
                code,
                detail,
            ),
            Self::Cancelled { provider } => streaming_error_value(
                *provider,
                TranscriptionAttemptOutcome::RequestFailed,
                "streaming_cancelled",
                "streaming transcription was cancelled",
            ),
        }
    }
}

#[async_trait]
pub trait StreamingTranscriptionSession: Send {
    async fn send_audio(&mut self, frame: AudioFrame) -> Result<(), StreamingTranscriptionError>;
    async fn finish(
        self: Box<Self>,
    ) -> Result<StreamingTranscriptOutcome, StreamingTranscriptionError>;
    async fn cancel(self: Box<Self>);
}

#[async_trait]
pub trait StreamingTranscriptionProvider: Send + Sync {
    async fn start(
        &self,
        resolved: &ResolvedUtteranceConfig,
    ) -> Result<Box<dyn StreamingTranscriptionSession>, StreamingTranscriptionError>;
}

#[derive(Clone)]
pub struct StreamingTranscriptionProviderEntry {
    provider: TranscriptionProvider,
    implementation: Arc<dyn StreamingTranscriptionProvider>,
}

impl StreamingTranscriptionProviderEntry {
    #[must_use]
    pub fn new<P>(provider: TranscriptionProvider, implementation: P) -> Self
    where
        P: StreamingTranscriptionProvider + 'static,
    {
        Self {
            provider,
            implementation: Arc::new(implementation),
        }
    }

    #[must_use]
    pub const fn provider(&self) -> TranscriptionProvider {
        self.provider
    }
}

pub struct ActiveStreamingTranscription {
    controller: Option<StreamingTranscriptionController>,
    startup_outcome: Option<StreamingTranscriptOutcome>,
}

impl ActiveStreamingTranscription {
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            controller: None,
            startup_outcome: None,
        }
    }

    pub async fn start(resolved: &ResolvedUtteranceConfig) -> Self {
        Self::start_with_providers(resolved, default_provider_entries()).await
    }

    pub async fn start_with_providers(
        resolved: &ResolvedUtteranceConfig,
        providers: impl IntoIterator<Item = StreamingTranscriptionProviderEntry>,
    ) -> Self {
        let Some(provider) = resolved.streaming_transcription_route().first().copied() else {
            return Self::disabled();
        };

        let Some(entry) = providers
            .into_iter()
            .find(|entry| entry.provider == provider)
        else {
            return Self::from_start_error(
                StreamingTranscriptionError::unavailable_runtime_capability(
                    provider,
                    "streaming_provider_not_registered",
                    "streaming provider is not registered",
                ),
                resolved,
            );
        };

        match entry.implementation.start(resolved).await {
            Ok(session) => Self {
                controller: Some(StreamingTranscriptionController::new(
                    provider, session, resolved,
                )),
                startup_outcome: None,
            },
            Err(error) => Self::from_start_error(error, resolved),
        }
    }

    #[must_use]
    pub fn sink(&self) -> Option<mpsc::Sender<AudioFrame>> {
        self.controller
            .as_ref()
            .map(StreamingTranscriptionController::sink)
    }

    pub async fn finish(self) -> Option<StreamingTranscriptOutcome> {
        if let Some(controller) = self.controller {
            return controller.finish().await;
        }
        self.startup_outcome
    }

    pub async fn cancel(self) {
        if let Some(controller) = self.controller {
            controller.cancel().await;
        }
    }

    fn from_start_error(
        error: StreamingTranscriptionError,
        resolved: &ResolvedUtteranceConfig,
    ) -> Self {
        let fallback_to_recorded = resolved
            .effective_config
            .transcription
            .streaming
            .fallback_to_recorded_on_error;
        let is_cancelled = matches!(error, StreamingTranscriptionError::Cancelled { .. });
        let provider = error.provider();
        let detail = error.to_string();
        let outcome = StreamingTranscriptOutcome::from_error(error);

        if fallback_to_recorded || is_cancelled {
            warn!(
                provider = %provider,
                error = %detail,
                "streaming transcription unavailable; using recorded transcription route and preserving diagnostic"
            );
            Self {
                controller: None,
                startup_outcome: Some(outcome),
            }
        } else {
            Self {
                controller: None,
                startup_outcome: Some(outcome),
            }
        }
    }
}

struct StreamingTranscriptionController {
    provider: TranscriptionProvider,
    sink: Option<mpsc::Sender<AudioFrame>>,
    cancel_signal: Option<oneshot::Sender<()>>,
    worker: Option<JoinHandle<Result<StreamingTranscriptOutcome, StreamingTranscriptionError>>>,
    finish_timeout: Duration,
    fallback_to_recorded_on_error: bool,
}

impl StreamingTranscriptionController {
    fn new(
        provider: TranscriptionProvider,
        session: Box<dyn StreamingTranscriptionSession>,
        resolved: &ResolvedUtteranceConfig,
    ) -> Self {
        let (sink, frames) = mpsc::channel(STREAMING_FRAME_QUEUE_CAPACITY);
        let (cancel_signal, cancel_rx) = oneshot::channel();
        let worker = tokio::spawn(run_streaming_session(provider, session, frames, cancel_rx));
        let streaming = &resolved.effective_config.transcription.streaming;

        Self {
            provider,
            sink: Some(sink),
            cancel_signal: Some(cancel_signal),
            worker: Some(worker),
            finish_timeout: Duration::from_millis(streaming.finish_timeout_ms),
            fallback_to_recorded_on_error: streaming.fallback_to_recorded_on_error,
        }
    }

    fn sink(&self) -> mpsc::Sender<AudioFrame> {
        self.sink
            .as_ref()
            .expect("streaming controller sink should exist while active")
            .clone()
    }

    async fn finish(mut self) -> Option<StreamingTranscriptOutcome> {
        self.sink.take();
        let mut worker = self.worker.take()?;

        let result = tokio::select! {
            result = &mut worker => StreamingWorkerResult::Finished(result),
            _ = tokio::time::sleep(self.finish_timeout) => {
                worker.abort();
                StreamingWorkerResult::TimedOut
            }
        };

        match result {
            StreamingWorkerResult::Finished(Ok(Ok(outcome))) if outcome.has_final_text() => {
                Some(outcome)
            }
            StreamingWorkerResult::Finished(Ok(Ok(outcome)))
                if self.fallback_to_recorded_on_error =>
            {
                warn!(
                    provider = %outcome.provider,
                    code = %outcome.attempt.code,
                    "streaming transcription produced no final text; using recorded transcription route and preserving diagnostic"
                );
                Some(outcome)
            }
            StreamingWorkerResult::Finished(Ok(Ok(outcome))) => Some(outcome),
            StreamingWorkerResult::Finished(Ok(Err(error)))
                if self.fallback_to_recorded_on_error =>
            {
                let outcome = StreamingTranscriptOutcome::from_error(error);
                warn!(
                    provider = %outcome.provider,
                    code = %outcome.attempt.code,
                    "streaming transcription failed; using recorded transcription route and preserving diagnostic"
                );
                Some(outcome)
            }
            StreamingWorkerResult::Finished(Ok(Err(error))) => {
                Some(StreamingTranscriptOutcome::from_error(error))
            }
            StreamingWorkerResult::Finished(Err(error)) if self.fallback_to_recorded_on_error => {
                let detail = error.to_string();
                let outcome = streaming_task_failed_outcome(self.provider, error);
                warn!(
                    provider = %self.provider,
                    error = %detail,
                    "streaming transcription task failed; using recorded transcription route and preserving diagnostic"
                );
                Some(outcome)
            }
            StreamingWorkerResult::Finished(Err(error)) => {
                Some(streaming_task_failed_outcome(self.provider, error))
            }
            StreamingWorkerResult::TimedOut if self.fallback_to_recorded_on_error => {
                let outcome = streaming_finish_timeout_outcome(self.provider, self.finish_timeout);
                warn!(
                    provider = %self.provider,
                    timeout_ms = self.finish_timeout.as_millis(),
                    "streaming transcription finish timed out; using recorded transcription route and preserving diagnostic"
                );
                Some(outcome)
            }
            StreamingWorkerResult::TimedOut => Some(streaming_finish_timeout_outcome(
                self.provider,
                self.finish_timeout,
            )),
        }
    }

    async fn cancel(mut self) {
        if let Some(cancel_signal) = self.cancel_signal.take() {
            let _ = cancel_signal.send(());
        }
        let Some(mut worker) = self.worker.take() else {
            return;
        };

        tokio::select! {
            _ = &mut worker => {}
            _ = tokio::time::sleep(self.finish_timeout) => {
                worker.abort();
            }
        }
        self.sink.take();
    }
}

enum StreamingWorkerResult {
    Finished(
        Result<
            Result<StreamingTranscriptOutcome, StreamingTranscriptionError>,
            tokio::task::JoinError,
        >,
    ),
    TimedOut,
}

fn streaming_task_failed_outcome(
    provider: TranscriptionProvider,
    error: tokio::task::JoinError,
) -> StreamingTranscriptOutcome {
    StreamingTranscriptOutcome::from_error(StreamingTranscriptionError::failed(
        provider,
        "streaming_session_task_failed",
        format!("streaming session task failed: {error}"),
    ))
}

fn streaming_finish_timeout_outcome(
    provider: TranscriptionProvider,
    finish_timeout: Duration,
) -> StreamingTranscriptOutcome {
    StreamingTranscriptOutcome::from_error(StreamingTranscriptionError::failed(
        provider,
        "streaming_finish_timeout",
        format!(
            "streaming transcription did not finish within {} ms",
            finish_timeout.as_millis()
        ),
    ))
}

async fn run_streaming_session(
    provider: TranscriptionProvider,
    mut session: Box<dyn StreamingTranscriptionSession>,
    mut frames: mpsc::Receiver<AudioFrame>,
    mut cancel_rx: oneshot::Receiver<()>,
) -> Result<StreamingTranscriptOutcome, StreamingTranscriptionError> {
    loop {
        tokio::select! {
            biased;

            _ = &mut cancel_rx => {
                session.cancel().await;
                return Err(StreamingTranscriptionError::cancelled(provider));
            }
            maybe_frame = frames.recv() => {
                match maybe_frame {
                    Some(frame) => session.send_audio(frame).await?,
                    None => return session.finish().await,
                }
            }
        }
    }
}

fn default_provider_entries() -> Vec<StreamingTranscriptionProviderEntry> {
    vec![
        StreamingTranscriptionProviderEntry::new(
            TranscriptionProvider::Deepgram,
            deepgram::DeepgramStreamingTranscriptionProvider,
        ),
        StreamingTranscriptionProviderEntry::new(
            TranscriptionProvider::OpenAi,
            openai::OpenAiStreamingTranscriptionProvider,
        ),
        StreamingTranscriptionProviderEntry::new(
            TranscriptionProvider::Google,
            google::GoogleStreamingTranscriptionProvider,
        ),
    ]
}

fn streaming_error_value(
    provider: TranscriptionProvider,
    outcome: TranscriptionAttemptOutcome,
    code: &str,
    message: &str,
) -> Value {
    json!({
        "source": "streaming_transcription",
        "provider": provider,
        "code": code,
        "message": message,
        "transcription_outcome": outcome,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{config::PipelineStepConfig, AppConfig, OnErrorPolicy, PayloadFormat};

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
        async fn send_audio(
            &mut self,
            _frame: AudioFrame,
        ) -> Result<(), StreamingTranscriptionError> {
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

    #[tokio::test(flavor = "current_thread")]
    async fn streaming_success_forwards_frames_and_returns_outcome() {
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
        let sink = active.sink().expect("streaming should expose frame sink");
        sink.send(AudioFrame {
            samples: vec![1, 2, 3],
            sample_rate_hz: 16_000,
            channels: 1,
        })
        .await
        .expect("frame should enqueue");
        drop(sink);

        let outcome = active.finish().await.expect("streaming outcome expected");

        assert_eq!(outcome.raw_text.as_deref(), Some("hello from streaming"));
        assert_eq!(outcome.provider, TranscriptionProvider::OpenAi);
        assert_eq!(state.lock().expect("fake state poisoned").start_calls, 1);
        assert_eq!(state.lock().expect("fake state poisoned").frame_count, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn streaming_failure_falls_back_when_enabled() {
        let provider = FakeStreamingProvider {
            state: Arc::new(Mutex::new(FakeStreamingState::default())),
            start_error: None,
            finish_result: Err(StreamingTranscriptionError::failed(
                TranscriptionProvider::Deepgram,
                "stream_closed",
                "connection closed",
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

        let outcome = active
            .finish()
            .await
            .expect("streaming error diagnostic should be preserved");

        assert!(outcome.raw_text.is_none());
        assert_eq!(outcome.provider, TranscriptionProvider::Deepgram);
        assert_eq!(outcome.attempt.code, "stream_closed");
        assert_eq!(
            outcome.attempt.outcome,
            TranscriptionAttemptOutcome::RequestFailed
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn streaming_failure_without_fallback_preserves_attempt() {
        let provider = FakeStreamingProvider {
            state: Arc::new(Mutex::new(FakeStreamingState::default())),
            start_error: Some(StreamingTranscriptionError::failed(
                TranscriptionProvider::Google,
                "stream_failed",
                "stream failed before capture",
            )),
            finish_result: Ok(StreamingTranscriptOutcome::produced(
                TranscriptionProvider::Google,
                "unused",
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
        let outcome = active
            .finish()
            .await
            .expect("failure outcome should be preserved");

        assert_eq!(outcome.provider, TranscriptionProvider::Google);
        assert_eq!(
            outcome.attempt.outcome,
            TranscriptionAttemptOutcome::RequestFailed
        );
        assert!(outcome.raw_text.is_none());
        assert_eq!(outcome.errors.len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancel_stops_session_without_outcome() {
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

        active.cancel().await;

        assert_eq!(state.lock().expect("fake state poisoned").cancel_calls, 1);
    }

    #[test]
    fn outcome_seeds_envelope_before_pipeline() {
        let mut envelope =
            MuninnEnvelopeV1::new("utt-1", "2026-06-16T00:00:00Z").with_audio(None, 100);

        StreamingTranscriptOutcome::produced(TranscriptionProvider::Deepgram, " streamed words ")
            .apply_to_envelope(&mut envelope);

        assert_eq!(envelope.transcript.provider.as_deref(), Some("deepgram"));
        assert_eq!(
            envelope.transcript.raw_text.as_deref(),
            Some("streamed words")
        );
        assert_eq!(crate::transcription_attempts(&envelope).len(), 1);
        assert!(envelope.errors.is_empty());
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
            .resolve_effective_config(crate::TargetContextSnapshot::default())
    }

    fn _pipeline_step_for_compile_check() -> PipelineStepConfig {
        PipelineStepConfig {
            id: "refine".to_string(),
            cmd: "refine".to_string(),
            args: Vec::new(),
            io_mode: crate::config::StepIoMode::Auto,
            timeout_ms: 100,
            on_error: OnErrorPolicy::Continue,
        }
    }

    fn _payload_format_for_compile_check() -> PayloadFormat {
        PayloadFormat::JsonObject
    }
}

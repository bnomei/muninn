//! Google Cloud Speech v2 live transcription adapter.
//!
//! Builds `StreamingRecognize` requests with explicit linear16 decoding and
//! chunks audio at 15 KiB. The official `google-cloud-speech-v2` crate currently
//! lacks a callable streaming RPC, so connect reports
//! `google_official_client_streaming_rpc_unavailable` after credentials resolve.
//! Implements [`StreamingTranscriptionProvider`] for [`TranscriptionProvider::Google`].

use std::marker::PhantomData;

use async_trait::async_trait;
use google_cloud_speech_v2::model::{
    explicit_decoding_config::AudioEncoding, ExplicitDecodingConfig, RecognitionConfig,
    StreamingRecognitionConfig, StreamingRecognitionFeatures, StreamingRecognizeRequest,
    StreamingRecognizeResponse,
};

use crate::{
    resolve_secret_from_env, AudioFrame, ResolvedUtteranceConfig, StreamingTranscriptOutcome,
    StreamingTranscriptionError, StreamingTranscriptionProvider, StreamingTranscriptionSession,
    TranscriptionProvider,
};

const PROVIDER: TranscriptionProvider = TranscriptionProvider::Google;
const MAX_GOOGLE_STREAMING_AUDIO_BYTES: usize = 15 * 1024;
const REQUIRED_CHANNELS_MAX: u16 = 8;
const MISSING_TOKEN_CODE: &str = "missing_google_streaming_token";
const API_KEY_ONLY_CODE: &str = "google_streaming_api_key_unsupported";
const MISSING_PROJECT_ID_CODE: &str = "missing_google_streaming_project_id";
const OFFICIAL_STREAMING_RPC_UNAVAILABLE_CODE: &str =
    "google_official_client_streaming_rpc_unavailable";

/// Google Speech v2 [`StreamingTranscriptionProvider`] (RPC stub until crate support lands).
#[derive(Debug, Clone, Copy, Default)]
pub struct GoogleStreamingTranscriptionProvider;

#[async_trait]
impl StreamingTranscriptionProvider for GoogleStreamingTranscriptionProvider {
    async fn start(
        &self,
        resolved: &ResolvedUtteranceConfig,
    ) -> Result<Box<dyn StreamingTranscriptionSession>, StreamingTranscriptionError> {
        let request = GoogleStreamingRequest::from_resolved(resolved)?;
        let client = OfficialGoogleSpeechStreamingClient::connect(&request).await?;
        Ok(Box::new(GoogleStreamingSession::new(client, request)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GoogleStreamingRequest {
    token: String,
    recognizer: String,
    model: Option<String>,
    language_codes: Vec<String>,
    interim_results: bool,
    default_sample_rate_hz: u32,
    default_channels: u16,
}

impl GoogleStreamingRequest {
    fn from_resolved(
        resolved: &ResolvedUtteranceConfig,
    ) -> Result<Self, StreamingTranscriptionError> {
        let config = &resolved.effective_config.providers.google;
        let token = resolve_secret_from_env("GOOGLE_STT_TOKEN", config.token.clone());
        let api_key = resolve_secret_from_env("GOOGLE_API_KEY", config.api_key.clone());

        let Some(token) = token else {
            if api_key.is_some() {
                return Err(StreamingTranscriptionError::unavailable_credentials(
                    PROVIDER,
                    API_KEY_ONLY_CODE,
                    "Google streaming transcription requires token or ADC-style credentials; API-key-only credentials remain available for the recorded REST fallback",
                ));
            }

            return Err(StreamingTranscriptionError::unavailable_credentials(
                PROVIDER,
                MISSING_TOKEN_CODE,
                "missing Google streaming credentials; set GOOGLE_STT_TOKEN or providers.google.token",
            ));
        };

        let recognizer = recognizer_name(
            config.streaming_project_id.as_deref(),
            &config.streaming_location,
            &config.streaming_recognizer,
        )?;

        Ok(Self {
            token,
            recognizer,
            model: config
                .streaming_model
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            language_codes: config.streaming_language_codes.clone(),
            interim_results: config.streaming_interim_results,
            default_sample_rate_hz: resolved.effective_config.recording.sample_rate_hz(),
            default_channels: if resolved.effective_config.recording.mono {
                1
            } else {
                2
            },
        })
    }

    fn initial_request(&self, sample_rate_hz: u32, channels: u16) -> StreamingRecognizeRequest {
        StreamingRecognizeRequest::new()
            .set_recognizer(self.recognizer.clone())
            .set_streaming_config(self.streaming_config(sample_rate_hz, channels))
    }

    fn streaming_config(&self, sample_rate_hz: u32, channels: u16) -> StreamingRecognitionConfig {
        StreamingRecognitionConfig::new()
            .set_config(self.recognition_config(sample_rate_hz, channels))
            .set_streaming_features(
                StreamingRecognitionFeatures::new().set_interim_results(self.interim_results),
            )
    }

    fn recognition_config(&self, sample_rate_hz: u32, channels: u16) -> RecognitionConfig {
        let mut config = RecognitionConfig::new()
            .set_language_codes(self.language_codes.clone())
            .set_explicit_decoding_config(
                ExplicitDecodingConfig::new()
                    .set_encoding(AudioEncoding::Linear16)
                    .set_sample_rate_hertz(sample_rate_hz as i32)
                    .set_audio_channel_count(channels as i32),
            );

        if let Some(model) = self.model.as_deref() {
            config = config.set_model(model);
        }

        config
    }
}

fn recognizer_name(
    project_id: Option<&str>,
    location: &str,
    recognizer: &str,
) -> Result<String, StreamingTranscriptionError> {
    let recognizer = recognizer.trim();
    if recognizer.starts_with("projects/") {
        return Ok(recognizer.to_string());
    }

    let project_id = project_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            StreamingTranscriptionError::unavailable_credentials(
                PROVIDER,
                MISSING_PROJECT_ID_CODE,
                "Google streaming transcription requires providers.google.streaming_project_id unless streaming_recognizer is fully qualified",
            )
        })?;

    Ok(format!(
        "projects/{project_id}/locations/{}/recognizers/{recognizer}",
        location.trim()
    ))
}

struct GoogleStreamingSession<C> {
    client: C,
    request: GoogleStreamingRequest,
    configured_audio: Option<(u32, u16)>,
    transcript: GoogleTranscriptAccumulator,
}

impl<C> GoogleStreamingSession<C> {
    fn new(client: C, request: GoogleStreamingRequest) -> Self {
        Self {
            client,
            request,
            configured_audio: None,
            transcript: GoogleTranscriptAccumulator::default(),
        }
    }
}

impl<C> GoogleStreamingSession<C>
where
    C: GoogleStreamingClient,
{
    async fn ensure_config_sent(
        &mut self,
        sample_rate_hz: u32,
        channels: u16,
    ) -> Result<(), StreamingTranscriptionError> {
        if self.configured_audio.is_some() {
            return Ok(());
        }

        self.client
            .send(self.request.initial_request(sample_rate_hz, channels))
            .await?;
        self.configured_audio = Some((sample_rate_hz, channels));
        Ok(())
    }
}

#[async_trait]
impl<C> StreamingTranscriptionSession for GoogleStreamingSession<C>
where
    C: GoogleStreamingClient + 'static,
{
    async fn send_audio(&mut self, frame: AudioFrame) -> Result<(), StreamingTranscriptionError> {
        if frame.channels == 0 || frame.channels > REQUIRED_CHANNELS_MAX {
            return Err(StreamingTranscriptionError::failed(
                PROVIDER,
                "google_streaming_invalid_channel_count",
                format!(
                    "Google streaming supports 1-{REQUIRED_CHANNELS_MAX} channels but received {}",
                    frame.channels
                ),
            ));
        }

        if let Some((sample_rate_hz, channels)) = self.configured_audio {
            if frame.sample_rate_hz != sample_rate_hz {
                return Err(StreamingTranscriptionError::failed(
                    PROVIDER,
                    "google_streaming_sample_rate_mismatch",
                    format!(
                        "Google streaming session expected {sample_rate_hz} Hz audio but received {} Hz",
                        frame.sample_rate_hz
                    ),
                ));
            }

            if frame.channels != channels {
                return Err(StreamingTranscriptionError::failed(
                    PROVIDER,
                    "google_streaming_channel_mismatch",
                    format!(
                        "Google streaming session expected {channels} audio channels but received {}",
                        frame.channels
                    ),
                ));
            }
        }

        if frame.samples.is_empty() {
            return Ok(());
        }

        self.ensure_config_sent(frame.sample_rate_hz, frame.channels)
            .await?;

        for chunk in linear16_audio_chunks(&frame.samples) {
            self.client
                .send(StreamingRecognizeRequest::new().set_audio(chunk))
                .await?;
        }

        Ok(())
    }

    async fn finish(
        mut self: Box<Self>,
    ) -> Result<StreamingTranscriptOutcome, StreamingTranscriptionError> {
        self.ensure_config_sent(
            self.request.default_sample_rate_hz,
            self.request.default_channels,
        )
        .await?;
        for response in self.client.finish().await? {
            self.transcript.handle_response(&response);
        }
        Ok(self.transcript.into_outcome())
    }

    async fn cancel(mut self: Box<Self>) {
        self.client.cancel().await;
    }
}

fn linear16_audio_chunks(samples: &[i16]) -> Vec<Vec<u8>> {
    let mut chunks = Vec::new();
    let mut current = Vec::with_capacity(MAX_GOOGLE_STREAMING_AUDIO_BYTES);

    for sample in samples {
        if current.len() + 2 > MAX_GOOGLE_STREAMING_AUDIO_BYTES {
            chunks.push(current);
            current = Vec::with_capacity(MAX_GOOGLE_STREAMING_AUDIO_BYTES);
        }
        current.extend_from_slice(&sample.to_le_bytes());
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

#[async_trait]
trait GoogleStreamingClient: Send {
    async fn send(
        &mut self,
        request: StreamingRecognizeRequest,
    ) -> Result<(), StreamingTranscriptionError>;

    async fn finish(
        &mut self,
    ) -> Result<Vec<StreamingRecognizeResponse>, StreamingTranscriptionError>;

    async fn cancel(&mut self);
}

struct OfficialGoogleSpeechStreamingClient {
    _official_client: PhantomData<google_cloud_speech_v2::client::Speech>,
}

impl OfficialGoogleSpeechStreamingClient {
    async fn connect(
        request: &GoogleStreamingRequest,
    ) -> Result<Self, StreamingTranscriptionError> {
        let _token = request.token.as_str();

        Err(StreamingTranscriptionError::unavailable_runtime_capability(
            PROVIDER,
            OFFICIAL_STREAMING_RPC_UNAVAILABLE_CODE,
            "google-cloud-speech-v2 1.12.0 exposes StreamingRecognize message types but no callable StreamingRecognize client method yet",
        ))
    }
}

#[async_trait]
impl GoogleStreamingClient for OfficialGoogleSpeechStreamingClient {
    async fn send(
        &mut self,
        _request: StreamingRecognizeRequest,
    ) -> Result<(), StreamingTranscriptionError> {
        Err(StreamingTranscriptionError::unavailable_runtime_capability(
            PROVIDER,
            OFFICIAL_STREAMING_RPC_UNAVAILABLE_CODE,
            "google-cloud-speech-v2 1.12.0 does not expose a callable StreamingRecognize client method",
        ))
    }

    async fn finish(
        &mut self,
    ) -> Result<Vec<StreamingRecognizeResponse>, StreamingTranscriptionError> {
        Err(StreamingTranscriptionError::unavailable_runtime_capability(
            PROVIDER,
            OFFICIAL_STREAMING_RPC_UNAVAILABLE_CODE,
            "google-cloud-speech-v2 1.12.0 does not expose a callable StreamingRecognize client method",
        ))
    }

    async fn cancel(&mut self) {}
}

#[derive(Debug, Default)]
struct GoogleTranscriptAccumulator {
    final_segments: Vec<String>,
}

impl GoogleTranscriptAccumulator {
    fn handle_response(&mut self, response: &StreamingRecognizeResponse) {
        for result in &response.results {
            if !result.is_final {
                continue;
            }

            if let Some(transcript) = result
                .alternatives
                .first()
                .map(|alternative| alternative.transcript.trim())
                .filter(|transcript| !transcript.is_empty())
            {
                self.final_segments.push(transcript.to_string());
            }
        }
    }

    fn into_outcome(self) -> StreamingTranscriptOutcome {
        let raw_text = self.final_segments.join(" ");
        let raw_text = raw_text.trim();
        if raw_text.is_empty() {
            StreamingTranscriptOutcome::empty(
                PROVIDER,
                "Google streaming transcription returned an empty transcript",
            )
        } else {
            StreamingTranscriptOutcome::produced(PROVIDER, raw_text)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{AppConfig, TargetContextSnapshot, TranscriptionAttemptOutcome};
    use google_cloud_speech_v2::model::{SpeechRecognitionAlternative, StreamingRecognitionResult};

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(previous) => std::env::set_var(self.key, previous),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn builds_recognizer_name_from_project_location_and_recognizer() {
        let name = recognizer_name(Some("project-123"), "eu", "muninn")
            .expect("recognizer name should build");

        assert_eq!(name, "projects/project-123/locations/eu/recognizers/muninn");
    }

    #[test]
    fn accepts_fully_qualified_recognizer_without_project_id() {
        let name = recognizer_name(
            None,
            "ignored",
            "projects/project-123/locations/global/recognizers/_",
        )
        .expect("fully qualified recognizer should be accepted");

        assert_eq!(name, "projects/project-123/locations/global/recognizers/_");
    }

    #[test]
    fn api_key_only_credentials_are_unavailable_for_streaming() {
        let _token_guard = EnvVarGuard::remove("GOOGLE_STT_TOKEN");
        let _api_key_guard = EnvVarGuard::remove("GOOGLE_API_KEY");
        let resolved = resolved_config(
            r#"
[transcription]
mode = "streaming"
providers = ["google"]

[providers.google]
api_key = "config-api-key"
streaming_project_id = "project-123"
"#,
        );

        let error = GoogleStreamingRequest::from_resolved(&resolved)
            .expect_err("API-key-only auth should not start streaming");

        match error {
            StreamingTranscriptionError::Unavailable {
                provider,
                outcome,
                code,
                detail,
            } => {
                assert_eq!(provider, PROVIDER);
                assert_eq!(outcome, TranscriptionAttemptOutcome::UnavailableCredentials);
                assert_eq!(code, API_KEY_ONLY_CODE);
                assert!(detail.contains("REST fallback"));
            }
            other => panic!("expected unavailable credentials, got {other:?}"),
        }
    }

    #[test]
    fn missing_token_credentials_are_unavailable_for_streaming() {
        let _token_guard = EnvVarGuard::remove("GOOGLE_STT_TOKEN");
        let _api_key_guard = EnvVarGuard::remove("GOOGLE_API_KEY");
        let resolved = resolved_config(
            r#"
[transcription]
mode = "streaming"
providers = ["google"]

[providers.google]
streaming_project_id = "project-123"
"#,
        );

        let error = GoogleStreamingRequest::from_resolved(&resolved)
            .expect_err("missing token auth should not start streaming");

        assert_eq!(error.to_attempt().code, MISSING_TOKEN_CODE);
        assert_eq!(
            error.to_attempt().outcome,
            TranscriptionAttemptOutcome::UnavailableCredentials
        );
    }

    #[test]
    fn request_uses_google_streaming_config_fields() {
        let request = google_request();
        let initial = request.initial_request(16_000, 1);
        let streaming_config = initial
            .streaming_config()
            .expect("initial request should carry streaming config");
        let recognition = streaming_config
            .config
            .as_ref()
            .expect("streaming config should include recognition config");
        let decoding = recognition
            .explicit_decoding_config()
            .expect("recognition config should use explicit PCM decoding");

        assert_eq!(initial.recognizer, request.recognizer);
        assert!(initial.audio().is_none());
        assert_eq!(recognition.model, "latest_short");
        assert_eq!(recognition.language_codes, vec!["en-US", "ga-IE"]);
        assert_eq!(decoding.encoding, AudioEncoding::Linear16);
        assert_eq!(decoding.sample_rate_hertz, 16_000);
        assert_eq!(decoding.audio_channel_count, 1);
        assert!(
            !streaming_config
                .streaming_features
                .as_ref()
                .expect("streaming features should be set")
                .interim_results
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_sends_config_first_then_audio_only_chunks_at_15kb() {
        let state = Arc::new(Mutex::new(FakeClientState::default()));
        let client = FakeGoogleStreamingClient::new(Arc::clone(&state), []);
        let mut session = GoogleStreamingSession::new(client, google_request());

        session
            .send_audio(AudioFrame {
                samples: vec![0x1234; 8_000],
                sample_rate_hz: 16_000,
                channels: 1,
            })
            .await
            .expect("audio should send");

        let requests = state.lock().expect("fake state poisoned").requests.clone();

        assert_eq!(requests.len(), 3);
        assert!(requests[0].streaming_config().is_some());
        assert!(requests[0].audio().is_none());
        assert_eq!(
            requests[0].recognizer,
            "projects/project-123/locations/eu/recognizers/_"
        );
        for request in requests.iter().skip(1) {
            assert!(request.recognizer.is_empty());
            assert!(request.streaming_config().is_none());
            assert!(request.audio().is_some());
            assert!(
                request.audio().expect("audio request").len() <= MAX_GOOGLE_STREAMING_AUDIO_BYTES
            );
        }
        assert_eq!(
            requests[1].audio().expect("first audio chunk").len(),
            MAX_GOOGLE_STREAMING_AUDIO_BYTES
        );
        assert_eq!(requests[2].audio().expect("second audio chunk").len(), 640);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finish_sends_config_even_without_audio_and_extracts_only_final_results() {
        let state = Arc::new(Mutex::new(FakeClientState::default()));
        let client = FakeGoogleStreamingClient::new(
            Arc::clone(&state),
            [
                streaming_response(false, "interim text"),
                streaming_response(true, "final text"),
            ],
        );

        let outcome = Box::new(GoogleStreamingSession::new(client, google_request()))
            .finish()
            .await
            .expect("session should finish");

        assert_eq!(outcome.raw_text.as_deref(), Some("final text"));
        assert_eq!(
            outcome.attempt.outcome,
            TranscriptionAttemptOutcome::ProducedTranscript
        );
        let requests = state.lock().expect("fake state poisoned").requests.clone();
        assert_eq!(requests.len(), 1);
        assert!(requests[0].streaming_config().is_some());
        assert!(requests[0].audio().is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn final_empty_results_return_empty_transcript_outcome() {
        let state = Arc::new(Mutex::new(FakeClientState::default()));
        let client =
            FakeGoogleStreamingClient::new(Arc::clone(&state), [streaming_response(true, "   ")]);

        let outcome = Box::new(GoogleStreamingSession::new(client, google_request()))
            .finish()
            .await
            .expect("session should finish");

        assert!(outcome.raw_text.is_none());
        assert_eq!(
            outcome.attempt.outcome,
            TranscriptionAttemptOutcome::EmptyTranscript
        );
        assert_eq!(outcome.errors.len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn provider_reports_official_streaming_rpc_unavailable_after_credentials_resolve() {
        let _token_guard = EnvVarGuard::remove("GOOGLE_STT_TOKEN");
        let _api_key_guard = EnvVarGuard::remove("GOOGLE_API_KEY");
        let resolved = resolved_config(
            r#"
[transcription]
mode = "streaming"
providers = ["google"]

[providers.google]
token = "config-token"
streaming_project_id = "project-123"
"#,
        );

        let error = GoogleStreamingTranscriptionProvider
            .start(&resolved)
            .await
            .err()
            .expect("official crate currently has no streaming RPC method");

        assert_eq!(
            error.to_attempt().code,
            OFFICIAL_STREAMING_RPC_UNAVAILABLE_CODE
        );
        assert_eq!(
            error.to_attempt().outcome,
            TranscriptionAttemptOutcome::UnavailableRuntimeCapability
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_cancel_forwards_to_client() {
        let state = Arc::new(Mutex::new(FakeClientState::default()));
        let client = FakeGoogleStreamingClient::new(Arc::clone(&state), []);

        Box::new(GoogleStreamingSession::new(client, google_request()))
            .cancel()
            .await;

        assert_eq!(state.lock().expect("fake state poisoned").cancel_calls, 1);
    }

    fn google_request() -> GoogleStreamingRequest {
        GoogleStreamingRequest {
            token: "token".to_string(),
            recognizer: "projects/project-123/locations/eu/recognizers/_".to_string(),
            model: Some("latest_short".to_string()),
            language_codes: vec!["en-US".to_string(), "ga-IE".to_string()],
            interim_results: false,
            default_sample_rate_hz: 16_000,
            default_channels: 1,
        }
    }

    fn streaming_response(is_final: bool, transcript: &str) -> StreamingRecognizeResponse {
        StreamingRecognizeResponse::new().set_results([StreamingRecognitionResult::new()
            .set_is_final(is_final)
            .set_alternatives([SpeechRecognitionAlternative::new().set_transcript(transcript)])])
    }

    #[derive(Default)]
    struct FakeClientState {
        requests: Vec<StreamingRecognizeRequest>,
        cancel_calls: usize,
    }

    struct FakeGoogleStreamingClient {
        state: Arc<Mutex<FakeClientState>>,
        responses: VecDeque<StreamingRecognizeResponse>,
    }

    impl FakeGoogleStreamingClient {
        fn new(
            state: Arc<Mutex<FakeClientState>>,
            responses: impl IntoIterator<Item = StreamingRecognizeResponse>,
        ) -> Self {
            Self {
                state,
                responses: responses.into_iter().collect(),
            }
        }
    }

    #[async_trait]
    impl GoogleStreamingClient for FakeGoogleStreamingClient {
        async fn send(
            &mut self,
            request: StreamingRecognizeRequest,
        ) -> Result<(), StreamingTranscriptionError> {
            self.state
                .lock()
                .expect("fake state poisoned")
                .requests
                .push(request);
            Ok(())
        }

        async fn finish(
            &mut self,
        ) -> Result<Vec<StreamingRecognizeResponse>, StreamingTranscriptionError> {
            Ok(self.responses.drain(..).collect())
        }

        async fn cancel(&mut self) {
            self.state.lock().expect("fake state poisoned").cancel_calls += 1;
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
            .resolve_effective_config(TargetContextSnapshot::default())
    }
}

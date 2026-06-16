use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use serde_json::Value;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{header::AUTHORIZATION, HeaderValue},
        Message,
    },
    MaybeTlsStream, WebSocketStream,
};

use crate::{
    resolve_secret_from_env, AudioFrame, ResolvedUtteranceConfig, StreamingTranscriptOutcome,
    StreamingTranscriptionError, StreamingTranscriptionProvider, StreamingTranscriptionSession,
    TranscriptionProvider,
};

const PROVIDER: TranscriptionProvider = TranscriptionProvider::Deepgram;
const MISSING_API_KEY_CODE: &str = "missing_deepgram_api_key";
const CLOSE_STREAM_CONTROL: &str = r#"{"type":"CloseStream"}"#;

#[derive(Debug, Clone, Copy, Default)]
pub struct DeepgramStreamingTranscriptionProvider;

#[async_trait]
impl StreamingTranscriptionProvider for DeepgramStreamingTranscriptionProvider {
    async fn start(
        &self,
        resolved: &ResolvedUtteranceConfig,
    ) -> Result<Box<dyn StreamingTranscriptionSession>, StreamingTranscriptionError> {
        let request = DeepgramStreamingRequest::from_resolved(resolved)?;
        let socket = connect_deepgram_websocket(&request).await?;
        Ok(Box::new(DeepgramStreamingSession::new(
            socket,
            request.sample_rate_hz,
        )))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DeepgramStreamingRequest {
    api_key: String,
    endpoint: String,
    model: String,
    language: String,
    sample_rate_hz: u32,
    interim_results: bool,
}

impl DeepgramStreamingRequest {
    fn from_resolved(
        resolved: &ResolvedUtteranceConfig,
    ) -> Result<Self, StreamingTranscriptionError> {
        let config = &resolved.effective_config.providers.deepgram;
        let Some(api_key) = resolve_secret_from_env("DEEPGRAM_API_KEY", config.api_key.clone())
        else {
            return Err(StreamingTranscriptionError::unavailable_credentials(
                PROVIDER,
                MISSING_API_KEY_CODE,
                "missing Deepgram API key; set DEEPGRAM_API_KEY or provide providers.deepgram.api_key in config",
            ));
        };

        Ok(Self {
            api_key,
            endpoint: config.streaming_endpoint.clone(),
            model: config.model.clone(),
            language: config.language.clone(),
            sample_rate_hz: resolved.effective_config.recording.sample_rate_hz(),
            interim_results: config.streaming_interim_results,
        })
    }

    fn handshake_url(&self) -> Result<Url, StreamingTranscriptionError> {
        let mut url = Url::parse(&self.endpoint).map_err(|source| {
            StreamingTranscriptionError::failed(
                PROVIDER,
                "invalid_deepgram_streaming_endpoint",
                format!(
                    "invalid Deepgram streaming endpoint {}: {source}",
                    self.endpoint
                ),
            )
        })?;

        match url.scheme() {
            "wss" | "ws" => {}
            scheme => {
                return Err(StreamingTranscriptionError::failed(
                    PROVIDER,
                    "invalid_deepgram_streaming_endpoint",
                    format!("Deepgram streaming endpoint must use ws or wss scheme, got {scheme}"),
                ));
            }
        }

        url.query_pairs_mut()
            .append_pair("model", self.model.trim())
            .append_pair("language", self.language.trim())
            .append_pair("encoding", "linear16")
            .append_pair("sample_rate", &self.sample_rate_hz.to_string())
            .append_pair(
                "interim_results",
                if self.interim_results {
                    "true"
                } else {
                    "false"
                },
            );

        Ok(url)
    }
}

async fn connect_deepgram_websocket(
    request: &DeepgramStreamingRequest,
) -> Result<TungsteniteDeepgramWebSocket, StreamingTranscriptionError> {
    let url = request.handshake_url()?;

    let mut websocket_request = url.as_str().into_client_request().map_err(|source| {
        StreamingTranscriptionError::failed(
            PROVIDER,
            "invalid_deepgram_streaming_request",
            format!("failed to build Deepgram streaming WebSocket request for {url}: {source}"),
        )
    })?;
    let authorization =
        HeaderValue::from_str(&format!("Token {}", request.api_key)).map_err(|source| {
            StreamingTranscriptionError::failed(
                PROVIDER,
                "invalid_deepgram_streaming_authorization",
                format!("failed to build Deepgram streaming authorization header: {source}"),
            )
        })?;
    websocket_request
        .headers_mut()
        .insert(AUTHORIZATION, authorization);

    let (socket, _response) = connect_async(websocket_request).await.map_err(|source| {
        StreamingTranscriptionError::failed(
            PROVIDER,
            "deepgram_streaming_connect_failed",
            format!("failed to connect to Deepgram streaming endpoint {url}: {source}"),
        )
    })?;

    Ok(TungsteniteDeepgramWebSocket::new(socket))
}

struct DeepgramStreamingSession<S> {
    socket: S,
    expected_sample_rate_hz: u32,
    transcript: DeepgramTranscriptAccumulator,
}

impl<S> DeepgramStreamingSession<S> {
    fn new(socket: S, expected_sample_rate_hz: u32) -> Self {
        Self {
            socket,
            expected_sample_rate_hz,
            transcript: DeepgramTranscriptAccumulator::default(),
        }
    }
}

#[async_trait]
impl<S> StreamingTranscriptionSession for DeepgramStreamingSession<S>
where
    S: DeepgramWebSocket + 'static,
{
    async fn send_audio(&mut self, frame: AudioFrame) -> Result<(), StreamingTranscriptionError> {
        if frame.sample_rate_hz != self.expected_sample_rate_hz {
            return Err(StreamingTranscriptionError::failed(
                PROVIDER,
                "deepgram_streaming_sample_rate_mismatch",
                format!(
                    "Deepgram streaming session expected {} Hz audio but received {} Hz",
                    self.expected_sample_rate_hz, frame.sample_rate_hz
                ),
            ));
        }

        if frame.samples.is_empty() {
            return Ok(());
        }

        self.socket
            .send(Message::Binary(linear16_bytes(&frame.samples)))
            .await
    }

    async fn finish(
        mut self: Box<Self>,
    ) -> Result<StreamingTranscriptOutcome, StreamingTranscriptionError> {
        self.socket
            .send(Message::Text(CLOSE_STREAM_CONTROL.to_string()))
            .await?;

        while let Some(message) = self.socket.next().await? {
            match message {
                Message::Text(text) => {
                    self.transcript.handle_text_message(&text)?;
                }
                Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
                Message::Ping(payload) => {
                    self.socket.send(Message::Pong(payload)).await?;
                }
                Message::Close(_) => break,
            }
        }

        Ok(self.transcript.into_outcome())
    }

    async fn cancel(mut self: Box<Self>) {
        let _ = self
            .socket
            .send(Message::Text(CLOSE_STREAM_CONTROL.to_string()))
            .await;
        let _ = self.socket.close().await;
    }
}

fn linear16_bytes(samples: &[i16]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

#[async_trait]
trait DeepgramWebSocket: Send {
    async fn send(&mut self, message: Message) -> Result<(), StreamingTranscriptionError>;
    async fn close(&mut self) -> Result<(), StreamingTranscriptionError>;
    async fn next(&mut self) -> Result<Option<Message>, StreamingTranscriptionError>;
}

struct TungsteniteDeepgramWebSocket {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl TungsteniteDeepgramWebSocket {
    fn new(socket: WebSocketStream<MaybeTlsStream<TcpStream>>) -> Self {
        Self { socket }
    }
}

#[async_trait]
impl DeepgramWebSocket for TungsteniteDeepgramWebSocket {
    async fn send(&mut self, message: Message) -> Result<(), StreamingTranscriptionError> {
        self.socket
            .send(message)
            .await
            .map_err(map_websocket_write_error)
    }

    async fn close(&mut self) -> Result<(), StreamingTranscriptionError> {
        self.socket
            .close(None)
            .await
            .map_err(map_websocket_write_error)
    }

    async fn next(&mut self) -> Result<Option<Message>, StreamingTranscriptionError> {
        self.socket
            .next()
            .await
            .transpose()
            .map_err(map_websocket_read_error)
    }
}

fn map_websocket_write_error(
    source: tokio_tungstenite::tungstenite::Error,
) -> StreamingTranscriptionError {
    StreamingTranscriptionError::failed(
        PROVIDER,
        "deepgram_streaming_write_failed",
        format!("failed to write Deepgram streaming WebSocket frame: {source}"),
    )
}

fn map_websocket_read_error(
    source: tokio_tungstenite::tungstenite::Error,
) -> StreamingTranscriptionError {
    StreamingTranscriptionError::failed(
        PROVIDER,
        "deepgram_streaming_read_failed",
        format!("failed to read Deepgram streaming WebSocket frame: {source}"),
    )
}

#[derive(Debug, Default)]
struct DeepgramTranscriptAccumulator {
    final_segments: Vec<String>,
    speech_final_seen: bool,
}

impl DeepgramTranscriptAccumulator {
    fn handle_text_message(&mut self, raw: &str) -> Result<(), StreamingTranscriptionError> {
        let value: Value = serde_json::from_str(raw).map_err(|source| {
            StreamingTranscriptionError::failed(
                PROVIDER,
                "invalid_deepgram_streaming_message",
                format!("failed to parse Deepgram streaming message JSON: {source}"),
            )
        })?;

        match value.get("type").and_then(Value::as_str) {
            Some("Results") => {
                self.handle_results(&value);
                Ok(())
            }
            Some("Error") => Err(deepgram_provider_error(&value)),
            Some(_) => Ok(()),
            None => Err(StreamingTranscriptionError::failed(
                PROVIDER,
                "invalid_deepgram_streaming_message",
                format!("Deepgram streaming message did not include a string type field: {value}"),
            )),
        }
    }

    fn handle_results(&mut self, value: &Value) {
        if value
            .get("speech_final")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            self.speech_final_seen = true;
        }

        if !value
            .get("is_final")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return;
        }

        for transcript in result_transcripts(value) {
            let transcript = transcript.trim();
            if !transcript.is_empty() {
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
                "Deepgram streaming transcription returned an empty transcript",
            )
        } else {
            StreamingTranscriptOutcome::produced(PROVIDER, raw_text)
        }
    }
}

fn result_transcripts(value: &Value) -> Vec<&str> {
    let mut transcripts = Vec::new();

    if let Some(transcript) = value
        .get("channel")
        .and_then(|channel| channel.get("alternatives"))
        .and_then(Value::as_array)
        .and_then(|alternatives| alternatives.first())
        .and_then(|alternative| alternative.get("transcript"))
        .and_then(Value::as_str)
    {
        transcripts.push(transcript);
    }

    if let Some(channels) = value
        .get("results")
        .and_then(|results| results.get("channels"))
        .and_then(Value::as_array)
    {
        for channel in channels {
            if let Some(transcript) = channel
                .get("alternatives")
                .and_then(Value::as_array)
                .and_then(|alternatives| alternatives.first())
                .and_then(|alternative| alternative.get("transcript"))
                .and_then(Value::as_str)
            {
                transcripts.push(transcript);
            }
        }
    }

    transcripts
}

fn deepgram_provider_error(value: &Value) -> StreamingTranscriptionError {
    let code = value
        .get("err_code")
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)
        .unwrap_or("deepgram_streaming_provider_error");
    let detail = value
        .get("description")
        .or_else(|| value.get("message"))
        .or_else(|| value.get("reason"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .unwrap_or_else(|| value.to_string());

    StreamingTranscriptionError::failed(PROVIDER, code, detail)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::{AppConfig, TargetContextSnapshot, TranscriptionAttemptOutcome};

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
    fn builds_websocket_handshake_url_from_config() {
        let request = DeepgramStreamingRequest {
            api_key: "secret".to_string(),
            endpoint: "wss://deepgram.example.test/v1/listen?tag=muninn".to_string(),
            model: "nova-3".to_string(),
            language: "en-IE".to_string(),
            sample_rate_hz: 16_000,
            interim_results: false,
        };

        let url = request
            .handshake_url()
            .expect("Deepgram streaming URL should build");

        assert_eq!(url.scheme(), "wss");
        assert_eq!(url.host_str(), Some("deepgram.example.test"));
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            vec![
                ("tag".into(), "muninn".into()),
                ("model".into(), "nova-3".into()),
                ("language".into(), "en-IE".into()),
                ("encoding".into(), "linear16".into()),
                ("sample_rate".into(), "16000".into()),
                ("interim_results".into(), "false".into()),
            ]
        );
    }

    #[test]
    fn missing_credentials_map_to_unavailable_credentials() {
        let _guard = EnvVarGuard::remove("DEEPGRAM_API_KEY");
        let resolved = resolved_config(
            r#"
[transcription]
mode = "streaming"
providers = ["deepgram"]
"#,
        );

        let error = DeepgramStreamingRequest::from_resolved(&resolved)
            .expect_err("missing credentials should fail before connecting");

        match error {
            StreamingTranscriptionError::Unavailable {
                provider,
                outcome,
                code,
                detail,
            } => {
                assert_eq!(provider, PROVIDER);
                assert_eq!(outcome, TranscriptionAttemptOutcome::UnavailableCredentials);
                assert_eq!(code, MISSING_API_KEY_CODE);
                assert!(detail.contains("DEEPGRAM_API_KEY"));
            }
            other => panic!("expected unavailable credentials, got {other:?}"),
        }
    }

    #[test]
    fn parser_keeps_interim_results_transient() {
        let mut accumulator = DeepgramTranscriptAccumulator::default();
        accumulator
            .handle_text_message(
                r#"{"type":"Results","is_final":false,"speech_final":false,"channel":{"alternatives":[{"transcript":"partial words"}]}}"#,
            )
            .expect("interim result should parse");

        let outcome = accumulator.into_outcome();

        assert!(outcome.raw_text.is_none());
        assert_eq!(
            outcome.attempt.outcome,
            TranscriptionAttemptOutcome::EmptyTranscript
        );
    }

    #[test]
    fn parser_accumulates_final_results() {
        let mut accumulator = DeepgramTranscriptAccumulator::default();
        accumulator
            .handle_text_message(
                r#"{"type":"Results","is_final":true,"speech_final":false,"channel":{"alternatives":[{"transcript":"hello"}]}}"#,
            )
            .expect("first final result should parse");
        accumulator
            .handle_text_message(
                r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"world"}]}}"#,
            )
            .expect("second final result should parse");

        let outcome = accumulator.into_outcome();

        assert_eq!(outcome.raw_text.as_deref(), Some("hello world"));
        assert_eq!(
            outcome.attempt.outcome,
            TranscriptionAttemptOutcome::ProducedTranscript
        );
    }

    #[test]
    fn parser_treats_speech_final_as_boundary_not_visible_partial() {
        let mut accumulator = DeepgramTranscriptAccumulator::default();
        accumulator
            .handle_text_message(
                r#"{"type":"Results","is_final":false,"speech_final":true,"channel":{"alternatives":[{"transcript":"boundary partial"}]}}"#,
            )
            .expect("speech-final interim result should parse");

        assert!(accumulator.speech_final_seen);
        let outcome = accumulator.into_outcome();

        assert!(outcome.raw_text.is_none());
    }

    #[test]
    fn parser_returns_empty_outcome_for_final_empty_transcript() {
        let mut accumulator = DeepgramTranscriptAccumulator::default();
        accumulator
            .handle_text_message(
                r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"   "}]} }"#,
            )
            .expect("empty final result should parse");

        let outcome = accumulator.into_outcome();

        assert!(outcome.raw_text.is_none());
        assert_eq!(
            outcome.attempt.outcome,
            TranscriptionAttemptOutcome::EmptyTranscript
        );
        assert_eq!(outcome.errors.len(), 1);
    }

    #[test]
    fn parser_rejects_malformed_messages() {
        let mut accumulator = DeepgramTranscriptAccumulator::default();

        let error = accumulator
            .handle_text_message("{not json")
            .expect_err("malformed JSON should fail");

        assert_eq!(error.provider(), PROVIDER);
        assert!(error.to_attempt().code.contains("invalid_deepgram"));
    }

    #[test]
    fn parser_rejects_messages_without_type() {
        let mut accumulator = DeepgramTranscriptAccumulator::default();

        let error = accumulator
            .handle_text_message(r#"{"is_final":true}"#)
            .expect_err("missing type should fail");

        assert_eq!(
            error.to_attempt().code,
            "invalid_deepgram_streaming_message"
        );
    }

    #[test]
    fn parser_maps_provider_errors() {
        let mut accumulator = DeepgramTranscriptAccumulator::default();

        let error = accumulator
            .handle_text_message(
                r#"{"type":"Error","err_code":"INVALID_AUTH","description":"bad token"}"#,
            )
            .expect_err("provider errors should fail");

        assert_eq!(error.to_attempt().code, "INVALID_AUTH");
        assert!(error.to_attempt().detail.contains("bad token"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_sends_audio_frames_and_finalizes() {
        let state = Arc::new(Mutex::new(FakeWebSocketState::default()));
        let socket = FakeDeepgramWebSocket::new(
            Arc::clone(&state),
            [
                Message::Text(
                    r#"{"type":"Results","is_final":true,"speech_final":true,"channel":{"alternatives":[{"transcript":"streamed text"}]}}"#
                        .to_string(),
                ),
                Message::Close(None),
            ],
        );
        let mut session = DeepgramStreamingSession::new(socket, 16_000);

        session
            .send_audio(AudioFrame {
                samples: vec![0x1234, -2],
                sample_rate_hz: 16_000,
                channels: 1,
            })
            .await
            .expect("audio should send");

        let outcome = Box::new(session)
            .finish()
            .await
            .expect("session should finalize");

        assert_eq!(outcome.raw_text.as_deref(), Some("streamed text"));
        let state = state.lock().expect("fake state poisoned");
        assert_eq!(state.binary_payloads, vec![vec![0x34, 0x12, 0xFE, 0xFF]]);
        assert_eq!(state.text_payloads, vec![CLOSE_STREAM_CONTROL]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_replies_to_ping_while_finishing() {
        let state = Arc::new(Mutex::new(FakeWebSocketState::default()));
        let socket = FakeDeepgramWebSocket::new(
            Arc::clone(&state),
            [Message::Ping(vec![1, 2, 3]), Message::Close(None)],
        );

        let _ = Box::new(DeepgramStreamingSession::new(socket, 16_000))
            .finish()
            .await
            .expect("session should finalize after ping");

        assert_eq!(
            state.lock().expect("fake state poisoned").pong_payloads,
            vec![vec![1, 2, 3]]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_rejects_sample_rate_mismatch() {
        let state = Arc::new(Mutex::new(FakeWebSocketState::default()));
        let socket = FakeDeepgramWebSocket::new(Arc::clone(&state), []);
        let mut session = DeepgramStreamingSession::new(socket, 16_000);

        let error = session
            .send_audio(AudioFrame {
                samples: vec![1],
                sample_rate_hz: 48_000,
                channels: 1,
            })
            .await
            .expect_err("sample-rate mismatch should fail");

        assert_eq!(
            error.to_attempt().code,
            "deepgram_streaming_sample_rate_mismatch"
        );
        assert!(state
            .lock()
            .expect("fake state poisoned")
            .binary_payloads
            .is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_cancel_sends_close_controls() {
        let state = Arc::new(Mutex::new(FakeWebSocketState::default()));
        let socket = FakeDeepgramWebSocket::new(Arc::clone(&state), []);

        Box::new(DeepgramStreamingSession::new(socket, 16_000))
            .cancel()
            .await;

        let state = state.lock().expect("fake state poisoned");
        assert_eq!(state.text_payloads, vec![CLOSE_STREAM_CONTROL]);
        assert_eq!(state.close_calls, 1);
    }

    #[derive(Default)]
    struct FakeWebSocketState {
        binary_payloads: Vec<Vec<u8>>,
        text_payloads: Vec<String>,
        pong_payloads: Vec<Vec<u8>>,
        close_calls: usize,
    }

    struct FakeDeepgramWebSocket {
        state: Arc<Mutex<FakeWebSocketState>>,
        incoming: VecDeque<Message>,
    }

    impl FakeDeepgramWebSocket {
        fn new(
            state: Arc<Mutex<FakeWebSocketState>>,
            incoming: impl IntoIterator<Item = Message>,
        ) -> Self {
            Self {
                state,
                incoming: incoming.into_iter().collect(),
            }
        }
    }

    #[async_trait]
    impl DeepgramWebSocket for FakeDeepgramWebSocket {
        async fn send(&mut self, message: Message) -> Result<(), StreamingTranscriptionError> {
            let mut state = self.state.lock().expect("fake state poisoned");
            match message {
                Message::Binary(payload) => state.binary_payloads.push(payload),
                Message::Text(payload) => state.text_payloads.push(payload),
                Message::Pong(payload) => state.pong_payloads.push(payload),
                _ => {}
            }
            Ok(())
        }

        async fn close(&mut self) -> Result<(), StreamingTranscriptionError> {
            self.state.lock().expect("fake state poisoned").close_calls += 1;
            Ok(())
        }

        async fn next(&mut self) -> Result<Option<Message>, StreamingTranscriptionError> {
            Ok(self.incoming.pop_front())
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

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use serde_json::{json, Value};
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

const PROVIDER: TranscriptionProvider = TranscriptionProvider::OpenAi;
const MISSING_API_KEY_CODE: &str = "missing_openai_api_key";
const REQUIRED_SAMPLE_RATE_HZ: u32 = 24_000;
const REQUIRED_CHANNELS: u16 = 1;

#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAiStreamingTranscriptionProvider;

#[async_trait]
impl StreamingTranscriptionProvider for OpenAiStreamingTranscriptionProvider {
    async fn start(
        &self,
        resolved: &ResolvedUtteranceConfig,
    ) -> Result<Box<dyn StreamingTranscriptionSession>, StreamingTranscriptionError> {
        let request = OpenAiRealtimeTranscriptionRequest::from_resolved(resolved)?;
        let socket = connect_openai_websocket(&request).await?;
        let mut session = OpenAiStreamingSession::new(socket, request);
        session.configure().await?;
        Ok(Box::new(session))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenAiRealtimeTranscriptionRequest {
    api_key: String,
    endpoint: String,
    model: String,
    language: Option<String>,
    delay: String,
}

impl OpenAiRealtimeTranscriptionRequest {
    fn from_resolved(
        resolved: &ResolvedUtteranceConfig,
    ) -> Result<Self, StreamingTranscriptionError> {
        let config = &resolved.effective_config.providers.openai;
        let Some(api_key) = resolve_secret_from_env("OPENAI_API_KEY", config.api_key.clone())
        else {
            return Err(StreamingTranscriptionError::unavailable_credentials(
                PROVIDER,
                MISSING_API_KEY_CODE,
                "missing OpenAI API key; set OPENAI_API_KEY or provide providers.openai.api_key in config",
            ));
        };

        Ok(Self {
            api_key,
            endpoint: config.realtime_endpoint.clone(),
            model: config.realtime_model.clone(),
            language: config
                .realtime_language
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string),
            delay: config.realtime_delay.trim().to_string(),
        })
    }

    fn handshake_url(&self) -> Result<Url, StreamingTranscriptionError> {
        let mut url = Url::parse(&self.endpoint).map_err(|source| {
            StreamingTranscriptionError::failed(
                PROVIDER,
                "invalid_openai_realtime_endpoint",
                format!(
                    "invalid OpenAI Realtime endpoint {}: {source}",
                    self.endpoint
                ),
            )
        })?;

        match url.scheme() {
            "wss" | "ws" => {}
            scheme => {
                return Err(StreamingTranscriptionError::failed(
                    PROVIDER,
                    "invalid_openai_realtime_endpoint",
                    format!("OpenAI Realtime endpoint must use ws or wss scheme, got {scheme}"),
                ));
            }
        }

        if !url.query_pairs().any(|(key, _)| key == "model") {
            url.query_pairs_mut()
                .append_pair("model", self.model.trim());
        }

        Ok(url)
    }

    fn session_update_event(&self) -> Value {
        let mut transcription = json!({
            "model": self.model,
            "delay": self.delay,
        });
        if let Some(language) = self.language.as_deref() {
            transcription["language"] = json!(language);
        }

        json!({
            "type": "session.update",
            "session": {
                "type": "transcription",
                "audio": {
                    "input": {
                        "format": {
                            "type": "audio/pcm",
                            "rate": REQUIRED_SAMPLE_RATE_HZ,
                        },
                        "transcription": transcription,
                        "turn_detection": null,
                    },
                },
            },
        })
    }
}

async fn connect_openai_websocket(
    request: &OpenAiRealtimeTranscriptionRequest,
) -> Result<TungsteniteOpenAiWebSocket, StreamingTranscriptionError> {
    let url = request.handshake_url()?;

    let mut websocket_request = url.as_str().into_client_request().map_err(|source| {
        StreamingTranscriptionError::failed(
            PROVIDER,
            "invalid_openai_realtime_request",
            format!("failed to build OpenAI Realtime WebSocket request for {url}: {source}"),
        )
    })?;
    let authorization =
        HeaderValue::from_str(&format!("Bearer {}", request.api_key)).map_err(|source| {
            StreamingTranscriptionError::failed(
                PROVIDER,
                "invalid_openai_realtime_authorization",
                format!("failed to build OpenAI Realtime authorization header: {source}"),
            )
        })?;
    websocket_request
        .headers_mut()
        .insert(AUTHORIZATION, authorization);

    let (socket, _response) = connect_async(websocket_request).await.map_err(|source| {
        StreamingTranscriptionError::failed(
            PROVIDER,
            "openai_realtime_connect_failed",
            format!("failed to connect to OpenAI Realtime endpoint {url}: {source}"),
        )
    })?;

    Ok(TungsteniteOpenAiWebSocket::new(socket))
}

struct OpenAiStreamingSession<S> {
    socket: S,
    request: OpenAiRealtimeTranscriptionRequest,
    transcript: OpenAiTranscriptAccumulator,
}

impl<S> OpenAiStreamingSession<S> {
    fn new(socket: S, request: OpenAiRealtimeTranscriptionRequest) -> Self {
        Self {
            socket,
            request,
            transcript: OpenAiTranscriptAccumulator::default(),
        }
    }
}

impl<S> OpenAiStreamingSession<S>
where
    S: OpenAiWebSocket,
{
    async fn configure(&mut self) -> Result<(), StreamingTranscriptionError> {
        self.socket
            .send(Message::Text(
                self.request.session_update_event().to_string(),
            ))
            .await
    }
}

#[async_trait]
impl<S> StreamingTranscriptionSession for OpenAiStreamingSession<S>
where
    S: OpenAiWebSocket + 'static,
{
    async fn send_audio(&mut self, frame: AudioFrame) -> Result<(), StreamingTranscriptionError> {
        if frame.sample_rate_hz != REQUIRED_SAMPLE_RATE_HZ {
            return Err(StreamingTranscriptionError::failed(
                PROVIDER,
                "openai_realtime_sample_rate_mismatch",
                format!(
                    "OpenAI Realtime transcription requires {REQUIRED_SAMPLE_RATE_HZ} Hz audio but received {} Hz",
                    frame.sample_rate_hz
                ),
            ));
        }
        if frame.channels != REQUIRED_CHANNELS {
            return Err(StreamingTranscriptionError::failed(
                PROVIDER,
                "openai_realtime_channel_mismatch",
                format!(
                    "OpenAI Realtime transcription requires mono audio but received {} channels",
                    frame.channels
                ),
            ));
        }
        if frame.samples.is_empty() {
            return Ok(());
        }

        self.socket
            .send(Message::Text(
                json!({
                    "type": "input_audio_buffer.append",
                    "audio": base64_pcm16(&frame.samples),
                })
                .to_string(),
            ))
            .await
    }

    async fn finish(
        mut self: Box<Self>,
    ) -> Result<StreamingTranscriptOutcome, StreamingTranscriptionError> {
        self.socket
            .send(Message::Text(
                json!({
                    "type": "input_audio_buffer.commit",
                })
                .to_string(),
            ))
            .await?;

        while let Some(message) = self.socket.next().await? {
            match message {
                Message::Text(text) => {
                    let completed_before = self.transcript.completed_count();
                    self.transcript.handle_text_message(&text)?;
                    if self.transcript.completed_count() > completed_before {
                        break;
                    }
                }
                Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
                Message::Ping(payload) => {
                    self.socket.send(Message::Pong(payload)).await?;
                }
                Message::Close(_) => break,
            }
        }

        let _ = self.socket.close().await;
        Ok(self.transcript.into_outcome())
    }

    async fn cancel(mut self: Box<Self>) {
        let _ = self.socket.close().await;
    }
}

fn base64_pcm16(samples: &[i16]) -> String {
    let mut bytes = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    BASE64_STANDARD.encode(bytes)
}

#[async_trait]
trait OpenAiWebSocket: Send {
    async fn send(&mut self, message: Message) -> Result<(), StreamingTranscriptionError>;
    async fn close(&mut self) -> Result<(), StreamingTranscriptionError>;
    async fn next(&mut self) -> Result<Option<Message>, StreamingTranscriptionError>;
}

struct TungsteniteOpenAiWebSocket {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl TungsteniteOpenAiWebSocket {
    fn new(socket: WebSocketStream<MaybeTlsStream<TcpStream>>) -> Self {
        Self { socket }
    }
}

#[async_trait]
impl OpenAiWebSocket for TungsteniteOpenAiWebSocket {
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
        "openai_realtime_write_failed",
        format!("failed to write OpenAI Realtime WebSocket frame: {source}"),
    )
}

fn map_websocket_read_error(
    source: tokio_tungstenite::tungstenite::Error,
) -> StreamingTranscriptionError {
    StreamingTranscriptionError::failed(
        PROVIDER,
        "openai_realtime_read_failed",
        format!("failed to read OpenAI Realtime WebSocket frame: {source}"),
    )
}

#[derive(Debug, Default)]
struct OpenAiTranscriptAccumulator {
    partial_text: String,
    completed: Vec<OpenAiCompletedTranscript>,
}

#[derive(Debug)]
struct OpenAiCompletedTranscript {
    item_id: String,
    content_index: u64,
    transcript: String,
}

impl OpenAiTranscriptAccumulator {
    fn handle_text_message(&mut self, raw: &str) -> Result<(), StreamingTranscriptionError> {
        let value: Value = serde_json::from_str(raw).map_err(|source| {
            StreamingTranscriptionError::failed(
                PROVIDER,
                "invalid_openai_realtime_message",
                format!("failed to parse OpenAI Realtime message JSON: {source}"),
            )
        })?;

        match value.get("type").and_then(Value::as_str) {
            Some("conversation.item.input_audio_transcription.delta") => self.handle_delta(&value),
            Some("conversation.item.input_audio_transcription.completed") => {
                self.handle_completed(&value)
            }
            Some("error") => Err(openai_provider_error(&value)),
            Some(_) => Ok(()),
            None => Err(StreamingTranscriptionError::failed(
                PROVIDER,
                "invalid_openai_realtime_message",
                format!("OpenAI Realtime message did not include a string type field: {value}"),
            )),
        }
    }

    fn handle_delta(&mut self, value: &Value) -> Result<(), StreamingTranscriptionError> {
        let Some(delta) = value.get("delta").and_then(Value::as_str) else {
            return Err(StreamingTranscriptionError::failed(
                PROVIDER,
                "invalid_openai_realtime_delta",
                format!(
                    "OpenAI Realtime delta event did not include a string delta field: {value}"
                ),
            ));
        };
        self.partial_text.push_str(delta);
        Ok(())
    }

    fn handle_completed(&mut self, value: &Value) -> Result<(), StreamingTranscriptionError> {
        let Some(item_id) = value.get("item_id").and_then(Value::as_str) else {
            return Err(StreamingTranscriptionError::failed(
                PROVIDER,
                "invalid_openai_realtime_completed",
                format!(
                    "OpenAI Realtime completed event did not include a string item_id field: {value}"
                ),
            ));
        };
        let Some(transcript) = value.get("transcript").and_then(Value::as_str) else {
            return Err(StreamingTranscriptionError::failed(
                PROVIDER,
                "invalid_openai_realtime_completed",
                format!(
                    "OpenAI Realtime completed event did not include a string transcript field: {value}"
                ),
            ));
        };
        let content_index = value
            .get("content_index")
            .and_then(Value::as_u64)
            .unwrap_or(0);

        if let Some(existing) = self
            .completed
            .iter_mut()
            .find(|entry| entry.item_id == item_id && entry.content_index == content_index)
        {
            existing.transcript = transcript.to_string();
        } else {
            self.completed.push(OpenAiCompletedTranscript {
                item_id: item_id.to_string(),
                content_index,
                transcript: transcript.to_string(),
            });
        }
        Ok(())
    }

    fn completed_count(&self) -> usize {
        self.completed.len()
    }

    fn into_outcome(self) -> StreamingTranscriptOutcome {
        let raw_text = self
            .completed
            .into_iter()
            .filter_map(|entry| {
                let transcript = entry.transcript.trim();
                (!transcript.is_empty()).then(|| transcript.to_string())
            })
            .collect::<Vec<_>>()
            .join(" ");
        let raw_text = raw_text.trim();
        if raw_text.is_empty() {
            StreamingTranscriptOutcome::empty(
                PROVIDER,
                "OpenAI Realtime transcription returned an empty transcript",
            )
        } else {
            StreamingTranscriptOutcome::produced(PROVIDER, raw_text)
        }
    }
}

fn openai_provider_error(value: &Value) -> StreamingTranscriptionError {
    let error = value.get("error").unwrap_or(value);
    let code = error
        .get("code")
        .or_else(|| error.get("type"))
        .or_else(|| value.get("code"))
        .and_then(Value::as_str)
        .unwrap_or("openai_realtime_provider_error");
    let detail = error
        .get("message")
        .or_else(|| value.get("message"))
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

    fn request() -> OpenAiRealtimeTranscriptionRequest {
        OpenAiRealtimeTranscriptionRequest {
            api_key: "secret".to_string(),
            endpoint: "wss://openai.example.test/v1/realtime?tag=muninn".to_string(),
            model: "gpt-realtime-whisper".to_string(),
            language: Some("en-IE".to_string()),
            delay: "low".to_string(),
        }
    }

    #[test]
    fn builds_websocket_handshake_url_from_config() {
        let url = request()
            .handshake_url()
            .expect("OpenAI Realtime URL should build");

        assert_eq!(url.scheme(), "wss");
        assert_eq!(url.host_str(), Some("openai.example.test"));
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            vec![
                ("tag".into(), "muninn".into()),
                ("model".into(), "gpt-realtime-whisper".into()),
            ]
        );
    }

    #[test]
    fn session_update_configures_transcription_session() {
        let event = request().session_update_event();

        assert_eq!(event["type"], json!("session.update"));
        assert_eq!(event["session"]["type"], json!("transcription"));
        assert_eq!(
            event["session"]["audio"]["input"]["format"],
            json!({
                "type": "audio/pcm",
                "rate": 24000,
            })
        );
        assert_eq!(
            event["session"]["audio"]["input"]["transcription"],
            json!({
                "model": "gpt-realtime-whisper",
                "language": "en-IE",
                "delay": "low",
            })
        );
        assert!(event["session"]["audio"]["input"]["turn_detection"].is_null());
    }

    #[test]
    fn missing_credentials_map_to_unavailable_credentials() {
        let _guard = EnvVarGuard::remove("OPENAI_API_KEY");
        let resolved = resolved_config(
            r#"
[transcription]
mode = "streaming"
providers = ["openai"]
"#,
        );

        let error = OpenAiRealtimeTranscriptionRequest::from_resolved(&resolved)
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
                assert!(detail.contains("OPENAI_API_KEY"));
            }
            other => panic!("expected unavailable credentials, got {other:?}"),
        }
    }

    #[test]
    fn parser_keeps_delta_events_transient() {
        let mut accumulator = OpenAiTranscriptAccumulator::default();
        accumulator
            .handle_text_message(
                r#"{"type":"conversation.item.input_audio_transcription.delta","item_id":"item_001","content_index":0,"delta":"partial words"}"#,
            )
            .expect("delta should parse");

        assert_eq!(accumulator.partial_text, "partial words");
        let outcome = accumulator.into_outcome();

        assert!(outcome.raw_text.is_none());
        assert_eq!(
            outcome.attempt.outcome,
            TranscriptionAttemptOutcome::EmptyTranscript
        );
    }

    #[test]
    fn parser_accumulates_completed_events_in_arrival_order() {
        let mut accumulator = OpenAiTranscriptAccumulator::default();
        accumulator
            .handle_text_message(
                r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_002","content_index":0,"transcript":"second"}"#,
            )
            .expect("first completion should parse");
        accumulator
            .handle_text_message(
                r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_001","content_index":0,"transcript":"first"}"#,
            )
            .expect("second completion should parse");

        let outcome = accumulator.into_outcome();

        assert_eq!(outcome.raw_text.as_deref(), Some("second first"));
        assert_eq!(
            outcome.attempt.outcome,
            TranscriptionAttemptOutcome::ProducedTranscript
        );
    }

    #[test]
    fn parser_updates_duplicate_completed_item() {
        let mut accumulator = OpenAiTranscriptAccumulator::default();
        accumulator
            .handle_text_message(
                r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_001","content_index":0,"transcript":"draft"}"#,
            )
            .expect("first completion should parse");
        accumulator
            .handle_text_message(
                r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_001","content_index":0,"transcript":"final"}"#,
            )
            .expect("duplicate completion should parse");

        let outcome = accumulator.into_outcome();

        assert_eq!(outcome.raw_text.as_deref(), Some("final"));
    }

    #[test]
    fn parser_returns_empty_outcome_for_empty_completed_transcript() {
        let mut accumulator = OpenAiTranscriptAccumulator::default();
        accumulator
            .handle_text_message(
                r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_001","content_index":0,"transcript":"   "}"#,
            )
            .expect("empty completion should parse");

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
        let mut accumulator = OpenAiTranscriptAccumulator::default();

        let error = accumulator
            .handle_text_message("{not json")
            .expect_err("malformed JSON should fail");

        assert_eq!(error.provider(), PROVIDER);
        assert_eq!(error.to_attempt().code, "invalid_openai_realtime_message");
    }

    #[test]
    fn parser_rejects_messages_without_type() {
        let mut accumulator = OpenAiTranscriptAccumulator::default();

        let error = accumulator
            .handle_text_message(r#"{"item_id":"item_001"}"#)
            .expect_err("missing type should fail");

        assert_eq!(error.to_attempt().code, "invalid_openai_realtime_message");
    }

    #[test]
    fn parser_maps_provider_errors() {
        let mut accumulator = OpenAiTranscriptAccumulator::default();

        let error = accumulator
            .handle_text_message(
                r#"{"type":"error","error":{"type":"invalid_request_error","code":"bad_request","message":"bad realtime request"}}"#,
            )
            .expect_err("provider errors should fail");

        assert_eq!(error.to_attempt().code, "bad_request");
        assert!(error.to_attempt().detail.contains("bad realtime request"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_sends_audio_frames_and_finalizes() {
        let state = Arc::new(Mutex::new(FakeWebSocketState::default()));
        let socket = FakeOpenAiWebSocket::new(
            Arc::clone(&state),
            [Message::Text(
                r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_001","content_index":0,"transcript":"streamed text"}"#
                    .to_string(),
            )],
        );
        let mut session = OpenAiStreamingSession::new(socket, request());
        session.configure().await.expect("session should configure");

        session
            .send_audio(AudioFrame {
                samples: vec![0x1234, -2],
                sample_rate_hz: 24_000,
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
        let text_payloads = state
            .text_payloads
            .iter()
            .map(|payload| serde_json::from_str::<Value>(payload).expect("payload should be JSON"))
            .collect::<Vec<_>>();
        assert_eq!(text_payloads[0]["type"], json!("session.update"));
        assert_eq!(
            text_payloads[1],
            json!({
                "type": "input_audio_buffer.append",
                "audio": "NBL+/w==",
            })
        );
        assert_eq!(
            text_payloads[2],
            json!({
                "type": "input_audio_buffer.commit",
            })
        );
        assert_eq!(state.close_calls, 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_replies_to_ping_while_finishing() {
        let state = Arc::new(Mutex::new(FakeWebSocketState::default()));
        let socket = FakeOpenAiWebSocket::new(
            Arc::clone(&state),
            [
                Message::Ping(vec![1, 2, 3]),
                Message::Text(
                    r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_001","content_index":0,"transcript":"done"}"#
                        .to_string(),
                ),
            ],
        );

        let _ = Box::new(OpenAiStreamingSession::new(socket, request()))
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
        let socket = FakeOpenAiWebSocket::new(Arc::clone(&state), []);
        let mut session = OpenAiStreamingSession::new(socket, request());

        let error = session
            .send_audio(AudioFrame {
                samples: vec![1],
                sample_rate_hz: 16_000,
                channels: 1,
            })
            .await
            .expect_err("sample-rate mismatch should fail");

        assert_eq!(
            error.to_attempt().code,
            "openai_realtime_sample_rate_mismatch"
        );
        assert!(state
            .lock()
            .expect("fake state poisoned")
            .text_payloads
            .is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_rejects_channel_mismatch() {
        let state = Arc::new(Mutex::new(FakeWebSocketState::default()));
        let socket = FakeOpenAiWebSocket::new(Arc::clone(&state), []);
        let mut session = OpenAiStreamingSession::new(socket, request());

        let error = session
            .send_audio(AudioFrame {
                samples: vec![1],
                sample_rate_hz: 24_000,
                channels: 2,
            })
            .await
            .expect_err("channel mismatch should fail");

        assert_eq!(error.to_attempt().code, "openai_realtime_channel_mismatch");
        assert!(state
            .lock()
            .expect("fake state poisoned")
            .text_payloads
            .is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn session_cancel_closes_socket_without_commit() {
        let state = Arc::new(Mutex::new(FakeWebSocketState::default()));
        let socket = FakeOpenAiWebSocket::new(Arc::clone(&state), []);

        Box::new(OpenAiStreamingSession::new(socket, request()))
            .cancel()
            .await;

        let state = state.lock().expect("fake state poisoned");
        assert!(state.text_payloads.is_empty());
        assert_eq!(state.close_calls, 1);
    }

    #[derive(Default)]
    struct FakeWebSocketState {
        text_payloads: Vec<String>,
        pong_payloads: Vec<Vec<u8>>,
        close_calls: usize,
    }

    struct FakeOpenAiWebSocket {
        state: Arc<Mutex<FakeWebSocketState>>,
        incoming: VecDeque<Message>,
    }

    impl FakeOpenAiWebSocket {
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
    impl OpenAiWebSocket for FakeOpenAiWebSocket {
        async fn send(&mut self, message: Message) -> Result<(), StreamingTranscriptionError> {
            let mut state = self.state.lock().expect("fake state poisoned");
            match message {
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

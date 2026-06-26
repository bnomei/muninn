use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use futures_util::{SinkExt, StreamExt};
use reqwest::Url;
use serde_json::{json, Value};
use std::collections::BTreeSet;
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
const RECORDING_CONFIG_UNSUPPORTED_CODE: &str = "openai_realtime_recording_config_unsupported";
const REQUIRED_SAMPLE_RATE_HZ: u32 = 24_000;
const REQUIRED_CHANNELS: u16 = 1;
const STREAM_CLOSED_WITH_ERROR_CODE: &str = "openai_realtime_closed_with_error";

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
        validate_recording_config(resolved)?;

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

fn validate_recording_config(
    resolved: &ResolvedUtteranceConfig,
) -> Result<(), StreamingTranscriptionError> {
    let recording = &resolved.effective_config.recording;
    if recording.sample_rate_hz() == REQUIRED_SAMPLE_RATE_HZ && recording.mono {
        return Ok(());
    }

    Err(StreamingTranscriptionError::unavailable_runtime_capability(
        PROVIDER,
        RECORDING_CONFIG_UNSUPPORTED_CODE,
        format!(
            "OpenAI Realtime transcription requires recording.sample_rate_khz = 24 and recording.mono = true; current recording.sample_rate_khz = {}, recording.mono = {}",
            recording.sample_rate_khz, recording.mono
        ),
    ))
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

        // The realtime session is persistent and does not close after a single
        // utterance. The server acknowledges this commit with the user item id
        // (`input_audio_buffer.committed`) and later finalizes the item with its
        // audio content indexes (`conversation.item.done`). Wait until each audio
        // content part of that committed item has a completed transcript instead of
        // using a fixed post-completion grace, which can truncate delayed final
        // events. The upstream finish timeout remains the outer bound for a missing
        // acknowledgement/finalization/completion.
        let mut error_close: Option<StreamingTranscriptionError> = None;
        loop {
            let Some(message) = self.socket.next().await? else {
                break;
            };

            match message {
                Message::Text(text) => {
                    self.transcript.handle_text_message(&text)?;
                    if self.transcript.committed_item_transcription_finished() {
                        break;
                    }
                }
                Message::Binary(_) | Message::Pong(_) | Message::Frame(_) => {}
                Message::Ping(payload) => {
                    self.socket.send(Message::Pong(payload)).await?;
                }
                Message::Close(frame) => {
                    // A server-initiated error close (auth/quota/policy) must be a
                    // provider failure, not an empty-success outcome that would be
                    // reported to the user as "no speech detected". Benign closes
                    // (1000 Normal, 1001 Going Away, or no frame) just end the loop.
                    if let Some(frame) = frame {
                        let code = u16::from(frame.code);
                        if !matches!(code, 1000 | 1001) {
                            error_close = Some(StreamingTranscriptionError::failed(
                                PROVIDER,
                                STREAM_CLOSED_WITH_ERROR_CODE,
                                format!(
                                    "OpenAI Realtime closed the stream with error code {code}: {}",
                                    frame.reason
                                ),
                            ));
                        }
                    }
                    break;
                }
            }
        }

        let _ = self.socket.close().await;
        let outcome = self.transcript.into_outcome();
        // Prefer a transcript that was produced before an error close; only surface
        // the close as a failure when there is no usable final text.
        match error_close {
            Some(error) if !outcome.has_final_text() => Err(error),
            _ => Ok(outcome),
        }
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
    committed_item_id: Option<String>,
    committed_audio_content_indexes: Option<BTreeSet<u64>>,
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
            Some("input_audio_buffer.committed") => self.handle_buffer_committed(&value),
            Some("conversation.item.done") => self.handle_item_done(&value),
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

    fn handle_buffer_committed(
        &mut self,
        value: &Value,
    ) -> Result<(), StreamingTranscriptionError> {
        let Some(item_id) = value.get("item_id").and_then(Value::as_str) else {
            return Err(StreamingTranscriptionError::failed(
                PROVIDER,
                "invalid_openai_realtime_committed",
                format!(
                    "OpenAI Realtime committed event did not include a string item_id field: {value}"
                ),
            ));
        };
        self.committed_item_id = Some(item_id.to_string());
        Ok(())
    }

    fn handle_item_done(&mut self, value: &Value) -> Result<(), StreamingTranscriptionError> {
        let Some(item) = value.get("item").and_then(Value::as_object) else {
            return Err(StreamingTranscriptionError::failed(
                PROVIDER,
                "invalid_openai_realtime_item_done",
                format!("OpenAI Realtime item.done event did not include an item object: {value}"),
            ));
        };
        let Some(item_id) = item.get("id").and_then(Value::as_str) else {
            return Err(StreamingTranscriptionError::failed(
                PROVIDER,
                "invalid_openai_realtime_item_done",
                format!("OpenAI Realtime item.done event did not include a string item.id field: {value}"),
            ));
        };
        if self.committed_item_id.as_deref() != Some(item_id) {
            return Ok(());
        }
        let Some(content) = item.get("content").and_then(Value::as_array) else {
            return Err(StreamingTranscriptionError::failed(
                PROVIDER,
                "invalid_openai_realtime_item_done",
                format!("OpenAI Realtime item.done event did not include an item.content array: {value}"),
            ));
        };

        let mut indexes = content
            .iter()
            .enumerate()
            .filter_map(|(index, part)| is_input_audio_content_part(part).then_some(index as u64))
            .collect::<BTreeSet<_>>();

        if indexes.is_empty() {
            indexes.insert(0);
        }
        self.committed_audio_content_indexes = Some(indexes);
        Ok(())
    }

    fn committed_item_transcription_finished(&self) -> bool {
        let Some(committed_item_id) = self.committed_item_id.as_deref() else {
            return false;
        };
        let Some(audio_content_indexes) = self.committed_audio_content_indexes.as_ref() else {
            return false;
        };

        audio_content_indexes.iter().all(|content_index| {
            self.completed.iter().any(|entry| {
                entry.item_id == committed_item_id && entry.content_index == *content_index
            })
        })
    }

    fn into_outcome(self) -> StreamingTranscriptOutcome {
        let mut completed = self.completed;
        if let Some(committed_item_id) = self.committed_item_id.as_deref() {
            completed.retain(|entry| entry.item_id == committed_item_id);
            completed.sort_by_key(|entry| entry.content_index);
        }

        let raw_text = completed
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

fn is_input_audio_content_part(part: &Value) -> bool {
    matches!(
        part.get("type").and_then(Value::as_str),
        Some("input_audio" | "audio")
    ) || part.get("transcript").is_some()
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
    fn recording_config_mismatch_maps_to_unavailable_runtime_capability_when_bypassing_resolution()
    {
        let _guard = EnvVarGuard::remove("OPENAI_API_KEY");
        let mut resolved = resolved_config(
            r#"
[recording]
sample_rate_khz = 24
mono = true

[transcription]
mode = "streaming"
providers = ["openai"]

[providers.openai]
api_key = "config-key"
"#,
        );
        resolved.effective_config.recording.sample_rate_khz = 16;

        let error = OpenAiRealtimeTranscriptionRequest::from_resolved(&resolved)
            .expect_err("recording config mismatch should fail before connecting");

        match error {
            StreamingTranscriptionError::Unavailable {
                provider,
                outcome,
                code,
                detail,
            } => {
                assert_eq!(provider, PROVIDER);
                assert_eq!(
                    outcome,
                    TranscriptionAttemptOutcome::UnavailableRuntimeCapability
                );
                assert_eq!(code, RECORDING_CONFIG_UNSUPPORTED_CODE);
                assert!(detail.contains("sample_rate_khz = 24"));
            }
            other => panic!("expected unavailable runtime capability, got {other:?}"),
        }
    }

    #[test]
    fn recording_config_accepts_24khz_mono() {
        let _guard = EnvVarGuard::remove("OPENAI_API_KEY");
        let resolved = resolved_config(
            r#"
[recording]
sample_rate_khz = 24
mono = true

[transcription]
mode = "streaming"
providers = ["openai"]

[providers.openai]
api_key = "config-key"
"#,
        );

        let request = OpenAiRealtimeTranscriptionRequest::from_resolved(&resolved)
            .expect("24 kHz mono recording should be compatible");

        assert_eq!(request.model, "gpt-realtime-whisper");
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
    async fn finish_accumulates_multiple_completed_segments() {
        let state = Arc::new(Mutex::new(FakeWebSocketState::default()));
        let socket = FakeOpenAiWebSocket::new(
            Arc::clone(&state),
            [
                Message::Text(
                    r#"{"type":"input_audio_buffer.committed","item_id":"item_002"}"#.to_string(),
                ),
                Message::Text(
                    r#"{"type":"conversation.item.done","item":{"id":"item_002","content":[{"type":"input_audio"},{"type":"input_audio"}]}}"#
                        .to_string(),
                ),
                Message::Text(
                    r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_001","content_index":0,"transcript":"previous segment"}"#
                        .to_string(),
                ),
                Message::Text(
                    r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_002","content_index":0,"transcript":"first segment"}"#
                        .to_string(),
                ),
                Message::Text(
                    r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_002","content_index":1,"transcript":"second segment"}"#
                        .to_string(),
                ),
            ],
        );

        let outcome = Box::new(OpenAiStreamingSession::new(socket, request()))
            .finish()
            .await
            .expect("session should finalize");

        // Regression: finish() must not stop after the first completed segment.
        assert_eq!(
            outcome.raw_text.as_deref(),
            Some("first segment second segment")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finish_waits_for_delayed_completion_for_committed_item() {
        let state = Arc::new(Mutex::new(FakeWebSocketState::default()));
        let socket = FakeOpenAiWebSocket::new_delayed(
            Arc::clone(&state),
            [
                FakeIncoming::message(Message::Text(
                    r#"{"type":"input_audio_buffer.committed","item_id":"item_001"}"#.to_string(),
                )),
                FakeIncoming::message(Message::Text(
                    r#"{"type":"conversation.item.done","item":{"id":"item_001","content":[{"type":"input_audio"}]}}"#
                        .to_string(),
                )),
                FakeIncoming::delayed(
                    std::time::Duration::from_millis(350),
                    Message::Text(
                        r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_001","content_index":0,"transcript":"delayed final text"}"#
                            .to_string(),
                    ),
                ),
            ],
        );

        let outcome = Box::new(OpenAiStreamingSession::new(socket, request()))
            .finish()
            .await
            .expect("session should wait for the committed item's delayed completion");

        // Regression: a fixed post-completion grace can close the persistent socket
        // before a delayed final event arrives.
        assert_eq!(outcome.raw_text.as_deref(), Some("delayed final text"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finish_waits_for_all_finalized_audio_content_indexes() {
        let state = Arc::new(Mutex::new(FakeWebSocketState::default()));
        let socket = FakeOpenAiWebSocket::new_delayed(
            Arc::clone(&state),
            [
                FakeIncoming::message(Message::Text(
                    r#"{"type":"input_audio_buffer.committed","item_id":"item_001"}"#.to_string(),
                )),
                FakeIncoming::message(Message::Text(
                    r#"{"type":"conversation.item.done","item":{"id":"item_001","content":[{"type":"input_audio"},{"type":"input_audio"}]}}"#
                        .to_string(),
                )),
                FakeIncoming::message(Message::Text(
                    r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_001","content_index":0,"transcript":"first part"}"#
                        .to_string(),
                )),
                FakeIncoming::delayed(
                    std::time::Duration::from_millis(350),
                    Message::Text(
                        r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_001","content_index":1,"transcript":"second part"}"#
                            .to_string(),
                    ),
                ),
            ],
        );

        let outcome = Box::new(OpenAiStreamingSession::new(socket, request()))
            .finish()
            .await
            .expect("session should wait for all finalized audio content indexes");

        assert_eq!(outcome.raw_text.as_deref(), Some("first part second part"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finish_orders_committed_audio_parts_by_content_index() {
        let state = Arc::new(Mutex::new(FakeWebSocketState::default()));
        let socket = FakeOpenAiWebSocket::new(
            Arc::clone(&state),
            [
                Message::Text(
                    r#"{"type":"input_audio_buffer.committed","item_id":"item_001"}"#.to_string(),
                ),
                Message::Text(
                    r#"{"type":"conversation.item.done","item":{"id":"item_001","content":[{"type":"input_audio"},{"type":"input_audio"}]}}"#
                        .to_string(),
                ),
                Message::Text(
                    r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_001","content_index":1,"transcript":"second part"}"#
                        .to_string(),
                ),
                Message::Text(
                    r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_001","content_index":0,"transcript":"first part"}"#
                        .to_string(),
                ),
            ],
        );

        let outcome = Box::new(OpenAiStreamingSession::new(socket, request()))
            .finish()
            .await
            .expect("session should order finalized audio content indexes");

        assert_eq!(outcome.raw_text.as_deref(), Some("first part second part"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finish_skips_empty_first_completion_for_later_text() {
        let state = Arc::new(Mutex::new(FakeWebSocketState::default()));
        let socket = FakeOpenAiWebSocket::new(
            Arc::clone(&state),
            [
                Message::Text(
                    r#"{"type":"input_audio_buffer.committed","item_id":"item_002"}"#.to_string(),
                ),
                Message::Text(
                    r#"{"type":"conversation.item.done","item":{"id":"item_002","content":[{"type":"input_audio"}]}}"#
                        .to_string(),
                ),
                Message::Text(
                    r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_001","content_index":0,"transcript":"   "}"#
                        .to_string(),
                ),
                Message::Text(
                    r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_002","content_index":0,"transcript":"real text"}"#
                        .to_string(),
                ),
            ],
        );

        let outcome = Box::new(OpenAiStreamingSession::new(socket, request()))
            .finish()
            .await
            .expect("session should finalize");

        assert_eq!(outcome.raw_text.as_deref(), Some("real text"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn finish_reports_error_close_frame_as_failure() {
        use tokio_tungstenite::tungstenite::protocol::{frame::coding::CloseCode, CloseFrame};

        let state = Arc::new(Mutex::new(FakeWebSocketState::default()));
        let socket = FakeOpenAiWebSocket::new(
            Arc::clone(&state),
            [Message::Close(Some(CloseFrame {
                code: CloseCode::Policy,
                reason: "insufficient_quota".into(),
            }))],
        );

        let error = Box::new(OpenAiStreamingSession::new(socket, request()))
            .finish()
            .await
            .expect_err("an error close with no transcript must surface as a failure");

        assert_eq!(error.to_attempt().code, "openai_realtime_closed_with_error");
        assert!(error.to_attempt().detail.contains("insufficient_quota"));
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
        incoming: VecDeque<FakeIncoming>,
    }

    struct FakeIncoming {
        delay: Option<std::time::Duration>,
        message: Message,
    }

    impl FakeOpenAiWebSocket {
        fn new(
            state: Arc<Mutex<FakeWebSocketState>>,
            incoming: impl IntoIterator<Item = Message>,
        ) -> Self {
            Self::new_delayed(
                state,
                incoming
                    .into_iter()
                    .map(FakeIncoming::message)
                    .collect::<Vec<_>>(),
            )
        }

        fn new_delayed(
            state: Arc<Mutex<FakeWebSocketState>>,
            incoming: impl IntoIterator<Item = FakeIncoming>,
        ) -> Self {
            Self {
                state,
                incoming: incoming.into_iter().collect(),
            }
        }
    }

    impl FakeIncoming {
        fn message(message: Message) -> Self {
            Self {
                delay: None,
                message,
            }
        }

        fn delayed(delay: std::time::Duration, message: Message) -> Self {
            Self {
                delay: Some(delay),
                message,
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
            let Some(incoming) = self.incoming.pop_front() else {
                return Ok(None);
            };
            if let Some(delay) = incoming.delay {
                tokio::time::sleep(delay).await;
            }
            Ok(Some(incoming.message))
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

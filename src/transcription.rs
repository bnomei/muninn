use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::envelope::MuninnEnvelopeV1;

const DEFAULT_STT_TIMEOUT_MS: u64 = 18_000;
const TRANSCRIPTION_EXTRA_KEY: &str = "transcription";
const ROUTE_EXTRA_KEY: &str = "route";
const ATTEMPTS_EXTRA_KEY: &str = "attempts";

const ALL_PROVIDERS: [TranscriptionProvider; 5] = [
    TranscriptionProvider::AppleSpeech,
    TranscriptionProvider::WhisperCpp,
    TranscriptionProvider::Deepgram,
    TranscriptionProvider::OpenAi,
    TranscriptionProvider::Google,
];

const DEFAULT_PROVIDER_ROUTE: [TranscriptionProvider; 5] = [
    TranscriptionProvider::AppleSpeech,
    TranscriptionProvider::WhisperCpp,
    TranscriptionProvider::Deepgram,
    TranscriptionProvider::OpenAi,
    TranscriptionProvider::Google,
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionProvider {
    AppleSpeech,
    WhisperCpp,
    Deepgram,
    #[serde(rename = "openai")]
    OpenAi,
    Google,
}

impl TranscriptionProvider {
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &ALL_PROVIDERS
    }

    #[must_use]
    pub const fn default_ordered_route() -> &'static [Self] {
        &DEFAULT_PROVIDER_ROUTE
    }

    #[must_use]
    pub const fn config_id(self) -> &'static str {
        match self {
            Self::AppleSpeech => "apple_speech",
            Self::WhisperCpp => "whisper_cpp",
            Self::Deepgram => "deepgram",
            Self::OpenAi => "openai",
            Self::Google => "google",
        }
    }

    #[must_use]
    pub const fn canonical_step_name(self) -> &'static str {
        match self {
            Self::AppleSpeech => "stt_apple_speech",
            Self::WhisperCpp => "stt_whisper_cpp",
            Self::Deepgram => "stt_deepgram",
            Self::OpenAi => "stt_openai",
            Self::Google => "stt_google",
        }
    }

    #[must_use]
    pub const fn default_timeout_ms(self) -> u64 {
        match self {
            Self::WhisperCpp => 45_000,
            _ => DEFAULT_STT_TIMEOUT_MS,
        }
    }

    #[must_use]
    pub const fn is_local(self) -> bool {
        matches!(self, Self::AppleSpeech | Self::WhisperCpp)
    }

    #[must_use]
    pub fn lookup_config_id(raw: &str) -> Option<Self> {
        match raw {
            "apple_speech" => Some(Self::AppleSpeech),
            "whisper_cpp" => Some(Self::WhisperCpp),
            "deepgram" => Some(Self::Deepgram),
            "openai" => Some(Self::OpenAi),
            "google" => Some(Self::Google),
            _ => None,
        }
    }

    #[must_use]
    pub fn lookup_step_name(raw: &str) -> Option<Self> {
        match raw {
            "stt_apple_speech" => Some(Self::AppleSpeech),
            "stt_whisper_cpp" => Some(Self::WhisperCpp),
            "stt_deepgram" => Some(Self::Deepgram),
            "stt_openai" => Some(Self::OpenAi),
            "stt_google" => Some(Self::Google),
            _ => None,
        }
    }
}

impl std::fmt::Display for TranscriptionProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.config_id())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionRouteSource {
    ExplicitConfig,
    PipelineInferred,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedTranscriptionRoute {
    pub providers: Vec<TranscriptionProvider>,
    pub source: TranscriptionRouteSource,
}

impl ResolvedTranscriptionRoute {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionAttemptOutcome {
    ProducedTranscript,
    EmptyTranscript,
    UnavailablePlatform,
    UnavailableCredentials,
    UnavailableAssets,
    UnavailableRuntimeCapability,
    RequestFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptionAttempt {
    pub provider: TranscriptionProvider,
    pub step_name: String,
    pub outcome: TranscriptionAttemptOutcome,
    pub code: String,
    pub detail: String,
}

impl TranscriptionAttempt {
    #[must_use]
    pub fn new(
        provider: TranscriptionProvider,
        outcome: TranscriptionAttemptOutcome,
        code: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            provider,
            step_name: provider.canonical_step_name().to_string(),
            outcome,
            code: code.into(),
            detail: detail.into(),
        }
    }
}

pub fn attach_transcription_route(
    envelope: &mut MuninnEnvelopeV1,
    route: &ResolvedTranscriptionRoute,
) {
    transcription_extra(envelope).insert(
        ROUTE_EXTRA_KEY.to_string(),
        serde_json::to_value(route).expect("transcription route should serialize"),
    );
}

pub fn append_transcription_attempt(
    envelope: &mut MuninnEnvelopeV1,
    attempt: TranscriptionAttempt,
) {
    let transcription = transcription_extra(envelope);
    let attempts = transcription
        .entry(ATTEMPTS_EXTRA_KEY.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    let Value::Array(items) = attempts else {
        *attempts = Value::Array(Vec::new());
        let Value::Array(items) = attempts else {
            unreachable!("attempts entry was just replaced with an array");
        };
        items.push(serde_json::to_value(attempt).expect("transcription attempt should serialize"));
        return;
    };
    items.push(serde_json::to_value(attempt).expect("transcription attempt should serialize"));
}

#[must_use]
pub fn transcription_attempts(envelope: &MuninnEnvelopeV1) -> Vec<TranscriptionAttempt> {
    envelope
        .extra
        .get(TRANSCRIPTION_EXTRA_KEY)
        .and_then(Value::as_object)
        .and_then(|transcription| transcription.get(ATTEMPTS_EXTRA_KEY))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| serde_json::from_value(item.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

#[must_use]
pub fn resolved_transcription_route(
    envelope: &MuninnEnvelopeV1,
) -> Option<ResolvedTranscriptionRoute> {
    envelope
        .extra
        .get(TRANSCRIPTION_EXTRA_KEY)
        .and_then(Value::as_object)
        .and_then(|transcription| transcription.get(ROUTE_EXTRA_KEY))
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

fn transcription_extra(envelope: &mut MuninnEnvelopeV1) -> &mut Map<String, Value> {
    let entry = envelope
        .extra
        .entry(TRANSCRIPTION_EXTRA_KEY.to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !entry.is_object() {
        *entry = Value::Object(Map::new());
    }
    entry
        .as_object_mut()
        .expect("transcription extra entry should be an object")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attaches_route_and_attempts_to_envelope() {
        let mut envelope = MuninnEnvelopeV1::new("utt-1", "2026-03-17T00:00:00Z");
        let route = ResolvedTranscriptionRoute {
            providers: vec![TranscriptionProvider::OpenAi, TranscriptionProvider::Google],
            source: TranscriptionRouteSource::ExplicitConfig,
        };

        attach_transcription_route(&mut envelope, &route);
        append_transcription_attempt(
            &mut envelope,
            TranscriptionAttempt::new(
                TranscriptionProvider::OpenAi,
                TranscriptionAttemptOutcome::RequestFailed,
                "http_request_failed",
                "request timed out",
            ),
        );

        assert_eq!(resolved_transcription_route(&envelope), Some(route));
        assert_eq!(transcription_attempts(&envelope).len(), 1);
        assert_eq!(
            transcription_attempts(&envelope)[0].provider,
            TranscriptionProvider::OpenAi
        );
    }
}

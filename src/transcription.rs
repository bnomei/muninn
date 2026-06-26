//! Transcription provider registry and envelope metadata for STT routing.
//!
//! Defines the canonical provider vocabulary, default fallback order, and
//! helpers that attach resolved routes and per-provider attempt records into
//! `envelope.extra["transcription"]` for pipeline diagnostics.

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

/// Supported speech-to-text backends and their config/step identifiers.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionProvider {
    /// On-device Apple Speech framework.
    AppleSpeech,
    /// Local `whisper.cpp` inference.
    WhisperCpp,
    /// Deepgram cloud API.
    Deepgram,
    /// OpenAI transcription API.
    #[serde(rename = "openai")]
    OpenAi,
    /// Google speech-to-text API.
    Google,
}

impl TranscriptionProvider {
    /// All known providers in declaration order.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &ALL_PROVIDERS
    }

    /// Default provider attempt order when configuration does not override it.
    #[must_use]
    pub const fn default_ordered_route() -> &'static [Self] {
        &DEFAULT_PROVIDER_ROUTE
    }

    /// Snake-case id used under `providers.*` in configuration.
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

    /// Builtin pipeline step command name for this provider.
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

    /// Suggested per-step timeout when configuration leaves `timeout_ms` unset.
    #[must_use]
    pub const fn default_timeout_ms(self) -> u64 {
        match self {
            Self::AppleSpeech => 30_000,
            Self::WhisperCpp => 45_000,
            _ => DEFAULT_STT_TIMEOUT_MS,
        }
    }

    /// True for on-device providers that do not require cloud credentials.
    #[must_use]
    pub const fn is_local(self) -> bool {
        matches!(self, Self::AppleSpeech | Self::WhisperCpp)
    }

    /// Parse a configuration provider id into a known variant.
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

    /// Parse a pipeline step command name into a known variant.
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

/// How the transcription provider route was chosen.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionRouteSource {
    /// User configuration supplied an explicit provider list.
    ExplicitConfig,
    /// Route inferred from pipeline builtin step ordering.
    PipelineInferred,
}

/// Ordered provider list attached to an envelope before STT attempts run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedTranscriptionRoute {
    pub providers: Vec<TranscriptionProvider>,
    pub source: TranscriptionRouteSource,
}

impl ResolvedTranscriptionRoute {
    /// True when no providers remain to attempt.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.providers.is_empty()
    }
}

/// Result category for one provider attempt in the transcription chain.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionAttemptOutcome {
    /// Provider returned non-empty `transcript.raw_text`.
    ProducedTranscript,
    /// Provider succeeded but produced no usable transcript text.
    EmptyTranscript,
    /// Provider is unsupported on the current platform.
    UnavailablePlatform,
    /// Required API keys or secrets were missing.
    UnavailableCredentials,
    /// Local models or assets required by the provider were missing.
    UnavailableAssets,
    /// Runtime capability (microphone, speech framework, etc.) was unavailable.
    UnavailableRuntimeCapability,
    /// Provider request failed after prerequisites were satisfied.
    RequestFailed,
}

/// One STT provider attempt recorded on the envelope for diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptionAttempt {
    pub provider: TranscriptionProvider,
    pub step_name: String,
    pub outcome: TranscriptionAttemptOutcome,
    pub code: String,
    pub detail: String,
}

impl TranscriptionAttempt {
    /// Build an attempt record with the provider's canonical step name.
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

/// Store `route` under `envelope.extra["transcription"]["route"]`.
pub fn attach_transcription_route(
    envelope: &mut MuninnEnvelopeV1,
    route: &ResolvedTranscriptionRoute,
) {
    transcription_extra(envelope).insert(
        ROUTE_EXTRA_KEY.to_string(),
        serde_json::to_value(route).expect("transcription route should serialize"),
    );
}

/// Append `attempt` to `envelope.extra["transcription"]["attempts"]`.
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

/// Read all recorded transcription attempts from the envelope extras.
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

/// Read the resolved provider route previously attached to the envelope.
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

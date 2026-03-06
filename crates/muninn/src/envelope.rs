use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MuninnEnvelopeV1 {
    #[serde(default = "default_schema")]
    pub schema: String,
    pub utterance_id: String,
    pub started_at: String,
    #[serde(default)]
    pub audio: EnvelopeAudio,
    #[serde(default)]
    pub transcript: EnvelopeTranscript,
    #[serde(default)]
    pub uncertain_spans: Vec<Value>,
    #[serde(default)]
    pub candidates: Vec<Value>,
    #[serde(default)]
    pub replacements: Vec<Value>,
    #[serde(default)]
    pub output: EnvelopeOutput,
    #[serde(default)]
    pub errors: Vec<Value>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

impl MuninnEnvelopeV1 {
    pub const SCHEMA: &'static str = "muninn.envelope.v1";

    pub fn new(utterance_id: impl Into<String>, started_at: impl Into<String>) -> Self {
        Self {
            schema: default_schema(),
            utterance_id: utterance_id.into(),
            started_at: started_at.into(),
            audio: EnvelopeAudio::default(),
            transcript: EnvelopeTranscript::default(),
            uncertain_spans: Vec::new(),
            candidates: Vec::new(),
            replacements: Vec::new(),
            output: EnvelopeOutput::default(),
            errors: Vec::new(),
            extra: Map::new(),
        }
    }

    pub fn with_audio(mut self, wav_path: Option<String>, duration_ms: u64) -> Self {
        self.audio.wav_path = wav_path;
        self.audio.duration_ms = duration_ms;
        self
    }

    pub fn with_transcript_raw_text(mut self, raw_text: impl Into<String>) -> Self {
        self.transcript.raw_text = Some(raw_text.into());
        self
    }

    pub fn with_transcript_provider(mut self, provider: impl Into<String>) -> Self {
        self.transcript.provider = Some(provider.into());
        self
    }

    pub fn with_transcript_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.transcript.system_prompt = Some(system_prompt.into());
        self
    }

    pub fn with_output_final_text(mut self, final_text: impl Into<String>) -> Self {
        self.output.final_text = Some(final_text.into());
        self
    }

    pub fn push_uncertain_span(mut self, span: impl Into<Value>) -> Self {
        self.uncertain_spans.push(span.into());
        self
    }

    pub fn push_candidate(mut self, candidate: impl Into<Value>) -> Self {
        self.candidates.push(candidate.into());
        self
    }

    pub fn push_replacement(mut self, replacement: impl Into<Value>) -> Self {
        self.replacements.push(replacement.into());
        self
    }

    pub fn push_error(mut self, error: impl Into<Value>) -> Self {
        self.errors.push(error.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EnvelopeAudio {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wav_path: Option<String>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EnvelopeTranscript {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EnvelopeOutput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_text: Option<String>,
    #[serde(flatten, default)]
    pub extra: Map<String, Value>,
}

fn default_schema() -> String {
    MuninnEnvelopeV1::SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrip_serialization_preserves_contract_fields() {
        let mut envelope = MuninnEnvelopeV1::new("utt-123", "2026-03-05T17:04:43Z")
            .with_transcript_system_prompt("Expand abbreviations but keep punctuation.")
            .with_audio(Some("/tmp/utt-123.wav".to_string()), 1450)
            .with_transcript_raw_text("ship to sf")
            .with_transcript_provider("openai")
            .with_output_final_text("Ship to San Francisco")
            .push_uncertain_span(json!({"start": 8, "end": 10, "text": "sf", "score": 0.62}))
            .push_candidate(json!({"span": "sf", "value": "SF", "score": 0.72}))
            .push_replacement(json!({"from": "sf", "to": "San Francisco", "score": 0.93}))
            .push_error(json!({"code": "provider_warning", "message": "low confidence"}));

        envelope
            .extra
            .insert("step_metadata".to_string(), json!({"stage": "postprocess"}));
        envelope
            .transcript
            .extra
            .insert("language".to_string(), json!("en-US"));

        let encoded = serde_json::to_string(&envelope).expect("serialize envelope");
        let decoded: MuninnEnvelopeV1 =
            serde_json::from_str(&encoded).expect("deserialize envelope");

        assert_eq!(decoded, envelope);
    }

    #[test]
    fn missing_optional_sections_deserialize_with_defaults() {
        let decoded: MuninnEnvelopeV1 = serde_json::from_value(json!({
            "utterance_id": "utt-456",
            "started_at": "2026-03-05T17:10:00Z",
            "transcript": {
                "system_prompt": "Keep capitalization and acronyms."
            }
        }))
        .expect("deserialize minimal envelope");

        assert_eq!(decoded.schema, MuninnEnvelopeV1::SCHEMA);
        assert_eq!(decoded.audio.duration_ms, 0);
        assert!(decoded.audio.wav_path.is_none());
        assert_eq!(
            decoded.transcript.system_prompt.as_deref(),
            Some("Keep capitalization and acronyms.")
        );
        assert!(decoded.uncertain_spans.is_empty());
        assert!(decoded.candidates.is_empty());
        assert!(decoded.replacements.is_empty());
        assert!(decoded.output.final_text.is_none());
        assert!(decoded.errors.is_empty());
    }
}

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::process::ExitCode;

use muninn::config::{RefineConfig, RefineProvider};
use muninn::resolve_secret;
use muninn::AppConfig;
use muninn::MuninnEnvelopeV1;
use serde_json::{json, Value};

const BUILTIN_SYSTEM_PROMPT: &str = r#"You are Muninn, a minimal transcript corrector for developer dictation.

Your job is to lightly repair a speech-to-text transcript while preserving the original meaning exactly.
Make the fewest possible changes.

Priorities:
1. Preserve meaning exactly.
2. Prefer no change over a risky change.
3. Correct obvious technical terms and dictation mistakes.
4. Keep wording, order, and tone as spoken unless a correction is clearly needed.

Allowed changes:
- correct obvious misheard technical terms
- correct tool, library, framework, package, product, and company names
- correct commands, flags, file names, paths, env vars, URLs, emails, and version strings
- correct obvious acronyms and capitalization
- correct obvious spelling mistakes when the intended term is clear
- correct punctuation only when it clearly improves correctness

Do not:
- paraphrase
- summarize
- reorder content
- rewrite for style
- make the text more formal
- add information
- guess when uncertain

If the transcript is already acceptable, return it unchanged.

Return only the corrected transcript text."#;

#[derive(Debug, Clone, PartialEq, Eq)]
struct CliError {
    code: &'static str,
    message: String,
}

impl CliError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn to_stderr_json(&self) -> String {
        json!({
            "error": {
                "code": self.code,
                "message": self.message,
            }
        })
        .to_string()
    }
}

#[derive(Debug, Clone)]
struct ResolvedRefineConfig {
    provider: RefineProvider,
    hint_prompt: String,
    endpoint: String,
    model: String,
    temperature: f32,
    max_output_tokens: u32,
    max_length_delta_ratio: f32,
    max_token_change_ratio: f32,
    max_new_word_count: u32,
    api_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct RejectionMetrics {
    reasons: Vec<&'static str>,
    length_delta_ratio: f32,
    token_change_ratio: f32,
    new_word_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
enum CandidateEvaluation {
    Accept(String),
    Reject(RejectionMetrics),
}

pub fn run_as_internal_tool() -> ExitCode {
    match run_cli() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", error.to_stderr_json());
            ExitCode::FAILURE
        }
    }
}

fn run_cli() -> Result<(), CliError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| {
            CliError::new(
                "runtime_init_failed",
                format!("failed to initialize async runtime: {source}"),
            )
        })?;

    runtime.block_on(async {
        let mut input = String::new();
        io::stdin().read_to_string(&mut input).map_err(|source| {
            CliError::new(
                "stdin_read_failed",
                format!("failed to read stdin: {source}"),
            )
        })?;

        let config = load_refine_config_from_config();
        let env_lookup = |key: &str| std::env::var(key).ok();
        let output = process_input(&input, &env_lookup, &config).await?;

        io::stdout()
            .write_all(output.as_bytes())
            .map_err(|source| {
                CliError::new(
                    "stdout_write_failed",
                    format!("failed to write stdout: {source}"),
                )
            })?;

        Ok(())
    })
}

async fn process_input<F>(
    input: &str,
    get_env: &F,
    config: &ResolvedRefineConfig,
) -> Result<String, CliError>
where
    F: Fn(&str) -> Option<String>,
{
    let mut envelope: MuninnEnvelopeV1 = serde_json::from_str(input).map_err(|source| {
        CliError::new(
            "invalid_envelope_json",
            format!("stdin must be valid MuninnEnvelopeV1 JSON: {source}"),
        )
    })?;

    let Some(raw_text) = non_empty_text(&envelope.transcript.raw_text).map(ToOwned::to_owned)
    else {
        return serialize_envelope(&envelope);
    };

    let candidate = if let Some(stub_text) =
        resolve_secret(get_env("MUNINN_REFINE_STUB_TEXT"), None)
    {
        stub_text
    } else {
        match config.provider {
            RefineProvider::OpenAi => {
                let api_key = resolve_secret(get_env("OPENAI_API_KEY"), config.api_key.clone())
                    .ok_or_else(|| {
                        CliError::new(
                            "missing_openai_api_key",
                            "missing OpenAI API key; set OPENAI_API_KEY or provide providers.openai.api_key in config",
                        )
                    })?;
                refine_with_openai(&api_key, config, &config.hint_prompt, &raw_text).await?
            }
        }
    };

    apply_refinement(&mut envelope, &raw_text, &candidate, config);
    serialize_envelope(&envelope)
}

async fn refine_with_openai(
    api_key: &str,
    config: &ResolvedRefineConfig,
    hint_prompt: &str,
    raw_text: &str,
) -> Result<String, CliError> {
    let response = reqwest::Client::new()
        .post(&config.endpoint)
        .bearer_auth(api_key)
        .json(&json!({
            "model": config.model,
            "temperature": config.temperature,
            "max_completion_tokens": config.max_output_tokens,
            "messages": [
                {
                    "role": "system",
                    "content": BUILTIN_SYSTEM_PROMPT,
                },
                {
                    "role": "user",
                    "content": format!(
                        "Project/user hints:\n{}\n\nTranscript:\n{}",
                        hint_prompt.trim(),
                        raw_text,
                    ),
                }
            ]
        }))
        .send()
        .await
        .map_err(|source| {
            CliError::new(
                "http_request_failed",
                format!("OpenAI refine request failed: {source}"),
            )
        })?;

    let status = response.status();
    let body = response.bytes().await.map_err(|source| {
        CliError::new(
            "http_body_read_failed",
            format!("failed to read OpenAI refine response body: {source}"),
        )
    })?;

    if !status.is_success() {
        return Err(CliError::new(
            "openai_http_error",
            format!(
                "OpenAI refine request failed with status {}: {}",
                status,
                summarize_error_body(&body),
            ),
        ));
    }

    extract_chat_completion_text(&body)
}

fn extract_chat_completion_text(body: &[u8]) -> Result<String, CliError> {
    let value: Value = serde_json::from_slice(body).map_err(|source| {
        CliError::new(
            "invalid_openai_response_json",
            format!("failed to parse OpenAI refine response JSON: {source}"),
        )
    })?;

    let Some(content) = value.pointer("/choices/0/message/content") else {
        return Err(CliError::new(
            "missing_refine_text",
            format!(
                "OpenAI refine response JSON did not include choices[0].message.content: {}",
                summarize_json(&value),
            ),
        ));
    };

    if let Some(text) = content.as_str() {
        return Ok(text.to_string());
    }

    if let Some(items) = content.as_array() {
        let mut chunks = Vec::new();
        for item in items {
            if let Some(text) = item.get("text").and_then(Value::as_str) {
                chunks.push(text);
                continue;
            }
            if let Some(text) = item.get("value").and_then(Value::as_str) {
                chunks.push(text);
            }
        }

        let joined = chunks.join("").trim().to_string();
        if !joined.is_empty() {
            return Ok(joined);
        }
    }

    Err(CliError::new(
        "missing_refine_text",
        format!(
            "OpenAI refine response JSON did not include textual content: {}",
            summarize_json(&value),
        ),
    ))
}

fn apply_refinement(
    envelope: &mut MuninnEnvelopeV1,
    raw_text: &str,
    candidate: &str,
    config: &ResolvedRefineConfig,
) {
    match evaluate_candidate(raw_text, candidate, config) {
        CandidateEvaluation::Accept(text) => {
            envelope.output.final_text = Some(text);
        }
        CandidateEvaluation::Reject(metrics) => {
            envelope.errors.push(json!({
                "code": "refine_rejected",
                "message": "Refinement exceeded acceptance gate",
                "details": {
                    "reasons": metrics.reasons,
                    "length_delta_ratio": metrics.length_delta_ratio,
                    "token_change_ratio": metrics.token_change_ratio,
                    "new_word_count": metrics.new_word_count,
                    "candidate_text": candidate.trim(),
                }
            }));
        }
    }
}

fn evaluate_candidate(
    raw_text: &str,
    candidate: &str,
    config: &ResolvedRefineConfig,
) -> CandidateEvaluation {
    let raw_trimmed = raw_text.trim();
    let candidate_trimmed = candidate.trim();

    if candidate_trimmed.is_empty() {
        return CandidateEvaluation::Reject(RejectionMetrics {
            reasons: vec!["empty_output"],
            length_delta_ratio: 1.0,
            token_change_ratio: 1.0,
            new_word_count: 0,
        });
    }

    if raw_trimmed == candidate_trimmed {
        return CandidateEvaluation::Accept(candidate_trimmed.to_string());
    }

    let raw_len = raw_trimmed.chars().count();
    let candidate_len = candidate_trimmed.chars().count();
    let length_delta_ratio = if raw_len == 0 {
        1.0
    } else {
        raw_len.abs_diff(candidate_len) as f32 / raw_len as f32
    };

    let raw_tokens = tokenize(raw_trimmed);
    let candidate_tokens = tokenize(candidate_trimmed);
    let token_change_ratio = if raw_tokens.is_empty() {
        1.0
    } else {
        token_edit_distance(&raw_tokens, &candidate_tokens) as f32 / raw_tokens.len() as f32
    };
    let new_word_count = count_new_tokens(&raw_tokens, &candidate_tokens);

    let mut reasons = Vec::new();
    if length_delta_ratio > config.max_length_delta_ratio {
        reasons.push("length_delta_ratio_exceeded");
    }
    if token_change_ratio > config.max_token_change_ratio {
        reasons.push("token_change_ratio_exceeded");
    }
    if new_word_count > config.max_new_word_count as usize {
        reasons.push("new_word_count_exceeded");
    }

    if reasons.is_empty() {
        CandidateEvaluation::Accept(candidate_trimmed.to_string())
    } else {
        CandidateEvaluation::Reject(RejectionMetrics {
            reasons,
            length_delta_ratio,
            token_change_ratio,
            new_word_count,
        })
    }
}

fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(normalize_token)
        .filter(|token| !token.is_empty())
        .collect()
}

fn normalize_token(token: &str) -> String {
    token
        .trim_matches(|ch: char| {
            !ch.is_ascii_alphanumeric() && !matches!(ch, '_' | '-' | '/' | '.' | ':' | '$' | '@')
        })
        .to_ascii_lowercase()
}

fn token_edit_distance(left: &[String], right: &[String]) -> usize {
    if left.is_empty() {
        return right.len();
    }
    if right.is_empty() {
        return left.len();
    }

    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0_usize; right.len() + 1];

    for (row, left_token) in left.iter().enumerate() {
        current[0] = row + 1;
        for (col, right_token) in right.iter().enumerate() {
            let substitution_cost = usize::from(left_token != right_token);
            current[col + 1] = min3(
                previous[col + 1] + 1,
                current[col] + 1,
                previous[col] + substitution_cost,
            );
        }
        previous.clone_from_slice(&current);
    }

    previous[right.len()]
}

fn count_new_tokens(raw_tokens: &[String], candidate_tokens: &[String]) -> usize {
    let mut counts = HashMap::<&str, usize>::new();
    for token in raw_tokens {
        *counts.entry(token.as_str()).or_default() += 1;
    }

    let mut new_tokens = 0_usize;
    for token in candidate_tokens {
        match counts.get_mut(token.as_str()) {
            Some(remaining) if *remaining > 0 => *remaining -= 1,
            _ => new_tokens += 1,
        }
    }

    new_tokens
}

const fn min3(a: usize, b: usize, c: usize) -> usize {
    let ab = if a < b { a } else { b };
    if ab < c {
        ab
    } else {
        c
    }
}

fn serialize_envelope(envelope: &MuninnEnvelopeV1) -> Result<String, CliError> {
    serde_json::to_string(envelope).map_err(|source| {
        CliError::new(
            "envelope_serialize_failed",
            format!("failed to serialize output envelope: {source}"),
        )
    })
}

fn load_refine_config_from_config() -> ResolvedRefineConfig {
    let defaults = AppConfig::default();

    AppConfig::load()
        .ok()
        .map(|config| ResolvedRefineConfig {
            provider: config.refine.provider,
            hint_prompt: config.transcript.system_prompt,
            endpoint: config.refine.endpoint,
            model: config.refine.model,
            temperature: config.refine.temperature,
            max_output_tokens: config.refine.max_output_tokens,
            max_length_delta_ratio: config.refine.max_length_delta_ratio,
            max_token_change_ratio: config.refine.max_token_change_ratio,
            max_new_word_count: config.refine.max_new_word_count,
            api_key: resolve_secret(None, config.providers.openai.api_key),
        })
        .unwrap_or_else(|| {
            let mut resolved = ResolvedRefineConfig::from_config(
                &defaults.refine,
                defaults.providers.openai.api_key,
            );
            resolved.hint_prompt = defaults.transcript.system_prompt;
            resolved
        })
}

impl ResolvedRefineConfig {
    fn from_config(config: &RefineConfig, api_key: Option<String>) -> Self {
        Self {
            provider: config.provider,
            hint_prompt: String::new(),
            endpoint: config.endpoint.clone(),
            model: config.model.clone(),
            temperature: config.temperature,
            max_output_tokens: config.max_output_tokens,
            max_length_delta_ratio: config.max_length_delta_ratio,
            max_token_change_ratio: config.max_token_change_ratio,
            max_new_word_count: config.max_new_word_count,
            api_key: resolve_secret(None, api_key),
        }
    }
}

fn summarize_error_body(bytes: &[u8]) -> String {
    let body = String::from_utf8_lossy(bytes);
    let trimmed = body.trim();
    if trimmed.is_empty() {
        "<empty body>".to_string()
    } else {
        trimmed.chars().take(200).collect()
    }
}

fn summarize_json(value: &Value) -> String {
    let rendered =
        serde_json::to_string(value).unwrap_or_else(|_| "<unrenderable json>".to_string());
    rendered.chars().take(300).collect()
}

fn non_empty_text(text: &Option<String>) -> Option<&str> {
    text.as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn baseline_envelope() -> MuninnEnvelopeV1 {
        let mut envelope = MuninnEnvelopeV1::new("utt-123", "2026-03-05T23:00:00Z")
            .with_transcript_raw_text("post gog env variable path dot env")
            .with_output_final_text("existing final");
        envelope
            .extra
            .insert("keep".to_string(), json!({"ok": true}));
        envelope
    }

    fn config() -> ResolvedRefineConfig {
        let mut config = ResolvedRefineConfig::from_config(&AppConfig::default().refine, None);
        config.hint_prompt = "Prefer minimal corrections for technical terms.".to_string();
        config
    }

    #[tokio::test]
    async fn missing_raw_text_is_noop() {
        let mut envelope = baseline_envelope();
        envelope.transcript.raw_text = None;
        let input = serde_json::to_string(&envelope).expect("serialize");

        let output = process_input(&input, &|_| Some("unused".to_string()), &config())
            .await
            .expect("process");
        let actual: MuninnEnvelopeV1 = serde_json::from_str(&output).expect("decode");

        assert_eq!(actual, envelope);
    }

    #[tokio::test]
    async fn stub_refinement_sets_output_final_text_and_preserves_raw() {
        let envelope = baseline_envelope().with_output_final_text("");
        let input = serde_json::to_string(&envelope).expect("serialize");

        let output = process_input(
            &input,
            &|key| match key {
                "MUNINN_REFINE_STUB_TEXT" => Some("PostHog env variable path .env".to_string()),
                _ => None,
            },
            &config(),
        )
        .await
        .expect("process");
        let actual: MuninnEnvelopeV1 = serde_json::from_str(&output).expect("decode");

        assert_eq!(
            actual.transcript.raw_text.as_deref(),
            Some("post gog env variable path dot env")
        );
        assert_eq!(
            actual.output.final_text.as_deref(),
            Some("PostHog env variable path .env")
        );
        assert_eq!(actual.extra.get("keep"), Some(&json!({"ok": true})));
    }

    #[tokio::test]
    async fn rejected_rewrite_preserves_text_and_records_error() {
        let envelope = baseline_envelope().with_output_final_text("");
        let input = serde_json::to_string(&envelope).expect("serialize");

        let output = process_input(
            &input,
            &|key| match key {
                "MUNINN_REFINE_STUB_TEXT" => Some(
                    "This is a completely rewritten sentence with many extra tokens added"
                        .to_string(),
                ),
                _ => None,
            },
            &config(),
        )
        .await
        .expect("process");
        let actual: MuninnEnvelopeV1 = serde_json::from_str(&output).expect("decode");

        assert_eq!(
            actual.transcript.raw_text.as_deref(),
            Some("post gog env variable path dot env")
        );
        assert_eq!(actual.output.final_text.as_deref(), Some(""));
        assert_eq!(actual.errors.len(), 1);
        assert_eq!(
            actual.errors[0].get("code").and_then(Value::as_str),
            Some("refine_rejected")
        );
    }

    #[tokio::test]
    async fn missing_api_key_errors_without_stub() {
        let envelope = baseline_envelope().with_output_final_text("");
        let input = serde_json::to_string(&envelope).expect("serialize");

        let error = process_input(&input, &|_| None, &config())
            .await
            .expect_err("missing key must fail");

        assert_eq!(error.code, "missing_openai_api_key");
    }

    #[test]
    fn evaluate_candidate_accepts_identical_text() {
        assert_eq!(
            evaluate_candidate("hello world", "hello world", &config()),
            CandidateEvaluation::Accept("hello world".to_string())
        );
    }

    #[test]
    fn evaluate_candidate_rejects_large_changes() {
        let CandidateEvaluation::Reject(metrics) = evaluate_candidate(
            "hello world",
            "hello world rewritten with many extra words now",
            &config(),
        ) else {
            panic!("expected rejection");
        };

        assert!(metrics.reasons.contains(&"length_delta_ratio_exceeded"));
        assert!(metrics.reasons.contains(&"token_change_ratio_exceeded"));
        assert!(metrics.reasons.contains(&"new_word_count_exceeded"));
    }

    #[test]
    fn extract_chat_completion_text_accepts_string_content() {
        let text = extract_chat_completion_text(
            br#"{"choices":[{"message":{"content":"Ship it to PostHog"}}]}"#,
        )
        .expect("extract text");

        assert_eq!(text, "Ship it to PostHog");
    }
}

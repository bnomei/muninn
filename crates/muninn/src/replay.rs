use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use chrono::Utc;
use muninn::AppConfig;
use muninn::InjectionRoute;
use muninn::MuninnEnvelopeV1;
use muninn::PipelineOutcome;
use muninn::PipelineTraceEntry;
use muninn::RecordedAudio;
use serde::Serialize;
use serde_json::{Map, Value};

const REDACTED_VALUE: &str = "[redacted]";
const SECRET_KEYS: &[&str] = &[
    "api_key",
    "openai_api_key",
    "google_api_key",
    "token",
    "google_stt_token",
];

#[derive(Debug, Clone, Serialize)]
struct ReplayRecord {
    schema: &'static str,
    persisted_at: String,
    utterance_id: String,
    started_at: String,
    copied_audio_file: Option<String>,
    warnings: Vec<String>,
    redacted_config: Value,
    refine_context: Option<ReplayRefineContext>,
    input_envelope: MuninnEnvelopeV1,
    pipeline_outcome: PipelineOutcome,
    injection_route: InjectionRoute,
}

#[derive(Debug, Clone, Serialize)]
struct ReplayRefineContext {
    system_prompt: String,
}

#[derive(Debug, Clone)]
struct ReplayEntryMeta {
    path: PathBuf,
    modified_at: SystemTime,
    bytes: u64,
}

pub fn persist_replay(
    config: &AppConfig,
    input_envelope: &MuninnEnvelopeV1,
    outcome: &PipelineOutcome,
    route: &InjectionRoute,
    recorded: &RecordedAudio,
) -> Result<Option<PathBuf>> {
    if !config.logging.replay_enabled {
        return Ok(None);
    }

    let replay_root = expand_replay_dir(&config.logging.replay_dir)
        .context("resolving replay_dir to an absolute path")?;
    fs::create_dir_all(&replay_root)
        .with_context(|| format!("creating replay root {}", replay_root.display()))?;

    let artifact_dir = replay_root.join(artifact_dir_name(input_envelope));
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("creating replay artifact dir {}", artifact_dir.display()))?;

    let mut warnings = Vec::new();
    let copied_audio_file = match copy_audio_if_available(&artifact_dir, recorded) {
        Ok(path) => path,
        Err(error) => {
            warnings.push(format!("audio_copy_failed: {error:#}"));
            None
        }
    };

    let record = ReplayRecord {
        schema: "muninn.replay.v1",
        persisted_at: Utc::now().to_rfc3339(),
        utterance_id: input_envelope.utterance_id.clone(),
        started_at: input_envelope.started_at.clone(),
        copied_audio_file,
        warnings,
        redacted_config: redacted_config_snapshot(config),
        refine_context: replay_refine_context(config),
        input_envelope: sanitized_envelope_for_replay(input_envelope),
        pipeline_outcome: sanitized_pipeline_outcome_for_replay(outcome),
        injection_route: route.clone(),
    };

    let record_json =
        serde_json::to_vec_pretty(&record).context("serializing replay record to JSON")?;
    fs::write(artifact_dir.join("record.json"), record_json)
        .with_context(|| format!("writing replay record in {}", artifact_dir.display()))?;

    prune_replay_root(
        &replay_root,
        config.logging.replay_retention_days,
        config.logging.replay_max_bytes,
    )?;

    Ok(Some(artifact_dir))
}

fn redacted_config_snapshot(config: &AppConfig) -> Value {
    let mut snapshot = config.clone();
    snapshot.providers.openai.api_key = None;
    snapshot.providers.google.api_key = None;
    snapshot.providers.google.token = None;
    let mut value = serde_json::to_value(snapshot).expect("AppConfig should serialize to JSON");
    if let Some(transcript) = value.get_mut("transcript").and_then(Value::as_object_mut) {
        transcript.remove("system_prompt");
    }
    value
}

fn replay_refine_context(config: &AppConfig) -> Option<ReplayRefineContext> {
    let uses_refine = config
        .pipeline
        .steps
        .iter()
        .any(|step| step.id == "refine" || step.cmd == "refine");
    let system_prompt = config.transcript.system_prompt.trim();
    if !uses_refine || system_prompt.is_empty() {
        return None;
    }

    Some(ReplayRefineContext {
        system_prompt: system_prompt.to_string(),
    })
}

fn sanitized_envelope_for_replay(envelope: &MuninnEnvelopeV1) -> MuninnEnvelopeV1 {
    sanitize_envelope(envelope, true)
}

fn sanitized_pipeline_outcome_for_replay(outcome: &PipelineOutcome) -> PipelineOutcome {
    match outcome {
        PipelineOutcome::Completed { envelope, trace } => PipelineOutcome::Completed {
            envelope: sanitized_envelope_for_replay(envelope),
            trace: sanitized_trace_for_replay(trace),
        },
        PipelineOutcome::FallbackRaw {
            envelope,
            trace,
            reason,
        } => PipelineOutcome::FallbackRaw {
            envelope: sanitized_envelope_for_replay(envelope),
            trace: sanitized_trace_for_replay(trace),
            reason: reason.clone(),
        },
        PipelineOutcome::Aborted { trace, reason } => PipelineOutcome::Aborted {
            trace: sanitized_trace_for_replay(trace),
            reason: reason.clone(),
        },
    }
}

fn sanitized_trace_for_replay(trace: &[PipelineTraceEntry]) -> Vec<PipelineTraceEntry> {
    trace
        .iter()
        .cloned()
        .map(|mut entry| {
            entry.stderr.clear();
            entry
        })
        .collect()
}

fn sanitize_envelope(envelope: &MuninnEnvelopeV1, clear_system_prompt: bool) -> MuninnEnvelopeV1 {
    let mut value = serde_json::to_value(envelope).expect("MuninnEnvelopeV1 should serialize");
    sanitize_value(&mut value);

    let mut sanitized: MuninnEnvelopeV1 =
        serde_json::from_value(value).expect("sanitized envelope should deserialize");
    if clear_system_prompt {
        sanitized.transcript.system_prompt = None;
    }
    sanitized
}

fn sanitize_value(value: &mut Value) {
    match value {
        Value::Object(map) => sanitize_object(map),
        Value::Array(items) => {
            for item in items {
                sanitize_value(item);
            }
        }
        _ => {}
    }
}

fn sanitize_object(map: &mut Map<String, Value>) {
    for (key, value) in map.iter_mut() {
        if is_secret_key(key) {
            *value = Value::String(REDACTED_VALUE.to_string());
            continue;
        }
        sanitize_value(value);
    }
}

fn is_secret_key(key: &str) -> bool {
    SECRET_KEYS
        .iter()
        .any(|secret_key| key.eq_ignore_ascii_case(secret_key))
}

fn expand_replay_dir(path: &Path) -> Result<PathBuf> {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set for replay_dir expansion");
    }

    if let Some(rest) = raw.strip_prefix("~/") {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set for replay_dir expansion")?;
        return Ok(home.join(rest));
    }

    Ok(path.to_path_buf())
}

fn artifact_dir_name(input_envelope: &MuninnEnvelopeV1) -> String {
    format!(
        "{}--{}",
        sanitize_component(&input_envelope.started_at),
        sanitize_component(&input_envelope.utterance_id)
    )
}

fn sanitize_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '-',
        })
        .collect()
}

fn copy_audio_if_available(
    artifact_dir: &Path,
    recorded: &RecordedAudio,
) -> Result<Option<String>> {
    if !recorded.wav_path.exists() {
        return Ok(None);
    }

    let extension = recorded
        .wav_path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("wav");
    let file_name = format!("audio.{extension}");
    let target = artifact_dir.join(&file_name);

    fs::copy(&recorded.wav_path, &target).with_context(|| {
        format!(
            "copying recorded audio from {} to {}",
            recorded.wav_path.display(),
            target.display()
        )
    })?;

    Ok(Some(file_name))
}

fn prune_replay_root(root: &Path, retention_days: u32, max_bytes: u64) -> Result<()> {
    let entries = collect_replay_entries(root)?;
    let removals = plan_replay_prune(&entries, SystemTime::now(), retention_days, max_bytes);

    for path in removals {
        if path.is_dir() {
            fs::remove_dir_all(&path)
                .with_context(|| format!("removing replay artifact dir {}", path.display()))?;
        } else if path.exists() {
            fs::remove_file(&path)
                .with_context(|| format!("removing replay artifact file {}", path.display()))?;
        }
    }

    Ok(())
}

fn collect_replay_entries(root: &Path) -> Result<Vec<ReplayEntryMeta>> {
    let mut entries = Vec::new();

    for entry in
        fs::read_dir(root).with_context(|| format!("reading replay root {}", root.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry in {}", root.display()))?;
        let path = entry.path();
        let metadata = fs::metadata(&path)
            .with_context(|| format!("reading metadata for {}", path.display()))?;
        let modified_at = metadata
            .modified()
            .with_context(|| format!("reading modified time for {}", path.display()))?;
        let bytes = recursive_size(&path)?;

        entries.push(ReplayEntryMeta {
            path,
            modified_at,
            bytes,
        });
    }

    Ok(entries)
}

fn recursive_size(path: &Path) -> Result<u64> {
    let metadata =
        fs::metadata(path).with_context(|| format!("reading metadata for {}", path.display()))?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }

    let mut total = 0_u64;
    for entry in fs::read_dir(path).with_context(|| format!("reading {}", path.display()))? {
        let entry = entry.with_context(|| format!("reading entry in {}", path.display()))?;
        total = total.saturating_add(recursive_size(&entry.path())?);
    }

    Ok(total)
}

fn plan_replay_prune(
    entries: &[ReplayEntryMeta],
    now: SystemTime,
    retention_days: u32,
    max_bytes: u64,
) -> Vec<PathBuf> {
    let mut sorted_entries = entries.to_vec();
    sorted_entries.sort_by_key(|entry| entry.modified_at);

    let retention_window = Duration::from_secs(u64::from(retention_days) * 24 * 60 * 60);
    let mut removals = Vec::new();
    let mut retained = Vec::new();

    for entry in sorted_entries {
        let expired = now
            .duration_since(entry.modified_at)
            .map(|elapsed| elapsed > retention_window)
            .unwrap_or(false);
        if expired {
            removals.push(entry.path);
        } else {
            retained.push(entry);
        }
    }

    let mut total_bytes = retained.iter().map(|entry| entry.bytes).sum::<u64>();
    for entry in retained {
        if total_bytes <= max_bytes {
            break;
        }
        total_bytes = total_bytes.saturating_sub(entry.bytes);
        removals.push(entry.path);
    }

    removals
}

#[cfg(test)]
mod tests {
    use super::*;
    use muninn::config::{OnErrorPolicy, PipelineStepConfig, StepIoMode};
    use muninn::PipelineOutcome;
    use muninn::{InjectionRoute, InjectionRouteReason, InjectionTarget, PipelinePolicyApplied};
    use serde_json::Value;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "muninn-replay-test-{}-{}-{}",
            name,
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn sample_config(root: &Path) -> AppConfig {
        let mut config = AppConfig::launchable_default();
        config.logging.replay_enabled = true;
        config.logging.replay_dir = root.to_path_buf();
        config.logging.replay_retention_days = 7;
        config.logging.replay_max_bytes = 10_000_000;
        config.providers.openai.api_key = Some("secret-openai".to_string());
        config.providers.google.api_key = Some("secret-google".to_string());
        config.providers.google.token = Some("secret-token".to_string());
        config.pipeline.steps = vec![PipelineStepConfig {
            id: "echo".to_string(),
            cmd: "/bin/cat".to_string(),
            args: Vec::new(),
            io_mode: StepIoMode::TextFilter,
            timeout_ms: 250,
            on_error: OnErrorPolicy::Continue,
        }];
        config
    }

    fn sample_envelope() -> MuninnEnvelopeV1 {
        MuninnEnvelopeV1::new("utt-123", "2026-03-05T22:30:00Z")
            .with_transcript_system_prompt("Keep code tokens intact.")
            .with_audio(Some("/tmp/input.wav".to_string()), 1450)
            .with_transcript_raw_text("hello")
            .with_output_final_text("HELLO")
    }

    fn sample_outcome() -> PipelineOutcome {
        PipelineOutcome::Completed {
            envelope: sample_envelope().with_output_final_text("HELLO"),
            trace: vec![PipelineTraceEntry {
                id: "echo".to_string(),
                duration_ms: 12,
                timed_out: false,
                exit_status: Some(0),
                policy_applied: PipelinePolicyApplied::None,
                stderr: "raw stderr should not persist".to_string(),
            }],
        }
    }

    fn sample_route() -> InjectionRoute {
        InjectionRoute {
            target: InjectionTarget::OutputFinalText("HELLO".to_string()),
            reason: InjectionRouteReason::SelectedOutputFinalText,
            pipeline_stop_reason: None,
        }
    }

    #[test]
    fn expands_home_relative_replay_dir() {
        let home = temp_dir("home");
        let previous = env::var_os("HOME");
        // SAFETY: tests in this crate do not run replay path expansion concurrently with HOME mutation.
        unsafe {
            env::set_var("HOME", &home);
        }
        let expanded =
            expand_replay_dir(Path::new("~/Library/Application Support/Muninn/replay")).unwrap();
        match previous {
            Some(value) => {
                // SAFETY: restore HOME after scoped test mutation.
                unsafe {
                    env::set_var("HOME", value);
                }
            }
            None => {
                // SAFETY: restore HOME after scoped test mutation.
                unsafe {
                    env::remove_var("HOME");
                }
            }
        }

        assert_eq!(
            expanded,
            home.join("Library/Application Support/Muninn/replay")
        );
    }

    #[test]
    fn redacts_provider_secrets_from_replay_snapshot() {
        let config = sample_config(Path::new("/tmp/replay"));
        let snapshot = redacted_config_snapshot(&config);

        assert_eq!(snapshot["providers"]["openai"]["api_key"], Value::Null);
        assert_eq!(snapshot["providers"]["google"]["api_key"], Value::Null);
        assert_eq!(snapshot["providers"]["google"]["token"], Value::Null);
        assert_eq!(snapshot["transcript"].get("system_prompt"), None);
    }

    #[test]
    fn recursively_redacts_secret_keys_in_envelope_extra() {
        let mut envelope = sample_envelope();
        envelope.extra.insert(
            "providers".to_string(),
            serde_json::json!({
                "openai": {
                    "api_key": "openai-secret",
                },
                "google": {
                    "token": "google-secret",
                },
            }),
        );
        envelope.extra.insert(
            "nested".to_string(),
            serde_json::json!({
                "children": [
                    {"google_stt_token": "secret-token"},
                    {"api_key": "shared-secret"},
                ],
            }),
        );

        let sanitized = sanitized_envelope_for_replay(&envelope);

        assert_eq!(
            sanitized.extra["providers"]["openai"]["api_key"],
            Value::String(REDACTED_VALUE.to_string())
        );
        assert_eq!(
            sanitized.extra["providers"]["google"]["token"],
            Value::String(REDACTED_VALUE.to_string())
        );
        assert_eq!(
            sanitized.extra["nested"]["children"][0]["google_stt_token"],
            Value::String(REDACTED_VALUE.to_string())
        );
        assert_eq!(
            sanitized.extra["nested"]["children"][1]["api_key"],
            Value::String(REDACTED_VALUE.to_string())
        );
    }

    #[test]
    fn plan_replay_prune_removes_expired_then_oldest_over_budget() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10 * 24 * 60 * 60);
        let entries = vec![
            ReplayEntryMeta {
                path: PathBuf::from("old-expired"),
                modified_at: SystemTime::UNIX_EPOCH,
                bytes: 10,
            },
            ReplayEntryMeta {
                path: PathBuf::from("oldest-retained"),
                modified_at: now - Duration::from_secs(2),
                bytes: 60,
            },
            ReplayEntryMeta {
                path: PathBuf::from("newest-retained"),
                modified_at: now - Duration::from_secs(1),
                bytes: 60,
            },
        ];

        let removals = plan_replay_prune(&entries, now, 7, 100);

        assert_eq!(
            removals,
            vec![
                PathBuf::from("old-expired"),
                PathBuf::from("oldest-retained")
            ]
        );
    }

    #[test]
    fn persist_replay_writes_record_and_audio_copy() {
        let root = temp_dir("persist");
        let config = sample_config(&root);
        let source_audio = root.join("source.wav");
        fs::write(&source_audio, b"wave").expect("write source audio");
        let recorded = RecordedAudio::new(&source_audio, 1450);

        let artifact_dir = persist_replay(
            &config,
            &sample_envelope(),
            &sample_outcome(),
            &sample_route(),
            &recorded,
        )
        .expect("persist replay should succeed")
        .expect("replay should be written");

        let record_path = artifact_dir.join("record.json");
        assert!(record_path.exists());
        assert!(artifact_dir.join("audio.wav").exists());

        let record: Value =
            serde_json::from_str(&fs::read_to_string(&record_path).expect("read replay record"))
                .expect("parse replay record");

        assert_eq!(
            record["schema"],
            Value::String("muninn.replay.v1".to_string())
        );
        assert_eq!(record["utterance_id"], Value::String("utt-123".to_string()));
        assert_eq!(
            record["redacted_config"]["providers"]["openai"]["api_key"],
            Value::Null
        );
        assert_eq!(
            record["redacted_config"]["transcript"].get("system_prompt"),
            None
        );
        assert_eq!(record["refine_context"], Value::Null);
        assert_eq!(
            record["input_envelope"]["transcript"].get("system_prompt"),
            None
        );
        assert_eq!(
            record["pipeline_outcome"]["Completed"]["envelope"]["transcript"].get("system_prompt"),
            None
        );
        assert_eq!(
            record["pipeline_outcome"]["Completed"]["trace"][0]["stderr"],
            Value::String(String::new())
        );
        assert_eq!(
            record["copied_audio_file"],
            Value::String("audio.wav".to_string())
        );
    }

    #[test]
    fn persist_replay_includes_system_prompt_only_in_refine_context_when_refine_is_present() {
        let root = temp_dir("persist-refine");
        let mut config = AppConfig::launchable_default();
        config.logging.replay_enabled = true;
        config.logging.replay_dir = root.clone();
        config.logging.replay_retention_days = 7;
        config.logging.replay_max_bytes = 10_000_000;

        let source_audio = root.join("source.wav");
        fs::write(&source_audio, b"wave").expect("write source audio");
        let recorded = RecordedAudio::new(&source_audio, 1450);

        let artifact_dir = persist_replay(
            &config,
            &sample_envelope(),
            &sample_outcome(),
            &sample_route(),
            &recorded,
        )
        .expect("persist replay should succeed")
        .expect("replay should be written");

        let record: Value = serde_json::from_str(
            &fs::read_to_string(artifact_dir.join("record.json")).expect("read replay record"),
        )
        .expect("parse replay record");

        assert_eq!(
            record["refine_context"]["system_prompt"],
            Value::String(config.transcript.system_prompt)
        );
        assert_eq!(
            record["input_envelope"]["transcript"].get("system_prompt"),
            None
        );
        assert_eq!(
            record["pipeline_outcome"]["Completed"]["envelope"]["transcript"].get("system_prompt"),
            None
        );
    }

    #[test]
    fn persist_replay_redacts_envelope_secret_fields() {
        let root = temp_dir("persist-envelope-secrets");
        let config = sample_config(&root);
        let source_audio = root.join("source.wav");
        fs::write(&source_audio, b"wave").expect("write source audio");
        let recorded = RecordedAudio::new(&source_audio, 1450);

        let mut envelope = sample_envelope();
        envelope.extra.insert(
            "config".to_string(),
            serde_json::json!({
                "providers": {
                    "google": {
                        "token": "secret-google-token"
                    }
                }
            }),
        );

        let artifact_dir = persist_replay(
            &config,
            &envelope,
            &sample_outcome(),
            &sample_route(),
            &recorded,
        )
        .expect("persist replay should succeed")
        .expect("replay should be written");

        let record: Value = serde_json::from_str(
            &fs::read_to_string(artifact_dir.join("record.json")).expect("read replay record"),
        )
        .expect("parse replay record");

        assert_eq!(
            record["input_envelope"]["config"]["providers"]["google"]["token"],
            Value::String(REDACTED_VALUE.to_string())
        );
    }
}

use std::env;
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use chrono::Utc;
use muninn::AppConfig;
use muninn::InjectionRoute;
use muninn::MuninnEnvelopeV1;
use muninn::PipelineOutcome;
use muninn::PipelineTraceEntry;
use muninn::RecordedAudio;
use muninn::ResolvedUtteranceConfig;
use muninn::TargetContextSnapshot;
use serde::Serialize;
use serde_json::{Map, Value};

const REDACTED_VALUE: &str = "[redacted]";
const REPLAY_PRUNE_THROTTLE_SECS: u64 = 60;
const SECRET_KEYS: &[&str] = &[
    "api_key",
    "openai_api_key",
    "google_api_key",
    "token",
    "google_stt_token",
];
static LAST_REPLAY_PRUNE_AT_SECS: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize)]
struct ReplayRecord {
    schema: &'static str,
    persisted_at: String,
    utterance_id: String,
    started_at: String,
    copied_audio_file: Option<String>,
    warnings: Vec<String>,
    redacted_config: Value,
    resolution: ReplayResolutionContext,
    refine_context: Option<ReplayRefineContext>,
    input_envelope: MuninnEnvelopeV1,
    pipeline_outcome: PipelineOutcome,
    injection_route: InjectionRoute,
}

#[derive(Debug, Clone, Serialize)]
struct ReplayRefineContext {
    system_prompt: String,
}

#[derive(Debug, Clone, Serialize)]
struct ReplayResolutionContext {
    target_context: TargetContextSnapshot,
    matched_rule_id: Option<String>,
    profile_id: String,
    voice_id: Option<String>,
    voice_glyph: Option<char>,
    fallback_reason: Option<String>,
}

#[derive(Debug, Clone)]
struct ReplayEntryMeta {
    path: PathBuf,
    modified_at: SystemTime,
    bytes: u64,
}

pub fn persist_replay(
    resolved: ResolvedUtteranceConfig,
    input_envelope: MuninnEnvelopeV1,
    outcome: PipelineOutcome,
    route: InjectionRoute,
    recorded: RecordedAudio,
) -> Result<Option<PathBuf>> {
    let config = &resolved.effective_config;
    if !config.logging.replay_enabled {
        return Ok(None);
    }

    let replay_root = expand_replay_dir(&config.logging.replay_dir)
        .context("resolving replay_dir to an absolute path")?;
    fs::create_dir_all(&replay_root)
        .with_context(|| format!("creating replay root {}", replay_root.display()))?;

    let artifact_dir = replay_root.join(artifact_dir_name(&input_envelope));
    fs::create_dir_all(&artifact_dir)
        .with_context(|| format!("creating replay artifact dir {}", artifact_dir.display()))?;

    let mut warnings = Vec::new();
    let copied_audio_file = match retain_audio_if_available(
        &artifact_dir,
        &recorded,
        config.logging.replay_retain_audio,
    ) {
        Ok(path) => path,
        Err(error) => {
            warnings.push(format!("audio_retention_failed: {error:#}"));
            None
        }
    };
    let replay_retention_days = config.logging.replay_retention_days;
    let replay_max_bytes = config.logging.replay_max_bytes;
    let refine_context = replay_refine_context(&config);
    let redacted_config = redacted_config_snapshot(config.clone());
    let resolution = ReplayResolutionContext {
        target_context: resolved.target_context,
        matched_rule_id: resolved.matched_rule_id,
        profile_id: resolved.profile_id,
        voice_id: resolved.voice_id,
        voice_glyph: resolved.voice_glyph,
        fallback_reason: resolved.fallback_reason,
    };

    let record = ReplayRecord {
        schema: "muninn.replay.v1",
        persisted_at: Utc::now().to_rfc3339(),
        utterance_id: input_envelope.utterance_id.clone(),
        started_at: input_envelope.started_at.clone(),
        copied_audio_file,
        warnings,
        redacted_config,
        resolution,
        refine_context,
        input_envelope: sanitized_envelope_for_replay(input_envelope),
        pipeline_outcome: sanitized_pipeline_outcome_for_replay(outcome),
        injection_route: route,
    };

    let record_path = artifact_dir.join("record.json");
    let file = fs::File::create(&record_path)
        .with_context(|| format!("creating replay record at {}", record_path.display()))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer_pretty(&mut writer, &record)
        .context("serializing replay record to JSON")?;
    writer
        .flush()
        .with_context(|| format!("flushing replay record at {}", record_path.display()))?;

    maybe_prune_replay_root(
        &replay_root,
        replay_retention_days,
        replay_max_bytes,
        SystemTime::now(),
    )?;

    Ok(Some(artifact_dir))
}

fn redacted_config_snapshot(mut config: AppConfig) -> Value {
    config.providers.openai.api_key = None;
    config.providers.google.api_key = None;
    config.providers.google.token = None;
    let mut value = serde_json::to_value(config).expect("AppConfig should serialize to JSON");
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

fn sanitized_envelope_for_replay(envelope: MuninnEnvelopeV1) -> MuninnEnvelopeV1 {
    sanitize_envelope(envelope, true)
}

fn sanitized_pipeline_outcome_for_replay(mut outcome: PipelineOutcome) -> PipelineOutcome {
    match &mut outcome {
        PipelineOutcome::Completed { envelope, trace } => {
            sanitize_trace_for_replay(trace);
            sanitize_envelope_in_place(envelope, true);
        }
        PipelineOutcome::FallbackRaw {
            envelope, trace, ..
        } => {
            sanitize_trace_for_replay(trace);
            sanitize_envelope_in_place(envelope, true);
        }
        PipelineOutcome::Aborted { trace, .. } => {
            sanitize_trace_for_replay(trace);
        }
    }
    outcome
}

fn sanitize_trace_for_replay(trace: &mut [PipelineTraceEntry]) {
    for entry in trace {
        entry.stderr.clear();
    }
}

fn sanitize_envelope(
    mut envelope: MuninnEnvelopeV1,
    clear_system_prompt: bool,
) -> MuninnEnvelopeV1 {
    sanitize_envelope_in_place(&mut envelope, clear_system_prompt);
    envelope
}

fn sanitize_envelope_in_place(envelope: &mut MuninnEnvelopeV1, clear_system_prompt: bool) {
    sanitize_object(&mut envelope.audio.extra);
    sanitize_object(&mut envelope.transcript.extra);
    sanitize_object(&mut envelope.output.extra);
    sanitize_object(&mut envelope.extra);
    sanitize_values(&mut envelope.uncertain_spans);
    sanitize_values(&mut envelope.candidates);
    sanitize_values(&mut envelope.replacements);
    sanitize_values(&mut envelope.errors);
    if clear_system_prompt {
        envelope.transcript.system_prompt = None;
    }
}

fn sanitize_values(values: &mut [Value]) {
    for value in values {
        sanitize_value(value);
    }
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

fn retain_audio_if_available(
    artifact_dir: &Path,
    recorded: &RecordedAudio,
    replay_retain_audio: bool,
) -> Result<Option<String>> {
    if !replay_retain_audio {
        return Ok(None);
    }

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

    retain_audio_file(&recorded.wav_path, &target)?;

    Ok(Some(file_name))
}

fn retain_audio_file(source: &Path, target: &Path) -> Result<()> {
    match fs::hard_link(source, target) {
        Ok(()) => Ok(()),
        Err(link_error) => {
            fs::copy(source, target).with_context(|| {
                format!(
                    "retaining recorded audio from {} to {} after hard_link failed: {}",
                    source.display(),
                    target.display(),
                    link_error
                )
            })?;
            Ok(())
        }
    }
}

fn maybe_prune_replay_root(
    root: &Path,
    retention_days: u32,
    max_bytes: u64,
    now: SystemTime,
) -> Result<()> {
    let now_secs = seconds_since_epoch(now);
    let last_prune_secs = LAST_REPLAY_PRUNE_AT_SECS.load(Ordering::Relaxed);
    if !should_prune_replay(last_prune_secs, now_secs, REPLAY_PRUNE_THROTTLE_SECS) {
        return Ok(());
    }

    match LAST_REPLAY_PRUNE_AT_SECS.compare_exchange(
        last_prune_secs,
        now_secs,
        Ordering::SeqCst,
        Ordering::SeqCst,
    ) {
        Ok(_) => prune_replay_root(root, retention_days, max_bytes),
        Err(_) => Ok(()),
    }
}

fn should_prune_replay(last_prune_secs: u64, now_secs: u64, interval_secs: u64) -> bool {
    last_prune_secs == 0 || now_secs.saturating_sub(last_prune_secs) >= interval_secs
}

fn seconds_since_epoch(now: SystemTime) -> u64 {
    now.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
    use muninn::{
        InjectionRoute, InjectionRouteReason, InjectionTarget, PipelinePolicyApplied,
        ResolvedUtteranceConfig, TargetContextSnapshot,
    };
    use serde_json::Value;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;

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
        config.logging.replay_retain_audio = true;
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

    fn sample_resolved(root: &Path) -> ResolvedUtteranceConfig {
        ResolvedUtteranceConfig {
            target_context: TargetContextSnapshot {
                bundle_id: Some("com.openai.codex".to_string()),
                app_name: Some("Codex".to_string()),
                window_title: Some("muninn".to_string()),
                captured_at: "2026-03-06T10:00:00Z".to_string(),
            },
            matched_rule_id: Some("codex".to_string()),
            profile_id: "default".to_string(),
            voice_id: Some("codex_focus".to_string()),
            voice_glyph: Some('C'),
            fallback_reason: None,
            effective_config: sample_config(root),
        }
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
        let snapshot = redacted_config_snapshot(config);

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

        let sanitized = sanitized_envelope_for_replay(envelope);

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
    fn persist_replay_writes_record_and_audio_retention_artifact() {
        let root = temp_dir("persist");
        let resolved = sample_resolved(&root);
        let source_audio = root.join("source.wav");
        fs::write(&source_audio, b"wave").expect("write source audio");
        let recorded = RecordedAudio::new(&source_audio, 1450);

        let artifact_dir = persist_replay(
            resolved,
            sample_envelope(),
            sample_outcome(),
            sample_route(),
            recorded,
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
        assert_eq!(
            record["resolution"]["target_context"]["bundle_id"],
            Value::String("com.openai.codex".to_string())
        );
        assert_eq!(
            record["resolution"]["matched_rule_id"],
            Value::String("codex".to_string())
        );
        assert_eq!(
            record["resolution"]["voice_glyph"],
            Value::String("C".to_string())
        );
    }

    #[test]
    fn persist_replay_skips_audio_retention_when_disabled() {
        let root = temp_dir("persist-no-audio");
        let mut resolved = sample_resolved(&root);
        resolved.effective_config.logging.replay_retain_audio = false;
        let source_audio = root.join("source.wav");
        fs::write(&source_audio, b"wave").expect("write source audio");
        let recorded = RecordedAudio::new(&source_audio, 1450);

        let artifact_dir = persist_replay(
            resolved,
            sample_envelope(),
            sample_outcome(),
            sample_route(),
            recorded,
        )
        .expect("persist replay should succeed")
        .expect("replay should be written");

        let record: Value = serde_json::from_str(
            &fs::read_to_string(artifact_dir.join("record.json")).expect("read replay record"),
        )
        .expect("parse replay record");

        assert!(!artifact_dir.join("audio.wav").exists());
        assert_eq!(record["copied_audio_file"], Value::Null);
    }

    #[test]
    #[cfg(unix)]
    fn replay_audio_retention_prefers_hard_link() {
        let root = temp_dir("retain-link");
        let source_audio = root.join("source.wav");
        let artifact_dir = root.join("artifact");
        fs::create_dir_all(&artifact_dir).expect("create artifact dir");
        fs::write(&source_audio, b"wave").expect("write source audio");
        let recorded = RecordedAudio::new(&source_audio, 1450);

        let retained = retain_audio_if_available(&artifact_dir, &recorded, true)
            .expect("audio retention should succeed")
            .expect("audio file should be retained");
        let retained_path = artifact_dir.join(retained);
        let source_metadata = fs::metadata(&source_audio).expect("source metadata");
        let retained_metadata = fs::metadata(&retained_path).expect("retained metadata");

        assert_eq!(source_metadata.dev(), retained_metadata.dev());
        assert_eq!(source_metadata.ino(), retained_metadata.ino());
    }

    #[test]
    fn replay_audio_retention_falls_back_to_copy_when_link_fails() {
        let root = temp_dir("retain-copy");
        let source_audio = root.join("source.wav");
        let target_audio = root.join("artifact").join("audio.wav");
        fs::create_dir_all(target_audio.parent().expect("target parent")).expect("artifact dir");
        fs::write(&source_audio, b"wave").expect("write source audio");
        fs::write(&target_audio, b"stale").expect("seed target to force hard_link failure");

        retain_audio_file(&source_audio, &target_audio).expect("copy fallback should succeed");

        assert_eq!(
            fs::read(&target_audio).expect("read retained audio"),
            b"wave".to_vec()
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
        let resolved = ResolvedUtteranceConfig {
            target_context: TargetContextSnapshot {
                bundle_id: Some("com.apple.Terminal".to_string()),
                app_name: Some("Terminal".to_string()),
                window_title: Some("cargo test".to_string()),
                captured_at: "2026-03-06T11:00:00Z".to_string(),
            },
            matched_rule_id: Some("terminal".to_string()),
            profile_id: "default".to_string(),
            voice_id: Some("terminal_terse".to_string()),
            voice_glyph: Some('T'),
            fallback_reason: None,
            effective_config: config.clone(),
        };

        let artifact_dir = persist_replay(
            resolved,
            sample_envelope(),
            sample_outcome(),
            sample_route(),
            recorded,
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
        let resolved = sample_resolved(&root);
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

        let artifact_dir =
            persist_replay(resolved, envelope, sample_outcome(), sample_route(), recorded)
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

    #[test]
    fn prune_throttle_requires_interval_gap() {
        assert!(should_prune_replay(0, 10, 60));
        assert!(!should_prune_replay(100, 120, 60));
        assert!(should_prune_replay(100, 160, 60));
    }
}

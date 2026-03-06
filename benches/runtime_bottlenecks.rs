use std::fs;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion, Throughput,
};
use muninn::audio::benchmark_render_output_checksum;
use muninn::config::{
    AppConfig, OnErrorPolicy, PayloadFormat, PipelineConfig, PipelineStepConfig, ProfileConfig,
    ProfileRuleConfig, RecordingConfig, StepIoMode, TranscriptOverrides, VoiceConfig,
};
use muninn::scoring::{apply_scored_replacements_to_envelope, Thresholds};
use muninn::{
    InProcessStepError, InProcessStepExecutor, InjectionRoute, InjectionRouteReason,
    InjectionTarget, MuninnEnvelopeV1, PipelineOutcome, PipelinePolicyApplied, PipelineRunner,
    PipelineTraceEntry, RecordedAudio, ResolvedUtteranceConfig, TargetContextSnapshot,
};
use serde_json::json;
use tempfile::{NamedTempFile, TempDir};

const BENCH_TIMESTAMP: &str = "2026-03-06T00:00:00Z";

mod google_tool_impl {
    #![allow(dead_code)]
    #![allow(clippy::items_after_test_module)]
    #![allow(unused_imports)]
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/src/stt_google_tool.rs"
    ));

    pub fn bench_google_request_body(
        wav_path: &std::path::Path,
        sample_rate_hz: u32,
        channels: u16,
        model: Option<&str>,
    ) -> Result<usize, String> {
        google_request_body(
            wav_path,
            WavMetadata {
                sample_rate_hz,
                channels,
            },
            model,
        )
        .map(|body| body.len())
        .map_err(|error| error.message().to_string())
    }
}

mod replay_impl {
    #![allow(dead_code)]
    #![allow(unused_imports)]
    include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/replay.rs"));
}

struct BenchExecutor;

struct ReplayBenchInput {
    _temp_dir: TempDir,
    resolved: ResolvedUtteranceConfig,
    input_envelope: MuninnEnvelopeV1,
    outcome: PipelineOutcome,
    route: InjectionRoute,
    recorded: RecordedAudio,
}

#[async_trait]
impl InProcessStepExecutor for BenchExecutor {
    async fn try_execute(
        &self,
        step: &PipelineStepConfig,
        input: &MuninnEnvelopeV1,
    ) -> Option<Result<MuninnEnvelopeV1, InProcessStepError>> {
        if step.cmd != "bench_builtin" {
            return None;
        }

        let mut envelope = input.clone();
        envelope
            .extra
            .insert(format!("bench_{}", step.id), json!(step.timeout_ms));

        if step.id == "normalize" && envelope.transcript.raw_text.is_none() {
            envelope.transcript.raw_text = Some("benchmark transcript".to_string());
        }
        if step.id == "finalize" && envelope.output.final_text.is_none() {
            envelope.output.final_text = envelope.transcript.raw_text.clone();
        }

        Some(Ok(envelope))
    }
}

fn bench_audio_transform(c: &mut Criterion) {
    let mut group = c.benchmark_group("audio_transform");

    let stereo_48khz = synthetic_pcm(48_000, 2, 10);
    group.throughput(Throughput::Elements(stereo_48khz.len() as u64));
    group.bench_function("48khz_stereo_to_16khz_mono_10s", |b| {
        let output = RecordingConfig::default();
        b.iter(|| {
            black_box(benchmark_render_output_checksum(
                black_box(&stereo_48khz),
                48_000,
                2,
                &output,
            ))
        });
    });

    let mono_16khz = synthetic_pcm(16_000, 1, 10);
    group.throughput(Throughput::Elements(mono_16khz.len() as u64));
    group.bench_function("16khz_mono_passthrough_10s", |b| {
        let output = RecordingConfig::default();
        b.iter(|| {
            black_box(benchmark_render_output_checksum(
                black_box(&mono_16khz),
                16_000,
                1,
                &output,
            ))
        });
    });

    group.finish();
}

fn bench_envelope_json_roundtrip(c: &mut Criterion) {
    let mut group = c.benchmark_group("envelope_json_roundtrip");

    for (label, envelope) in [
        ("medium", runner_envelope(96, 24)),
        ("large", runner_envelope(384, 96)),
    ] {
        let encoded = serde_json::to_vec(&envelope).expect("serialize benchmark envelope");
        group.throughput(Throughput::Bytes(encoded.len() as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(label),
            &encoded,
            |b, encoded| {
                b.iter(|| {
                    let decoded: MuninnEnvelopeV1 =
                        serde_json::from_slice(black_box(encoded)).expect("decode envelope");
                    black_box(serde_json::to_vec(&decoded).expect("encode envelope"));
                });
            },
        );
    }

    group.finish();
}

fn bench_google_request_body(c: &mut Criterion) {
    let mut group = c.benchmark_group("google_request_body");

    for (label, sample_rate_hz, channels) in [
        ("mono_16khz_15s", 16_000_u32, 1_u16),
        ("stereo_48khz_15s", 48_000_u32, 2_u16),
    ] {
        let wav = synthetic_wav_fixture(sample_rate_hz, channels, 15);
        let wav_bytes = fs::metadata(wav.path()).expect("wav metadata").len();
        group.throughput(Throughput::Bytes(wav_bytes));
        group.bench_with_input(BenchmarkId::from_parameter(label), &label, |b, _| {
            b.iter(|| {
                black_box(
                    google_tool_impl::bench_google_request_body(
                        black_box(wav.path()),
                        sample_rate_hz,
                        channels,
                        Some("latest_long"),
                    )
                    .expect("google request body"),
                );
            });
        });
    }

    group.finish();
}

fn bench_config_resolution(c: &mut Criterion) {
    let mut group = c.benchmark_group("config_resolution");

    for rule_count in [32_usize, 256] {
        let (config, target_context) = profiled_config(rule_count);
        group.throughput(Throughput::Elements(rule_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(rule_count),
            &rule_count,
            |b, _| {
                b.iter_batched(
                    || target_context.clone(),
                    |target_context| {
                        black_box(config.resolve_effective_config(target_context));
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn bench_scoring(c: &mut Criterion) {
    let mut group = c.benchmark_group("replacement_scoring");
    let thresholds = Thresholds::default();

    for span_count in [32_usize, 128] {
        let envelope = scoring_envelope(span_count, 4);
        group.throughput(Throughput::Elements(span_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(span_count),
            &span_count,
            |b, _| {
                b.iter_batched(
                    || envelope.clone(),
                    |mut envelope| {
                        black_box(apply_scored_replacements_to_envelope(
                            &mut envelope,
                            &thresholds,
                        ));
                        black_box(&envelope.output.final_text);
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }

    group.finish();
}

fn bench_pipeline_runner(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime for benches");
    let runner = PipelineRunner::with_in_process_step_executor(true, Arc::new(BenchExecutor));
    let mut group = c.benchmark_group("pipeline_runner");

    for step_count in [3_usize, 6] {
        let pipeline = in_process_pipeline(step_count);
        let envelope = runner_envelope(96, 24);
        group.throughput(Throughput::Elements(step_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(step_count),
            &step_count,
            |b, _| {
                b.iter(|| {
                    black_box(runtime.block_on(runner.run(envelope.clone(), &pipeline)));
                });
            },
        );
    }

    group.finish();
}

fn bench_replay_persist(c: &mut Criterion) {
    let mut group = c.benchmark_group("replay_persist");
    group.sample_size(10);
    group.throughput(Throughput::Bytes((64 * 1024) as u64));

    for (label, replay_retain_audio) in [("metadata_only", false), ("retain_audio", true)] {
        group.bench_with_input(
            BenchmarkId::from_parameter(label),
            &replay_retain_audio,
            |b, replay_retain_audio| {
                b.iter_batched(
                    || replay_bench_input(*replay_retain_audio),
                    |input| {
                        let artifact_dir = replay_impl::persist_replay(
                            input.resolved,
                            input.input_envelope,
                            input.outcome,
                            input.route,
                            input.recorded,
                        )
                        .expect("persist replay")
                        .expect("artifact dir");
                        black_box(artifact_dir);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn synthetic_pcm(sample_rate_hz: u32, channels: u16, seconds: usize) -> Vec<i16> {
    let total_samples = sample_rate_hz as usize * channels as usize * seconds;
    (0..total_samples)
        .map(|index| {
            let phase = ((index * 97) % u16::MAX as usize) as i32;
            (phase - i16::MAX as i32) as i16
        })
        .collect()
}

fn synthetic_wav_fixture(sample_rate_hz: u32, channels: u16, seconds: usize) -> NamedTempFile {
    let file = NamedTempFile::new().expect("create wav fixture");
    let spec = hound::WavSpec {
        channels,
        sample_rate: sample_rate_hz,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let samples = synthetic_pcm(sample_rate_hz, channels, seconds);
    let mut writer = hound::WavWriter::create(file.path(), spec).expect("create wav writer");
    for sample in samples {
        writer.write_sample(sample).expect("write wav sample");
    }
    writer.finalize().expect("finalize wav writer");
    file
}

fn profiled_config(rule_count: usize) -> (AppConfig, TargetContextSnapshot) {
    let mut config = AppConfig::launchable_default();
    config.app.profile = "default".to_string();
    config.voices.clear();
    config.profiles.clear();
    config.profile_rules.clear();

    config.voices.insert(
        "voice_default".to_string(),
        VoiceConfig {
            indicator_glyph: Some("D".to_string()),
            system_prompt: Some("Default prompt".to_string()),
            ..VoiceConfig::default()
        },
    );
    config.profiles.insert(
        "default".to_string(),
        ProfileConfig {
            voice: Some("voice_default".to_string()),
            ..ProfileConfig::default()
        },
    );

    for index in 0..rule_count {
        let voice_id = format!("voice_{index}");
        let profile_id = format!("profile_{index}");

        config.voices.insert(
            voice_id.clone(),
            VoiceConfig {
                indicator_glyph: Some("T".to_string()),
                system_prompt: Some(format!("Prompt {index}")),
                temperature: Some(0.0),
                max_output_tokens: Some(128),
                ..VoiceConfig::default()
            },
        );
        config.profiles.insert(
            profile_id.clone(),
            ProfileConfig {
                voice: Some(voice_id),
                transcript: Some(TranscriptOverrides {
                    system_prompt: Some(format!("profile prompt {index}")),
                }),
                ..ProfileConfig::default()
            },
        );

        config.profile_rules.push(ProfileRuleConfig {
            id: format!("rule_{index}"),
            profile: profile_id,
            bundle_id: Some(if index + 1 == rule_count {
                "com.example.target".to_string()
            } else {
                format!("com.example.miss.{index}")
            }),
            ..ProfileRuleConfig::default()
        });
    }

    config.validate().expect("bench config should validate");

    (
        config,
        TargetContextSnapshot {
            bundle_id: Some("com.example.target".to_string()),
            app_name: Some("TargetApp".to_string()),
            window_title: Some("Muninn Bench".to_string()),
            captured_at: BENCH_TIMESTAMP.to_string(),
        },
    )
}

fn scoring_envelope(span_count: usize, candidates_per_span: usize) -> MuninnEnvelopeV1 {
    let mut raw_text = String::new();
    let mut envelope = MuninnEnvelopeV1::new("utt-bench", BENCH_TIMESTAMP);

    for span_index in 0..span_count {
        if !raw_text.is_empty() {
            raw_text.push(' ');
        }

        let from = format!("pth{span_index}");
        let start = raw_text.len();
        raw_text.push_str(&from);
        let end = raw_text.len();

        envelope
            .uncertain_spans
            .push(json!({"start": start, "end": end, "text": from}));

        for candidate_index in 0..candidates_per_span {
            let score = match candidate_index {
                0 => 0.96,
                1 => 0.81,
                2 => 0.77,
                _ => 0.70,
            };
            envelope.replacements.push(json!({
                "from": format!("pth{span_index}"),
                "to": format!("path_{span_index}_{candidate_index}"),
                "score": score,
            }));
        }
    }

    envelope.transcript.raw_text = Some(raw_text);
    envelope
}

fn in_process_pipeline(step_count: usize) -> PipelineConfig {
    PipelineConfig {
        deadline_ms: 40_000,
        payload_format: PayloadFormat::JsonObject,
        steps: (0..step_count)
            .map(|index| PipelineStepConfig {
                id: match index {
                    0 => "normalize".to_string(),
                    x if x + 1 == step_count => "finalize".to_string(),
                    _ => format!("stage_{index}"),
                },
                cmd: "bench_builtin".to_string(),
                args: Vec::new(),
                io_mode: StepIoMode::EnvelopeJson,
                timeout_ms: 250,
                on_error: OnErrorPolicy::Continue,
            })
            .collect(),
    }
}

fn runner_envelope(span_count: usize, error_count: usize) -> MuninnEnvelopeV1 {
    let mut envelope = scoring_envelope(span_count, 3)
        .with_audio(Some("/tmp/muninn-bench.wav".to_string()), 1_450)
        .with_transcript_system_prompt("Prefer minimal corrections.");

    for error_index in 0..error_count {
        envelope
            .errors
            .push(json!({"code": format!("warn_{error_index}"), "message": "bench warning"}));
    }
    envelope.extra.insert(
        "target_context".to_string(),
        json!({
            "bundle_id": "com.example.target",
            "app_name": "TargetApp",
            "window_title": "Muninn Bench",
        }),
    );
    envelope
}

fn replay_bench_input(replay_retain_audio: bool) -> ReplayBenchInput {
    let temp_dir = TempDir::new().expect("create replay temp dir");
    let replay_root = temp_dir.path().join("replay");
    let source_audio = temp_dir.path().join("source.wav");
    fs::write(&source_audio, vec![0_u8; 64 * 1024]).expect("write synthetic replay audio");

    let mut config = AppConfig::launchable_default();
    config.logging.replay_enabled = true;
    config.logging.replay_retain_audio = replay_retain_audio;
    config.logging.replay_dir = replay_root;
    config.logging.replay_retention_days = 7;
    config.logging.replay_max_bytes = 32 * 1024 * 1024;
    config.providers.openai.api_key = Some("bench-openai-key".to_string());
    config.providers.google.api_key = Some("bench-google-key".to_string());
    config.providers.google.token = Some("bench-google-token".to_string());

    let input_envelope = runner_envelope(192, 48);
    let route_text = input_envelope
        .transcript
        .raw_text
        .clone()
        .expect("runner envelope transcript");
    let outcome = PipelineOutcome::Completed {
        envelope: input_envelope.clone(),
        trace: vec![
            PipelineTraceEntry {
                id: "stt_openai".to_string(),
                duration_ms: 18,
                timed_out: false,
                exit_status: Some(0),
                policy_applied: PipelinePolicyApplied::None,
                stderr: "synthetic stderr".to_string(),
            },
            PipelineTraceEntry {
                id: "refine".to_string(),
                duration_ms: 6,
                timed_out: false,
                exit_status: Some(0),
                policy_applied: PipelinePolicyApplied::None,
                stderr: "synthetic stderr".to_string(),
            },
        ],
    };

    ReplayBenchInput {
        _temp_dir: temp_dir,
        resolved: ResolvedUtteranceConfig {
            target_context: TargetContextSnapshot {
                bundle_id: Some("com.openai.codex".to_string()),
                app_name: Some("Codex".to_string()),
                window_title: Some("Muninn Bench".to_string()),
                captured_at: BENCH_TIMESTAMP.to_string(),
            },
            matched_rule_id: Some("codex-rule".to_string()),
            profile_id: "default".to_string(),
            voice_id: Some("voice_default".to_string()),
            voice_glyph: Some('D'),
            fallback_reason: None,
            effective_config: config,
        },
        input_envelope,
        outcome,
        route: InjectionRoute {
            target: InjectionTarget::TranscriptRawText(route_text),
            reason: InjectionRouteReason::SelectedTranscriptRawText,
            pipeline_stop_reason: None,
        },
        recorded: RecordedAudio::new(&source_audio, 1_450),
    }
}

criterion_group!(
    name = runtime_bottlenecks;
    config = Criterion::default()
        .sample_size(20)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(2));
    targets =
        bench_audio_transform,
        bench_envelope_json_roundtrip,
        bench_google_request_body,
        bench_config_resolution,
        bench_scoring,
        bench_pipeline_runner,
        bench_replay_persist
);
criterion_main!(runtime_bottlenecks);

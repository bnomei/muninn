//! `tracing` initialization and structured diagnostic helpers.
//!
//! On macOS, routes [`TARGET_*`] constants into unified logging (oslog) by target.
//! [`init_logging`] runs once during bootstrap before the event loop starts.

use anyhow::{anyhow, Result};
use muninn::AppConfig;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};
use tracing::{error, info, warn};
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

#[cfg(target_os = "macos")]
use tracing_oslog::OsLogger;
#[cfg(target_os = "macos")]
use tracing_subscriber::filter::filter_fn;

/// tracing target for runtime lifecycle and worker events.
pub const TARGET_RUNTIME: &str = muninn::TARGET_RUNTIME;
/// tracing target for pipeline execution and injection.
pub const TARGET_PIPELINE: &str = muninn::TARGET_PIPELINE;
/// tracing target for STT and refine provider calls.
pub const TARGET_PROVIDER: &str = muninn::TARGET_PROVIDER;
/// tracing target for config load, reload, and watchers.
pub const TARGET_CONFIG: &str = muninn::TARGET_CONFIG;
/// tracing target for global hotkey listener events.
pub const TARGET_HOTKEY: &str = muninn::TARGET_HOTKEY;
/// tracing target for audio capture and permission prompts.
pub const TARGET_RECORDING: &str = muninn::TARGET_RECORDING;
/// tracing target for logs that do not match a dedicated subsystem target.
pub const TARGET_DEFAULT: &str = muninn::TARGET_DEFAULT;

#[cfg(target_os = "macos")]
const OSLOG_SUBSYSTEM: &str = "com.bnomei.muninn";

/// Structured diagnostic events captured for tests and mirrored to tracing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiagnosticEvent {
    /// Main-thread event-loop proxy rejected a [`UserEvent`] delivery.
    ProxySendFailed {
        target: &'static str,
        context: &'static str,
        detail: String,
    },
    /// Background config or preview watcher thread started.
    WatcherStarted {
        kind: &'static str,
        path: Option<String>,
    },
    /// Config file change detected on disk.
    ConfigChanged {
        path: String,
    },
    /// Recording capture began after permissions passed.
    RecordingStarted {
        profile_id: String,
        voice_id: Option<String>,
        voice_glyph: Option<char>,
        recording_mode: &'static str,
        sample_rate_hz: u32,
        mono: bool,
        source: &'static str,
    },
    /// Runtime worker failed to build or exited with an error.
    RuntimeWorkerFailed {
        stage: &'static str,
        detail: String,
    },
}

#[cfg(test)]
static DIAGNOSTIC_EVENTS: OnceLock<Mutex<Vec<DiagnosticEvent>>> = OnceLock::new();

fn record_diagnostic_event(event: DiagnosticEvent) {
    #[cfg(test)]
    {
        DIAGNOSTIC_EVENTS
            .get_or_init(|| Mutex::new(Vec::new()))
            .lock()
            .expect("diagnostic event buffer lock should not be poisoned")
            .push(event);
    }

    #[cfg(not(test))]
    let _ = event;
}

/// Log a failed delivery to the tao event-loop proxy.
pub(crate) fn log_proxy_send_failed(context: &'static str, detail: impl Into<String>) {
    let detail = detail.into();
    record_diagnostic_event(DiagnosticEvent::ProxySendFailed {
        target: TARGET_RUNTIME,
        context,
        detail: detail.clone(),
    });
    warn!(
        target: TARGET_RUNTIME,
        context,
        detail = detail.as_str(),
        "failed to deliver event loop message"
    );
}

/// Log startup of a background config or preview watcher.
pub(crate) fn log_watcher_started(kind: &'static str, path: Option<&std::path::Path>) {
    let path = path.map(|value| value.display().to_string());
    record_diagnostic_event(DiagnosticEvent::WatcherStarted {
        kind,
        path: path.clone(),
    });
    match path {
        Some(path) => {
            info!(target: TARGET_CONFIG, kind, path, "started background watcher");
        }
        None => {
            info!(target: TARGET_CONFIG, kind, "started background watcher");
        }
    }
}

/// Log detection of an on-disk config file change.
pub(crate) fn log_config_changed(path: impl AsRef<std::path::Path>) {
    let path = path.as_ref().display().to_string();
    record_diagnostic_event(DiagnosticEvent::ConfigChanged { path: path.clone() });
    info!(target: TARGET_CONFIG, path, "detected config change");
}

/// Log structured metadata when recording capture starts.
pub(crate) fn log_recording_started(
    profile_id: &str,
    voice_id: Option<&str>,
    voice_glyph: Option<char>,
    recording_mode: &'static str,
    sample_rate_hz: u32,
    mono: bool,
    source: &'static str,
) {
    record_diagnostic_event(DiagnosticEvent::RecordingStarted {
        profile_id: profile_id.to_string(),
        voice_id: voice_id.map(str::to_string),
        voice_glyph,
        recording_mode,
        sample_rate_hz,
        mono,
        source,
    });
    info!(
        target: TARGET_RUNTIME,
        profile_id,
        voice_id = voice_id.unwrap_or(""),
        voice_glyph = ?voice_glyph,
        recording_mode,
        sample_rate_hz,
        mono,
        source,
        "recording started"
    );
}

/// Log a fatal runtime worker build or run failure.
pub(crate) fn log_runtime_worker_failed(stage: &'static str, detail: impl Into<String>) {
    let detail = detail.into();
    record_diagnostic_event(DiagnosticEvent::RuntimeWorkerFailed {
        stage,
        detail: detail.clone(),
    });
    error!(
        target: TARGET_RUNTIME,
        stage,
        detail = detail.as_str(),
        "runtime worker failure"
    );
}

#[cfg(test)]
pub(crate) fn take_diagnostic_events() -> Vec<DiagnosticEvent> {
    DIAGNOSTIC_EVENTS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .expect("diagnostic event buffer lock should not be poisoned")
        .drain(..)
        .collect()
}

/// Install the global `tracing` subscriber and emit replay settings.
///
/// Honors `RUST_LOG` when set; otherwise defaults to `info`. On macOS, mirrors
/// subsystem targets into unified logging (oslog).
pub fn init_logging(config: &AppConfig) -> Result<()> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = tracing_subscriber::fmt::layer().with_target(true).compact();

    let subscriber = tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer);

    #[cfg(target_os = "macos")]
    let subscriber = subscriber
        .with(
            OsLogger::new(OSLOG_SUBSYSTEM, TARGET_RUNTIME)
                .with_filter(filter_fn(|metadata| metadata.target() == TARGET_RUNTIME)),
        )
        .with(
            OsLogger::new(OSLOG_SUBSYSTEM, TARGET_PIPELINE)
                .with_filter(filter_fn(|metadata| metadata.target() == TARGET_PIPELINE)),
        )
        .with(
            OsLogger::new(OSLOG_SUBSYSTEM, TARGET_PROVIDER)
                .with_filter(filter_fn(|metadata| metadata.target() == TARGET_PROVIDER)),
        )
        .with(
            OsLogger::new(OSLOG_SUBSYSTEM, TARGET_CONFIG)
                .with_filter(filter_fn(|metadata| metadata.target() == TARGET_CONFIG)),
        )
        .with(
            OsLogger::new(OSLOG_SUBSYSTEM, TARGET_HOTKEY)
                .with_filter(filter_fn(|metadata| metadata.target() == TARGET_HOTKEY)),
        )
        .with(
            OsLogger::new(OSLOG_SUBSYSTEM, TARGET_RECORDING)
                .with_filter(filter_fn(|metadata| metadata.target() == TARGET_RECORDING)),
        )
        .with(
            OsLogger::new(OSLOG_SUBSYSTEM, TARGET_DEFAULT).with_filter(filter_fn(|metadata| {
                let target = metadata.target();
                target != TARGET_RUNTIME
                    && target != TARGET_PIPELINE
                    && target != TARGET_PROVIDER
                    && target != TARGET_CONFIG
                    && target != TARGET_HOTKEY
                    && target != TARGET_RECORDING
            })),
        );

    subscriber
        .try_init()
        .map_err(|error| anyhow!("initializing tracing subscriber: {error}"))?;

    info!(
        target: TARGET_RUNTIME,
        replay_enabled = config.logging.replay_enabled,
        replay_detail = ?config.logging.replay_detail,
        replay_dir = %config.logging.replay_dir.display(),
        replay_retention_days = config.logging.replay_retention_days,
        replay_max_bytes = config.logging.replay_max_bytes,
        "logging initialized"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_started_helper_records_structured_event() {
        let _ = take_diagnostic_events();

        log_recording_started(
            "engineering",
            Some("irish"),
            Some('I'),
            "push_to_talk",
            16_000,
            true,
            "hotkey",
        );

        assert_eq!(
            take_diagnostic_events(),
            vec![DiagnosticEvent::RecordingStarted {
                profile_id: "engineering".to_string(),
                voice_id: Some("irish".to_string()),
                voice_glyph: Some('I'),
                recording_mode: "push_to_talk",
                sample_rate_hz: 16_000,
                mono: true,
                source: "hotkey",
            }]
        );
    }

    #[test]
    fn proxy_send_failure_helper_records_structured_event() {
        let _ = take_diagnostic_events();

        log_proxy_send_failed("config_reload", "event loop closed");

        assert_eq!(
            take_diagnostic_events(),
            vec![DiagnosticEvent::ProxySendFailed {
                target: TARGET_RUNTIME,
                context: "config_reload",
                detail: "event loop closed".to_string(),
            }]
        );
    }
}

use anyhow::{anyhow, Result};
use muninn::AppConfig;
use tracing::info;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

#[cfg(target_os = "macos")]
use tracing_oslog::OsLogger;
#[cfg(target_os = "macos")]
use tracing_subscriber::filter::filter_fn;

pub const TARGET_RUNTIME: &str = "runtime";
pub const TARGET_PIPELINE: &str = "pipeline";
pub const TARGET_PROVIDER: &str = "provider";
pub const TARGET_CONFIG: &str = "config";
pub const TARGET_HOTKEY: &str = "hotkey";
pub const TARGET_RECORDING: &str = "recording";
pub const TARGET_DEFAULT: &str = "default";

#[cfg(target_os = "macos")]
const OSLOG_SUBSYSTEM: &str = "com.bnomei.muninn";

pub fn init_logging(config: &AppConfig) -> Result<()> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .compact();

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
        replay_dir = %config.logging.replay_dir.display(),
        replay_retention_days = config.logging.replay_retention_days,
        replay_max_bytes = config.logging.replay_max_bytes,
        "logging initialized"
    );

    Ok(())
}

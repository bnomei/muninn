use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use muninn::{capture_frontmost_target_context, AppConfig, TargetContextSnapshot};
use tao::event_loop::EventLoopProxy;

use crate::logging;
use crate::runtime_tray::send_user_event;
use crate::runtime_tray::UserEvent;

const CONFIG_WATCH_POLL_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
const CONFIG_WATCH_POLL_MAX_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
const PREVIEW_CONTEXT_POLL_MIN_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(400);
const PREVIEW_CONTEXT_POLL_MAX_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

pub fn spawn_config_watcher(config_path: PathBuf, proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        logging::log_watcher_started("config", Some(config_path.as_path()));
        let mut last_fingerprint = read_config_fingerprint(&config_path);
        let mut last_snapshot = read_config_snapshot(&config_path);
        let mut poll_interval = CONFIG_WATCH_POLL_MIN_INTERVAL;

        loop {
            std::thread::sleep(poll_interval);

            let fingerprint = read_config_fingerprint(&config_path);
            if fingerprint == last_fingerprint {
                poll_interval = next_poll_interval(
                    poll_interval,
                    false,
                    CONFIG_WATCH_POLL_MIN_INTERVAL,
                    CONFIG_WATCH_POLL_MAX_INTERVAL,
                );
                continue;
            }

            let snapshot = read_config_snapshot(&config_path);
            if snapshot == last_snapshot {
                last_fingerprint = fingerprint;
                poll_interval = next_poll_interval(
                    poll_interval,
                    false,
                    CONFIG_WATCH_POLL_MIN_INTERVAL,
                    CONFIG_WATCH_POLL_MAX_INTERVAL,
                );
                continue;
            }

            logging::log_config_changed(&config_path);

            match &snapshot {
                ConfigSnapshot::Contents(contents) => match AppConfig::from_toml_str(contents) {
                    Ok(config) => {
                        send_user_event(
                            &proxy,
                            UserEvent::ConfigReloaded(Box::new(config)),
                            "config_reload_success",
                        );
                    }
                    Err(error) => {
                        send_user_event(
                            &proxy,
                            UserEvent::ConfigReloadFailed(format!(
                                "{}: {error}",
                                config_path.display()
                            )),
                            "config_reload_parse_failed",
                        );
                    }
                },
                ConfigSnapshot::Missing => {
                    send_user_event(
                        &proxy,
                        UserEvent::ConfigReloadFailed(format!(
                            "{}: config file missing",
                            config_path.display()
                        )),
                        "config_reload_missing",
                    );
                }
                ConfigSnapshot::Unreadable(error) => {
                    send_user_event(
                        &proxy,
                        UserEvent::ConfigReloadFailed(format!(
                            "{}: {error}",
                            config_path.display()
                        )),
                        "config_reload_unreadable",
                    );
                }
            }

            last_fingerprint = fingerprint;
            last_snapshot = snapshot;
            poll_interval = CONFIG_WATCH_POLL_MIN_INTERVAL;
        }
    });
}

pub fn spawn_preview_context_watcher(proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        logging::log_watcher_started("preview_context", None::<&Path>);
        let mut last_key = preview_context_key(&capture_frontmost_target_context());
        let mut poll_interval = PREVIEW_CONTEXT_POLL_MIN_INTERVAL;
        loop {
            std::thread::sleep(poll_interval);
            let context = capture_frontmost_target_context();
            let key = preview_context_key(&context);
            if key != last_key {
                last_key = key;
                poll_interval = PREVIEW_CONTEXT_POLL_MIN_INTERVAL;
                send_user_event(
                    &proxy,
                    UserEvent::PreviewContextUpdated(context),
                    "preview_context_updated",
                );
            } else {
                poll_interval = next_poll_interval(
                    poll_interval,
                    false,
                    PREVIEW_CONTEXT_POLL_MIN_INTERVAL,
                    PREVIEW_CONTEXT_POLL_MAX_INTERVAL,
                );
            }
        }
    });
}

fn next_poll_interval(
    current: std::time::Duration,
    observed_change: bool,
    min: std::time::Duration,
    max: std::time::Duration,
) -> std::time::Duration {
    if observed_change {
        return min;
    }

    let doubled_ms = current.as_millis().saturating_mul(2);
    let capped_ms = doubled_ms.min(max.as_millis());
    std::time::Duration::from_millis(capped_ms as u64)
}

pub(crate) fn preview_context_key(
    context: &TargetContextSnapshot,
) -> (Option<String>, Option<String>, Option<String>) {
    (
        context.bundle_id.clone(),
        context.app_name.clone(),
        context.window_title.clone(),
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConfigFingerprint {
    Missing,
    Metadata {
        modified_at: Option<SystemTime>,
        len: u64,
    },
    Unreadable(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigSnapshot {
    Missing,
    Contents(String),
    Unreadable(String),
}

pub(crate) fn read_config_fingerprint(path: &Path) -> ConfigFingerprint {
    match fs::metadata(path) {
        Ok(metadata) => ConfigFingerprint::Metadata {
            modified_at: metadata.modified().ok(),
            len: metadata.len(),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ConfigFingerprint::Missing,
        Err(error) => ConfigFingerprint::Unreadable(error.to_string()),
    }
}

fn read_config_snapshot(path: &Path) -> ConfigSnapshot {
    match fs::read_to_string(path) {
        Ok(contents) => ConfigSnapshot::Contents(contents),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ConfigSnapshot::Missing,
        Err(error) => ConfigSnapshot::Unreadable(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_poll_interval_doubles_until_maximum() {
        let min = std::time::Duration::from_millis(250);
        let max = std::time::Duration::from_secs(2);

        assert_eq!(
            next_poll_interval(min, false, min, max),
            std::time::Duration::from_millis(500)
        );
        assert_eq!(
            next_poll_interval(std::time::Duration::from_secs(1), false, min, max),
            max
        );
        assert_eq!(next_poll_interval(max, false, min, max), max);
    }

    #[test]
    fn next_poll_interval_resets_to_minimum_after_change() {
        let min = std::time::Duration::from_millis(250);
        let max = std::time::Duration::from_secs(2);

        assert_eq!(
            next_poll_interval(std::time::Duration::from_secs(2), true, min, max),
            min
        );
    }
}

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use muninn::{capture_frontmost_target_context, AppConfig, TargetContextSnapshot};
use tao::event_loop::EventLoopProxy;

use crate::runtime_tray::UserEvent;

const PREVIEW_CONTEXT_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(400);

pub fn spawn_config_watcher(config_path: PathBuf, proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        let mut last_fingerprint = read_config_fingerprint(&config_path);
        let mut last_snapshot = read_config_snapshot(&config_path);

        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));

            let fingerprint = read_config_fingerprint(&config_path);
            if fingerprint == last_fingerprint {
                continue;
            }

            let snapshot = read_config_snapshot(&config_path);
            if snapshot == last_snapshot {
                last_fingerprint = fingerprint;
                continue;
            }

            match &snapshot {
                ConfigSnapshot::Contents(contents) => match AppConfig::from_toml_str(contents) {
                    Ok(config) => {
                        let _ = proxy.send_event(UserEvent::ConfigReloaded(Box::new(config)));
                    }
                    Err(error) => {
                        let _ = proxy.send_event(UserEvent::ConfigReloadFailed(format!(
                            "{}: {error}",
                            config_path.display()
                        )));
                    }
                },
                ConfigSnapshot::Missing => {
                    let _ = proxy.send_event(UserEvent::ConfigReloadFailed(format!(
                        "{}: config file missing",
                        config_path.display()
                    )));
                }
                ConfigSnapshot::Unreadable(error) => {
                    let _ = proxy.send_event(UserEvent::ConfigReloadFailed(format!(
                        "{}: {error}",
                        config_path.display()
                    )));
                }
            }

            last_fingerprint = fingerprint;
            last_snapshot = snapshot;
        }
    });
}

pub fn spawn_preview_context_watcher(proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        let mut last_key = preview_context_key(&capture_frontmost_target_context());
        loop {
            std::thread::sleep(PREVIEW_CONTEXT_POLL_INTERVAL);
            let context = capture_frontmost_target_context();
            let key = preview_context_key(&context);
            if key != last_key {
                last_key = key;
                let _ = proxy.send_event(UserEvent::PreviewContextUpdated(context));
            }
        }
    });
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
    Metadata { modified_at: SystemTime, len: u64 },
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
        Ok(metadata) => match metadata.modified() {
            Ok(modified_at) => ConfigFingerprint::Metadata {
                modified_at,
                len: metadata.len(),
            },
            Err(error) => ConfigFingerprint::Unreadable(error.to_string()),
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

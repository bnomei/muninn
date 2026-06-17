use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ReplayDetailMode {
    #[default]
    Minimal,
    FullDebug,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    pub replay_enabled: bool,
    pub replay_detail: ReplayDetailMode,
    pub replay_retain_audio: bool,
    pub replay_dir: PathBuf,
    pub replay_retention_days: u32,
    pub replay_max_bytes: u64,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            replay_enabled: false,
            replay_detail: ReplayDetailMode::Minimal,
            replay_retain_audio: false,
            replay_dir: PathBuf::from("~/.local/state/muninn/replay"),
            replay_retention_days: 7,
            replay_max_bytes: 52_428_800,
        }
    }
}

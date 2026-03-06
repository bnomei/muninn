use std::collections::HashSet;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const DEFAULT_CONFIG_FILE_NAME: &str = "config.toml";
const DEFAULT_CONFIG_DIR_NAME: &str = "muninn";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub app: AppSettings,
    pub hotkeys: HotkeysConfig,
    pub indicator: IndicatorConfig,
    pub pipeline: PipelineConfig,
    pub scoring: ScoringConfig,
    pub transcript: TranscriptConfig,
    pub refine: RefineConfig,
    pub logging: LoggingConfig,
    pub providers: ProvidersConfig,
}

impl AppConfig {
    pub fn load() -> Result<Self, ConfigError> {
        let path = resolve_config_path()?;
        Self::load_or_create_default(path)
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            return Err(ConfigError::NotFound {
                path: path.to_path_buf(),
            });
        }

        let raw = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;

        let config: Self = toml::from_str(&raw).map_err(|source| ConfigError::ParseTomlAtPath {
            path: path.to_path_buf(),
            source,
        })?;
        config.validate()?;

        Ok(config)
    }

    pub fn from_toml_str(raw: &str) -> Result<Self, ConfigError> {
        let config: Self =
            toml::from_str(raw).map_err(|source| ConfigError::ParseToml { source })?;
        config.validate()?;
        Ok(config)
    }

    pub fn launchable_default() -> Self {
        let mut config = Self::default();
        config.pipeline.deadline_ms = 40_000;
        config.pipeline.steps = vec![
            PipelineStepConfig {
                id: "stt_openai".to_string(),
                cmd: "stt_openai".to_string(),
                args: Vec::new(),
                io_mode: StepIoMode::Auto,
                timeout_ms: 18_000,
                on_error: OnErrorPolicy::Continue,
            },
            PipelineStepConfig {
                id: "stt_google".to_string(),
                cmd: "stt_google".to_string(),
                args: Vec::new(),
                io_mode: StepIoMode::Auto,
                timeout_ms: 18_000,
                on_error: OnErrorPolicy::Abort,
            },
            PipelineStepConfig {
                id: "refine".to_string(),
                cmd: "refine".to_string(),
                args: Vec::new(),
                io_mode: StepIoMode::Auto,
                timeout_ms: 2_500,
                on_error: OnErrorPolicy::Continue,
            },
        ];
        config
    }

    fn load_or_create_default(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        if !path.exists() {
            write_default_config(path)?;
        }

        Self::load_from_path(path)
    }

    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        if self.pipeline.deadline_ms == 0 {
            return Err(ConfigValidationError::PipelineDeadlineMsMustBePositive);
        }

        if self.pipeline.steps.is_empty() {
            return Err(ConfigValidationError::PipelineMustContainAtLeastOneStep);
        }

        self.refine.validate()?;

        let mut seen_ids = HashSet::new();
        for step in &self.pipeline.steps {
            if step.timeout_ms == 0 {
                return Err(ConfigValidationError::StepTimeoutMsMustBePositive {
                    step_id: step.id.clone(),
                });
            }

            if !seen_ids.insert(step.id.as_str()) {
                return Err(ConfigValidationError::DuplicatePipelineStepId {
                    step_id: step.id.clone(),
                });
            }
        }

        for (name, binding) in [
            ("push_to_talk", &self.hotkeys.push_to_talk),
            ("done_mode_toggle", &self.hotkeys.done_mode_toggle),
            (
                "cancel_current_capture",
                &self.hotkeys.cancel_current_capture,
            ),
        ] {
            if binding.chord.is_empty() {
                return Err(ConfigValidationError::HotkeyChordMustNotBeEmpty {
                    hotkey_name: name.to_string(),
                });
            }

            if matches!(binding.trigger, TriggerType::DoubleTap)
                && binding.double_tap_timeout_ms == Some(0)
            {
                return Err(ConfigValidationError::DoubleTapTimeoutMsMustBePositive {
                    hotkey_name: name.to_string(),
                });
            }
        }

        self.indicator.colors.validate()?;

        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AppSettings {
    pub profile: String,
    pub strict_step_contract: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            profile: "default".to_string(),
            strict_step_contract: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct HotkeysConfig {
    pub push_to_talk: HotkeyBinding,
    pub done_mode_toggle: HotkeyBinding,
    pub cancel_current_capture: HotkeyBinding,
}

impl Default for HotkeysConfig {
    fn default() -> Self {
        Self {
            push_to_talk: HotkeyBinding {
                trigger: TriggerType::DoubleTap,
                chord: vec!["ctrl".to_string()],
                double_tap_timeout_ms: Some(default_double_tap_timeout_ms()),
            },
            done_mode_toggle: HotkeyBinding {
                trigger: TriggerType::Press,
                chord: vec!["ctrl".to_string(), "shift".to_string(), "d".to_string()],
                double_tap_timeout_ms: None,
            },
            cancel_current_capture: HotkeyBinding {
                trigger: TriggerType::Press,
                chord: vec!["ctrl".to_string(), "shift".to_string(), "x".to_string()],
                double_tap_timeout_ms: None,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct HotkeyBinding {
    pub trigger: TriggerType,
    pub chord: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub double_tap_timeout_ms: Option<u64>,
}

impl Default for HotkeyBinding {
    fn default() -> Self {
        Self {
            trigger: TriggerType::Press,
            chord: Vec::new(),
            double_tap_timeout_ms: None,
        }
    }
}

impl HotkeyBinding {
    #[must_use]
    pub fn effective_double_tap_timeout_ms(&self) -> u64 {
        self.double_tap_timeout_ms
            .unwrap_or_else(default_double_tap_timeout_ms)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TriggerType {
    Hold,
    #[default]
    Press,
    DoubleTap,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct IndicatorConfig {
    pub show_recording: bool,
    pub show_processing: bool,
    #[serde(default)]
    pub colors: IndicatorColorsConfig,
}

impl Default for IndicatorConfig {
    fn default() -> Self {
        Self {
            show_recording: true,
            show_processing: true,
            colors: IndicatorColorsConfig::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct IndicatorColorsConfig {
    pub idle: String,
    pub recording: String,
    #[serde(alias = "processing")]
    pub transcribing: String,
    pub pipeline: String,
    #[serde(alias = "injecting")]
    pub output: String,
    pub cancelled: String,
    #[serde(alias = "outer_ring")]
    pub outline: String,
    pub glyph: String,
}

impl IndicatorColorsConfig {
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        for (color_name, color_value) in [
            ("indicator.colors.idle", self.idle.as_str()),
            ("indicator.colors.recording", self.recording.as_str()),
            ("indicator.colors.transcribing", self.transcribing.as_str()),
            ("indicator.colors.pipeline", self.pipeline.as_str()),
            ("indicator.colors.output", self.output.as_str()),
            ("indicator.colors.cancelled", self.cancelled.as_str()),
            ("indicator.colors.outline", self.outline.as_str()),
            ("indicator.colors.glyph", self.glyph.as_str()),
        ] {
            if !is_valid_hex_color(color_value) {
                return Err(ConfigValidationError::IndicatorColorMustBeHex {
                    color_name: color_name.to_string(),
                    color_value: color_value.to_string(),
                });
            }
        }

        Ok(())
    }
}

impl Default for IndicatorColorsConfig {
    fn default() -> Self {
        Self {
            idle: "#636366".to_string(),
            recording: "#FF9F0A".to_string(),
            transcribing: "#0A84FF".to_string(),
            pipeline: "#BF5AF2".to_string(),
            output: "#30D158".to_string(),
            cancelled: "#FF453A".to_string(),
            outline: "#2C2C2E".to_string(),
            glyph: "#FFFFFF".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PipelineConfig {
    pub deadline_ms: u64,
    pub payload_format: PayloadFormat,
    pub steps: Vec<PipelineStepConfig>,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            deadline_ms: 500,
            payload_format: PayloadFormat::JsonObject,
            steps: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PipelineStepConfig {
    pub id: String,
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub io_mode: StepIoMode,
    #[serde(default = "default_step_timeout_ms")]
    pub timeout_ms: u64,
    #[serde(default)]
    pub on_error: OnErrorPolicy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum StepIoMode {
    #[default]
    Auto,
    EnvelopeJson,
    TextFilter,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OnErrorPolicy {
    Continue,
    FallbackRaw,
    #[default]
    Abort,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum PayloadFormat {
    #[default]
    JsonObject,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ScoringConfig {
    pub min_top_score: f32,
    pub min_margin: f32,
    pub acronym_min_top_score: f32,
    pub acronym_min_margin: f32,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            min_top_score: 0.84,
            min_margin: 0.10,
            acronym_min_top_score: 0.90,
            acronym_min_margin: 0.15,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct TranscriptConfig {
    pub system_prompt: String,
}

impl Default for TranscriptConfig {
    fn default() -> Self {
        Self {
            system_prompt: "Prefer minimal corrections. Focus on technical terms, developer tools, package names, commands, flags, file names, paths, env vars, acronyms, and obvious dictation errors. If uncertain, keep the original wording.".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RefineConfig {
    pub provider: RefineProvider,
    pub endpoint: String,
    pub model: String,
    pub temperature: f32,
    pub max_output_tokens: u32,
    pub max_length_delta_ratio: f32,
    pub max_token_change_ratio: f32,
    pub max_new_word_count: u32,
}

impl RefineConfig {
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        if self.endpoint.trim().is_empty() {
            return Err(ConfigValidationError::RefineEndpointMustNotBeEmpty);
        }
        if self.model.trim().is_empty() {
            return Err(ConfigValidationError::RefineModelMustNotBeEmpty);
        }
        if !self.temperature.is_finite() || self.temperature < 0.0 {
            return Err(ConfigValidationError::RefineTemperatureMustBeNonNegative);
        }
        if self.max_output_tokens == 0 {
            return Err(ConfigValidationError::RefineMaxOutputTokensMustBePositive);
        }
        for (field_name, value) in [
            ("refine.max_length_delta_ratio", self.max_length_delta_ratio),
            ("refine.max_token_change_ratio", self.max_token_change_ratio),
        ] {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(ConfigValidationError::RefineRatioMustBeBetweenZeroAndOne {
                    field_name: field_name.to_string(),
                    value: value.to_string(),
                });
            }
        }

        Ok(())
    }
}

impl Default for RefineConfig {
    fn default() -> Self {
        Self {
            provider: RefineProvider::OpenAi,
            endpoint: "https://api.openai.com/v1/chat/completions".to_string(),
            model: "gpt-4.1-mini".to_string(),
            temperature: 0.0,
            max_output_tokens: 512,
            max_length_delta_ratio: 0.25,
            max_token_change_ratio: 0.60,
            max_new_word_count: 2,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum RefineProvider {
    #[serde(rename = "openai", alias = "open_ai")]
    #[default]
    OpenAi,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    pub replay_enabled: bool,
    pub replay_dir: PathBuf,
    pub replay_retention_days: u32,
    pub replay_max_bytes: u64,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            replay_enabled: false,
            replay_dir: PathBuf::from("~/.local/state/muninn/replay"),
            replay_retention_days: 7,
            replay_max_bytes: 52_428_800,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ProvidersConfig {
    pub openai: OpenAiProviderConfig,
    pub google: GoogleProviderConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct OpenAiProviderConfig {
    pub api_key: Option<String>,
    pub endpoint: String,
    pub model: String,
}

impl Default for OpenAiProviderConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            endpoint: "https://api.openai.com/v1/audio/transcriptions".to_string(),
            model: "gpt-4o-mini-transcribe".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct GoogleProviderConfig {
    pub api_key: Option<String>,
    pub token: Option<String>,
    pub endpoint: String,
    pub model: Option<String>,
}

impl Default for GoogleProviderConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            token: None,
            endpoint: "https://speech.googleapis.com/v1/speech:recognize".to_string(),
            model: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("unable to resolve config path because HOME is not set")]
    HomeDirectoryNotSet,
    #[error("config file not found at expected path: {path}")]
    NotFound { path: PathBuf },
    #[error("failed to read config file at {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config TOML at {path}: {source}")]
    ParseTomlAtPath {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to parse config TOML: {source}")]
    ParseToml {
        #[source]
        source: toml::de::Error,
    },
    #[error("failed to create config directory at {path}: {source}")]
    CreateConfigDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to serialize launchable default config: {source}")]
    SerializeDefaultConfig {
        #[source]
        source: toml::ser::Error,
    },
    #[error("failed to write default config file at {path}: {source}")]
    WriteDefaultConfig {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Validation(#[from] ConfigValidationError),
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ConfigValidationError {
    #[error("pipeline.deadline_ms must be greater than 0")]
    PipelineDeadlineMsMustBePositive,
    #[error("pipeline must include at least one step")]
    PipelineMustContainAtLeastOneStep,
    #[error("pipeline step timeout_ms must be greater than 0 (step id: {step_id})")]
    StepTimeoutMsMustBePositive { step_id: String },
    #[error("pipeline step ids must be unique (duplicate id: {step_id})")]
    DuplicatePipelineStepId { step_id: String },
    #[error("hotkey chord must not be empty ({hotkey_name})")]
    HotkeyChordMustNotBeEmpty { hotkey_name: String },
    #[error("double_tap timeout must be greater than 0 ({hotkey_name})")]
    DoubleTapTimeoutMsMustBePositive { hotkey_name: String },
    #[error("indicator color must be a #RRGGBB hex string ({color_name}={color_value})")]
    IndicatorColorMustBeHex {
        color_name: String,
        color_value: String,
    },
    #[error("refine.endpoint must not be empty")]
    RefineEndpointMustNotBeEmpty,
    #[error("refine.model must not be empty")]
    RefineModelMustNotBeEmpty,
    #[error("refine.temperature must be non-negative")]
    RefineTemperatureMustBeNonNegative,
    #[error("refine.max_output_tokens must be greater than 0")]
    RefineMaxOutputTokensMustBePositive,
    #[error("{field_name} must be between 0.0 and 1.0 inclusive (got {value})")]
    RefineRatioMustBeBetweenZeroAndOne { field_name: String, value: String },
}

pub fn resolve_config_path() -> Result<PathBuf, ConfigError> {
    resolve_config_path_with(
        |key| env::var_os(key),
        env::var_os("HOME").map(PathBuf::from),
    )
}

fn resolve_config_path_with<F>(
    lookup_var: F,
    home_dir: Option<PathBuf>,
) -> Result<PathBuf, ConfigError>
where
    F: Fn(&str) -> Option<OsString>,
{
    if let Some(path) = lookup_var("MUNINN_CONFIG").and_then(non_empty_os_string) {
        return Ok(PathBuf::from(path));
    }

    if let Some(xdg_config_home) = lookup_var("XDG_CONFIG_HOME").and_then(non_empty_os_string) {
        return Ok(PathBuf::from(xdg_config_home)
            .join(DEFAULT_CONFIG_DIR_NAME)
            .join(DEFAULT_CONFIG_FILE_NAME));
    }

    let home = home_dir.ok_or(ConfigError::HomeDirectoryNotSet)?;
    Ok(home
        .join(".config")
        .join(DEFAULT_CONFIG_DIR_NAME)
        .join(DEFAULT_CONFIG_FILE_NAME))
}

fn non_empty_os_string(value: OsString) -> Option<OsString> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

const fn default_step_timeout_ms() -> u64 {
    250
}

const fn default_double_tap_timeout_ms() -> u64 {
    300
}

fn is_valid_hex_color(value: &str) -> bool {
    let Some(hex) = value.strip_prefix('#') else {
        return false;
    };

    hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn write_default_config(path: &Path) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ConfigError::CreateConfigDir {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let rendered = toml::to_string_pretty(&AppConfig::launchable_default())
        .map_err(|source| ConfigError::SerializeDefaultConfig { source })?;
    fs::write(path, rendered).map_err(|source| ConfigError::WriteDefaultConfig {
        path: path.to_path_buf(),
        source,
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        resolve_config_path_with, AppConfig, ConfigError, ConfigValidationError, OnErrorPolicy,
        PayloadFormat, RefineProvider, TriggerType,
    };

    #[test]
    fn parses_valid_config_and_applies_defaults() {
        let config = AppConfig::from_toml_str(valid_pipeline_toml()).expect("valid config");

        assert_eq!(config.pipeline.deadline_ms, 500);
        assert_eq!(config.pipeline.payload_format, PayloadFormat::JsonObject);
        assert_eq!(config.pipeline.steps.len(), 2);
        assert!(!config.logging.replay_enabled);
        assert_eq!(config.providers.openai.model, "gpt-4o-mini-transcribe");
        assert_eq!(config.refine.model, "gpt-4.1-mini");
        assert_eq!(config.indicator.colors.idle, "#636366");
    }

    #[test]
    fn defaults_match_plan() {
        let config = AppConfig::default();

        assert_eq!(config.pipeline.deadline_ms, 500);
        assert_eq!(config.hotkeys.push_to_talk.chord, vec!["ctrl"]);
        assert_eq!(config.hotkeys.push_to_talk.trigger, TriggerType::DoubleTap);
        assert_eq!(config.hotkeys.push_to_talk.double_tap_timeout_ms, Some(300));
        assert_eq!(
            config.hotkeys.done_mode_toggle.chord,
            vec!["ctrl", "shift", "d"]
        );
        assert_eq!(config.hotkeys.done_mode_toggle.double_tap_timeout_ms, None);
        assert_eq!(
            config.hotkeys.cancel_current_capture.chord,
            vec!["ctrl", "shift", "x"]
        );
        assert_eq!(
            config.hotkeys.cancel_current_capture.double_tap_timeout_ms,
            None
        );
        assert!(!config.logging.replay_enabled);
        assert_eq!(config.scoring.min_top_score, 0.84);
        assert_eq!(config.scoring.min_margin, 0.10);
        assert_eq!(config.scoring.acronym_min_top_score, 0.90);
        assert_eq!(config.scoring.acronym_min_margin, 0.15);
        assert_eq!(config.indicator.colors.recording, "#FF9F0A");
        assert_eq!(config.indicator.colors.transcribing, "#0A84FF");
        assert_eq!(config.indicator.colors.pipeline, "#BF5AF2");
        assert_eq!(config.indicator.colors.output, "#30D158");
        assert_eq!(config.indicator.colors.cancelled, "#FF453A");
        assert_eq!(config.indicator.colors.outline, "#2C2C2E");
        assert_eq!(config.indicator.colors.glyph, "#FFFFFF");
        assert_eq!(config.refine.provider, RefineProvider::OpenAi);
        assert_eq!(
            config.transcript.system_prompt,
            "Prefer minimal corrections. Focus on technical terms, developer tools, package names, commands, flags, file names, paths, env vars, acronyms, and obvious dictation errors. If uncertain, keep the original wording."
        );
    }

    #[test]
    fn launchable_default_is_valid_and_uses_ordered_stt_fallback() {
        let config = AppConfig::launchable_default();

        config.validate().expect("launchable default must validate");
        assert_eq!(config.pipeline.deadline_ms, 40_000);
        assert_eq!(config.pipeline.steps.len(), 3);
        assert_eq!(config.pipeline.steps[0].id, "stt_openai");
        assert_eq!(config.pipeline.steps[0].cmd, "stt_openai");
        assert_eq!(config.pipeline.steps[0].timeout_ms, 18_000);
        assert_eq!(config.pipeline.steps[0].on_error, OnErrorPolicy::Continue);
        assert_eq!(config.pipeline.steps[1].id, "stt_google");
        assert_eq!(config.pipeline.steps[1].cmd, "stt_google");
        assert_eq!(config.pipeline.steps[1].timeout_ms, 18_000);
        assert_eq!(config.pipeline.steps[1].on_error, OnErrorPolicy::Abort);
        assert_eq!(config.pipeline.steps[2].id, "refine");
        assert_eq!(config.pipeline.steps[2].cmd, "refine");
        assert_eq!(config.pipeline.steps[2].timeout_ms, 2_500);
    }

    #[test]
    fn rejects_invalid_refine_ratio() {
        let error = AppConfig::from_toml_str(
            r#"
[refine]
max_token_change_ratio = 1.5

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "step-a"
timeout_ms = 100
on_error = "abort"
"#,
        )
        .expect_err("refine ratio above one must fail");

        assert_eq!(
            error.to_validation_error(),
            Some(ConfigValidationError::RefineRatioMustBeBetweenZeroAndOne {
                field_name: "refine.max_token_change_ratio".to_string(),
                value: "1.5".to_string(),
            })
        );
    }

    #[test]
    fn accepts_refine_provider_openai() {
        let config = AppConfig::from_toml_str(
            r#"
[refine]
provider = "openai"

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "step-a"
timeout_ms = 100
on_error = "abort"
"#,
        )
        .expect("openai provider should parse");

        assert_eq!(config.refine.provider, RefineProvider::OpenAi);
    }

    #[test]
    fn accepts_legacy_refine_provider_open_ai_alias() {
        let config = AppConfig::from_toml_str(
            r#"
[refine]
provider = "open_ai"

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "step-a"
timeout_ms = 100
on_error = "abort"
"#,
        )
        .expect("legacy open_ai provider should parse");

        assert_eq!(config.refine.provider, RefineProvider::OpenAi);
    }

    #[test]
    fn rejects_empty_refine_model() {
        let error = AppConfig::from_toml_str(
            r#"
[refine]
model = "   "

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "step-a"
timeout_ms = 100
on_error = "abort"
"#,
        )
        .expect_err("empty refine model must fail");

        assert_eq!(
            error.to_validation_error(),
            Some(ConfigValidationError::RefineModelMustNotBeEmpty)
        );
    }

    #[test]
    fn rejects_duplicate_pipeline_step_ids() {
        let error = AppConfig::from_toml_str(
            r#"
[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "step-a"
timeout_ms = 100
on_error = "continue"

[[pipeline.steps]]
id = "stt"
cmd = "step-b"
timeout_ms = 200
on_error = "abort"
"#,
        )
        .expect_err("duplicate ids must fail");

        assert_eq!(
            error.to_validation_error(),
            Some(ConfigValidationError::DuplicatePipelineStepId {
                step_id: "stt".to_owned(),
            })
        );
    }

    #[test]
    fn rejects_non_positive_pipeline_deadline() {
        let error = AppConfig::from_toml_str(
            r#"
[pipeline]
deadline_ms = 0
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "step-a"
timeout_ms = 100
on_error = "continue"
"#,
        )
        .expect_err("deadline_ms must be > 0");

        assert_eq!(
            error.to_validation_error(),
            Some(ConfigValidationError::PipelineDeadlineMsMustBePositive)
        );
    }

    #[test]
    fn rejects_non_positive_step_timeout() {
        let error = AppConfig::from_toml_str(
            r#"
[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "step-a"
timeout_ms = 0
on_error = "continue"
"#,
        )
        .expect_err("timeout_ms must be > 0");

        assert_eq!(
            error.to_validation_error(),
            Some(ConfigValidationError::StepTimeoutMsMustBePositive {
                step_id: "stt".to_owned(),
            })
        );
    }

    #[test]
    fn rejects_empty_pipeline() {
        let error = AppConfig::from_toml_str(
            r#"
[pipeline]
deadline_ms = 500
payload_format = "json_object"
"#,
        )
        .expect_err("pipeline without steps must fail");

        assert_eq!(
            error.to_validation_error(),
            Some(ConfigValidationError::PipelineMustContainAtLeastOneStep)
        );
    }

    #[test]
    fn rejects_empty_hotkey_chord() {
        let error = AppConfig::from_toml_str(
            r#"
[hotkeys.push_to_talk]
trigger = "double_tap"
chord = []

[hotkeys.done_mode_toggle]
trigger = "press"
chord = ["ctrl", "shift", "d"]

[hotkeys.cancel_current_capture]
trigger = "press"
chord = ["ctrl", "shift", "x"]

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "step-a"
timeout_ms = 100
on_error = "abort"
"#,
        )
        .expect_err("empty chord should fail");

        assert_eq!(
            error.to_validation_error(),
            Some(ConfigValidationError::HotkeyChordMustNotBeEmpty {
                hotkey_name: "push_to_talk".to_string(),
            })
        );
    }

    #[test]
    fn rejects_non_positive_double_tap_timeout() {
        let error = AppConfig::from_toml_str(
            r#"
[hotkeys.push_to_talk]
trigger = "double_tap"
chord = ["ctrl"]
double_tap_timeout_ms = 0

[hotkeys.done_mode_toggle]
trigger = "press"
chord = ["ctrl", "shift", "d"]

[hotkeys.cancel_current_capture]
trigger = "press"
chord = ["ctrl", "shift", "x"]

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "step-a"
timeout_ms = 100
on_error = "abort"
"#,
        )
        .expect_err("double_tap_timeout_ms must be > 0");

        assert_eq!(
            error.to_validation_error(),
            Some(ConfigValidationError::DoubleTapTimeoutMsMustBePositive {
                hotkey_name: "push_to_talk".to_string(),
            })
        );
    }

    #[test]
    fn rejects_invalid_indicator_color() {
        let error = AppConfig::from_toml_str(
            r#"
[indicator.colors]
recording = "red"

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "step-a"
timeout_ms = 100
on_error = "abort"
"#,
        )
        .expect_err("indicator color should require #RRGGBB format");

        assert_eq!(
            error.to_validation_error(),
            Some(ConfigValidationError::IndicatorColorMustBeHex {
                color_name: "indicator.colors.recording".to_string(),
                color_value: "red".to_string(),
            })
        );
    }

    #[test]
    fn rejects_unknown_enum_values() {
        let error = AppConfig::from_toml_str(
            r#"
[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "step-a"
timeout_ms = 100
on_error = "skip"
"#,
        )
        .expect_err("unknown enum must fail");

        assert!(matches!(error, ConfigError::ParseToml { .. }));
    }

    #[test]
    fn resolve_config_path_uses_expected_precedence() {
        let from_env = resolve_config_path_with(
            |name| match name {
                "MUNINN_CONFIG" => Some(OsString::from("/tmp/override.toml")),
                "XDG_CONFIG_HOME" => Some(OsString::from("/xdg")),
                _ => None,
            },
            Some(PathBuf::from("/Users/alice")),
        )
        .expect("env override should resolve");
        assert_eq!(from_env, PathBuf::from("/tmp/override.toml"));

        let from_xdg = resolve_config_path_with(
            |name| match name {
                "XDG_CONFIG_HOME" => Some(OsString::from("/xdg")),
                _ => None,
            },
            Some(PathBuf::from("/Users/alice")),
        )
        .expect("xdg should resolve");
        assert_eq!(from_xdg, PathBuf::from("/xdg/muninn/config.toml"));

        let from_home = resolve_config_path_with(|_| None, Some(PathBuf::from("/Users/alice")))
            .expect("home should resolve");
        assert_eq!(
            from_home,
            PathBuf::from("/Users/alice/.config/muninn/config.toml")
        );
    }

    #[test]
    fn load_from_path_returns_not_found_with_expected_path() {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before UNIX_EPOCH")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "muninn-missing-config-{}-{}.toml",
            std::process::id(),
            unique_suffix
        ));

        let error = AppConfig::load_from_path(&path).expect_err("missing path must fail");
        match error {
            ConfigError::NotFound { path: actual } => assert_eq!(actual, path),
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn load_creates_launchable_default_when_config_is_missing() {
        let unique_suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before UNIX_EPOCH")
            .as_nanos();
        let config_root = std::env::temp_dir().join(format!(
            "muninn-auto-config-{}-{}",
            std::process::id(),
            unique_suffix
        ));
        let config_path = config_root.join("muninn").join("config.toml");

        let config = AppConfig::load_or_create_default(&config_path)
            .expect("missing config should auto-create");

        assert_eq!(config, AppConfig::launchable_default());
        assert!(config_path.exists(), "config file should be written");

        let rendered = std::fs::read_to_string(&config_path).expect("read written config");
        let reparsed = AppConfig::from_toml_str(&rendered).expect("reparse written config");
        assert_eq!(reparsed, AppConfig::launchable_default());
    }

    fn valid_pipeline_toml() -> &'static str {
        r#"
[hotkeys.push_to_talk]
trigger = "double_tap"
chord = ["ctrl"]

[hotkeys.done_mode_toggle]
trigger = "press"
chord = ["ctrl", "shift", "d"]

[hotkeys.cancel_current_capture]
trigger = "press"
chord = ["ctrl", "shift", "x"]

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "stt_openai"
timeout_ms = 300
on_error = "abort"

[[pipeline.steps]]
id = "normalize"
cmd = "muninn-normalize"
timeout_ms = 60
on_error = "continue"
"#
    }

    trait ValidationErrorExt {
        fn to_validation_error(&self) -> Option<ConfigValidationError>;
    }

    impl ValidationErrorExt for ConfigError {
        fn to_validation_error(&self) -> Option<ConfigValidationError> {
            match self {
                ConfigError::Validation(validation) => Some(validation.clone()),
                _ => None,
            }
        }
    }
}

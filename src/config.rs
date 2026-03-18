use std::collections::{BTreeMap, HashSet};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::target_context::TargetContextSnapshot;
use crate::transcription::{
    ResolvedTranscriptionRoute, TranscriptionProvider, TranscriptionRouteSource,
};

const DEFAULT_CONFIG_FILE_NAME: &str = "config.toml";
const DEFAULT_CONFIG_DIR_NAME: &str = "muninn";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub app: AppSettings,
    pub hotkeys: HotkeysConfig,
    pub indicator: IndicatorConfig,
    pub recording: RecordingConfig,
    pub pipeline: PipelineConfig,
    pub scoring: ScoringConfig,
    pub transcription: TranscriptionConfig,
    pub transcript: TranscriptConfig,
    pub refine: RefineConfig,
    pub logging: LoggingConfig,
    pub providers: ProvidersConfig,
    #[serde(default)]
    pub voices: BTreeMap<String, VoiceConfig>,
    #[serde(default)]
    pub profiles: BTreeMap<String, ProfileConfig>,
    #[serde(default)]
    pub profile_rules: Vec<ProfileRuleConfig>,
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
        config.transcription.providers =
            Some(TranscriptionProvider::default_ordered_route().to_vec());
        config.pipeline.steps = vec![PipelineStepConfig {
            id: "refine".to_string(),
            cmd: "refine".to_string(),
            args: Vec::new(),
            io_mode: StepIoMode::Auto,
            timeout_ms: 2_500,
            on_error: OnErrorPolicy::Continue,
        }];
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
        validate_identifier(self.app.profile.trim(), "app.profile")?;

        if self.pipeline.deadline_ms == 0 {
            return Err(ConfigValidationError::PipelineDeadlineMsMustBePositive);
        }

        if self.pipeline.steps.is_empty() {
            return Err(ConfigValidationError::PipelineMustContainAtLeastOneStep);
        }

        self.refine.validate()?;
        self.transcription.validate("transcription.providers")?;
        self.providers.validate()?;

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
        self.recording.validate()?;
        validate_pipeline_steps(&self.pipeline.steps)?;
        validate_voices(&self.voices)?;
        validate_profiles(self)?;
        validate_profile_rules(self)?;

        Ok(())
    }

    #[must_use]
    pub fn resolve_profile_selection(
        &self,
        target_context: &TargetContextSnapshot,
    ) -> ResolvedProfileSelection {
        let matched_rule = self
            .profile_rules
            .iter()
            .find(|rule| rule.matches(target_context));
        let used_default_profile_fallback =
            matched_rule.is_none() && !self.profile_rules.is_empty();
        let profile_id = matched_rule
            .map(|rule| rule.profile.clone())
            .unwrap_or_else(|| self.app.profile.clone());
        let explicit_profile = self.profiles.get(&profile_id);
        let voice_id = explicit_profile.and_then(|profile| profile.voice.clone());
        // Preserve the default profile behavior, but let the indicator render the
        // generic M glyph until a contextual rule matches.
        let voice_glyph = if used_default_profile_fallback {
            None
        } else {
            voice_id
                .as_deref()
                .and_then(|voice_id| self.voices.get(voice_id))
                .and_then(VoiceConfig::normalized_indicator_glyph)
        };
        let fallback_reason = if used_default_profile_fallback {
            Some(fallback_reason(target_context, &self.app.profile))
        } else {
            None
        };

        ResolvedProfileSelection {
            matched_rule_id: matched_rule.map(|rule| rule.id.clone()),
            profile_id,
            voice_id,
            voice_glyph,
            fallback_reason,
        }
    }

    #[must_use]
    pub fn resolve_effective_config(
        &self,
        target_context: TargetContextSnapshot,
    ) -> ResolvedUtteranceConfig {
        let selection = self.resolve_profile_selection(&target_context);
        let mut effective_config = self.clone();

        if let Some(voice_id) = selection.voice_id.as_deref() {
            if let Some(voice) = self.voices.get(voice_id) {
                voice.apply_to(
                    &mut effective_config.transcript,
                    &mut effective_config.refine,
                );
            }
        }

        if let Some(profile) = self.profiles.get(&selection.profile_id) {
            profile.apply_to(&mut effective_config);
        }

        let transcription_route = resolve_transcription_route(&effective_config);
        effective_config.pipeline = expand_pipeline_with_transcription_route(
            &effective_config.pipeline,
            &transcription_route,
        );

        ResolvedUtteranceConfig {
            target_context,
            matched_rule_id: selection.matched_rule_id,
            profile_id: selection.profile_id,
            voice_id: selection.voice_id,
            voice_glyph: selection.voice_glyph,
            fallback_reason: selection.fallback_reason,
            transcription_route,
            builtin_steps: ResolvedBuiltinStepConfig::from_app_config(&effective_config),
            effective_config,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AppSettings {
    pub profile: String,
    pub strict_step_contract: bool,
    pub autostart: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            profile: "default".to_string(),
            strict_step_contract: true,
            autostart: false,
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
    pub transcribing: String,
    pub pipeline: String,
    pub output: String,
    pub cancelled: String,
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
pub struct RecordingConfig {
    pub mono: bool,
    pub sample_rate_khz: u32,
}

impl RecordingConfig {
    pub fn validate(&self) -> Result<(), ConfigValidationError> {
        if self.sample_rate_khz == 0 {
            return Err(ConfigValidationError::RecordingSampleRateKhzMustBePositive);
        }

        Ok(())
    }

    #[must_use]
    pub const fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_khz * 1_000
    }
}

impl Default for RecordingConfig {
    fn default() -> Self {
        Self {
            mono: true,
            sample_rate_khz: 16,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct TranscriptionConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<Vec<TranscriptionProvider>>,
}

impl TranscriptionConfig {
    fn validate(&self, field_name: &str) -> Result<(), ConfigValidationError> {
        if self.providers.as_ref().is_some_and(Vec::is_empty) {
            return Err(
                ConfigValidationError::TranscriptionProvidersMustNotBeEmpty {
                    field_name: field_name.to_string(),
                },
            );
        }

        if let Some(providers) = self.providers.as_ref() {
            let mut seen = HashSet::new();
            let mut duplicates = HashSet::new();
            for provider in providers {
                if !seen.insert(*provider) {
                    duplicates.insert(*provider);
                }
            }
            if !duplicates.is_empty() {
                let mut provider_ids = duplicates
                    .into_iter()
                    .map(|provider| provider.config_id().to_string())
                    .collect::<Vec<_>>();
                provider_ids.sort();
                return Err(ConfigValidationError::DuplicateTranscriptionProviders {
                    field_name: field_name.to_string(),
                    provider_ids,
                });
            }
        }

        Ok(())
    }

    fn apply_to(&self, target: &mut TranscriptionConfig) {
        if let Some(providers) = self.providers.as_ref() {
            target.providers = Some(providers.clone());
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
    #[serde(rename = "openai")]
    #[default]
    OpenAi,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct VoiceConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indicator_glyph: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length_delta_ratio: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_token_change_ratio: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_new_word_count: Option<u32>,
}

impl VoiceConfig {
    #[must_use]
    pub fn normalized_indicator_glyph(&self) -> Option<char> {
        self.indicator_glyph
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .and_then(|value| value.chars().next())
            .map(|glyph| glyph.to_ascii_uppercase())
    }

    fn validate(&self, voice_id: &str) -> Result<(), ConfigValidationError> {
        if let Some(glyph) = self.indicator_glyph.as_deref() {
            let glyph = glyph.trim();
            let mut chars = glyph.chars();
            match (chars.next(), chars.next()) {
                (Some(letter), None) if letter.is_ascii_alphabetic() => {}
                _ => {
                    return Err(
                        ConfigValidationError::VoiceIndicatorGlyphMustBeSingleAsciiLetter {
                            voice_id: voice_id.to_string(),
                            value: glyph.to_string(),
                        },
                    );
                }
            }
        }

        validate_optional_refine_fields(
            self.temperature,
            self.max_output_tokens,
            self.max_length_delta_ratio,
            self.max_token_change_ratio,
            "voices",
            voice_id,
        )
    }

    fn apply_to(&self, transcript: &mut TranscriptConfig, refine: &mut RefineConfig) {
        if let Some(system_prompt) = self.system_prompt.as_ref() {
            transcript.system_prompt = system_prompt.clone();
        }
        if let Some(temperature) = self.temperature {
            refine.temperature = temperature;
        }
        if let Some(max_output_tokens) = self.max_output_tokens {
            refine.max_output_tokens = max_output_tokens;
        }
        if let Some(max_length_delta_ratio) = self.max_length_delta_ratio {
            refine.max_length_delta_ratio = max_length_delta_ratio;
        }
        if let Some(max_token_change_ratio) = self.max_token_change_ratio {
            refine.max_token_change_ratio = max_token_change_ratio;
        }
        if let Some(max_new_word_count) = self.max_new_word_count {
            refine.max_new_word_count = max_new_word_count;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording: Option<RecordingOverrides>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipeline: Option<PipelineOverrides>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcription: Option<TranscriptionConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<TranscriptOverrides>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refine: Option<RefineOverrides>,
}

impl ProfileConfig {
    fn validate(&self, profile_id: &str) -> Result<(), ConfigValidationError> {
        if let Some(voice_id) = self.voice.as_deref() {
            validate_identifier(voice_id.trim(), &format!("profiles.{profile_id}.voice"))?;
        }
        if let Some(recording) = self.recording.as_ref() {
            recording.validate(profile_id)?;
        }
        if let Some(pipeline) = self.pipeline.as_ref() {
            pipeline.validate(profile_id)?;
        }
        if let Some(transcription) = self.transcription.as_ref() {
            transcription.validate(&format!("profiles.{profile_id}.transcription.providers"))?;
        }
        if let Some(transcript) = self.transcript.as_ref() {
            transcript.validate(profile_id)?;
        }
        if let Some(refine) = self.refine.as_ref() {
            refine.validate(profile_id)?;
        }
        Ok(())
    }

    fn apply_to(&self, config: &mut AppConfig) {
        if let Some(recording) = self.recording.as_ref() {
            recording.apply_to(&mut config.recording);
        }
        if let Some(pipeline) = self.pipeline.as_ref() {
            pipeline.apply_to(&mut config.pipeline);
        }
        if let Some(transcription) = self.transcription.as_ref() {
            transcription.apply_to(&mut config.transcription);
        }
        if let Some(transcript) = self.transcript.as_ref() {
            transcript.apply_to(&mut config.transcript);
        }
        if let Some(refine) = self.refine.as_ref() {
            refine.apply_to(&mut config.refine);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct RecordingOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mono: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_rate_khz: Option<u32>,
}

impl RecordingOverrides {
    fn validate(&self, profile_id: &str) -> Result<(), ConfigValidationError> {
        if matches!(self.sample_rate_khz, Some(0)) {
            return Err(ConfigValidationError::RecordingSampleRateKhzMustBePositive);
        }
        validate_identifier(profile_id, &format!("profiles.{profile_id}"))
    }

    fn apply_to(&self, recording: &mut RecordingConfig) {
        if let Some(mono) = self.mono {
            recording.mono = mono;
        }
        if let Some(sample_rate_khz) = self.sample_rate_khz {
            recording.sample_rate_khz = sample_rate_khz;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct PipelineOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_format: Option<PayloadFormat>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steps: Option<Vec<PipelineStepConfig>>,
}

impl PipelineOverrides {
    fn validate(&self, _profile_id: &str) -> Result<(), ConfigValidationError> {
        if matches!(self.deadline_ms, Some(0)) {
            return Err(ConfigValidationError::PipelineDeadlineMsMustBePositive);
        }
        if let Some(steps) = self.steps.as_ref() {
            if steps.is_empty() {
                return Err(ConfigValidationError::PipelineMustContainAtLeastOneStep);
            }
            validate_pipeline_steps(steps)?;
        }
        Ok(())
    }

    fn apply_to(&self, pipeline: &mut PipelineConfig) {
        if let Some(deadline_ms) = self.deadline_ms {
            pipeline.deadline_ms = deadline_ms;
        }
        if let Some(payload_format) = self.payload_format {
            pipeline.payload_format = payload_format;
        }
        if let Some(steps) = self.steps.as_ref() {
            pipeline.steps = steps.clone();
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct TranscriptOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
}

impl TranscriptOverrides {
    fn validate(&self, profile_id: &str) -> Result<(), ConfigValidationError> {
        if let Some(system_prompt) = self.system_prompt.as_deref() {
            if system_prompt.trim().is_empty() {
                return Err(ConfigValidationError::ConfigIdentifierMustNotBeEmpty {
                    field_name: format!("profiles.{profile_id}.transcript.system_prompt"),
                });
            }
        }
        Ok(())
    }

    fn apply_to(&self, transcript: &mut TranscriptConfig) {
        if let Some(system_prompt) = self.system_prompt.as_ref() {
            transcript.system_prompt = system_prompt.clone();
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct RefineOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<RefineProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length_delta_ratio: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_token_change_ratio: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_new_word_count: Option<u32>,
}

impl RefineOverrides {
    fn validate(&self, profile_id: &str) -> Result<(), ConfigValidationError> {
        if self
            .endpoint
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(ConfigValidationError::RefineEndpointMustNotBeEmpty);
        }
        if self
            .model
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(ConfigValidationError::RefineModelMustNotBeEmpty);
        }
        validate_optional_refine_fields(
            self.temperature,
            self.max_output_tokens,
            self.max_length_delta_ratio,
            self.max_token_change_ratio,
            "profiles",
            profile_id,
        )
    }

    fn apply_to(&self, refine: &mut RefineConfig) {
        if let Some(provider) = self.provider {
            refine.provider = provider;
        }
        if let Some(endpoint) = self.endpoint.as_ref() {
            refine.endpoint = endpoint.clone();
        }
        if let Some(model) = self.model.as_ref() {
            refine.model = model.clone();
        }
        if let Some(temperature) = self.temperature {
            refine.temperature = temperature;
        }
        if let Some(max_output_tokens) = self.max_output_tokens {
            refine.max_output_tokens = max_output_tokens;
        }
        if let Some(max_length_delta_ratio) = self.max_length_delta_ratio {
            refine.max_length_delta_ratio = max_length_delta_ratio;
        }
        if let Some(max_token_change_ratio) = self.max_token_change_ratio {
            refine.max_token_change_ratio = max_token_change_ratio;
        }
        if let Some(max_new_word_count) = self.max_new_word_count {
            refine.max_new_word_count = max_new_word_count;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ProfileRuleConfig {
    pub id: String,
    pub profile: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_name_contains: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title_contains: Option<String>,
}

impl ProfileRuleConfig {
    fn validate(&self, app: &AppSettings) -> Result<(), ConfigValidationError> {
        validate_identifier(self.id.trim(), "profile_rules.id")?;
        validate_identifier(
            self.profile.trim(),
            &format!("profile_rules.{}.profile", self.id),
        )?;

        if !self.has_matcher() {
            return Err(
                ConfigValidationError::ProfileRuleMustIncludeAtLeastOneMatcher {
                    rule_id: self.id.clone(),
                },
            );
        }

        for (field_name, value) in [
            ("bundle_id", self.bundle_id.as_deref()),
            ("bundle_id_prefix", self.bundle_id_prefix.as_deref()),
            ("app_name", self.app_name.as_deref()),
            ("app_name_contains", self.app_name_contains.as_deref()),
            (
                "window_title_contains",
                self.window_title_contains.as_deref(),
            ),
        ] {
            if value.is_some_and(|value| value.trim().is_empty()) {
                return Err(ConfigValidationError::ProfileRuleFieldMustNotBeEmpty {
                    rule_id: self.id.clone(),
                    field_name: field_name.to_string(),
                });
            }
        }

        if self.profile != app.profile && !self.profile.is_empty() {
            // Profile existence is validated at the AppConfig level.
        }

        Ok(())
    }

    fn has_matcher(&self) -> bool {
        self.bundle_id.is_some()
            || self.bundle_id_prefix.is_some()
            || self.app_name.is_some()
            || self.app_name_contains.is_some()
            || self.window_title_contains.is_some()
    }

    #[must_use]
    pub fn matches(&self, target_context: &TargetContextSnapshot) -> bool {
        if !match_optional_exact(
            self.bundle_id.as_deref(),
            target_context.bundle_id.as_deref(),
        ) {
            return false;
        }
        if !match_optional_prefix(
            self.bundle_id_prefix.as_deref(),
            target_context.bundle_id.as_deref(),
        ) {
            return false;
        }
        if !match_optional_exact(self.app_name.as_deref(), target_context.app_name.as_deref()) {
            return false;
        }
        if !match_optional_contains(
            self.app_name_contains.as_deref(),
            target_context.app_name.as_deref(),
        ) {
            return false;
        }
        match_optional_contains(
            self.window_title_contains.as_deref(),
            target_context.window_title.as_deref(),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProfileSelection {
    pub matched_rule_id: Option<String>,
    pub profile_id: String,
    pub voice_id: Option<String>,
    pub voice_glyph: Option<char>,
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedUtteranceConfig {
    pub target_context: TargetContextSnapshot,
    pub matched_rule_id: Option<String>,
    pub profile_id: String,
    pub voice_id: Option<String>,
    pub voice_glyph: Option<char>,
    pub fallback_reason: Option<String>,
    pub transcription_route: ResolvedTranscriptionRoute,
    pub effective_config: AppConfig,
    pub builtin_steps: ResolvedBuiltinStepConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedBuiltinStepConfig {
    pub transcript: TranscriptConfig,
    pub refine: RefineConfig,
    pub providers: ProvidersConfig,
}

impl ResolvedBuiltinStepConfig {
    #[must_use]
    pub fn from_app_config(config: &AppConfig) -> Self {
        Self {
            transcript: config.transcript.clone(),
            refine: config.refine.clone(),
            providers: config.providers.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    pub replay_enabled: bool,
    pub replay_retain_audio: bool,
    pub replay_dir: PathBuf,
    pub replay_retention_days: u32,
    pub replay_max_bytes: u64,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            replay_enabled: false,
            replay_retain_audio: true,
            replay_dir: PathBuf::from("~/.local/state/muninn/replay"),
            replay_retention_days: 7,
            replay_max_bytes: 52_428_800,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct ProvidersConfig {
    pub apple_speech: AppleSpeechProviderConfig,
    pub whisper_cpp: WhisperCppProviderConfig,
    pub deepgram: DeepgramProviderConfig,
    pub openai: OpenAiProviderConfig,
    pub google: GoogleProviderConfig,
}

impl ProvidersConfig {
    fn validate(&self) -> Result<(), ConfigValidationError> {
        self.apple_speech.validate()?;
        self.whisper_cpp.validate()?;
        self.deepgram.validate()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AppleSpeechProviderConfig {
    pub locale: Option<String>,
    pub install_assets: bool,
}

impl AppleSpeechProviderConfig {
    fn validate(&self) -> Result<(), ConfigValidationError> {
        if self
            .locale
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(ConfigValidationError::AppleSpeechLocaleMustNotBeEmpty);
        }
        Ok(())
    }
}

impl Default for AppleSpeechProviderConfig {
    fn default() -> Self {
        Self {
            locale: None,
            install_assets: true,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WhisperCppDevicePreference {
    #[default]
    Auto,
    Cpu,
    Gpu,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct WhisperCppProviderConfig {
    pub model: Option<String>,
    pub model_dir: PathBuf,
    pub device: WhisperCppDevicePreference,
}

impl WhisperCppProviderConfig {
    fn validate(&self) -> Result<(), ConfigValidationError> {
        if self
            .model
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err(ConfigValidationError::WhisperCppModelMustNotBeEmpty);
        }
        if self.model_dir.as_os_str().is_empty() {
            return Err(ConfigValidationError::WhisperCppModelDirMustNotBeEmpty);
        }
        Ok(())
    }
}

impl Default for WhisperCppProviderConfig {
    fn default() -> Self {
        Self {
            model: None,
            model_dir: PathBuf::from("~/.local/share/muninn/models"),
            device: WhisperCppDevicePreference::Auto,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DeepgramProviderConfig {
    pub api_key: Option<String>,
    pub endpoint: String,
    pub model: String,
    pub language: String,
}

impl Default for DeepgramProviderConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            endpoint: "https://api.deepgram.com/v1/listen".to_string(),
            model: "nova-3".to_string(),
            language: "en".to_string(),
        }
    }
}

impl DeepgramProviderConfig {
    fn validate(&self) -> Result<(), ConfigValidationError> {
        if self.endpoint.trim().is_empty() {
            return Err(ConfigValidationError::DeepgramEndpointMustNotBeEmpty);
        }
        if self.model.trim().is_empty() {
            return Err(ConfigValidationError::DeepgramModelMustNotBeEmpty);
        }
        if self.language.trim().is_empty() {
            return Err(ConfigValidationError::DeepgramLanguageMustNotBeEmpty);
        }
        Ok(())
    }
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
    #[error("{field_name} must not be empty")]
    ConfigIdentifierMustNotBeEmpty { field_name: String },
    #[error("{field_name} must include at least one provider")]
    TranscriptionProvidersMustNotBeEmpty { field_name: String },
    #[error("{field_name} must not contain duplicate providers ({provider_ids:?})")]
    DuplicateTranscriptionProviders {
        field_name: String,
        provider_ids: Vec<String>,
    },
    #[error("providers.apple_speech.locale must not be empty")]
    AppleSpeechLocaleMustNotBeEmpty,
    #[error("providers.whisper_cpp.model must not be empty")]
    WhisperCppModelMustNotBeEmpty,
    #[error("providers.whisper_cpp.model_dir must not be empty")]
    WhisperCppModelDirMustNotBeEmpty,
    #[error("providers.deepgram.endpoint must not be empty")]
    DeepgramEndpointMustNotBeEmpty,
    #[error("providers.deepgram.model must not be empty")]
    DeepgramModelMustNotBeEmpty,
    #[error("providers.deepgram.language must not be empty")]
    DeepgramLanguageMustNotBeEmpty,
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
    #[error("recording.sample_rate_khz must be greater than 0")]
    RecordingSampleRateKhzMustBePositive,
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
    #[error("voice indicator_glyph must be exactly one ASCII letter ({voice_id}={value})")]
    VoiceIndicatorGlyphMustBeSingleAsciiLetter { voice_id: String, value: String },
    #[error("profile references unknown voice ({profile_id} -> {voice_id})")]
    UnknownVoiceReference {
        profile_id: String,
        voice_id: String,
    },
    #[error("{field_name} references unknown profile ({profile_id})")]
    UnknownProfileReference {
        field_name: String,
        profile_id: String,
    },
    #[error("profile rule ids must be unique (duplicate id: {rule_id})")]
    DuplicateProfileRuleId { rule_id: String },
    #[error("profile rule must include at least one matcher ({rule_id})")]
    ProfileRuleMustIncludeAtLeastOneMatcher { rule_id: String },
    #[error("profile rule field must not be empty ({rule_id}.{field_name})")]
    ProfileRuleFieldMustNotBeEmpty { rule_id: String, field_name: String },
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

fn validate_identifier(value: &str, field_name: &str) -> Result<(), ConfigValidationError> {
    if value.trim().is_empty() {
        return Err(ConfigValidationError::ConfigIdentifierMustNotBeEmpty {
            field_name: field_name.to_string(),
        });
    }
    Ok(())
}

fn validate_pipeline_steps(steps: &[PipelineStepConfig]) -> Result<(), ConfigValidationError> {
    let mut seen_ids = HashSet::new();
    for step in steps {
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

    Ok(())
}

fn validate_optional_refine_fields(
    temperature: Option<f32>,
    max_output_tokens: Option<u32>,
    max_length_delta_ratio: Option<f32>,
    max_token_change_ratio: Option<f32>,
    scope_prefix: &str,
    scope_id: &str,
) -> Result<(), ConfigValidationError> {
    if temperature.is_some_and(|value| !value.is_finite() || value < 0.0) {
        return Err(ConfigValidationError::RefineTemperatureMustBeNonNegative);
    }
    if max_output_tokens == Some(0) {
        return Err(ConfigValidationError::RefineMaxOutputTokensMustBePositive);
    }
    for (field_name, value) in [
        (
            format!("{scope_prefix}.{scope_id}.max_length_delta_ratio"),
            max_length_delta_ratio,
        ),
        (
            format!("{scope_prefix}.{scope_id}.max_token_change_ratio"),
            max_token_change_ratio,
        ),
    ] {
        if let Some(value) = value {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(ConfigValidationError::RefineRatioMustBeBetweenZeroAndOne {
                    field_name,
                    value: value.to_string(),
                });
            }
        }
    }

    Ok(())
}

fn validate_voices(voices: &BTreeMap<String, VoiceConfig>) -> Result<(), ConfigValidationError> {
    for (voice_id, voice) in voices {
        validate_identifier(voice_id, &format!("voices.{voice_id}"))?;
        voice.validate(voice_id)?;
    }
    Ok(())
}

fn validate_profiles(config: &AppConfig) -> Result<(), ConfigValidationError> {
    for (profile_id, profile) in &config.profiles {
        validate_identifier(profile_id, &format!("profiles.{profile_id}"))?;
        profile.validate(profile_id)?;
        if let Some(voice_id) = profile.voice.as_deref() {
            if !config.voices.contains_key(voice_id) {
                return Err(ConfigValidationError::UnknownVoiceReference {
                    profile_id: profile_id.clone(),
                    voice_id: voice_id.to_string(),
                });
            }
        }
    }
    Ok(())
}

fn validate_profile_rules(config: &AppConfig) -> Result<(), ConfigValidationError> {
    let mut seen_ids = HashSet::new();

    for rule in &config.profile_rules {
        rule.validate(&config.app)?;
        if !seen_ids.insert(rule.id.as_str()) {
            return Err(ConfigValidationError::DuplicateProfileRuleId {
                rule_id: rule.id.clone(),
            });
        }
        if rule.profile != config.app.profile && !config.profiles.contains_key(&rule.profile) {
            return Err(ConfigValidationError::UnknownProfileReference {
                field_name: format!("profile_rules.{}.profile", rule.id),
                profile_id: rule.profile.clone(),
            });
        }
    }

    Ok(())
}

fn resolve_transcription_route(config: &AppConfig) -> ResolvedTranscriptionRoute {
    if let Some(providers) = config.transcription.providers.clone() {
        return ResolvedTranscriptionRoute {
            providers,
            source: TranscriptionRouteSource::ExplicitConfig,
        };
    }

    ResolvedTranscriptionRoute {
        providers: infer_transcription_route_from_pipeline(&config.pipeline),
        source: TranscriptionRouteSource::PipelineInferred,
    }
}

fn infer_transcription_route_from_pipeline(
    pipeline: &PipelineConfig,
) -> Vec<TranscriptionProvider> {
    pipeline
        .steps
        .iter()
        .filter_map(|step| TranscriptionProvider::lookup_step_name(&step.cmd))
        .collect()
}

fn expand_pipeline_with_transcription_route(
    pipeline: &PipelineConfig,
    route: &ResolvedTranscriptionRoute,
) -> PipelineConfig {
    if route.source == TranscriptionRouteSource::PipelineInferred {
        return pipeline.clone();
    }

    let mut steps = route
        .providers
        .iter()
        .copied()
        .map(route_step_for_provider)
        .collect::<Vec<_>>();
    steps.extend(
        pipeline
            .steps
            .iter()
            .filter(|step| TranscriptionProvider::lookup_step_name(&step.cmd).is_none())
            .cloned(),
    );

    PipelineConfig {
        deadline_ms: pipeline.deadline_ms,
        payload_format: pipeline.payload_format,
        steps,
    }
}

fn route_step_for_provider(provider: TranscriptionProvider) -> PipelineStepConfig {
    PipelineStepConfig {
        id: provider.canonical_step_name().to_string(),
        cmd: provider.canonical_step_name().to_string(),
        args: Vec::new(),
        io_mode: StepIoMode::EnvelopeJson,
        timeout_ms: provider.default_timeout_ms(),
        on_error: OnErrorPolicy::Continue,
    }
}

fn fallback_reason(target_context: &TargetContextSnapshot, default_profile: &str) -> String {
    if target_context.bundle_id.is_none() && target_context.app_name.is_none() {
        return format!("frontmost app unavailable; using default profile `{default_profile}`");
    }
    if target_context.window_title.is_none() {
        return format!("no profile rule matched with app-only context; using default profile `{default_profile}`");
    }
    format!("no profile rule matched; using default profile `{default_profile}`")
}

fn match_optional_exact(expected: Option<&str>, actual: Option<&str>) -> bool {
    match (
        expected.and_then(normalize_match_string),
        actual.and_then(normalize_match_string),
    ) {
        (Some(expected), Some(actual)) => actual == expected,
        (Some(_), None) => false,
        (None, _) => true,
    }
}

fn match_optional_prefix(expected: Option<&str>, actual: Option<&str>) -> bool {
    match (
        expected.and_then(normalize_match_string),
        actual.and_then(normalize_match_string),
    ) {
        (Some(expected), Some(actual)) => actual.starts_with(&expected),
        (Some(_), None) => false,
        (None, _) => true,
    }
}

fn match_optional_contains(expected: Option<&str>, actual: Option<&str>) -> bool {
    match (
        expected.and_then(normalize_match_string),
        actual.and_then(normalize_match_string),
    ) {
        (Some(expected), Some(actual)) => actual.contains(&expected),
        (Some(_), None) => false,
        (None, _) => true,
    }
}

fn normalize_match_string(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        None
    } else {
        Some(value.to_ascii_lowercase())
    }
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

    use crate::transcription::{TranscriptionProvider, TranscriptionRouteSource};

    use super::{
        resolve_config_path_with, AppConfig, ConfigError, ConfigValidationError, PayloadFormat,
        RefineProvider, TargetContextSnapshot, TriggerType, WhisperCppDevicePreference,
    };

    #[test]
    fn parses_valid_config_and_applies_defaults() {
        let config = AppConfig::from_toml_str(valid_pipeline_toml()).expect("valid config");

        assert_eq!(config.pipeline.deadline_ms, 500);
        assert_eq!(config.pipeline.payload_format, PayloadFormat::JsonObject);
        assert_eq!(config.pipeline.steps.len(), 2);
        assert!(!config.logging.replay_enabled);
        assert!(config.logging.replay_retain_audio);
        assert_eq!(config.providers.openai.model, "gpt-4o-mini-transcribe");
        assert_eq!(config.providers.apple_speech.locale, None);
        assert!(config.providers.apple_speech.install_assets);
        assert_eq!(config.providers.whisper_cpp.model, None);
        assert_eq!(
            config.providers.whisper_cpp.model_dir,
            PathBuf::from("~/.local/share/muninn/models")
        );
        assert_eq!(
            config.providers.whisper_cpp.device,
            WhisperCppDevicePreference::Auto
        );
        assert_eq!(
            config.providers.deepgram.endpoint,
            "https://api.deepgram.com/v1/listen"
        );
        assert_eq!(config.providers.deepgram.model, "nova-3");
        assert_eq!(config.providers.deepgram.language, "en");
        assert_eq!(config.refine.model, "gpt-4.1-mini");
        assert_eq!(config.indicator.colors.idle, "#636366");
        assert!(config.recording.mono);
        assert_eq!(config.recording.sample_rate_khz, 16);
    }

    #[test]
    fn defaults_match_plan() {
        let config = AppConfig::default();

        assert_eq!(config.pipeline.deadline_ms, 500);
        assert!(!config.app.autostart);
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
        assert!(config.logging.replay_retain_audio);
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
        assert!(config.recording.mono);
        assert_eq!(config.recording.sample_rate_khz, 16);
        assert_eq!(config.providers.apple_speech.locale, None);
        assert!(config.providers.apple_speech.install_assets);
        assert_eq!(config.providers.whisper_cpp.model, None);
        assert_eq!(
            config.providers.whisper_cpp.model_dir,
            PathBuf::from("~/.local/share/muninn/models")
        );
        assert_eq!(
            config.providers.whisper_cpp.device,
            WhisperCppDevicePreference::Auto
        );
        assert_eq!(
            config.providers.deepgram.endpoint,
            "https://api.deepgram.com/v1/listen"
        );
        assert_eq!(config.providers.deepgram.model, "nova-3");
        assert_eq!(config.providers.deepgram.language, "en");
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
        assert_eq!(
            config.transcription.providers,
            Some(TranscriptionProvider::default_ordered_route().to_vec())
        );
        assert_eq!(config.pipeline.steps.len(), 1);
        assert_eq!(config.pipeline.steps[0].id, "refine");
        assert_eq!(config.pipeline.steps[0].cmd, "refine");
        assert_eq!(config.pipeline.steps[0].timeout_ms, 2_500);
    }

    #[test]
    fn resolve_effective_config_expands_explicit_transcription_route_before_postprocessing() {
        let config = AppConfig::launchable_default();

        let resolved = config.resolve_effective_config(target_context(
            Some("com.openai.codex"),
            Some("Codex"),
            Some("Spec 29"),
        ));

        assert_eq!(
            resolved.transcription_route,
            crate::ResolvedTranscriptionRoute {
                providers: TranscriptionProvider::default_ordered_route().to_vec(),
                source: TranscriptionRouteSource::ExplicitConfig,
            }
        );
        assert_eq!(
            resolved
                .effective_config
                .pipeline
                .steps
                .iter()
                .map(|step| step.cmd.as_str())
                .collect::<Vec<_>>(),
            vec![
                "stt_apple_speech",
                "stt_whisper_cpp",
                "stt_deepgram",
                "stt_openai",
                "stt_google",
                "refine",
            ]
        );
    }

    #[test]
    fn parses_whisper_cpp_provider_overrides() {
        let config = AppConfig::from_toml_str(
            r#"
[providers.whisper_cpp]
model = "base.en"
model_dir = "/tmp/muninn-models"
device = "cpu"

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
        .expect("whisper.cpp provider overrides should parse");

        assert_eq!(
            config.providers.whisper_cpp,
            super::WhisperCppProviderConfig {
                model: Some("base.en".to_string()),
                model_dir: PathBuf::from("/tmp/muninn-models"),
                device: WhisperCppDevicePreference::Cpu,
            }
        );
    }

    #[test]
    fn parses_deepgram_provider_overrides() {
        let config = AppConfig::from_toml_str(
            r#"
[providers.deepgram]
api_key = "config-deepgram-key"
endpoint = "https://api.deepgram.test/v1/listen"
model = "nova-3-medical"
language = "en-IE"

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
        .expect("Deepgram provider overrides should parse");

        assert_eq!(
            config.providers.deepgram,
            super::DeepgramProviderConfig {
                api_key: Some("config-deepgram-key".to_string()),
                endpoint: "https://api.deepgram.test/v1/listen".to_string(),
                model: "nova-3-medical".to_string(),
                language: "en-IE".to_string(),
            }
        );
    }

    #[test]
    fn parses_apple_speech_provider_overrides() {
        let config = AppConfig::from_toml_str(
            r#"
[providers.apple_speech]
locale = "en-IE"
install_assets = false

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
        .expect("apple speech provider overrides should parse");

        assert_eq!(
            config.providers.apple_speech,
            super::AppleSpeechProviderConfig {
                locale: Some("en-IE".to_string()),
                install_assets: false,
            }
        );
    }

    #[test]
    fn rejects_empty_apple_speech_locale() {
        let error = AppConfig::from_toml_str(
            r#"
[providers.apple_speech]
locale = "   "

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
        .expect_err("apple speech locale must not be empty");

        assert_eq!(
            error.to_validation_error(),
            Some(ConfigValidationError::AppleSpeechLocaleMustNotBeEmpty)
        );
    }

    #[test]
    fn rejects_empty_whisper_cpp_model() {
        let error = AppConfig::from_toml_str(
            r#"
[providers.whisper_cpp]
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
        .expect_err("whisper.cpp model must not be empty");

        assert_eq!(
            error.to_validation_error(),
            Some(ConfigValidationError::WhisperCppModelMustNotBeEmpty)
        );
    }

    #[test]
    fn rejects_empty_whisper_cpp_model_dir() {
        let error = AppConfig::from_toml_str(
            r#"
[providers.whisper_cpp]
model_dir = ""

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
        .expect_err("whisper.cpp model dir must not be empty");

        assert_eq!(
            error.to_validation_error(),
            Some(ConfigValidationError::WhisperCppModelDirMustNotBeEmpty)
        );
    }

    #[test]
    fn rejects_invalid_deepgram_provider_values() {
        let error = AppConfig::from_toml_str(
            r#"
[providers.deepgram]
endpoint = ""
model = "nova-3"
language = "en"

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
        .expect_err("empty Deepgram endpoint must fail");

        assert_eq!(
            error.to_validation_error(),
            Some(ConfigValidationError::DeepgramEndpointMustNotBeEmpty)
        );
    }

    #[test]
    fn resolve_effective_config_preserves_pipeline_only_transcription_order() {
        let config = AppConfig::from_toml_str(
            r#"
[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt-openai"
cmd = "stt_openai"
timeout_ms = 100
on_error = "continue"

[[pipeline.steps]]
id = "stt-google"
cmd = "stt_google"
timeout_ms = 100
on_error = "abort"

[[pipeline.steps]]
id = "refine"
cmd = "refine"
timeout_ms = 100
on_error = "continue"
"#,
        )
        .expect("pipeline-only config should parse");

        let resolved = config.resolve_effective_config(target_context(None, None, None));

        assert_eq!(
            resolved.transcription_route.providers,
            vec![TranscriptionProvider::OpenAi, TranscriptionProvider::Google]
        );
        assert_eq!(
            resolved.transcription_route.source,
            TranscriptionRouteSource::PipelineInferred
        );
        assert_eq!(
            resolved
                .effective_config
                .pipeline
                .steps
                .iter()
                .map(|step| step.cmd.as_str())
                .collect::<Vec<_>>(),
            vec!["stt_openai", "stt_google", "refine"]
        );
    }

    #[test]
    fn resolve_effective_config_infers_interleaved_pipeline_transcription_steps() {
        let config = AppConfig::from_toml_str(
            r#"
[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt-openai"
cmd = "stt_openai"
timeout_ms = 100
on_error = "continue"

[[pipeline.steps]]
id = "uppercase"
cmd = "/usr/bin/tr"
timeout_ms = 100
on_error = "continue"

[[pipeline.steps]]
id = "stt-google"
cmd = "stt_google"
timeout_ms = 100
on_error = "abort"

[[pipeline.steps]]
id = "refine"
cmd = "refine"
timeout_ms = 100
on_error = "continue"
"#,
        )
        .expect("pipeline-only config should parse");

        let resolved = config.resolve_effective_config(target_context(None, None, None));

        assert_eq!(
            resolved.transcription_route.providers,
            vec![TranscriptionProvider::OpenAi, TranscriptionProvider::Google]
        );
        assert_eq!(
            resolved
                .effective_config
                .pipeline
                .steps
                .iter()
                .map(|step| step.cmd.as_str())
                .collect::<Vec<_>>(),
            vec!["stt_openai", "/usr/bin/tr", "stt_google", "refine"]
        );
    }

    #[test]
    fn resolve_effective_config_explicit_route_strips_all_existing_transcription_steps() {
        let config = AppConfig::from_toml_str(
            r#"
[transcription]
providers = ["deepgram", "google"]

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt-openai"
cmd = "stt_openai"
timeout_ms = 100
on_error = "continue"

[[pipeline.steps]]
id = "uppercase"
cmd = "/usr/bin/tr"
timeout_ms = 100
on_error = "continue"

[[pipeline.steps]]
id = "stt-google"
cmd = "stt_google"
timeout_ms = 100
on_error = "abort"

[[pipeline.steps]]
id = "refine"
cmd = "refine"
timeout_ms = 100
on_error = "continue"
"#,
        )
        .expect("explicit-route config should parse");

        let resolved = config.resolve_effective_config(target_context(None, None, None));

        assert_eq!(
            resolved.transcription_route.providers,
            vec![
                TranscriptionProvider::Deepgram,
                TranscriptionProvider::Google
            ]
        );
        assert_eq!(
            resolved
                .effective_config
                .pipeline
                .steps
                .iter()
                .map(|step| step.cmd.as_str())
                .collect::<Vec<_>>(),
            vec!["stt_deepgram", "stt_google", "/usr/bin/tr", "refine"]
        );
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
    fn rejects_duplicate_transcription_provider_list() {
        let error = AppConfig::from_toml_str(
            r#"
[transcription]
providers = ["openai", "openai"]

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "refine"
cmd = "refine"
timeout_ms = 100
on_error = "continue"
"#,
        )
        .expect_err("duplicate transcription provider list should fail");

        assert_eq!(
            error.to_validation_error(),
            Some(ConfigValidationError::DuplicateTranscriptionProviders {
                field_name: "transcription.providers".to_string(),
                provider_ids: vec!["openai".to_string()],
            })
        );
    }

    #[test]
    fn resolve_effective_config_keeps_default_route_when_profile_transcription_table_is_empty() {
        let config = AppConfig::from_toml_str(
            r#"
[app]
profile = "mail"

[transcription]
providers = ["openai", "google"]

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "refine"
cmd = "refine"
timeout_ms = 100
on_error = "continue"

[profiles.mail.transcription]
"#,
        )
        .expect("profile transcription override should parse");

        let resolved = config.resolve_effective_config(target_context(None, None, None));

        assert_eq!(
            resolved.transcription_route.providers,
            vec![TranscriptionProvider::OpenAi, TranscriptionProvider::Google]
        );
        assert_eq!(
            resolved
                .effective_config
                .pipeline
                .steps
                .iter()
                .map(|step| step.cmd.as_str())
                .collect::<Vec<_>>(),
            vec!["stt_openai", "stt_google", "refine"]
        );
    }

    #[test]
    fn rejects_empty_transcription_provider_list() {
        let error = AppConfig::from_toml_str(
            r#"
[transcription]
providers = []

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "refine"
cmd = "refine"
timeout_ms = 100
on_error = "continue"
"#,
        )
        .expect_err("empty transcription provider list should fail");

        assert_eq!(
            error.to_validation_error(),
            Some(
                ConfigValidationError::TranscriptionProvidersMustNotBeEmpty {
                    field_name: "transcription.providers".to_string(),
                }
            )
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
    fn rejects_legacy_refine_provider_open_ai_alias() {
        let error = AppConfig::from_toml_str(
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
        .expect_err("legacy open_ai provider should fail");

        assert!(matches!(error, ConfigError::ParseToml { .. }));
    }

    #[test]
    fn rejects_legacy_indicator_color_aliases() {
        let error = AppConfig::from_toml_str(
            r##"
[indicator.colors]
idle = "#111111"
recording = "#222222"
processing = "#333333"
pipeline = "#444444"
injecting = "#555555"
cancelled = "#666666"
outer_ring = "#777777"
glyph = "#888888"

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "step-a"
timeout_ms = 100
on_error = "abort"
"##,
        )
        .expect_err("legacy indicator aliases should fail");

        assert!(matches!(error, ConfigError::ParseToml { .. }));
    }

    #[test]
    fn parses_replay_audio_retention_override() {
        let config = AppConfig::from_toml_str(
            r#"
[logging]
replay_retain_audio = false

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
        .expect("replay audio retention override should parse");

        assert!(!config.logging.replay_retain_audio);
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
    fn rejects_voice_indicator_glyphs_that_are_not_single_ascii_letters() {
        let error = AppConfig::from_toml_str(
            r#"
[voices.dev_mode]
indicator_glyph = "DM"

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "stt_openai"
timeout_ms = 100
on_error = "abort"
"#,
        )
        .expect_err("multi-character voice glyphs must fail validation");

        assert_eq!(
            error.to_validation_error(),
            Some(
                ConfigValidationError::VoiceIndicatorGlyphMustBeSingleAsciiLetter {
                    voice_id: "dev_mode".to_string(),
                    value: "DM".to_string(),
                }
            )
        );
    }

    #[test]
    fn resolve_profile_selection_matches_rules_and_falls_back_to_default_profile() {
        let config = AppConfig::from_toml_str(
            r#"
[app]
profile = "default"

[voices.dev_mode]
indicator_glyph = "d"

[profiles.default]

[profiles.codex]
voice = "dev_mode"

[[profile_rules]]
id = "codex_window"
profile = "codex"
bundle_id_prefix = "com.openai."
window_title_contains = "codex"

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "stt_openai"
timeout_ms = 100
on_error = "abort"
"#,
        )
        .expect("contextual config should parse");

        let matched = config.resolve_profile_selection(&target_context(
            Some("com.openai.codex"),
            Some("Codex"),
            Some("Muninn - Codex"),
        ));
        assert_eq!(matched.matched_rule_id.as_deref(), Some("codex_window"));
        assert_eq!(matched.profile_id, "codex");
        assert_eq!(matched.voice_id.as_deref(), Some("dev_mode"));
        assert_eq!(matched.voice_glyph, Some('D'));
        assert_eq!(matched.fallback_reason, None);

        let fallback = config.resolve_profile_selection(&target_context(
            Some("com.apple.Terminal"),
            Some("Terminal"),
            Some("notes.txt"),
        ));
        assert_eq!(fallback.matched_rule_id, None);
        assert_eq!(fallback.profile_id, "default");
        assert_eq!(fallback.voice_id, None);
        assert_eq!(fallback.voice_glyph, None);
        assert_eq!(
            fallback.fallback_reason.as_deref(),
            Some("no profile rule matched; using default profile `default`")
        );
    }

    #[test]
    fn resolve_profile_selection_uses_generic_m_mode_for_unknown_contextual_apps() {
        let config = AppConfig::from_toml_str(
            r#"
[app]
profile = "default"

[voices.default_mode]
indicator_glyph = "d"

[profiles.default]
voice = "default_mode"

[profiles.codex]
voice = "default_mode"

[[profile_rules]]
id = "codex_window"
profile = "codex"
bundle_id_prefix = "com.openai."

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "stt_openai"
timeout_ms = 100
on_error = "abort"
"#,
        )
        .expect("contextual config should parse");

        let fallback = config.resolve_profile_selection(&target_context(
            Some("com.apple.Terminal"),
            Some("Terminal"),
            Some("notes.txt"),
        ));

        assert_eq!(fallback.matched_rule_id, None);
        assert_eq!(fallback.profile_id, "default");
        assert_eq!(fallback.voice_id.as_deref(), Some("default_mode"));
        assert_eq!(fallback.voice_glyph, None);
        assert_eq!(
            fallback.fallback_reason.as_deref(),
            Some("no profile rule matched; using default profile `default`")
        );
    }

    #[test]
    fn resolve_effective_config_prefers_profile_overrides_over_voice_defaults() {
        let config = AppConfig::from_toml_str(
            r#"
[app]
profile = "default"

[recording]
sample_rate_khz = 16

[transcription]
providers = ["openai", "google"]

[transcript]
system_prompt = "base prompt"

[refine]
temperature = 0.0
max_output_tokens = 512
max_length_delta_ratio = 0.25
max_token_change_ratio = 0.60

[voices.dev_mode]
indicator_glyph = "d"
system_prompt = "voice prompt"
temperature = 0.8
max_output_tokens = 128
max_length_delta_ratio = 0.4

[profiles.default]

[profiles.codex]
voice = "dev_mode"
[profiles.codex.recording]
sample_rate_khz = 48
[profiles.codex.transcription]
providers = ["whisper_cpp", "google"]
[profiles.codex.transcript]
system_prompt = "profile prompt"
[profiles.codex.refine]
temperature = 0.2
max_output_tokens = 256

[[profile_rules]]
id = "codex_window"
profile = "codex"
app_name = "Codex"

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "stt_openai"
timeout_ms = 100
on_error = "abort"
"#,
        )
        .expect("contextual config should parse");

        let resolved = config.resolve_effective_config(target_context(
            Some("com.openai.codex"),
            Some("Codex"),
            Some("Spec"),
        ));

        assert_eq!(resolved.profile_id, "codex");
        assert_eq!(resolved.voice_id.as_deref(), Some("dev_mode"));
        assert_eq!(resolved.voice_glyph, Some('D'));
        assert_eq!(resolved.effective_config.recording.sample_rate_khz, 48);
        assert_eq!(
            resolved.effective_config.transcript.system_prompt,
            "profile prompt"
        );
        assert_eq!(
            resolved.transcription_route.providers,
            vec![
                TranscriptionProvider::WhisperCpp,
                TranscriptionProvider::Google
            ]
        );
        assert_eq!(
            resolved.transcription_route.source,
            TranscriptionRouteSource::ExplicitConfig
        );
        assert_eq!(
            resolved
                .effective_config
                .pipeline
                .steps
                .iter()
                .take(2)
                .map(|step| step.cmd.as_str())
                .collect::<Vec<_>>(),
            vec!["stt_whisper_cpp", "stt_google"]
        );
        assert_eq!(resolved.effective_config.refine.temperature, 0.2);
        assert_eq!(resolved.effective_config.refine.max_output_tokens, 256);
        assert_eq!(resolved.effective_config.refine.max_length_delta_ratio, 0.4);
        assert_eq!(
            resolved.effective_config.refine.max_token_change_ratio,
            0.60
        );
    }

    #[test]
    fn resolve_profile_selection_hides_default_profile_glyph_when_no_rule_matches() {
        let config = AppConfig::from_toml_str(
            r#"
[app]
profile = "default"

[recording]
sample_rate_khz = 16

[transcript]
system_prompt = "base prompt"

[refine]
temperature = 0.0
max_output_tokens = 512
max_length_delta_ratio = 0.25
max_token_change_ratio = 0.60

[voices.mail]
indicator_glyph = "e"
system_prompt = "mail prompt"

[profiles.default]
voice = "mail"

[profiles.codex]

[[profile_rules]]
id = "codex_app"
profile = "codex"
app_name = "Codex"

[pipeline]
deadline_ms = 500
payload_format = "json_object"

[[pipeline.steps]]
id = "stt"
cmd = "stt_openai"
timeout_ms = 100
on_error = "abort"
"#,
        )
        .expect("config with default fallback voice should parse");

        let fallback = config.resolve_profile_selection(&target_context(
            Some("com.apple.Terminal"),
            Some("Terminal"),
            Some("notes.txt"),
        ));

        assert_eq!(fallback.matched_rule_id, None);
        assert_eq!(fallback.profile_id, "default");
        assert_eq!(fallback.voice_id.as_deref(), Some("mail"));
        assert_eq!(fallback.voice_glyph, None);
        assert_eq!(
            fallback.fallback_reason.as_deref(),
            Some("no profile rule matched; using default profile `default`")
        );
    }

    #[test]
    fn accepts_recording_overrides() {
        let config = AppConfig::from_toml_str(
            r#"
[recording]
mono = false
sample_rate_khz = 48

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
        .expect("recording overrides should parse");

        assert!(!config.recording.mono);
        assert_eq!(config.recording.sample_rate_khz, 48);
        assert_eq!(config.recording.sample_rate_hz(), 48_000);
    }

    #[test]
    fn rejects_non_positive_recording_sample_rate() {
        let error = AppConfig::from_toml_str(
            r#"
[recording]
sample_rate_khz = 0

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
        .expect_err("recording sample rate must be > 0");

        assert_eq!(
            error.to_validation_error(),
            Some(ConfigValidationError::RecordingSampleRateKhzMustBePositive)
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

    fn target_context(
        bundle_id: Option<&str>,
        app_name: Option<&str>,
        window_title: Option<&str>,
    ) -> TargetContextSnapshot {
        TargetContextSnapshot {
            bundle_id: bundle_id.map(ToOwned::to_owned),
            app_name: app_name.map(ToOwned::to_owned),
            window_title: window_title.map(ToOwned::to_owned),
            captured_at: "2026-03-06T00:00:00Z".to_string(),
        }
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

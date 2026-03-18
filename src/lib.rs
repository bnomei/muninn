#![doc = include_str!("../README.md")]

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;

pub mod audio;
pub mod config;
pub mod envelope;
pub mod error;
pub mod hotkeys;
pub mod injector;
pub mod mock;
pub mod orchestrator;
pub mod permissions;
pub mod platform;
pub mod runner;
pub mod runtime_flow;
pub mod scoring;
pub mod secrets;
pub mod state;
pub mod target_context;
pub mod transcription;

pub use audio::MacosAudioRecorder;
pub use config::{
    resolve_config_path, AppConfig, AppleSpeechProviderConfig, ConfigError, ConfigValidationError,
    DeepgramProviderConfig, OnErrorPolicy, PayloadFormat, ProfileConfig, ProfileRuleConfig,
    ProvidersConfig, RecordingConfig, RefineOverrides, ResolvedBuiltinStepConfig,
    ResolvedProfileSelection, ResolvedUtteranceConfig, TranscriptOverrides, TranscriptionConfig,
    TriggerType, VoiceConfig, WhisperCppDevicePreference, WhisperCppProviderConfig,
};
pub use envelope::MuninnEnvelopeV1;
pub use error::{MacosAdapterError, MacosAdapterResult, PermissionKind};
pub use hotkeys::{MacosHotkeyBinding, MacosHotkeyBindings, MacosHotkeyEventSource};
pub use injector::MacosTextInjector;
pub use mock::{
    MockAudioRecorder, MockHotkeyEventSource, MockIndicatorAdapter, MockPermissionsAdapter,
    MockTextInjector,
};
pub use orchestrator::{InjectionRoute, InjectionRouteReason, InjectionTarget, Orchestrator};
pub use permissions::MacosPermissionsAdapter;
pub use platform::{detect_platform, ensure_supported_platform, is_supported_platform, Platform};
pub use runner::{
    InProcessStepError, InProcessStepExecutor, PipelineOutcome, PipelinePolicyApplied,
    PipelineRunner, PipelineStopReason, PipelineTraceEntry, StepFailureKind,
};
pub use runtime_flow::{map_hotkey_event, RuntimeFlowCoordinator};
pub use scoring::{
    DecisionReason, ReplacementDecision, ReplacementDecisionInput, SpanMetadata, Thresholds,
};
pub use secrets::{resolve_secret, resolve_secret_from_env};
pub use state::{AppEvent, AppState};
pub use target_context::{capture_frontmost_target_context, TargetContextSnapshot};
pub use transcription::{
    append_transcription_attempt, attach_transcription_route, resolved_transcription_route,
    transcription_attempts, ResolvedTranscriptionRoute, TranscriptionAttempt,
    TranscriptionAttemptOutcome, TranscriptionProvider, TranscriptionRouteSource,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingMode {
    PushToTalk,
    DoneMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndicatorState {
    Idle,
    Recording { mode: RecordingMode },
    Transcribing,
    Pipeline,
    Output,
    MissingCredentials,
    Cancelled,
}

impl IndicatorState {
    #[must_use]
    pub const fn is_recording(self) -> bool {
        matches!(self, Self::Recording { .. })
    }

    #[must_use]
    pub const fn is_processing(self) -> bool {
        matches!(self, Self::Transcribing | Self::Pipeline)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionStatus {
    Granted,
    Denied,
    NotDetermined,
    Restricted,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermissionPreflightStatus {
    pub microphone: PermissionStatus,
    pub accessibility: PermissionStatus,
    pub input_monitoring: PermissionStatus,
}

impl Default for PermissionPreflightStatus {
    fn default() -> Self {
        Self {
            microphone: PermissionStatus::NotDetermined,
            accessibility: PermissionStatus::NotDetermined,
            input_monitoring: PermissionStatus::NotDetermined,
        }
    }
}

impl PermissionPreflightStatus {
    #[must_use]
    pub const fn all_granted() -> Self {
        Self {
            microphone: PermissionStatus::Granted,
            accessibility: PermissionStatus::Granted,
            input_monitoring: PermissionStatus::Granted,
        }
    }

    #[must_use]
    pub const fn unsupported() -> Self {
        Self {
            microphone: PermissionStatus::Unsupported,
            accessibility: PermissionStatus::Unsupported,
            input_monitoring: PermissionStatus::Unsupported,
        }
    }

    #[must_use]
    pub const fn allows_recording(self) -> bool {
        status_is_granted(self.microphone) && status_is_granted(self.input_monitoring)
    }

    #[must_use]
    pub const fn allows_injection(self) -> bool {
        status_is_granted(self.accessibility)
    }

    #[must_use]
    pub const fn allows_hotkeys(self) -> bool {
        status_is_granted(self.input_monitoring)
    }

    #[must_use]
    pub fn missing_for_recording(self) -> Vec<PermissionKind> {
        let mut missing = Vec::new();
        if !status_is_granted(self.microphone) {
            missing.push(PermissionKind::Microphone);
        }
        if !status_is_granted(self.input_monitoring) {
            missing.push(PermissionKind::InputMonitoring);
        }
        missing
    }

    #[must_use]
    pub fn missing_for_tray_recording(self) -> Vec<PermissionKind> {
        let mut missing = Vec::new();
        if status_blocks_recording_start(self.microphone) {
            missing.push(PermissionKind::Microphone);
        }
        missing
    }

    #[must_use]
    pub fn missing_for_injection(self) -> Vec<PermissionKind> {
        let mut missing = Vec::new();
        if !status_is_granted(self.accessibility) {
            missing.push(PermissionKind::Accessibility);
        }
        missing
    }

    pub fn ensure_recording_allowed(self) -> MacosAdapterResult<()> {
        let permissions = self.missing_for_recording();
        if permissions.is_empty() {
            return Ok(());
        }
        Err(MacosAdapterError::MissingPermissions { permissions })
    }

    pub fn ensure_tray_recording_allowed(self) -> MacosAdapterResult<()> {
        let permissions = self.missing_for_tray_recording();
        if permissions.is_empty() {
            return Ok(());
        }
        Err(MacosAdapterError::MissingPermissions { permissions })
    }

    pub fn ensure_injection_allowed(self) -> MacosAdapterResult<()> {
        let permissions = self.missing_for_injection();
        if permissions.is_empty() {
            return Ok(());
        }
        Err(MacosAdapterError::MissingPermissions { permissions })
    }
}

const fn status_is_granted(status: PermissionStatus) -> bool {
    matches!(status, PermissionStatus::Granted)
}

const fn status_blocks_recording_start(status: PermissionStatus) -> bool {
    matches!(
        status,
        PermissionStatus::Denied | PermissionStatus::Restricted | PermissionStatus::Unsupported
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyAction {
    PushToTalk,
    DoneModeToggle,
    CancelCurrentCapture,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyEventKind {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HotkeyEvent {
    pub action: HotkeyAction,
    pub kind: HotkeyEventKind,
}

impl HotkeyEvent {
    #[must_use]
    pub const fn new(action: HotkeyAction, kind: HotkeyEventKind) -> Self {
        Self { action, kind }
    }

    #[must_use]
    pub const fn is_pressed(self) -> bool {
        matches!(self.kind, HotkeyEventKind::Pressed)
    }

    #[must_use]
    pub const fn is_released(self) -> bool {
        matches!(self.kind, HotkeyEventKind::Released)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedAudio {
    pub wav_path: PathBuf,
    pub duration_ms: u64,
}

impl RecordedAudio {
    #[must_use]
    pub fn new(wav_path: impl Into<PathBuf>, duration_ms: u64) -> Self {
        Self {
            wav_path: wav_path.into(),
            duration_ms,
        }
    }
}

#[async_trait]
pub trait IndicatorAdapter: Send + Sync {
    async fn initialize(&mut self) -> MacosAdapterResult<()>;
    async fn set_state(&mut self, state: IndicatorState) -> MacosAdapterResult<()>;
    async fn set_state_with_glyph(
        &mut self,
        state: IndicatorState,
        glyph: Option<char>,
    ) -> MacosAdapterResult<()> {
        let _ = glyph;
        self.set_state(state).await
    }
    async fn set_temporary_state(
        &mut self,
        state: IndicatorState,
        min_duration: Duration,
        fallback_state: IndicatorState,
    ) -> MacosAdapterResult<()> {
        let _ = min_duration;
        let _ = fallback_state;
        self.set_state(state).await
    }
    async fn set_temporary_state_with_glyph(
        &mut self,
        state: IndicatorState,
        glyph: Option<char>,
        min_duration: Duration,
        fallback_state: IndicatorState,
        fallback_glyph: Option<char>,
    ) -> MacosAdapterResult<()> {
        let _ = glyph;
        let _ = fallback_glyph;
        self.set_temporary_state(state, min_duration, fallback_state)
            .await
    }
    async fn state(&self) -> MacosAdapterResult<IndicatorState>;
    async fn indicator_glyph(&self) -> MacosAdapterResult<Option<char>> {
        Ok(None)
    }
}

#[async_trait]
pub trait PermissionsAdapter: Send + Sync {
    async fn preflight(&self) -> MacosAdapterResult<PermissionPreflightStatus>;
    async fn request_microphone_access(&self) -> MacosAdapterResult<bool>;
    async fn request_input_monitoring_access(&self) -> MacosAdapterResult<bool>;
    async fn request_accessibility_access(&self) -> MacosAdapterResult<bool>;
}

#[async_trait]
pub trait HotkeyEventSource: Send {
    async fn next_event(&mut self) -> MacosAdapterResult<HotkeyEvent>;
}

#[async_trait(?Send)]
pub trait AudioRecorder {
    async fn start_recording(&mut self) -> MacosAdapterResult<()>;
    async fn stop_recording(&mut self) -> MacosAdapterResult<RecordedAudio>;
    async fn cancel_recording(&mut self) -> MacosAdapterResult<()>;
}

#[async_trait]
pub trait TextInjector: Send + Sync {
    async fn inject_unicode_text(&self, text: &str) -> MacosAdapterResult<()>;

    async fn inject_checked(&self, text: &str) -> MacosAdapterResult<()> {
        if text.is_empty() {
            return Err(MacosAdapterError::EmptyInjectionText);
        }
        self.inject_unicode_text(text).await
    }
}

#[cfg(test)]
mod tests {
    use super::{
        IndicatorState, MacosAdapterError, PermissionKind, PermissionPreflightStatus,
        PermissionStatus, RecordingMode,
    };

    #[test]
    fn indicator_state_helpers_reflect_recording_and_processing() {
        assert!(!IndicatorState::Idle.is_recording());
        assert!(IndicatorState::Recording {
            mode: RecordingMode::PushToTalk
        }
        .is_recording());
        assert!(IndicatorState::Transcribing.is_processing());
        assert!(IndicatorState::Pipeline.is_processing());
        assert!(!IndicatorState::Output.is_processing());
        assert!(!IndicatorState::MissingCredentials.is_processing());
        assert!(!IndicatorState::Cancelled.is_processing());
    }

    #[test]
    fn recording_preflight_requires_microphone_and_input_monitoring() {
        let status = PermissionPreflightStatus {
            microphone: PermissionStatus::Denied,
            accessibility: PermissionStatus::Granted,
            input_monitoring: PermissionStatus::NotDetermined,
        };

        assert_eq!(
            status.missing_for_recording(),
            vec![PermissionKind::Microphone, PermissionKind::InputMonitoring]
        );
        assert_eq!(
            status.ensure_recording_allowed().unwrap_err(),
            MacosAdapterError::MissingPermissions {
                permissions: vec![PermissionKind::Microphone, PermissionKind::InputMonitoring]
            }
        );
    }

    #[test]
    fn tray_recording_preflight_only_blocks_on_microphone_failures() {
        let status = PermissionPreflightStatus {
            microphone: PermissionStatus::Denied,
            accessibility: PermissionStatus::Granted,
            input_monitoring: PermissionStatus::NotDetermined,
        };

        assert_eq!(
            status.missing_for_tray_recording(),
            vec![PermissionKind::Microphone]
        );
        assert_eq!(
            status.ensure_tray_recording_allowed().unwrap_err(),
            MacosAdapterError::MissingPermissions {
                permissions: vec![PermissionKind::Microphone]
            }
        );
    }

    #[test]
    fn tray_recording_allows_microphone_bootstrap_without_input_monitoring() {
        let status = PermissionPreflightStatus {
            microphone: PermissionStatus::NotDetermined,
            accessibility: PermissionStatus::Granted,
            input_monitoring: PermissionStatus::Denied,
        };

        assert!(status.missing_for_tray_recording().is_empty());
        status
            .ensure_tray_recording_allowed()
            .expect("tray recording should allow microphone bootstrap");
    }

    #[test]
    fn injection_preflight_requires_accessibility() {
        let status = PermissionPreflightStatus {
            microphone: PermissionStatus::Granted,
            accessibility: PermissionStatus::Restricted,
            input_monitoring: PermissionStatus::Granted,
        };

        assert_eq!(
            status.missing_for_injection(),
            vec![PermissionKind::Accessibility]
        );
        assert_eq!(
            status.ensure_injection_allowed().unwrap_err(),
            MacosAdapterError::MissingPermissions {
                permissions: vec![PermissionKind::Accessibility]
            }
        );
    }

    #[test]
    fn all_granted_allows_recording_injection_and_hotkeys() {
        let status = PermissionPreflightStatus::all_granted();
        assert!(status.allows_recording());
        assert!(status.allows_injection());
        assert!(status.allows_hotkeys());
    }
}

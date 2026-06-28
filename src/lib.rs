#![doc = include_str!("../README.md")]

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use tracing::warn;

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
pub mod streaming_transcription;
pub mod target_context;
pub mod transcription;

/// Tracing target for the tao runtime worker and tray event loop.
pub const TARGET_RUNTIME: &str = "runtime";
/// Tracing target for in-process pipeline steps and orchestration.
pub const TARGET_PIPELINE: &str = "pipeline";
/// Tracing target for transcription provider I/O.
pub const TARGET_PROVIDER: &str = "provider";
/// Tracing target for [`AppConfig`] load and validation.
pub const TARGET_CONFIG: &str = "config";
/// Tracing target for global hotkey registration and events.
pub const TARGET_HOTKEY: &str = "hotkey";
/// Tracing target for audio capture lifecycle.
pub const TARGET_RECORDING: &str = "recording";
/// Fallback tracing target when no subsystem label applies.
pub const TARGET_DEFAULT: &str = "default";

#[doc(hidden)]
pub fn load_builtin_step_config<T, FDefaults, FResolved>(
    step_label: &'static str,
    on_not_found: FDefaults,
    resolve: FResolved,
) -> Result<T, String>
where
    FDefaults: FnOnce() -> T,
    FResolved: FnOnce(&ResolvedBuiltinStepConfig) -> T,
{
    resolve_builtin_step_config_from_load_result(
        step_label,
        AppConfig::load(),
        on_not_found,
        resolve,
    )
}

#[doc(hidden)]
pub fn resolve_builtin_step_config_from_load_result<T, FDefaults, FResolved>(
    step_label: &'static str,
    load_result: Result<AppConfig, ConfigError>,
    on_not_found: FDefaults,
    resolve: FResolved,
) -> Result<T, String>
where
    FDefaults: FnOnce() -> T,
    FResolved: FnOnce(&ResolvedBuiltinStepConfig) -> T,
{
    load_result
        .map(|config| resolve(&ResolvedBuiltinStepConfig::from_app_config(&config)))
        .or_else(|error| match &error {
            ConfigError::NotFound { .. } => {
                warn!(
                    target: TARGET_CONFIG,
                    step = step_label,
                    error = %error,
                    "built-in step config missing; using default provider settings"
                );
                Ok(on_not_found())
            }
            _ => Err(format!(
                "failed to load AppConfig for {step_label}: {error}"
            )),
        })
}

pub use audio::MacosAudioRecorder;
pub use config::{
    resolve_config_path, AppConfig, AppleSpeechProviderConfig, ConfigError, ConfigValidationError,
    DeepgramProviderConfig, ExternalControlConfig, OnErrorPolicy, PayloadFormat, ProfileConfig,
    ProfileRuleConfig, ProvidersConfig, RecordingConfig, RefineOverrides, ReplayDetailMode,
    ResolvedBuiltinStepConfig, ResolvedProfileSelection, ResolvedUtteranceConfig,
    TranscriptOverrides, TranscriptionConfig, TriggerType, VoiceConfig, WhisperCppDevicePreference,
    WhisperCppProviderConfig,
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
pub use streaming_transcription::{
    ActiveStreamingTranscription, StreamingTranscriptOutcome, StreamingTranscriptionError,
    StreamingTranscriptionProvider, StreamingTranscriptionProviderEntry,
    StreamingTranscriptionSession,
};
pub use target_context::{capture_frontmost_target_context, TargetContextSnapshot};
pub use transcription::{
    append_transcription_attempt, attach_transcription_route, resolved_transcription_route,
    transcription_attempts, ResolvedTranscriptionRoute, TranscriptionAttempt,
    TranscriptionAttemptOutcome, TranscriptionProvider, TranscriptionRouteSource,
};

/// How an active capture was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordingMode {
    /// Hold-to-talk: release ends capture and starts processing.
    PushToTalk,
    /// Toggle-to-done: second toggle ends capture and starts processing.
    DoneMode,
}

/// Menu-bar indicator phase surfaced to the user during capture and pipeline work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndicatorState {
    /// No capture or pipeline activity.
    Idle,
    /// Audio capture in progress for the given [`RecordingMode`].
    Recording { mode: RecordingMode },
    /// Streaming or batch transcription running.
    Transcribing,
    /// Post-transcription refinement steps running.
    Pipeline,
    /// Final text is being delivered to the target application.
    Output,
    /// A required provider credential is missing from config or environment.
    MissingCredentials,
    /// Capture was discarded without running the pipeline.
    Cancelled,
}

impl IndicatorState {
    /// Returns `true` for [`IndicatorState::Recording`].
    #[must_use]
    pub const fn is_recording(self) -> bool {
        matches!(self, Self::Recording { .. })
    }

    /// Returns `true` during transcription or pipeline refinement.
    #[must_use]
    pub const fn is_processing(self) -> bool {
        matches!(self, Self::Transcribing | Self::Pipeline)
    }
}

/// macOS TCC authorization state for a single permission category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionStatus {
    /// User granted access.
    Granted,
    /// User denied access.
    Denied,
    /// Prompt has not been shown yet.
    NotDetermined,
    /// Parental controls or enterprise policy blocks access.
    Restricted,
    /// Permission is not applicable on this platform build.
    Unsupported,
}

/// Snapshot of microphone, accessibility, and input-monitoring TCC status.
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
    /// Preflight snapshot with every permission marked [`PermissionStatus::Granted`].
    #[must_use]
    pub const fn all_granted() -> Self {
        Self {
            microphone: PermissionStatus::Granted,
            accessibility: PermissionStatus::Granted,
            input_monitoring: PermissionStatus::Granted,
        }
    }

    /// Preflight snapshot with every permission marked [`PermissionStatus::Unsupported`].
    #[must_use]
    pub const fn unsupported() -> Self {
        Self {
            microphone: PermissionStatus::Unsupported,
            accessibility: PermissionStatus::Unsupported,
            input_monitoring: PermissionStatus::Unsupported,
        }
    }

    /// Returns `true` when microphone and input monitoring are both granted.
    #[must_use]
    pub const fn allows_recording(self) -> bool {
        status_is_granted(self.microphone) && status_is_granted(self.input_monitoring)
    }

    /// Returns `true` when accessibility is granted for Unicode injection.
    #[must_use]
    pub const fn allows_injection(self) -> bool {
        status_is_granted(self.accessibility)
    }

    /// Returns `true` when input monitoring is granted for global hotkeys.
    #[must_use]
    pub const fn allows_hotkeys(self) -> bool {
        status_is_granted(self.input_monitoring)
    }

    /// Lists permissions that block hotkey-driven recording start.
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

    /// Lists permissions that block tray-initiated recording.
    ///
    /// Only hard microphone failures block tray start; input monitoring may
    /// still be [`PermissionStatus::NotDetermined`] so the user can bootstrap
    /// microphone access from the menu.
    #[must_use]
    pub fn missing_for_tray_recording(self) -> Vec<PermissionKind> {
        let mut missing = Vec::new();
        if status_blocks_recording_start(self.microphone) {
            missing.push(PermissionKind::Microphone);
        }
        missing
    }

    /// Lists permissions that block text injection.
    #[must_use]
    pub fn missing_for_injection(self) -> Vec<PermissionKind> {
        let mut missing = Vec::new();
        if !status_is_granted(self.accessibility) {
            missing.push(PermissionKind::Accessibility);
        }
        missing
    }

    /// Returns [`MacosAdapterError::MissingPermissions`] when hotkey recording
    /// is not allowed.
    pub fn ensure_recording_allowed(self) -> MacosAdapterResult<()> {
        let permissions = self.missing_for_recording();
        if permissions.is_empty() {
            return Ok(());
        }
        Err(MacosAdapterError::MissingPermissions { permissions })
    }

    /// Returns [`MacosAdapterError::MissingPermissions`] when tray recording
    /// is not allowed.
    pub fn ensure_tray_recording_allowed(self) -> MacosAdapterResult<()> {
        let permissions = self.missing_for_tray_recording();
        if permissions.is_empty() {
            return Ok(());
        }
        Err(MacosAdapterError::MissingPermissions { permissions })
    }

    /// Returns [`MacosAdapterError::MissingPermissions`] when injection is not
    /// allowed.
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

/// Logical hotkey binding mapped from configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyAction {
    /// Hold-to-talk capture.
    PushToTalk,
    /// Toggle done-mode capture on or off.
    DoneModeToggle,
    /// Discard the active capture without running the pipeline.
    CancelCurrentCapture,
}

/// Whether a hotkey binding was pressed or released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HotkeyEventKind {
    Pressed,
    Released,
}

/// Normalized hotkey input delivered by [`HotkeyEventSource`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HotkeyEvent {
    pub action: HotkeyAction,
    pub kind: HotkeyEventKind,
}

impl HotkeyEvent {
    /// Pair a [`HotkeyAction`] with a press or release [`HotkeyEventKind`].
    #[must_use]
    pub const fn new(action: HotkeyAction, kind: HotkeyEventKind) -> Self {
        Self { action, kind }
    }

    /// Returns `true` when `kind` is [`HotkeyEventKind::Pressed`].
    #[must_use]
    pub const fn is_pressed(self) -> bool {
        matches!(self.kind, HotkeyEventKind::Pressed)
    }

    /// Returns `true` when `kind` is [`HotkeyEventKind::Released`].
    #[must_use]
    pub const fn is_released(self) -> bool {
        matches!(self.kind, HotkeyEventKind::Released)
    }
}

/// WAV artifact produced when [`AudioRecorder::stop_recording`] completes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedAudio {
    pub wav_path: PathBuf,
    pub duration_ms: u64,
}

impl RecordedAudio {
    /// Build capture metadata for a finalized WAV artifact.
    #[must_use]
    pub fn new(wav_path: impl Into<PathBuf>, duration_ms: u64) -> Self {
        Self {
            wav_path: wav_path.into(),
            duration_ms,
        }
    }
}

/// Mono or interleaved PCM chunk streamed to optional transcription sinks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioFrame {
    pub samples: Vec<i16>,
    pub sample_rate_hz: u32,
    pub channels: u16,
}

/// Platform hook for menu-bar or overlay recording indicators.
#[async_trait]
pub trait IndicatorAdapter: Send + Sync {
    /// Prepare native indicator resources before the runtime loop starts.
    async fn initialize(&mut self) -> MacosAdapterResult<()>;
    /// Update the visible indicator phase.
    async fn set_state(&mut self, state: IndicatorState) -> MacosAdapterResult<()>;
    /// Update indicator phase with an optional glyph override.
    ///
    /// Default implementation ignores `glyph` and delegates to [`IndicatorAdapter::set_state`].
    async fn set_state_with_glyph(
        &mut self,
        state: IndicatorState,
        glyph: Option<char>,
    ) -> MacosAdapterResult<()> {
        let _ = glyph;
        self.set_state(state).await
    }
    /// Show `state` for at least `min_duration`, then revert to `fallback_state`.
    async fn set_temporary_state(
        &mut self,
        state: IndicatorState,
        min_duration: Duration,
        fallback_state: IndicatorState,
    ) -> MacosAdapterResult<()>;
    /// Temporary indicator update with optional primary and fallback glyphs.
    async fn set_temporary_state_with_glyph(
        &mut self,
        state: IndicatorState,
        glyph: Option<char>,
        min_duration: Duration,
        fallback_state: IndicatorState,
        fallback_glyph: Option<char>,
    ) -> MacosAdapterResult<()>;
    /// Read the indicator phase last applied by the adapter.
    async fn state(&self) -> MacosAdapterResult<IndicatorState>;
    /// Returns the glyph currently shown, if any.
    ///
    /// Default implementation returns `None`.
    async fn indicator_glyph(&self) -> MacosAdapterResult<Option<char>> {
        Ok(None)
    }
}

/// Platform hook for macOS TCC permission queries and prompts.
#[async_trait]
pub trait PermissionsAdapter: Send + Sync {
    /// Read current microphone, accessibility, and input-monitoring status.
    async fn preflight(&self) -> MacosAdapterResult<PermissionPreflightStatus>;
    /// Prompt for microphone access; returns whether access was granted.
    async fn request_microphone_access(&self) -> MacosAdapterResult<bool>;
    /// Prompt for input monitoring access; returns whether access was granted.
    async fn request_input_monitoring_access(&self) -> MacosAdapterResult<bool>;
    /// Prompt for accessibility access; returns whether access was granted.
    async fn request_accessibility_access(&self) -> MacosAdapterResult<bool>;
}

/// Async source of normalized [`HotkeyEvent`] values from the platform layer.
#[async_trait]
pub trait HotkeyEventSource: Send {
    /// Wait for the next hotkey press or release.
    ///
    /// Returns [`MacosAdapterError::HotkeyEventStreamClosed`] when the source
    /// shuts down.
    async fn next_event(&mut self) -> MacosAdapterResult<HotkeyEvent>;
}

/// Platform audio capture boundary used by the runtime coordinator.
#[async_trait(?Send)]
pub trait AudioRecorder {
    /// Begin capturing audio to a WAV file.
    ///
    /// Returns [`MacosAdapterError::RecorderAlreadyActive`] when a session is
    /// already open.
    async fn start_recording(&mut self) -> MacosAdapterResult<()>;
    /// Begin capture and optionally stream PCM frames to `sink`.
    ///
    /// Default implementation ignores `sink` and delegates to
    /// [`AudioRecorder::start_recording`].
    async fn start_recording_with_audio_sink(
        &mut self,
        sink: Option<tokio::sync::mpsc::Sender<AudioFrame>>,
    ) -> MacosAdapterResult<()> {
        let _ = sink;
        self.start_recording().await
    }
    /// Finalize the WAV artifact and return capture metadata.
    ///
    /// Returns [`MacosAdapterError::RecorderNotActive`] when idle.
    async fn stop_recording(&mut self) -> MacosAdapterResult<RecordedAudio>;
    /// Discard the active capture without producing an artifact.
    ///
    /// Returns [`MacosAdapterError::RecorderNotActive`] when idle.
    async fn cancel_recording(&mut self) -> MacosAdapterResult<()>;
}

/// Platform hook for delivering finalized text into the focused application.
#[async_trait]
pub trait TextInjector: Send + Sync {
    /// Inject Unicode text through the platform accessibility APIs.
    async fn inject_unicode_text(&self, text: &str) -> MacosAdapterResult<()>;

    /// Inject after rejecting whitespace-only payloads.
    ///
    /// Returns [`MacosAdapterError::EmptyInjectionText`] when `text` is empty
    /// after trimming.
    async fn inject_checked(&self, text: &str) -> MacosAdapterResult<()> {
        if text.trim().is_empty() {
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

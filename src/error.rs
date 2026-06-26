//! Typed errors and result alias for macOS adapter operations.
//!
//! [`MacosAdapterError`] covers platform gating, TCC permission preflight,
//! recorder lifecycle, and injection validation. Adapter traits return
//! [`MacosAdapterResult`] so callers can branch on structured failure modes.

use std::fmt;

use thiserror::Error;

/// Result type returned by macOS adapter traits and platform helpers.
pub type MacosAdapterResult<T> = Result<T, MacosAdapterError>;

/// macOS permission category checked during preflight and surfaced in
/// [`MacosAdapterError::MissingPermissions`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionKind {
    /// Microphone access for audio capture.
    Microphone,
    /// Accessibility access for Unicode text injection.
    Accessibility,
    /// Input Monitoring access for global hotkey registration.
    InputMonitoring,
}

impl PermissionKind {
    /// Stable snake-case label used in error messages and logging.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Microphone => "microphone",
            Self::Accessibility => "accessibility",
            Self::InputMonitoring => "input_monitoring",
        }
    }
}

impl fmt::Display for PermissionKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Failure modes for platform adapters, preflight, and runtime I/O boundaries.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MacosAdapterError {
    /// Muninn adapters are only supported on macOS builds.
    #[error("unsupported platform")]
    UnsupportedPlatform,
    /// One or more required TCC permissions are not granted.
    #[error("missing required permissions: {permissions:?}")]
    MissingPermissions { permissions: Vec<PermissionKind> },
    /// The hotkey event source closed its channel or stream.
    #[error("hotkey event stream closed")]
    HotkeyEventStreamClosed,
    /// [`crate::AudioRecorder::start_recording`] called while a capture is already active.
    #[error("audio recorder is already active")]
    RecorderAlreadyActive,
    /// Stop or cancel called with no active capture.
    #[error("audio recorder is not active")]
    RecorderNotActive,
    /// [`crate::TextInjector::inject_checked`] rejects whitespace-only payloads.
    #[error("text injection payload must not be empty")]
    EmptyInjectionText,
    /// Adapter operation failed with a free-form message.
    #[error("{operation} failed: {message}")]
    OperationFailed {
        operation: &'static str,
        message: String,
    },
}

impl MacosAdapterError {
    /// Construct an [`MacosAdapterError::OperationFailed`] with the given label.
    #[must_use]
    pub fn operation_failed(operation: &'static str, message: impl Into<String>) -> Self {
        Self::OperationFailed {
            operation,
            message: message.into(),
        }
    }
}
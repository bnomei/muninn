use std::fmt;

use thiserror::Error;

pub type MacosAdapterResult<T> = Result<T, MacosAdapterError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PermissionKind {
    Microphone,
    Accessibility,
    InputMonitoring,
}

impl PermissionKind {
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

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MacosAdapterError {
    #[error("unsupported platform")]
    UnsupportedPlatform,
    #[error("missing required permissions: {permissions:?}")]
    MissingPermissions { permissions: Vec<PermissionKind> },
    #[error("hotkey event stream closed")]
    HotkeyEventStreamClosed,
    #[error("audio recorder is already active")]
    RecorderAlreadyActive,
    #[error("audio recorder is not active")]
    RecorderNotActive,
    #[error("text injection payload must not be empty")]
    EmptyInjectionText,
    #[error("{operation} failed: {message}")]
    OperationFailed {
        operation: &'static str,
        message: String,
    },
}

impl MacosAdapterError {
    #[must_use]
    pub fn operation_failed(operation: &'static str, message: impl Into<String>) -> Self {
        Self::OperationFailed {
            operation,
            message: message.into(),
        }
    }
}

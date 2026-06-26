//! Compile-time and runtime platform gate for macOS-only adapters.
//!
//! Non-macOS builds compile stub implementations that report
//! [`Platform::Unsupported`] and return
//! [`crate::MacosAdapterError::UnsupportedPlatform`] from [`ensure_supported_platform`].

#[cfg(not(target_os = "macos"))]
use crate::MacosAdapterError;
use crate::{MacosAdapterResult, PermissionPreflightStatus};

/// Host platform detected at compile time and exposed for runtime checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// macOS target; adapter implementations are available.
    Macos,
    /// Non-macOS target; adapters return errors or no-op stubs.
    Unsupported,
}

/// Detect the platform for the current build target.
///
/// Compile-time `cfg` selects the implementation; there is no runtime probing.
#[cfg(target_os = "macos")]
#[must_use]
pub const fn detect_platform() -> Platform {
    Platform::Macos
}

/// Detect the platform for the current build target.
///
/// Compile-time `cfg` selects the implementation; there is no runtime probing.
#[cfg(not(target_os = "macos"))]
#[must_use]
pub const fn detect_platform() -> Platform {
    Platform::Unsupported
}

/// Returns `true` when the build target is macOS.
#[must_use]
pub const fn is_supported_platform() -> bool {
    matches!(detect_platform(), Platform::Macos)
}

/// Succeeds on macOS; returns [`crate::MacosAdapterError::UnsupportedPlatform`] elsewhere.
#[cfg(target_os = "macos")]
pub fn ensure_supported_platform() -> MacosAdapterResult<()> {
    Ok(())
}

/// Succeeds on macOS; returns [`crate::MacosAdapterError::UnsupportedPlatform`] elsewhere.
#[cfg(not(target_os = "macos"))]
pub fn ensure_supported_platform() -> MacosAdapterResult<()> {
    Err(MacosAdapterError::UnsupportedPlatform)
}

/// Preflight snapshot with every permission marked [`crate::PermissionStatus::Unsupported`].
///
/// Used on non-macOS targets so callers can exercise the same permission API
/// without special-casing the platform.
#[must_use]
pub const fn unsupported_preflight_status() -> PermissionPreflightStatus {
    PermissionPreflightStatus::unsupported()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_platform_matches_target() {
        #[cfg(target_os = "macos")]
        assert_eq!(detect_platform(), Platform::Macos);

        #[cfg(not(target_os = "macos"))]
        assert_eq!(detect_platform(), Platform::Unsupported);
    }

    #[test]
    fn is_supported_platform_matches_target() {
        #[cfg(target_os = "macos")]
        assert!(is_supported_platform());

        #[cfg(not(target_os = "macos"))]
        assert!(!is_supported_platform());
    }

    #[test]
    fn ensure_supported_platform_matches_target() {
        #[cfg(target_os = "macos")]
        assert!(ensure_supported_platform().is_ok());

        #[cfg(not(target_os = "macos"))]
        assert_eq!(
            ensure_supported_platform(),
            Err(MacosAdapterError::UnsupportedPlatform)
        );
    }

    #[test]
    fn unsupported_preflight_status_marks_all_permissions_unsupported() {
        assert_eq!(
            unsupported_preflight_status(),
            PermissionPreflightStatus::unsupported()
        );
    }
}
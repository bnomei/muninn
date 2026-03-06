#[cfg(not(target_os = "macos"))]
use crate::MacosAdapterError;
use crate::{MacosAdapterResult, PermissionPreflightStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Macos,
    Unsupported,
}

#[cfg(target_os = "macos")]
#[must_use]
pub const fn detect_platform() -> Platform {
    Platform::Macos
}

#[cfg(not(target_os = "macos"))]
#[must_use]
pub const fn detect_platform() -> Platform {
    Platform::Unsupported
}

#[must_use]
pub const fn is_supported_platform() -> bool {
    matches!(detect_platform(), Platform::Macos)
}

#[cfg(target_os = "macos")]
pub fn ensure_supported_platform() -> MacosAdapterResult<()> {
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn ensure_supported_platform() -> MacosAdapterResult<()> {
    Err(MacosAdapterError::UnsupportedPlatform)
}

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

use async_trait::async_trait;

#[cfg(not(target_os = "macos"))]
use crate::{
    MacosAdapterError, MacosAdapterResult, PermissionPreflightStatus, PermissionStatus,
    PermissionsAdapter,
};
#[cfg(target_os = "macos")]
use crate::{MacosAdapterResult, PermissionPreflightStatus, PermissionStatus, PermissionsAdapter};

#[derive(Debug, Clone, Copy, Default)]
pub struct MacosPermissionsAdapter;

impl MacosPermissionsAdapter {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl PermissionsAdapter for MacosPermissionsAdapter {
    async fn preflight(&self) -> MacosAdapterResult<PermissionPreflightStatus> {
        #[cfg(target_os = "macos")]
        {
            Ok(preflight_permissions())
        }

        #[cfg(not(target_os = "macos"))]
        {
            Ok(PermissionPreflightStatus::unsupported())
        }
    }

    async fn request_input_monitoring_access(&self) -> MacosAdapterResult<bool> {
        #[cfg(target_os = "macos")]
        {
            Ok(request_input_monitoring_access())
        }

        #[cfg(not(target_os = "macos"))]
        {
            Err(MacosAdapterError::UnsupportedPlatform)
        }
    }

    async fn request_accessibility_access(&self) -> MacosAdapterResult<bool> {
        #[cfg(target_os = "macos")]
        {
            Ok(request_accessibility_access())
        }

        #[cfg(not(target_os = "macos"))]
        {
            Err(MacosAdapterError::UnsupportedPlatform)
        }
    }
}

#[cfg(target_os = "macos")]
fn preflight_permissions() -> PermissionPreflightStatus {
    PermissionPreflightStatus {
        microphone: microphone_status(),
        accessibility: accessibility_status(),
        input_monitoring: input_monitoring_status(),
    }
}

#[cfg(target_os = "macos")]
fn accessibility_status() -> PermissionStatus {
    if macos_accessibility_client::accessibility::application_is_trusted() {
        PermissionStatus::Granted
    } else {
        PermissionStatus::Denied
    }
}

#[cfg(target_os = "macos")]
fn input_monitoring_status() -> PermissionStatus {
    if objc2_core_graphics::CGPreflightListenEventAccess() {
        PermissionStatus::Granted
    } else {
        PermissionStatus::Denied
    }
}

#[cfg(target_os = "macos")]
fn request_input_monitoring_access() -> bool {
    objc2_core_graphics::CGRequestListenEventAccess()
}

#[cfg(target_os = "macos")]
fn request_accessibility_access() -> bool {
    macos_accessibility_client::accessibility::application_is_trusted_with_prompt()
}

#[cfg(target_os = "macos")]
fn microphone_status() -> PermissionStatus {
    use objc2_avf_audio::{AVAudioApplication, AVAudioApplicationRecordPermission};

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let permission = std::panic::catch_unwind(|| {
        let application = unsafe { AVAudioApplication::sharedInstance() };
        unsafe { application.recordPermission() }
    });
    std::panic::set_hook(previous_hook);

    match permission {
        Ok(permission) if permission == AVAudioApplicationRecordPermission::Granted => {
            PermissionStatus::Granted
        }
        Ok(permission) if permission == AVAudioApplicationRecordPermission::Denied => {
            PermissionStatus::Denied
        }
        Ok(_) | Err(_) => PermissionStatus::NotDetermined,
    }
}

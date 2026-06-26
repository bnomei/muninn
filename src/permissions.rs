//! macOS TCC permission preflight and request prompts.
//!
//! [`MacosPermissionsAdapter`] implements [`PermissionsAdapter`], querying
//! microphone (`AVAudioApplication`), accessibility (`AXIsProcessTrusted`), and
//! input monitoring (`CGPreflightListenEventAccess`) status. Request methods show
//! system prompts on macOS; non-macOS preflight reports unsupported and requests
//! return [`crate::MacosAdapterError::UnsupportedPlatform`].

use async_trait::async_trait;

#[cfg(not(target_os = "macos"))]
use crate::{
    MacosAdapterError, MacosAdapterResult, PermissionPreflightStatus, PermissionStatus,
    PermissionsAdapter,
};
#[cfg(target_os = "macos")]
use crate::{MacosAdapterResult, PermissionPreflightStatus, PermissionStatus, PermissionsAdapter};

/// macOS [`PermissionsAdapter`] for microphone, accessibility, and input monitoring.
#[derive(Debug, Clone, Copy, Default)]
pub struct MacosPermissionsAdapter;

impl MacosPermissionsAdapter {
    /// Returns the shared zero-sized permissions adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl PermissionsAdapter for MacosPermissionsAdapter {
    /// Read current TCC status without showing system prompts.
    ///
    /// On macOS, queries microphone, accessibility, and input-monitoring state.
    /// Non-macOS returns [`PermissionPreflightStatus::unsupported`].
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

    /// Show the macOS microphone consent dialog and return whether access was granted.
    ///
    /// Blocks until the `AVAudioApplication` completion handler fires. Non-macOS
    /// returns [`crate::MacosAdapterError::UnsupportedPlatform`].
    async fn request_microphone_access(&self) -> MacosAdapterResult<bool> {
        #[cfg(target_os = "macos")]
        {
            Ok(request_microphone_access())
        }

        #[cfg(not(target_os = "macos"))]
        {
            Err(MacosAdapterError::UnsupportedPlatform)
        }
    }

    /// Show the macOS Input Monitoring consent dialog via `CGRequestListenEventAccess`.
    ///
    /// Non-macOS returns [`crate::MacosAdapterError::UnsupportedPlatform`].
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

    /// Prompt for Accessibility trust via `application_is_trusted_with_prompt`.
    ///
    /// Non-macOS returns [`crate::MacosAdapterError::UnsupportedPlatform`].
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
fn request_microphone_access() -> bool {
    use std::sync::{mpsc, Arc, Mutex};

    use block2::RcBlock;
    use objc2_avf_audio::AVAudioApplication;

    let (sender, receiver) = mpsc::channel::<bool>();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let response_sender = Arc::clone(&sender);
    let response = RcBlock::new(move |granted: objc2::runtime::Bool| {
        if let Ok(mut guard) = response_sender.lock() {
            if let Some(sender) = guard.take() {
                let _ = sender.send(granted.as_bool());
            }
        }
    });

    unsafe {
        AVAudioApplication::requestRecordPermissionWithCompletionHandler(&response);
    }

    receiver.recv().unwrap_or(false)
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

use async_trait::async_trait;

use crate::{MacosAdapterError, MacosAdapterResult, TextInjector};

#[derive(Debug, Clone, Copy, Default)]
pub struct MacosTextInjector;

impl MacosTextInjector {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[async_trait]
impl TextInjector for MacosTextInjector {
    async fn inject_unicode_text(&self, text: &str) -> MacosAdapterResult<()> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = text;
            return Err(MacosAdapterError::UnsupportedPlatform);
        }

        #[cfg(target_os = "macos")]
        {
            inject_unicode_text(text)
        }
    }
}

#[cfg(target_os = "macos")]
fn inject_unicode_text(text: &str) -> MacosAdapterResult<()> {
    use core_graphics::event::{CGEvent, CGEventTapLocation};
    use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};
    use macos_accessibility_client::accessibility::application_is_trusted_with_prompt;

    if !application_is_trusted_with_prompt() {
        return Err(MacosAdapterError::MissingPermissions {
            permissions: vec![crate::PermissionKind::Accessibility],
        });
    }

    let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).map_err(|_| {
        MacosAdapterError::operation_failed("create_event_source", "unable to create event source")
    })?;

    let key_down = CGEvent::new_keyboard_event(source.clone(), 0, true).map_err(|_| {
        MacosAdapterError::operation_failed(
            "create_key_down_event",
            "unable to create key down event",
        )
    })?;
    key_down.set_string(text);
    key_down.post(CGEventTapLocation::HID);

    let key_up = CGEvent::new_keyboard_event(source, 0, false).map_err(|_| {
        MacosAdapterError::operation_failed("create_key_up_event", "unable to create key up event")
    })?;
    key_up.set_string(text);
    key_up.post(CGEventTapLocation::HID);

    Ok(())
}

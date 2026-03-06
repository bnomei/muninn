use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TargetContextSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    pub captured_at: String,
}

impl TargetContextSnapshot {
    #[must_use]
    pub fn empty_now() -> Self {
        Self {
            bundle_id: None,
            app_name: None,
            window_title: None,
            captured_at: Utc::now().to_rfc3339(),
        }
    }
}

#[must_use]
pub fn capture_frontmost_target_context() -> TargetContextSnapshot {
    #[cfg(target_os = "macos")]
    {
        capture_frontmost_target_context_macos()
    }

    #[cfg(not(target_os = "macos"))]
    {
        TargetContextSnapshot::empty_now()
    }
}

#[cfg(target_os = "macos")]
fn capture_frontmost_target_context_macos() -> TargetContextSnapshot {
    use objc2_app_kit::NSWorkspace;

    let captured_at = Utc::now().to_rfc3339();
    let frontmost_application = unsafe {
        NSWorkspace::sharedWorkspace().frontmostApplication()
    };

    let Some(application) = frontmost_application else {
        return TargetContextSnapshot {
            captured_at,
            ..TargetContextSnapshot::default()
        };
    };

    let pid = unsafe { application.processIdentifier() };
    let bundle_id = unsafe { application.bundleIdentifier() }
        .map(|value| value.to_string())
        .and_then(normalize_string);
    let app_name = unsafe { application.localizedName() }
        .map(|value| value.to_string())
        .and_then(normalize_string);
    let window_title = best_effort_window_title_for_pid(pid);

    TargetContextSnapshot {
        bundle_id,
        app_name,
        window_title,
        captured_at,
    }
}

#[cfg(target_os = "macos")]
fn best_effort_window_title_for_pid(pid: i32) -> Option<String> {
    use objc2_core_foundation::CFDictionary;
    use objc2_core_graphics::{CGWindowListCopyWindowInfo, CGWindowListOption};

    let windows = CGWindowListCopyWindowInfo(
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements,
        0,
    )?;

    let mut layer_zero_title = None;
    let mut any_title = None;

    for index in 0..windows.count() {
        let dictionary_ref = unsafe { windows.value_at_index(index) } as *const CFDictionary;
        if dictionary_ref.is_null() {
            continue;
        }
        let dictionary = unsafe { &*dictionary_ref };

        let Ok(owner_pid) = get_cf_number_i32_value(dictionary, "kCGWindowOwnerPID") else {
            continue;
        };
        if owner_pid != pid {
            continue;
        }

        let title = get_cf_string_value(dictionary, "kCGWindowName")
            .ok()
            .and_then(normalize_string);
        if any_title.is_none() {
            any_title = title.clone();
        }

        let layer = get_cf_number_i32_value(dictionary, "kCGWindowLayer").unwrap_or_default();
        if layer == 0 && layer_zero_title.is_none() {
            layer_zero_title = title;
        }

        if layer_zero_title.is_some() && any_title.is_some() {
            break;
        }
    }

    layer_zero_title.or(any_title)
}

#[cfg(target_os = "macos")]
fn get_cf_string_value(
    dictionary: &objc2_core_foundation::CFDictionary,
    key: &str,
) -> Result<String, ()> {
    use objc2_core_foundation::CFString;

    let key = CFString::from_str(key);
    let value = unsafe { dictionary.value(key.as_ref() as *const CFString as *const _) };
    if value.is_null() {
        return Err(());
    }

    let value = unsafe { &*(value as *const CFString) };
    Ok(value.to_string())
}

#[cfg(target_os = "macos")]
fn get_cf_number_i32_value(
    dictionary: &objc2_core_foundation::CFDictionary,
    key: &str,
) -> Result<i32, ()> {
    use std::ffi::c_void;

    use objc2_core_foundation::{CFNumber, CFNumberType, CFString};

    let key = CFString::from_str(key);
    let value = unsafe { dictionary.value(key.as_ref() as *const CFString as *const _) };
    if value.is_null() {
        return Err(());
    }

    let number = unsafe { &*(value as *const CFNumber) };
    let mut result = 0_i32;
    let ok = unsafe {
        number.value(
            CFNumberType::IntType,
            &mut result as *mut i32 as *mut c_void,
        )
    };
    if ok { Ok(result) } else { Err(()) }
}

fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn normalize_string(value: String) -> Option<String> {
    normalize_optional_string(Some(value))
}

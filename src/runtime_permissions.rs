//! macOS TCC permission refresh before recording and injection.
//!
//! Hotkey recording requires Input Monitoring; tray and external controls use a
//! lighter preflight. Prompting during a user gesture may grant access but still
//! aborts that gesture so the user retries with stable permissions.

use anyhow::{anyhow, Result};
use muninn::{PermissionKind, PermissionPreflightStatus, PermissionStatus, PermissionsAdapter};
use tracing::{info, warn};

use crate::logging;

async fn refresh_permissions_status_with<A>(permissions: &A) -> Result<PermissionPreflightStatus>
where
    A: PermissionsAdapter,
{
    permissions
        .preflight()
        .await
        .map_err(|error| anyhow!(error))
}

/// Outcome of a recording permission refresh triggered by a user gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RecordingPermissionRefresh {
    /// Permission snapshot after any prompts completed.
    pub(crate) preflight: PermissionPreflightStatus,
    /// Whether a microphone access prompt was shown during this refresh.
    pub(crate) requested_microphone: bool,
    /// Whether an Input Monitoring prompt was shown during this refresh.
    pub(crate) requested_input_monitoring: bool,
}

/// Surface that initiated a recording start request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordingStartSource {
    /// Global hotkey listener; requires Input Monitoring when not already granted.
    Hotkey,
    /// Menu-bar tray click; does not bootstrap Input Monitoring on first use.
    Tray,
    /// MCP or `muninn://` external control action.
    External,
}

/// Stable log label for a [`RecordingStartSource`].
pub(crate) fn recording_source_name(source: RecordingStartSource) -> &'static str {
    match source {
        RecordingStartSource::Hotkey => "hotkey",
        RecordingStartSource::Tray => "tray",
        RecordingStartSource::External => "external",
    }
}

/// Prompt for missing recording permissions appropriate to the start source.
///
/// Microphone is requested for all sources. Input Monitoring prompts run only
/// for hotkey starts.
pub(crate) async fn refresh_recording_permissions_for_user_action<A>(
    permissions: &A,
    source: RecordingStartSource,
) -> Result<RecordingPermissionRefresh>
where
    A: PermissionsAdapter,
{
    let mut preflight = refresh_permissions_status_with(permissions).await?;
    let mut requested_microphone = false;
    let mut requested_input_monitoring = false;

    if matches!(
        preflight.microphone,
        PermissionStatus::Denied | PermissionStatus::NotDetermined
    ) {
        requested_microphone = true;
        let granted = permissions
            .request_microphone_access()
            .await
            .map_err(|error| anyhow!(error))?;
        info!(target: logging::TARGET_RECORDING, granted, "requested microphone access");
        preflight = refresh_permissions_status_with(permissions).await?;
    }

    if matches!(source, RecordingStartSource::Hotkey)
        && matches!(
            preflight.input_monitoring,
            PermissionStatus::Denied | PermissionStatus::NotDetermined
        )
    {
        requested_input_monitoring = true;
        let granted = permissions
            .request_input_monitoring_access()
            .await
            .map_err(|error| anyhow!(error))?;
        info!(
            target: logging::TARGET_HOTKEY,
            granted,
            "requested Input Monitoring access"
        );
        preflight = refresh_permissions_status_with(permissions).await?;
    }

    Ok(RecordingPermissionRefresh {
        preflight,
        requested_microphone,
        requested_input_monitoring,
    })
}

/// Outcome of an injection permission refresh triggered before text output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct InjectionPermissionRefresh {
    /// Permission snapshot after any prompts completed.
    pub(crate) preflight: PermissionPreflightStatus,
    /// Whether an Accessibility prompt was shown during this refresh.
    pub(crate) requested_accessibility: bool,
}

/// Prompt for Accessibility access when injection is about to run.
pub(crate) async fn refresh_injection_permissions_for_user_action<A>(
    permissions: &A,
) -> Result<InjectionPermissionRefresh>
where
    A: PermissionsAdapter,
{
    let mut preflight = refresh_permissions_status_with(permissions).await?;
    let mut requested_accessibility = false;

    if matches!(
        preflight.accessibility,
        PermissionStatus::Denied | PermissionStatus::NotDetermined
    ) {
        requested_accessibility = true;
        let granted = permissions
            .request_accessibility_access()
            .await
            .map_err(|error| anyhow!(error))?;
        info!(
            target: logging::TARGET_RUNTIME,
            granted,
            "requested Accessibility access"
        );
        preflight = refresh_permissions_status_with(permissions).await?;
    }

    Ok(InjectionPermissionRefresh {
        preflight,
        requested_accessibility,
    })
}

/// Whether to cancel this recording start after a permission refresh.
///
/// Aborts when required permissions are still missing, or when a prompt fired
/// during this gesture so the user can retry with stable TCC state. Hotkey
/// Input Monitoring prompts always abort; tray starts continue after IM prompts.
pub(crate) fn should_abort_recording_start(
    preflight: PermissionPreflightStatus,
    requested_microphone: bool,
    requested_input_monitoring: bool,
    source: RecordingStartSource,
) -> bool {
    match ensure_recording_can_start(preflight, source) {
        Ok(())
            if requested_microphone
                || (requested_input_monitoring
                    && matches!(source, RecordingStartSource::Hotkey)) =>
        {
            info!(
                target: logging::TARGET_RECORDING,
                "recording permissions changed during this interaction; retry the recording gesture"
            );
            true
        }
        Ok(()) => false,
        Err(error) => {
            log_recording_start_block(preflight, source, &error);
            true
        }
    }
}

fn log_recording_start_block(
    preflight: PermissionPreflightStatus,
    source: RecordingStartSource,
    error: &anyhow::Error,
) {
    let missing = match source {
        RecordingStartSource::Hotkey => preflight.missing_for_recording(),
        RecordingStartSource::Tray | RecordingStartSource::External => {
            preflight.missing_for_tray_recording()
        }
    };

    if matches!(source, RecordingStartSource::Hotkey)
        && missing.contains(&PermissionKind::InputMonitoring)
    {
        warn!(
            target: logging::TARGET_RECORDING,
            ?preflight,
            ?missing,
            error = %error,
            "recording blocked by missing Input Monitoring permission; enable Muninn in System Settings > Privacy & Security > Input Monitoring. If the prompt does not reappear, reset the service with `tccutil reset ListenEvent` and relaunch Muninn"
        );
        return;
    }

    if missing.contains(&PermissionKind::Microphone) {
        warn!(
            target: logging::TARGET_RECORDING,
            ?preflight,
            ?missing,
            error = %error,
            "recording blocked by missing microphone permission; enable Muninn in System Settings > Privacy & Security > Microphone"
        );
        return;
    }

    warn!(
        target: logging::TARGET_RECORDING,
        ?preflight,
        ?missing,
        error = %error,
        "recording blocked by missing permissions"
    );
}

/// Whether to skip injection after a permission refresh.
///
/// Proceeds when Accessibility is granted, including immediately after a
/// successful prompt. Aborts when injection permissions remain blocked.
pub(crate) fn should_abort_injection(
    preflight: PermissionPreflightStatus,
    requested_accessibility: bool,
) -> bool {
    match ensure_injection_allowed(preflight) {
        Ok(()) => {
            if requested_accessibility {
                info!(
                    target: logging::TARGET_RUNTIME,
                    "Accessibility access was granted during this interaction; proceeding with injection"
                );
            }
            false
        }
        Err(error) => {
            log_injection_block(preflight, &error);
            true
        }
    }
}

fn log_injection_block(preflight: PermissionPreflightStatus, error: &anyhow::Error) {
    let missing = preflight.missing_for_injection();

    if missing.contains(&PermissionKind::Accessibility) {
        warn!(
            target: logging::TARGET_RUNTIME,
            ?preflight,
            ?missing,
            error = %error,
            "injection blocked by missing Accessibility permission; enable Muninn in System Settings > Privacy & Security > Accessibility. If the prompt does not reappear, reset the service with `tccutil reset Accessibility` and relaunch Muninn"
        );
        return;
    }

    warn!(
        target: logging::TARGET_RUNTIME,
        ?preflight,
        ?missing,
        error = %error,
        "injection blocked by missing permissions"
    );
}

/// Validate that recording may start for the given source and preflight snapshot.
pub(crate) fn ensure_recording_can_start(
    preflight: PermissionPreflightStatus,
    source: RecordingStartSource,
) -> Result<()> {
    match source {
        RecordingStartSource::Hotkey => {
            if matches!(preflight.input_monitoring, PermissionStatus::Granted)
                && !matches!(
                    preflight.microphone,
                    PermissionStatus::Denied
                        | PermissionStatus::Restricted
                        | PermissionStatus::Unsupported
                )
            {
                return Ok(());
            }

            preflight
                .ensure_recording_allowed()
                .map_err(|error| anyhow!(error))
        }
        RecordingStartSource::Tray | RecordingStartSource::External => preflight
            .ensure_tray_recording_allowed()
            .map_err(|error| anyhow!(error)),
    }
}

fn ensure_injection_allowed(preflight: PermissionPreflightStatus) -> Result<()> {
    preflight
        .ensure_injection_allowed()
        .map_err(|error| anyhow!(error))
}

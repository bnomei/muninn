use std::sync::{Arc, Mutex};

use muninn::{IndicatorState, PermissionPreflightStatus, PermissionStatus};
use serde::Serialize;
use tracing::warn;

use crate::logging::TARGET_RUNTIME;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RuntimeStatusSnapshot {
    pub state: RuntimeStatusState,
    pub recording_active: bool,
    pub busy: bool,
    pub permissions: RuntimePermissionSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<RuntimeFailureSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimeStatusState {
    Idle,
    RecordingActive,
    PermissionBlocked,
    AlreadyRunning,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RuntimePermissionSnapshot {
    pub microphone: RuntimePermissionStatus,
    pub accessibility: RuntimePermissionStatus,
    pub input_monitoring: RuntimePermissionStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RuntimePermissionStatus {
    Granted,
    Denied,
    NotDetermined,
    Restricted,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RuntimeFailureSnapshot {
    pub message: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RuntimeStatusHandle {
    inner: Arc<Mutex<RuntimeStatusInner>>,
}

#[derive(Debug, Clone)]
struct RuntimeStatusInner {
    indicator_state: IndicatorState,
    permissions: PermissionPreflightStatus,
    failure: Option<RuntimeFailureSnapshot>,
}

impl RuntimeStatusHandle {
    pub(crate) fn new(permissions: PermissionPreflightStatus) -> Self {
        Self {
            inner: Arc::new(Mutex::new(RuntimeStatusInner {
                indicator_state: IndicatorState::Idle,
                permissions,
                failure: None,
            })),
        }
    }

    pub(crate) fn snapshot(&self) -> RuntimeStatusSnapshot {
        self.with_inner(|inner| snapshot_from_inner(inner))
    }

    pub(crate) fn set_indicator_state(&self, state: IndicatorState) {
        self.with_inner_mut("set_indicator_state", |inner| {
            inner.indicator_state = state;
            if state != IndicatorState::Idle {
                inner.failure = None;
            }
        });
    }

    pub(crate) fn set_permissions(&self, permissions: PermissionPreflightStatus) {
        self.with_inner_mut("set_permissions", |inner| {
            inner.permissions = permissions;
        });
    }

    pub(crate) fn set_failure(&self, message: String) {
        self.with_inner_mut("set_failure", |inner| {
            inner.indicator_state = IndicatorState::Idle;
            inner.failure = Some(RuntimeFailureSnapshot { message });
        });
    }

    fn with_inner<T>(&self, f: impl FnOnce(&RuntimeStatusInner) -> T) -> T {
        match self.inner.lock() {
            Ok(guard) => f(&guard),
            Err(poisoned) => {
                warn!(target: TARGET_RUNTIME, "runtime status mutex poisoned; recovering");
                let guard = poisoned.into_inner();
                f(&guard)
            }
        }
    }

    fn with_inner_mut(&self, context: &'static str, f: impl FnOnce(&mut RuntimeStatusInner)) {
        match self.inner.lock() {
            Ok(mut guard) => f(&mut guard),
            Err(poisoned) => {
                warn!(target: TARGET_RUNTIME, context, "runtime status mutex poisoned; recovering");
                let mut guard = poisoned.into_inner();
                f(&mut guard);
                self.inner.clear_poison();
            }
        }
    }
}

fn snapshot_from_inner(inner: &RuntimeStatusInner) -> RuntimeStatusSnapshot {
    let recording_active = inner.indicator_state.is_recording();
    let busy = recording_active || inner.indicator_state.is_processing();
    let permission_blocked = matches!(inner.indicator_state, IndicatorState::MissingCredentials)
        || !inner.permissions.allows_recording();
    let state = if inner.failure.is_some() {
        RuntimeStatusState::Failed
    } else if recording_active {
        RuntimeStatusState::RecordingActive
    } else if busy {
        RuntimeStatusState::AlreadyRunning
    } else if permission_blocked {
        RuntimeStatusState::PermissionBlocked
    } else {
        RuntimeStatusState::Idle
    };

    RuntimeStatusSnapshot {
        state,
        recording_active,
        busy,
        permissions: inner.permissions.into(),
        failure: inner.failure.clone(),
    }
}

impl From<PermissionPreflightStatus> for RuntimePermissionSnapshot {
    fn from(status: PermissionPreflightStatus) -> Self {
        Self {
            microphone: status.microphone.into(),
            accessibility: status.accessibility.into(),
            input_monitoring: status.input_monitoring.into(),
        }
    }
}

impl From<PermissionStatus> for RuntimePermissionStatus {
    fn from(status: PermissionStatus) -> Self {
        match status {
            PermissionStatus::Granted => Self::Granted,
            PermissionStatus::Denied => Self::Denied,
            PermissionStatus::NotDetermined => Self::NotDetermined,
            PermissionStatus::Restricted => Self::Restricted,
            PermissionStatus::Unsupported => Self::Unsupported,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use muninn::RecordingMode;

    #[test]
    fn status_contract_serializes_idle_snapshot() {
        let handle = RuntimeStatusHandle::new(PermissionPreflightStatus::all_granted());
        let value = serde_json::to_value(handle.snapshot()).expect("snapshot serializes");

        assert_eq!(value["state"], "idle");
        assert_eq!(value["recording_active"], false);
        assert_eq!(value["busy"], false);
        assert_eq!(value["permissions"]["microphone"], "granted");
        assert!(value.get("failure").is_none());
    }

    #[test]
    fn status_exposes_recording_busy_permission_and_failed_states() {
        let handle = RuntimeStatusHandle::new(PermissionPreflightStatus::all_granted());
        handle.set_indicator_state(IndicatorState::Recording {
            mode: RecordingMode::DoneMode,
        });
        assert_eq!(handle.snapshot().state, RuntimeStatusState::RecordingActive);
        assert!(handle.snapshot().recording_active);

        handle.set_indicator_state(IndicatorState::Pipeline);
        assert_eq!(handle.snapshot().state, RuntimeStatusState::AlreadyRunning);
        assert!(handle.snapshot().busy);

        let blocked = RuntimeStatusHandle::new(PermissionPreflightStatus {
            microphone: PermissionStatus::Denied,
            accessibility: PermissionStatus::Granted,
            input_monitoring: PermissionStatus::Granted,
        });
        assert_eq!(
            blocked.snapshot().state,
            RuntimeStatusState::PermissionBlocked
        );

        handle.set_failure("runtime stopped".to_string());
        let snapshot = handle.snapshot();
        assert_eq!(snapshot.state, RuntimeStatusState::Failed);
        assert_eq!(
            snapshot.failure.expect("failure").message,
            "runtime stopped"
        );
    }
}

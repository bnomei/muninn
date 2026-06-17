use muninn::{AppEvent, AppState};

/// A transport-agnostic recording-control request from an external agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalControlAction {
    /// Begin recording. No-op when a capture is already active.
    Start,
    /// Finish the active recording and run the pipeline. No-op when idle.
    Stop,
    /// Start when idle, otherwise finish the active recording.
    Toggle,
    /// Discard the active recording without running the pipeline.
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExternalControlOutcome {
    Enabled(AppEvent),
    Disabled,
    Noop,
}

impl ExternalControlAction {
    /// Map an action onto the [`AppEvent`] appropriate for the current state.
    ///
    /// Start requests are explicitly gated by config. `Noop` means the action
    /// is allowed but has no state transition to perform.
    pub(crate) fn resolve(
        self,
        state: AppState,
        start_recording_enabled: bool,
    ) -> ExternalControlOutcome {
        match self {
            ExternalControlAction::Start => match state {
                AppState::Idle if start_recording_enabled => {
                    ExternalControlOutcome::Enabled(AppEvent::DoneTogglePressed)
                }
                AppState::Idle => ExternalControlOutcome::Disabled,
                _ => ExternalControlOutcome::Noop,
            },
            ExternalControlAction::Stop => Self::stop_event(state).map_or(
                ExternalControlOutcome::Noop,
                ExternalControlOutcome::Enabled,
            ),
            ExternalControlAction::Toggle => match state {
                AppState::Idle if start_recording_enabled => {
                    ExternalControlOutcome::Enabled(AppEvent::DoneTogglePressed)
                }
                AppState::Idle => ExternalControlOutcome::Disabled,
                _ => Self::stop_event(state).map_or(
                    ExternalControlOutcome::Noop,
                    ExternalControlOutcome::Enabled,
                ),
            },
            ExternalControlAction::Cancel => match state {
                AppState::RecordingPushToTalk | AppState::RecordingDone => {
                    ExternalControlOutcome::Enabled(AppEvent::CancelPressed)
                }
                _ => ExternalControlOutcome::Noop,
            },
        }
    }

    pub(crate) fn to_app_event(self, state: AppState) -> Option<AppEvent> {
        match self.resolve(state, true) {
            ExternalControlOutcome::Enabled(app_event) => Some(app_event),
            ExternalControlOutcome::Disabled | ExternalControlOutcome::Noop => None,
        }
    }

    fn stop_event(state: AppState) -> Option<AppEvent> {
        match state {
            AppState::RecordingPushToTalk => Some(AppEvent::PttReleased),
            AppState::RecordingDone => Some(AppEvent::DoneTogglePressed),
            _ => None,
        }
    }
}

/// Parse a `muninn://` URL into an [`ExternalControlAction`].
///
/// Accepts both authority and path forms (`muninn://record`,
/// `muninn:///record`) and a small set of verb aliases. Returns `None` for
/// unknown schemes or verbs.
pub(crate) fn parse_url_action(url: &str) -> Option<ExternalControlAction> {
    let trimmed = url.trim();
    let rest = strip_scheme(trimmed, "muninn")?;
    let verb = rest
        .trim_start_matches('/')
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();

    match verb.as_str() {
        "record" | "start" => Some(ExternalControlAction::Start),
        "stop" | "done" => Some(ExternalControlAction::Stop),
        "toggle" => Some(ExternalControlAction::Toggle),
        "cancel" | "abort" => Some(ExternalControlAction::Cancel),
        _ => None,
    }
}

fn strip_scheme<'a>(url: &'a str, scheme: &str) -> Option<&'a str> {
    let prefix = format!("{scheme}://");
    if url.len() < prefix.len() {
        return None;
    }
    let (head, tail) = url.split_at(prefix.len());
    if head.eq_ignore_ascii_case(&prefix) {
        Some(tail)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_verbs_in_authority_and_path_forms() {
        assert_eq!(
            parse_url_action("muninn://record"),
            Some(ExternalControlAction::Start)
        );
        assert_eq!(
            parse_url_action("muninn:///start"),
            Some(ExternalControlAction::Start)
        );
        assert_eq!(
            parse_url_action("MUNINN://Stop"),
            Some(ExternalControlAction::Stop)
        );
        assert_eq!(
            parse_url_action("muninn://toggle/"),
            Some(ExternalControlAction::Toggle)
        );
        assert_eq!(
            parse_url_action("muninn://cancel?x=1"),
            Some(ExternalControlAction::Cancel)
        );
    }

    #[test]
    fn rejects_unknown_scheme_or_verb() {
        assert_eq!(parse_url_action("https://record"), None);
        assert_eq!(parse_url_action("muninn://explode"), None);
        assert_eq!(parse_url_action(""), None);
    }

    #[test]
    fn start_is_noop_unless_idle() {
        assert_eq!(
            ExternalControlAction::Start.resolve(AppState::Idle, true),
            ExternalControlOutcome::Enabled(AppEvent::DoneTogglePressed)
        );
        assert_eq!(
            ExternalControlAction::Start.resolve(AppState::RecordingDone, true),
            ExternalControlOutcome::Noop
        );
        assert_eq!(
            ExternalControlAction::Start.resolve(AppState::Idle, false),
            ExternalControlOutcome::Disabled
        );
    }

    #[test]
    fn stop_and_cancel_depend_on_active_capture() {
        assert_eq!(
            ExternalControlAction::Stop.resolve(AppState::RecordingPushToTalk, false),
            ExternalControlOutcome::Enabled(AppEvent::PttReleased)
        );
        assert_eq!(
            ExternalControlAction::Stop.resolve(AppState::RecordingDone, false),
            ExternalControlOutcome::Enabled(AppEvent::DoneTogglePressed)
        );
        assert_eq!(
            ExternalControlAction::Stop.resolve(AppState::Idle, false),
            ExternalControlOutcome::Noop
        );
        assert_eq!(
            ExternalControlAction::Cancel.resolve(AppState::RecordingDone, false),
            ExternalControlOutcome::Enabled(AppEvent::CancelPressed)
        );
        assert_eq!(
            ExternalControlAction::Cancel.resolve(AppState::Idle, false),
            ExternalControlOutcome::Noop
        );
    }

    #[test]
    fn toggle_starts_when_idle_and_stops_active_capture() {
        // A tray click resolves to Toggle; this is the logic that makes a click
        // stop a recording regardless of how it was started.
        assert_eq!(
            ExternalControlAction::Toggle.resolve(AppState::Idle, true),
            ExternalControlOutcome::Enabled(AppEvent::DoneTogglePressed)
        );
        // Regression: a recording started in done mode (hotkey, MCP, or URL
        // scheme) must stop on the next toggle instead of being a no-op.
        assert_eq!(
            ExternalControlAction::Toggle.resolve(AppState::RecordingDone, false),
            ExternalControlOutcome::Enabled(AppEvent::DoneTogglePressed)
        );
        assert_eq!(
            ExternalControlAction::Toggle.resolve(AppState::RecordingPushToTalk, false),
            ExternalControlOutcome::Enabled(AppEvent::PttReleased)
        );
    }
}

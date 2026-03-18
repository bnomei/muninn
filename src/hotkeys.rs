use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use crate::config::{HotkeyBinding, HotkeysConfig, TriggerType};
use async_trait::async_trait;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tracing::warn;

use crate::{
    HotkeyAction, HotkeyEvent, HotkeyEventKind, HotkeyEventSource, MacosAdapterError,
    MacosAdapterResult, TARGET_HOTKEY,
};

/// Platform-specific key type: `rdev::Key` on macOS, opaque u32 elsewhere.
#[cfg(target_os = "macos")]
type PlatformKey = rdev::Key;
#[cfg(not(target_os = "macos"))]
type PlatformKey = u32;

#[derive(Debug, Clone)]
pub struct MacosHotkeyBinding {
    action: HotkeyAction,
    trigger: TriggerType,
    key: Option<PlatformKey>,
    modifiers: ModifierSet,
    double_tap_modifier: Option<Modifier>,
    double_tap_timeout_ms: u64,
}

#[derive(Debug, Clone)]
pub struct MacosHotkeyBindings {
    bindings: Vec<MacosHotkeyBinding>,
}

#[derive(Debug)]
pub struct MacosHotkeyEventSource {
    receiver: Receiver<MacosAdapterResult<HotkeyEvent>>,
}

const HOTKEY_EVENT_BUFFER_CAPACITY: usize = 32;
const HOTKEY_DROP_WARN_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, Default)]
struct ModifierSet {
    control: bool,
    shift: bool,
    option: bool,
    command: bool,
}

#[derive(Debug, Default)]
struct HotkeyRuntimeState {
    modifiers: ModifierSet,
    active_hold_actions: Vec<HotkeyAction>,
    active_double_tap_actions: Vec<HotkeyAction>,
    last_modifier_taps: ModifierTapTimes,
}

#[derive(Debug, Default)]
struct HotkeyDropDiagnostics {
    dropped_since_last_warning: u64,
    last_warning_at: Option<Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HotkeyDropWarning {
    dropped_events: u64,
}

#[derive(Debug, Clone, Copy, Default)]
struct ModifierTapTimes {
    control: Option<SystemTime>,
    shift: Option<SystemTime>,
    option: Option<SystemTime>,
    command: Option<SystemTime>,
}

static HOTKEY_DROP_DIAGNOSTICS: OnceLock<Mutex<HotkeyDropDiagnostics>> = OnceLock::new();

impl MacosHotkeyBindings {
    pub fn from_config(config: &HotkeysConfig) -> MacosAdapterResult<Self> {
        Ok(Self {
            bindings: vec![
                parse_binding(HotkeyAction::PushToTalk, &config.push_to_talk)?,
                parse_binding(HotkeyAction::DoneModeToggle, &config.done_mode_toggle)?,
                parse_binding(
                    HotkeyAction::CancelCurrentCapture,
                    &config.cancel_current_capture,
                )?,
            ],
        })
    }
}

impl MacosHotkeyEventSource {
    pub fn from_config(config: &HotkeysConfig) -> MacosAdapterResult<Self> {
        let bindings = MacosHotkeyBindings::from_config(config)?;
        let (sender, receiver) = channel(HOTKEY_EVENT_BUFFER_CAPACITY);

        #[cfg(target_os = "macos")]
        {
            std::thread::spawn(move || {
                let mut runtime_state = HotkeyRuntimeState::default();
                let sender_for_errors = sender.clone();
                let send_error = move |message: String| {
                    let _ = try_send_hotkey_result(
                        &sender_for_errors,
                        Err(MacosAdapterError::operation_failed("hotkeys", message)),
                    );
                };

                let callback = move |event: rdev::Event| {
                    if let Err(error) =
                        handle_rdev_event(&bindings, &sender, &mut runtime_state, event)
                    {
                        let _ = try_send_hotkey_result(&sender, Err(error));
                    }
                };

                if let Err(error) = rdev::listen(callback) {
                    send_error(format!("global hotkey listener terminated: {error:?}"));
                }
            });
        }

        #[cfg(not(target_os = "macos"))]
        {
            let _ = sender.send(Err(MacosAdapterError::UnsupportedPlatform));
        }

        Ok(Self { receiver })
    }
}

#[async_trait]
impl HotkeyEventSource for MacosHotkeyEventSource {
    async fn next_event(&mut self) -> MacosAdapterResult<HotkeyEvent> {
        match self.receiver.recv().await {
            Some(result) => result,
            None => Err(MacosAdapterError::HotkeyEventStreamClosed),
        }
    }
}

impl MacosHotkeyEventSource {
    #[must_use]
    pub fn try_next_event(&mut self) -> Option<MacosAdapterResult<HotkeyEvent>> {
        self.receiver.try_recv().ok()
    }
}

#[cfg(target_os = "macos")]
fn handle_rdev_event(
    bindings: &MacosHotkeyBindings,
    sender: &Sender<MacosAdapterResult<HotkeyEvent>>,
    runtime_state: &mut HotkeyRuntimeState,
    event: rdev::Event,
) -> MacosAdapterResult<()> {
    use rdev::EventType::{KeyPress, KeyRelease};

    let event_time = event.time;

    match event.event_type {
        KeyPress(key) => {
            if let Some(modifier) = modifier_for_key(key) {
                runtime_state.modifiers.apply(modifier, true);

                for binding in &bindings.bindings {
                    if binding.trigger == TriggerType::DoubleTap
                        && binding.double_tap_modifier == Some(modifier)
                        && runtime_state.modifiers.matches_exact_modifier(modifier)
                        && runtime_state.last_modifier_taps.was_within(
                            modifier,
                            event_time,
                            binding.double_tap_timeout_ms,
                        )
                        && !runtime_state
                            .active_double_tap_actions
                            .contains(&binding.action)
                    {
                        runtime_state.active_double_tap_actions.push(binding.action);
                        runtime_state.last_modifier_taps.clear(modifier);
                        try_send_hotkey_event(
                            sender,
                            HotkeyEvent::new(binding.action, HotkeyEventKind::Pressed),
                        )?;
                    }
                }

                return Ok(());
            }

            for binding in &bindings.bindings {
                if binding.trigger == TriggerType::DoubleTap {
                    continue;
                }

                if Some(key) == binding.key && runtime_state.modifiers.satisfies(binding.modifiers)
                {
                    match binding.trigger {
                        TriggerType::Press => {
                            try_send_hotkey_event(
                                sender,
                                HotkeyEvent::new(binding.action, HotkeyEventKind::Pressed),
                            )?;
                        }
                        TriggerType::Hold => {
                            if !runtime_state.active_hold_actions.contains(&binding.action) {
                                runtime_state.active_hold_actions.push(binding.action);
                                try_send_hotkey_event(
                                    sender,
                                    HotkeyEvent::new(binding.action, HotkeyEventKind::Pressed),
                                )?;
                            }
                        }
                        TriggerType::DoubleTap => {}
                    }
                }
            }
        }
        KeyRelease(key) => {
            if let Some(modifier) = modifier_for_key(key) {
                runtime_state.modifiers.apply(modifier, false);

                let mut released_double_tap = Vec::new();
                for binding in &bindings.bindings {
                    if binding.trigger == TriggerType::DoubleTap
                        && binding.double_tap_modifier == Some(modifier)
                        && runtime_state
                            .active_double_tap_actions
                            .contains(&binding.action)
                    {
                        released_double_tap.push(binding.action);
                    }
                }
                for action in released_double_tap {
                    runtime_state
                        .active_double_tap_actions
                        .retain(|active| *active != action);
                    try_send_hotkey_event(
                        sender,
                        HotkeyEvent::new(action, HotkeyEventKind::Released),
                    )?;
                }

                if runtime_state
                    .active_double_tap_actions
                    .iter()
                    .all(|action| *action != HotkeyAction::PushToTalk)
                {
                    runtime_state
                        .last_modifier_taps
                        .record(modifier, event_time);
                }

                let mut released = Vec::new();
                for binding in &bindings.bindings {
                    if binding.trigger == TriggerType::Hold
                        && runtime_state.active_hold_actions.contains(&binding.action)
                        && !runtime_state.modifiers.satisfies(binding.modifiers)
                    {
                        released.push(binding.action);
                    }
                }
                for action in released {
                    runtime_state
                        .active_hold_actions
                        .retain(|active| *active != action);
                    try_send_hotkey_event(
                        sender,
                        HotkeyEvent::new(action, HotkeyEventKind::Released),
                    )?;
                }

                return Ok(());
            }

            for binding in &bindings.bindings {
                if binding.trigger == TriggerType::Hold
                    && Some(key) == binding.key
                    && runtime_state.active_hold_actions.contains(&binding.action)
                {
                    runtime_state
                        .active_hold_actions
                        .retain(|active| *active != binding.action);
                    try_send_hotkey_event(
                        sender,
                        HotkeyEvent::new(binding.action, HotkeyEventKind::Released),
                    )?;
                }
            }
        }
        _ => {}
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn try_send_hotkey_event(
    sender: &Sender<MacosAdapterResult<HotkeyEvent>>,
    event: HotkeyEvent,
) -> MacosAdapterResult<()> {
    try_send_hotkey_result(sender, Ok(event))
}

fn try_send_hotkey_result(
    sender: &Sender<MacosAdapterResult<HotkeyEvent>>,
    result: MacosAdapterResult<HotkeyEvent>,
) -> MacosAdapterResult<()> {
    let payload_kind = if result.is_ok() { "event" } else { "error" };

    match sender.try_send(result) {
        Ok(()) => Ok(()),
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
            record_hotkey_queue_drop(payload_kind);
            Ok(())
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            Err(MacosAdapterError::HotkeyEventStreamClosed)
        }
    }
}

impl HotkeyDropDiagnostics {
    fn record_drop_at(&mut self, now: Instant) -> Option<HotkeyDropWarning> {
        self.dropped_since_last_warning = self.dropped_since_last_warning.saturating_add(1);

        let should_warn = match self.last_warning_at {
            Some(last_warning_at) => {
                now.duration_since(last_warning_at) >= HOTKEY_DROP_WARN_INTERVAL
            }
            None => true,
        };

        if !should_warn {
            return None;
        }

        let warning = HotkeyDropWarning {
            dropped_events: self.dropped_since_last_warning,
        };
        self.dropped_since_last_warning = 0;
        self.last_warning_at = Some(now);
        Some(warning)
    }
}

fn record_hotkey_queue_drop(payload_kind: &'static str) {
    let Some(warning) = take_hotkey_drop_warning(hotkey_drop_diagnostics(), payload_kind) else {
        return;
    };

    warn!(
        target: TARGET_HOTKEY,
        payload_kind,
        dropped_events = warning.dropped_events,
        queue_capacity = HOTKEY_EVENT_BUFFER_CAPACITY,
        "dropping hotkey listener payload because hotkey event queue is full"
    );
}

fn take_hotkey_drop_warning(
    diagnostics_mutex: &Mutex<HotkeyDropDiagnostics>,
    payload_kind: &'static str,
) -> Option<HotkeyDropWarning> {
    match diagnostics_mutex.lock() {
        Ok(mut diagnostics) => diagnostics.record_drop_at(Instant::now()),
        Err(poisoned) => {
            warn!(
                target: TARGET_HOTKEY,
                payload_kind,
                "hotkey drop diagnostics mutex poisoned; recovering"
            );
            let mut diagnostics = poisoned.into_inner();
            let warning = diagnostics.record_drop_at(Instant::now());
            diagnostics_mutex.clear_poison();
            warning
        }
    }
}

fn hotkey_drop_diagnostics() -> &'static Mutex<HotkeyDropDiagnostics> {
    HOTKEY_DROP_DIAGNOSTICS.get_or_init(|| Mutex::new(HotkeyDropDiagnostics::default()))
}

fn parse_binding(
    action: HotkeyAction,
    binding: &HotkeyBinding,
) -> MacosAdapterResult<MacosHotkeyBinding> {
    let mut modifiers = ModifierSet::default();
    let mut key: Option<PlatformKey> = None;

    for part in &binding.chord {
        match part.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers.control = true,
            "shift" => modifiers.shift = true,
            "alt" | "option" => modifiers.option = true,
            "cmd" | "command" | "meta" => modifiers.command = true,
            other => {
                if key.is_some() {
                    return Err(MacosAdapterError::operation_failed(
                        "hotkeys",
                        format!(
                            "hotkey chord for {action:?} must contain exactly one non-modifier key"
                        ),
                    ));
                }
                key = Some(parse_key(other)?);
            }
        }
    }

    match binding.trigger {
        TriggerType::DoubleTap => {
            if key.is_some() {
                return Err(MacosAdapterError::operation_failed(
                    "hotkeys",
                    format!("double_tap hotkey for {action:?} must not include a non-modifier key"),
                ));
            }

            let modifier = modifiers.single_modifier().ok_or_else(|| {
                MacosAdapterError::operation_failed(
                    "hotkeys",
                    format!("double_tap hotkey for {action:?} must use exactly one modifier"),
                )
            })?;

            Ok(MacosHotkeyBinding {
                action,
                trigger: binding.trigger,
                key: None,
                modifiers: ModifierSet::default(),
                double_tap_modifier: Some(modifier),
                double_tap_timeout_ms: binding.effective_double_tap_timeout_ms(),
            })
        }
        _ => {
            let key = key.ok_or_else(|| {
                MacosAdapterError::operation_failed(
                    "hotkeys",
                    format!("hotkey chord for {action:?} must contain a non-modifier key"),
                )
            })?;

            Ok(MacosHotkeyBinding {
                action,
                trigger: binding.trigger,
                key: Some(key),
                modifiers,
                double_tap_modifier: None,
                double_tap_timeout_ms: binding.effective_double_tap_timeout_ms(),
            })
        }
    }
}

#[cfg(target_os = "macos")]
fn parse_key(value: &str) -> MacosAdapterResult<PlatformKey> {
    use rdev::Key;

    let key = match value {
        "space" => Key::Space,
        "escape" | "esc" => Key::Escape,
        "tab" => Key::Tab,
        "return" | "enter" => Key::Return,
        "a" => Key::KeyA,
        "b" => Key::KeyB,
        "c" => Key::KeyC,
        "d" => Key::KeyD,
        "e" => Key::KeyE,
        "f" => Key::KeyF,
        "g" => Key::KeyG,
        "h" => Key::KeyH,
        "i" => Key::KeyI,
        "j" => Key::KeyJ,
        "k" => Key::KeyK,
        "l" => Key::KeyL,
        "m" => Key::KeyM,
        "n" => Key::KeyN,
        "o" => Key::KeyO,
        "p" => Key::KeyP,
        "q" => Key::KeyQ,
        "r" => Key::KeyR,
        "s" => Key::KeyS,
        "t" => Key::KeyT,
        "u" => Key::KeyU,
        "v" => Key::KeyV,
        "w" => Key::KeyW,
        "x" => Key::KeyX,
        "y" => Key::KeyY,
        "z" => Key::KeyZ,
        _ => {
            return Err(MacosAdapterError::operation_failed(
                "hotkeys",
                format!("unsupported hotkey key: {value}"),
            ))
        }
    };

    Ok(key)
}

#[cfg(not(target_os = "macos"))]
fn parse_key(value: &str) -> MacosAdapterResult<PlatformKey> {
    // On non-macOS platforms, map key names to opaque numeric identifiers.
    // This allows config parsing to succeed even though runtime hotkey
    // listening is not supported outside macOS.
    let key = match value {
        "space" => 1,
        "escape" | "esc" => 2,
        "tab" => 3,
        "return" | "enter" => 4,
        "a" => 10,
        "b" => 11,
        "c" => 12,
        "d" => 13,
        "e" => 14,
        "f" => 15,
        "g" => 16,
        "h" => 17,
        "i" => 18,
        "j" => 19,
        "k" => 20,
        "l" => 21,
        "m" => 22,
        "n" => 23,
        "o" => 24,
        "p" => 25,
        "q" => 26,
        "r" => 27,
        "s" => 28,
        "t" => 29,
        "u" => 30,
        "v" => 31,
        "w" => 32,
        "x" => 33,
        "y" => 34,
        "z" => 35,
        _ => {
            return Err(MacosAdapterError::operation_failed(
                "hotkeys",
                format!("unsupported hotkey key: {value}"),
            ))
        }
    };

    Ok(key)
}

#[cfg(target_os = "macos")]
fn modifier_for_key(key: rdev::Key) -> Option<Modifier> {
    use rdev::Key;

    match key {
        Key::ControlLeft | Key::ControlRight => Some(Modifier::Control),
        Key::ShiftLeft | Key::ShiftRight => Some(Modifier::Shift),
        Key::Alt | Key::AltGr => Some(Modifier::Option),
        Key::MetaLeft | Key::MetaRight => Some(Modifier::Command),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Modifier {
    Control,
    Shift,
    Option,
    Command,
}

impl ModifierSet {
    fn apply(&mut self, modifier: Modifier, active: bool) {
        match modifier {
            Modifier::Control => self.control = active,
            Modifier::Shift => self.shift = active,
            Modifier::Option => self.option = active,
            Modifier::Command => self.command = active,
        }
    }

    const fn satisfies(self, required: ModifierSet) -> bool {
        (!required.control || self.control)
            && (!required.shift || self.shift)
            && (!required.option || self.option)
            && (!required.command || self.command)
    }

    const fn matches_exact_modifier(self, modifier: Modifier) -> bool {
        match modifier {
            Modifier::Control => self.control && !self.shift && !self.option && !self.command,
            Modifier::Shift => !self.control && self.shift && !self.option && !self.command,
            Modifier::Option => !self.control && !self.shift && self.option && !self.command,
            Modifier::Command => !self.control && !self.shift && !self.option && self.command,
        }
    }

    const fn single_modifier(self) -> Option<Modifier> {
        let count = self.control as u8 + self.shift as u8 + self.option as u8 + self.command as u8;
        if count != 1 {
            return None;
        }

        if self.control {
            Some(Modifier::Control)
        } else if self.shift {
            Some(Modifier::Shift)
        } else if self.option {
            Some(Modifier::Option)
        } else if self.command {
            Some(Modifier::Command)
        } else {
            None
        }
    }
}

impl ModifierTapTimes {
    fn record(&mut self, modifier: Modifier, time: SystemTime) {
        *self.slot_mut(modifier) = Some(time);
    }

    fn clear(&mut self, modifier: Modifier) {
        *self.slot_mut(modifier) = None;
    }

    fn was_within(&self, modifier: Modifier, now: SystemTime, timeout_ms: u64) -> bool {
        self.slot(modifier).is_some_and(|previous| {
            now.duration_since(previous)
                .map(|elapsed| elapsed <= Duration::from_millis(timeout_ms))
                .unwrap_or(false)
        })
    }

    const fn slot(self, modifier: Modifier) -> Option<SystemTime> {
        match modifier {
            Modifier::Control => self.control,
            Modifier::Shift => self.shift,
            Modifier::Option => self.option,
            Modifier::Command => self.command,
        }
    }

    fn slot_mut(&mut self, modifier: Modifier) -> &mut Option<SystemTime> {
        match modifier {
            Modifier::Control => &mut self.control,
            Modifier::Shift => &mut self.shift,
            Modifier::Option => &mut self.option,
            Modifier::Command => &mut self.command,
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use rdev::{Event, EventType, Key};

    fn config_with_double_tap_ctrl() -> HotkeysConfig {
        HotkeysConfig::default()
    }

    fn event_at(ms: u64, event_type: EventType) -> Event {
        Event {
            time: SystemTime::UNIX_EPOCH + Duration::from_millis(ms),
            name: None,
            event_type,
        }
    }

    #[test]
    fn double_tap_ctrl_emits_pressed_then_released() {
        let bindings = MacosHotkeyBindings::from_config(&config_with_double_tap_ctrl())
            .expect("default double_tap ctrl bindings");
        let (sender, mut receiver) = channel(HOTKEY_EVENT_BUFFER_CAPACITY);
        let mut state = HotkeyRuntimeState::default();

        handle_rdev_event(
            &bindings,
            &sender,
            &mut state,
            event_at(0, EventType::KeyPress(Key::ControlLeft)),
        )
        .expect("first ctrl press");
        assert!(receiver.try_recv().is_err());

        handle_rdev_event(
            &bindings,
            &sender,
            &mut state,
            event_at(20, EventType::KeyRelease(Key::ControlLeft)),
        )
        .expect("first ctrl release");
        assert!(receiver.try_recv().is_err());

        handle_rdev_event(
            &bindings,
            &sender,
            &mut state,
            event_at(120, EventType::KeyPress(Key::ControlLeft)),
        )
        .expect("second ctrl press");
        assert_eq!(
            receiver
                .try_recv()
                .expect("pressed event should be emitted")
                .expect("pressed event should be ok"),
            HotkeyEvent::new(HotkeyAction::PushToTalk, HotkeyEventKind::Pressed)
        );

        handle_rdev_event(
            &bindings,
            &sender,
            &mut state,
            event_at(180, EventType::KeyRelease(Key::ControlLeft)),
        )
        .expect("second ctrl release");
        assert_eq!(
            receiver
                .try_recv()
                .expect("released event should be emitted")
                .expect("released event should be ok"),
            HotkeyEvent::new(HotkeyAction::PushToTalk, HotkeyEventKind::Released)
        );
    }

    #[test]
    fn slow_second_ctrl_press_does_not_trigger_double_tap() {
        let bindings = MacosHotkeyBindings::from_config(&config_with_double_tap_ctrl())
            .expect("default double_tap ctrl bindings");
        let (sender, mut receiver) = channel(HOTKEY_EVENT_BUFFER_CAPACITY);
        let mut state = HotkeyRuntimeState::default();

        handle_rdev_event(
            &bindings,
            &sender,
            &mut state,
            event_at(0, EventType::KeyPress(Key::ControlLeft)),
        )
        .expect("first ctrl press");
        handle_rdev_event(
            &bindings,
            &sender,
            &mut state,
            event_at(20, EventType::KeyRelease(Key::ControlLeft)),
        )
        .expect("first ctrl release");
        handle_rdev_event(
            &bindings,
            &sender,
            &mut state,
            event_at(500, EventType::KeyPress(Key::ControlLeft)),
        )
        .expect("late second ctrl press");

        assert!(receiver.try_recv().is_err());
    }
}

#[cfg(test)]
mod drop_diagnostic_tests {
    use super::*;

    #[test]
    fn hotkey_drop_diagnostics_warns_on_first_drop_then_rate_limits() {
        let start = Instant::now();
        let mut diagnostics = HotkeyDropDiagnostics::default();

        assert_eq!(
            diagnostics.record_drop_at(start),
            Some(HotkeyDropWarning { dropped_events: 1 })
        );
        assert_eq!(
            diagnostics.record_drop_at(start + Duration::from_secs(1)),
            None
        );
        assert_eq!(
            diagnostics.record_drop_at(start + HOTKEY_DROP_WARN_INTERVAL),
            Some(HotkeyDropWarning { dropped_events: 2 })
        );
    }

    #[test]
    fn full_queue_hotkey_drop_remains_non_fatal() {
        let (sender, mut receiver) = channel(1);
        sender
            .try_send(Ok(HotkeyEvent::new(
                HotkeyAction::PushToTalk,
                HotkeyEventKind::Pressed,
            )))
            .expect("seed queue with one event");

        let result = try_send_hotkey_result(
            &sender,
            Ok(HotkeyEvent::new(
                HotkeyAction::PushToTalk,
                HotkeyEventKind::Released,
            )),
        );

        assert!(
            result.is_ok(),
            "queue backpressure should still drop instead of fail"
        );
        assert_eq!(
            receiver
                .try_recv()
                .expect("original event should remain queued")
                .expect("queued event should be ok"),
            HotkeyEvent::new(HotkeyAction::PushToTalk, HotkeyEventKind::Pressed)
        );
        assert!(
            receiver.try_recv().is_err(),
            "full-queue drop should not enqueue the second event"
        );
    }

    #[test]
    fn hotkey_drop_diagnostics_recovers_after_mutex_poison() {
        let diagnostics = Mutex::new(HotkeyDropDiagnostics::default());
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = diagnostics
                .lock()
                .expect("diagnostics lock should succeed before poison");
            panic!("poison diagnostics");
        }));

        assert_eq!(
            take_hotkey_drop_warning(&diagnostics, "event"),
            Some(HotkeyDropWarning { dropped_events: 1 })
        );
        assert!(
            diagnostics.lock().is_ok(),
            "recovered diagnostics should clear poison"
        );
    }
}

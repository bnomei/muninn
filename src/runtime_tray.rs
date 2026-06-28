//! Menu-bar tray icon and main-thread indicator bridge.
//!
//! [`TrayIconEvent`] callbacks arrive on an arbitrary thread; the bridge
//! re-posts them as [`UserEvent::TrayEvent`] on the tao loop.
//! [`EventLoopIndicator`] implements [`IndicatorAdapter`] by caching state on the
//! worker thread and forwarding appearance updates to the main thread.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use muninn::config::IndicatorConfig;
use muninn::{AppConfig, IndicatorAdapter, IndicatorState, RecordingMode, TargetContextSnapshot};
use tao::event_loop::EventLoopProxy;
use tracing::warn;
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

use crate::external_control::ExternalControlAction;
use crate::logging;

/// Custom tao user events handled on the main thread.
#[derive(Debug, Clone)]
pub(crate) enum UserEvent {
    /// Tray icon click or other menu-bar interaction.
    TrayEvent(TrayIconEvent),
    /// Indicator glyph or color changed; refresh the tray icon.
    IndicatorUpdated {
        state: IndicatorState,
        glyph: Option<char>,
    },
    /// Frontmost-app context changed; refresh idle preview glyph.
    PreviewContextUpdated(TargetContextSnapshot),
    /// Config file reload succeeded; shell applies tray/MCP settings locally.
    ConfigReloaded(Box<AppConfig>),
    /// Config file reload failed; previous config remains active.
    ConfigReloadFailed(String),
    /// Retry delivering a config reload that filled the runtime message queue.
    RetryPendingConfigReload,
    /// Runtime worker thread exited with an error.
    RuntimeFailure(String),
    /// MCP or `muninn://` URL scheme action forwarded from external control.
    ExternalControl(ExternalControlAction),
}

/// Forward [`TrayIconEvent`] callbacks onto the tao event loop as [`UserEvent`].
pub(crate) fn install_tray_event_bridge(proxy: EventLoopProxy<UserEvent>) {
    TrayIconEvent::set_event_handler(Some(move |event| {
        send_user_event(&proxy, UserEvent::TrayEvent(event), "tray_event_bridge");
    }));
}

/// Post a [`UserEvent`] to the main thread; returns false when the loop is gone.
pub(crate) fn send_user_event(
    proxy: &EventLoopProxy<UserEvent>,
    event: UserEvent,
    context: &'static str,
) -> bool {
    match proxy.send_event(event) {
        Ok(()) => true,
        Err(error) => {
            logging::log_proxy_send_failed(context, format!("{error:?}"));
            false
        }
    }
}

/// Build the Muninn menu-bar tray icon with the initial indicator image.
pub(crate) fn build_tray_icon(icon: Icon) -> Result<tray_icon::TrayIcon> {
    TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("Muninn")
        .build()
        .context("creating menu bar tray icon")
}

/// Refresh tray icon and tooltip for the current indicator and preview glyphs.
///
/// Honors `indicator.show_recording` and `indicator.show_processing` by
/// collapsing hidden busy states back to idle appearance.
pub(crate) fn update_tray_appearance(
    tray_icon: &tray_icon::TrayIcon,
    state: IndicatorState,
    active_glyph: Option<char>,
    preview_glyph: Option<char>,
    indicator_config: &IndicatorConfig,
) {
    let visible_state = visible_indicator_state(state, indicator_config);
    match indicator_icon(visible_state, active_glyph, preview_glyph, indicator_config) {
        Ok(icon) => {
            if let Err(error) = tray_icon.set_icon(Some(icon)) {
                warn!(%error, "failed to update tray icon");
            }
        }
        Err(error) => {
            warn!(%error, "failed to build tray icon");
        }
    }
    if let Err(error) = tray_icon.set_tooltip(Some(indicator_tooltip(state))) {
        warn!(%error, "failed to update tray tooltip");
    }
    tray_icon.set_title(None::<&str>);
}

/// Map a completed left click on the tray icon to a recording toggle.
///
/// Acting on the button release (a full click) makes the tray a toggle: it
/// starts recording when idle and stops the active recording otherwise. The
/// toggle is resolved against the authoritative runtime state by the runtime
/// worker, so a click reliably stops a recording no matter how it was started.
pub(crate) fn map_tray_event(event: &TrayIconEvent) -> Option<ExternalControlAction> {
    match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } => Some(ExternalControlAction::Toggle),
        _ => None,
    }
}

/// [`IndicatorAdapter`] that mirrors state on the worker thread and updates the tray on the main thread.
#[derive(Clone)]
pub(crate) struct EventLoopIndicator {
    proxy: EventLoopProxy<UserEvent>,
    state: Arc<Mutex<IndicatorState>>,
    glyph: Arc<Mutex<Option<char>>>,
    sequence: Arc<AtomicU64>,
}

impl EventLoopIndicator {
    /// Create an indicator bridge bound to the given event-loop proxy.
    pub(crate) fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            proxy,
            state: Arc::new(Mutex::new(IndicatorState::Idle)),
            glyph: Arc::new(Mutex::new(None)),
            sequence: Arc::new(AtomicU64::new(0)),
        }
    }
}

fn write_indicator_cache<T: Copy>(
    mutex: &Mutex<T>,
    value: T,
    cache: &'static str,
    context: &'static str,
) {
    match mutex.lock() {
        Ok(mut guard) => {
            *guard = value;
        }
        Err(poisoned) => {
            warn!(
                target: logging::TARGET_RUNTIME,
                cache,
                context,
                "indicator cache mutex poisoned; recovering"
            );
            let mut guard = poisoned.into_inner();
            *guard = value;
            mutex.clear_poison();
        }
    }
}

fn read_indicator_cache<T: Copy>(
    mutex: &Mutex<T>,
    cache: &'static str,
) -> muninn::MacosAdapterResult<T> {
    match mutex.lock() {
        Ok(guard) => Ok(*guard),
        Err(poisoned) => {
            warn!(
                target: logging::TARGET_RUNTIME,
                cache,
                "indicator cache mutex poisoned; recovering"
            );
            let guard = poisoned.into_inner();
            let value = *guard;
            drop(guard);
            mutex.clear_poison();
            Ok(value)
        }
    }
}

#[async_trait::async_trait]
impl IndicatorAdapter for EventLoopIndicator {
    async fn initialize(&mut self) -> muninn::MacosAdapterResult<()> {
        self.set_state(IndicatorState::Idle).await
    }

    async fn set_state(&mut self, state: IndicatorState) -> muninn::MacosAdapterResult<()> {
        self.set_state_with_glyph(state, None).await
    }

    async fn set_state_with_glyph(
        &mut self,
        state: IndicatorState,
        glyph: Option<char>,
    ) -> muninn::MacosAdapterResult<()> {
        self.sequence.fetch_add(1, Ordering::SeqCst);
        write_indicator_cache(&self.state, state, "state", "indicator_set_state");
        write_indicator_cache(&self.glyph, glyph, "glyph", "indicator_set_state");
        send_user_event(
            &self.proxy,
            UserEvent::IndicatorUpdated { state, glyph },
            "indicator_set_state",
        );
        Ok(())
    }

    async fn set_temporary_state(
        &mut self,
        state: IndicatorState,
        min_duration: Duration,
        fallback_state: IndicatorState,
    ) -> muninn::MacosAdapterResult<()> {
        self.set_temporary_state_with_glyph(state, None, min_duration, fallback_state, None)
            .await
    }

    async fn set_temporary_state_with_glyph(
        &mut self,
        state: IndicatorState,
        glyph: Option<char>,
        min_duration: Duration,
        fallback_state: IndicatorState,
        fallback_glyph: Option<char>,
    ) -> muninn::MacosAdapterResult<()> {
        let generation = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        write_indicator_cache(&self.state, state, "state", "indicator_set_temporary_state");
        write_indicator_cache(&self.glyph, glyph, "glyph", "indicator_set_temporary_state");
        send_user_event(
            &self.proxy,
            UserEvent::IndicatorUpdated { state, glyph },
            "indicator_set_temporary_state",
        );

        let proxy = self.proxy.clone();
        let state = Arc::clone(&self.state);
        let stored_glyph = Arc::clone(&self.glyph);
        let sequence = Arc::clone(&self.sequence);
        tokio::spawn(async move {
            tokio::time::sleep(min_duration).await;
            if sequence
                .compare_exchange(
                    generation,
                    generation + 1,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_err()
            {
                return;
            }
            write_indicator_cache(
                state.as_ref(),
                fallback_state,
                "state",
                "indicator_reset_temporary_state",
            );
            write_indicator_cache(
                stored_glyph.as_ref(),
                fallback_glyph,
                "glyph",
                "indicator_reset_temporary_state",
            );
            send_user_event(
                &proxy,
                UserEvent::IndicatorUpdated {
                    state: fallback_state,
                    glyph: fallback_glyph,
                },
                "indicator_reset_temporary_state",
            );
        });

        Ok(())
    }

    async fn state(&self) -> muninn::MacosAdapterResult<IndicatorState> {
        read_indicator_cache(&self.state, "state")
    }

    async fn indicator_glyph(&self) -> muninn::MacosAdapterResult<Option<char>> {
        read_indicator_cache(&self.glyph, "glyph")
    }
}

/// Build a menu-bar RGBA icon for the given indicator state and glyphs.
pub(crate) fn indicator_icon(
    state: IndicatorState,
    active_glyph: Option<char>,
    preview_glyph: Option<char>,
    indicator_config: &IndicatorConfig,
) -> Result<Icon> {
    let glyph = resolved_indicator_glyph(state, active_glyph, preview_glyph);
    let rgba = match state {
        IndicatorState::Idle => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.idle)?,
            parse_hex_rgb(&indicator_config.colors.outline)?,
            parse_hex_rgb(&indicator_config.colors.glyph)?,
            glyph,
        ),
        IndicatorState::Recording { .. } => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.recording)?,
            parse_hex_rgb(&indicator_config.colors.outline)?,
            parse_hex_rgb(&indicator_config.colors.glyph)?,
            glyph,
        ),
        IndicatorState::Transcribing => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.transcribing)?,
            parse_hex_rgb(&indicator_config.colors.outline)?,
            parse_hex_rgb(&indicator_config.colors.glyph)?,
            glyph,
        ),
        IndicatorState::Pipeline => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.pipeline)?,
            parse_hex_rgb(&indicator_config.colors.outline)?,
            parse_hex_rgb(&indicator_config.colors.glyph)?,
            glyph,
        ),
        IndicatorState::Output => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.output)?,
            parse_hex_rgb(&indicator_config.colors.outline)?,
            parse_hex_rgb(&indicator_config.colors.glyph)?,
            glyph,
        ),
        IndicatorState::MissingCredentials => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.cancelled)?,
            parse_hex_rgb(&indicator_config.colors.outline)?,
            parse_hex_rgb(&indicator_config.colors.glyph)?,
            IndicatorGlyph::Question,
        ),
        IndicatorState::Cancelled => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.cancelled)?,
            parse_hex_rgb(&indicator_config.colors.outline)?,
            parse_hex_rgb(&indicator_config.colors.glyph)?,
            glyph,
        ),
    };
    Icon::from_rgba(
        rgba,
        crate::INDICATOR_ICON_SIZE_PX,
        crate::INDICATOR_ICON_SIZE_PX,
    )
    .context("building indicator icon")
}

/// Resolve which glyph to paint for an indicator state.
///
/// Idle prefers the contextual preview glyph; busy states prefer the frozen
/// utterance glyph. [`IndicatorState::MissingCredentials`] always uses `?`.
pub(crate) fn resolved_indicator_glyph(
    state: IndicatorState,
    active_glyph: Option<char>,
    preview_glyph: Option<char>,
) -> IndicatorGlyph {
    match state {
        IndicatorState::MissingCredentials => IndicatorGlyph::Question,
        IndicatorState::Idle => {
            IndicatorGlyph::Letter(preview_glyph.unwrap_or(crate::DEFAULT_INDICATOR_GLYPH))
        }
        IndicatorState::Recording { .. }
        | IndicatorState::Transcribing
        | IndicatorState::Pipeline
        | IndicatorState::Output
        | IndicatorState::Cancelled => {
            IndicatorGlyph::Letter(active_glyph.unwrap_or(crate::DEFAULT_INDICATOR_GLYPH))
        }
    }
}

fn visible_indicator_state(
    state: IndicatorState,
    indicator_config: &IndicatorConfig,
) -> IndicatorState {
    match state {
        IndicatorState::Recording { .. } if !indicator_config.show_recording => {
            IndicatorState::Idle
        }
        state if state.is_processing() && !indicator_config.show_processing => IndicatorState::Idle,
        _ => state,
    }
}

fn indicator_tooltip(state: IndicatorState) -> &'static str {
    match state {
        IndicatorState::Idle => "Muninn idle",
        IndicatorState::Recording {
            mode: RecordingMode::PushToTalk,
        } => "Muninn recording (push-to-talk)",
        IndicatorState::Recording {
            mode: RecordingMode::DoneMode,
        } => "Muninn recording (done mode)",
        IndicatorState::Transcribing => "Muninn transcribing audio",
        IndicatorState::Pipeline => "Muninn refining transcript",
        IndicatorState::Output => "Muninn outputting text",
        IndicatorState::MissingCredentials => "Muninn missing provider credentials",
        IndicatorState::Cancelled => "Muninn cancelled",
    }
}

fn menu_bar_icon_rgba(
    background_rgb: [u8; 3],
    outline_rgb: [u8; 3],
    glyph_rgb: [u8; 3],
    glyph: IndicatorGlyph,
) -> Vec<u8> {
    let size = crate::INDICATOR_ICON_SIZE_PX as usize;
    let mut rgba = vec![0_u8; size * size * 4];
    let center = crate::INDICATOR_ICON_SIZE_PX as f32 / 2.0;
    let outline_radius = center - 1.0;
    let body_radius = outline_radius - 1.25;

    for y in 0..crate::INDICATOR_ICON_SIZE_PX {
        for x in 0..crate::INDICATOR_ICON_SIZE_PX {
            let idx = ((y * crate::INDICATOR_ICON_SIZE_PX + x) * 4) as usize;
            let dx = x as f32 + 0.5 - center;
            let dy = y as f32 + 0.5 - center;
            let distance_sq = dx * dx + dy * dy;
            let is_outline_disc = distance_sq <= outline_radius * outline_radius;
            let is_background_disc = distance_sq <= body_radius * body_radius;

            if is_outline_disc {
                write_rgba(&mut rgba, idx, outline_rgb);
            }

            if is_background_disc {
                write_rgba(&mut rgba, idx, background_rgb);
            }

            if is_background_disc && pixel_indicator_glyph(glyph, x, y) {
                write_rgba(&mut rgba, idx, glyph_rgb);
            }
        }
    }
    rgba
}

/// Pixel glyph drawn inside the circular menu-bar indicator.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndicatorGlyph {
    /// Voice or default letter glyph for the current state.
    Letter(char),
    /// Reserved `?` glyph for missing provider credentials.
    Question,
}

fn pixel_indicator_glyph(glyph: IndicatorGlyph, x: u32, y: u32) -> bool {
    match glyph {
        IndicatorGlyph::Letter(letter) => pixel_letter_glyph(letter, x, y),
        IndicatorGlyph::Question => pixel_question_glyph(x, y),
    }
}

fn pixel_letter_glyph(letter: char, x: u32, y: u32) -> bool {
    pixel_bitmap_glyph(letter_bitmap(letter), x, y)
}

fn pixel_bitmap_glyph(bitmap: &[&str; 8], x: u32, y: u32) -> bool {
    let scale = crate::INDICATOR_ICON_SIZE_PX as f32 / 22.0;
    let glyph_x = (7.0 * scale).round();
    let glyph_y = (6.0 * scale).round();
    let local_x = ((x as f32 - glyph_x) / scale).floor() as i32;
    let local_y = ((y as f32 - glyph_y) / scale).floor() as i32;
    if local_x < 0 || local_y < 0 {
        return false;
    }

    let Some(row) = bitmap.get(local_y as usize) else {
        return false;
    };

    matches!(row.as_bytes().get(local_x as usize), Some(b'1'))
}

fn pixel_question_glyph(x: u32, y: u32) -> bool {
    const GLYPH: [&str; 8] = [
        "0111110", "1000001", "0000010", "0001100", "0010000", "0000000", "0010000", "0000000",
    ];
    pixel_bitmap_glyph(&GLYPH, x, y)
}

fn letter_bitmap(letter: char) -> &'static [&'static str; 8] {
    match letter.to_ascii_uppercase() {
        'A' => &[
            "0111110", "1000001", "1000001", "1111111", "1000001", "1000001", "1000001", "0000000",
        ],
        'B' => &[
            "1111110", "1000001", "1000001", "1111110", "1000001", "1000001", "1111110", "0000000",
        ],
        'C' => &[
            "0111111", "1000000", "1000000", "1000000", "1000000", "1000000", "0111111", "0000000",
        ],
        'D' => &[
            "1111110", "1000001", "1000001", "1000001", "1000001", "1000001", "1111110", "0000000",
        ],
        'E' => &[
            "1111111", "1000000", "1000000", "1111110", "1000000", "1000000", "1111111", "0000000",
        ],
        'F' => &[
            "1111111", "1000000", "1000000", "1111110", "1000000", "1000000", "1000000", "0000000",
        ],
        'G' => &[
            "0111110", "1000001", "1000000", "1001111", "1000001", "1000001", "0111110", "0000000",
        ],
        'H' => &[
            "1000001", "1000001", "1000001", "1111111", "1000001", "1000001", "1000001", "0000000",
        ],
        'I' => &[
            "0111110", "0001000", "0001000", "0001000", "0001000", "0001000", "0111110", "0000000",
        ],
        'J' => &[
            "0001111", "0000010", "0000010", "0000010", "0000010", "1000010", "0111100", "0000000",
        ],
        'K' => &[
            "1000001", "1000010", "1000100", "1111000", "1000100", "1000010", "1000001", "0000000",
        ],
        'L' => &[
            "1000000", "1000000", "1000000", "1000000", "1000000", "1000000", "1111111", "0000000",
        ],
        'M' => &[
            "1000001", "1100011", "1010101", "1001001", "1000001", "1000001", "1000001", "1000001",
        ],
        'N' => &[
            "1000001", "1100001", "1010001", "1001001", "1000101", "1000011", "1000001", "0000000",
        ],
        'O' => &[
            "0111110", "1000001", "1000001", "1000001", "1000001", "1000001", "0111110", "0000000",
        ],
        'P' => &[
            "1111110", "1000001", "1000001", "1111110", "1000000", "1000000", "1000000", "0000000",
        ],
        'Q' => &[
            "0111110", "1000001", "1000001", "1000001", "1001001", "1000101", "0111110", "0000001",
        ],
        'R' => &[
            "1111110", "1000001", "1000001", "1111110", "1000100", "1000010", "1000001", "0000000",
        ],
        'S' => &[
            "0111111", "1000000", "1000000", "0111110", "0000001", "0000001", "1111110", "0000000",
        ],
        'T' => &[
            "1111111", "0001000", "0001000", "0001000", "0001000", "0001000", "0001000", "0000000",
        ],
        'U' => &[
            "1000001", "1000001", "1000001", "1000001", "1000001", "1000001", "0111110", "0000000",
        ],
        'V' => &[
            "1000001", "1000001", "1000001", "1000001", "1000001", "0100010", "0011100", "0000000",
        ],
        'W' => &[
            "1000001", "1000001", "1000001", "1001001", "1001001", "1001001", "0110110", "0000000",
        ],
        'X' => &[
            "1000001", "0100010", "0010100", "0001000", "0010100", "0100010", "1000001", "0000000",
        ],
        'Y' => &[
            "1000001", "0100010", "0010100", "0001000", "0001000", "0001000", "0001000", "0000000",
        ],
        'Z' => &[
            "1111111", "0000010", "0000100", "0001000", "0010000", "0100000", "1111111", "0000000",
        ],
        _ => letter_bitmap(crate::DEFAULT_INDICATOR_GLYPH),
    }
}

fn write_rgba(buffer: &mut [u8], idx: usize, color: [u8; 3]) {
    buffer[idx] = color[0];
    buffer[idx + 1] = color[1];
    buffer[idx + 2] = color[2];
    buffer[idx + 3] = 0xff;
}

fn parse_hex_rgb(value: &str) -> Result<[u8; 3]> {
    let Some(hex) = value.strip_prefix('#') else {
        anyhow::bail!("indicator color must start with '#': {value}");
    };
    if hex.len() != 6 {
        anyhow::bail!("indicator color must be exactly 6 hex digits: {value}");
    }

    let parse_component = |start| {
        u8::from_str_radix(&hex[start..start + 2], 16)
            .with_context(|| format!("indicator color contains invalid hex digits: {value}"))
    };

    Ok([
        parse_component(0)?,
        parse_component(2)?,
        parse_component(4)?,
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{catch_unwind, AssertUnwindSafe};

    #[test]
    fn indicator_cache_recovers_after_mutex_poison() {
        let state = Mutex::new(IndicatorState::Idle);
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let _guard = state.lock().expect("lock should succeed before poison");
            panic!("poison state cache");
        }));

        write_indicator_cache(&state, IndicatorState::Pipeline, "state", "test");

        assert_eq!(
            read_indicator_cache(&state, "state").expect("recovered state should read"),
            IndicatorState::Pipeline
        );
        assert!(state.lock().is_ok(), "recovered cache should clear poison");
    }

    #[test]
    fn parse_hex_rgb_rejects_invalid_colors_without_panicking() {
        assert!(parse_hex_rgb("112233").is_err());
        assert!(parse_hex_rgb("#11223").is_err());
        assert!(parse_hex_rgb("#11zz33").is_err());
    }
}

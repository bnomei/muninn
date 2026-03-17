use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use muninn::config::IndicatorConfig;
use muninn::{
    AppConfig, AppEvent, IndicatorAdapter, IndicatorState, RecordingMode, TargetContextSnapshot,
};
use tao::event_loop::EventLoopProxy;
use tracing::warn;
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

#[derive(Debug, Clone)]
pub(crate) enum UserEvent {
    TrayEvent(TrayIconEvent),
    IndicatorUpdated {
        state: IndicatorState,
        glyph: Option<char>,
    },
    PreviewContextUpdated(TargetContextSnapshot),
    ConfigReloaded(Box<AppConfig>),
    ConfigReloadFailed(String),
    RuntimeFailure(String),
}

pub(crate) fn install_tray_event_bridge(proxy: EventLoopProxy<UserEvent>) {
    TrayIconEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::TrayEvent(event));
    }));
}

pub(crate) fn build_tray_icon(icon: Icon) -> Result<tray_icon::TrayIcon> {
    TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("Muninn")
        .build()
        .context("creating menu bar tray icon")
}

pub(crate) fn update_tray_appearance(
    tray_icon: &tray_icon::TrayIcon,
    state: IndicatorState,
    active_glyph: Option<char>,
    preview_glyph: Option<char>,
    indicator_config: &IndicatorConfig,
) {
    let visible_state = visible_indicator_state(state, indicator_config);
    if let Err(error) = tray_icon.set_icon(Some(indicator_icon(
        visible_state,
        active_glyph,
        preview_glyph,
        indicator_config,
    ))) {
        warn!(%error, "failed to update tray icon");
    }
    if let Err(error) = tray_icon.set_tooltip(Some(indicator_tooltip(state))) {
        warn!(%error, "failed to update tray tooltip");
    }
    tray_icon.set_title(None::<&str>);
}

pub(crate) fn map_tray_event(event: &TrayIconEvent) -> Option<AppEvent> {
    match event {
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Down,
            ..
        } => Some(AppEvent::PttPressed),
        TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } => Some(AppEvent::PttReleased),
        _ => None,
    }
}

#[derive(Clone)]
pub(crate) struct EventLoopIndicator {
    proxy: EventLoopProxy<UserEvent>,
    state: Arc<Mutex<IndicatorState>>,
    glyph: Arc<Mutex<Option<char>>>,
    sequence: Arc<AtomicU64>,
}

impl EventLoopIndicator {
    pub(crate) fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            proxy,
            state: Arc::new(Mutex::new(IndicatorState::Idle)),
            glyph: Arc::new(Mutex::new(None)),
            sequence: Arc::new(AtomicU64::new(0)),
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
        if let Ok(mut guard) = self.state.lock() {
            *guard = state;
        }
        if let Ok(mut guard) = self.glyph.lock() {
            *guard = glyph;
        }
        let _ = self
            .proxy
            .send_event(UserEvent::IndicatorUpdated { state, glyph });
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
        if let Ok(mut guard) = self.state.lock() {
            *guard = state;
        }
        if let Ok(mut guard) = self.glyph.lock() {
            *guard = glyph;
        }
        let _ = self
            .proxy
            .send_event(UserEvent::IndicatorUpdated { state, glyph });

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
            if let Ok(mut guard) = state.lock() {
                *guard = fallback_state;
            }
            if let Ok(mut guard) = stored_glyph.lock() {
                *guard = fallback_glyph;
            }
            let _ = proxy.send_event(UserEvent::IndicatorUpdated {
                state: fallback_state,
                glyph: fallback_glyph,
            });
        });

        Ok(())
    }

    async fn state(&self) -> muninn::MacosAdapterResult<IndicatorState> {
        self.state.lock().map(|guard| *guard).map_err(|_| {
            muninn::MacosAdapterError::operation_failed("indicator", "state mutex poisoned")
        })
    }

    async fn indicator_glyph(&self) -> muninn::MacosAdapterResult<Option<char>> {
        self.glyph.lock().map(|guard| *guard).map_err(|_| {
            muninn::MacosAdapterError::operation_failed("indicator", "glyph mutex poisoned")
        })
    }
}

pub(crate) fn indicator_icon(
    state: IndicatorState,
    active_glyph: Option<char>,
    preview_glyph: Option<char>,
    indicator_config: &IndicatorConfig,
) -> Icon {
    let glyph = resolved_indicator_glyph(state, active_glyph, preview_glyph);
    let rgba = match state {
        IndicatorState::Idle => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.idle),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
            glyph,
        ),
        IndicatorState::Recording { .. } => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.recording),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
            glyph,
        ),
        IndicatorState::Transcribing => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.transcribing),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
            glyph,
        ),
        IndicatorState::Pipeline => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.pipeline),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
            glyph,
        ),
        IndicatorState::Output => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.output),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
            glyph,
        ),
        IndicatorState::MissingCredentials => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.cancelled),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
            IndicatorGlyph::Question,
        ),
        IndicatorState::Cancelled => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.cancelled),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
            glyph,
        ),
    };
    Icon::from_rgba(
        rgba,
        crate::INDICATOR_ICON_SIZE_PX,
        crate::INDICATOR_ICON_SIZE_PX,
    )
    .expect("building indicator icon")
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IndicatorGlyph {
    Letter(char),
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

fn parse_hex_rgb(value: &str) -> [u8; 3] {
    let hex = value
        .strip_prefix('#')
        .expect("indicator colors must validate before runtime");
    let parse_component = |start| {
        u8::from_str_radix(&hex[start..start + 2], 16)
            .expect("indicator colors must validate before runtime")
    };

    [parse_component(0), parse_component(2), parse_component(4)]
}

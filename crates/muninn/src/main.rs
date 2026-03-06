use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod internal_tools;
mod refine;
mod replay;
mod stt_google_tool;
mod stt_openai_tool;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use muninn::config::{resolve_config_path, IndicatorConfig, PipelineConfig};
use muninn::{
    detect_platform, ensure_supported_platform, AppConfig, AppEvent, AppState, AudioRecorder,
    HotkeyAction, HotkeyEvent, HotkeyEventKind, HotkeyEventSource, IndicatorAdapter,
    IndicatorState, MacosAudioRecorder, MacosHotkeyEventSource, MacosPermissionsAdapter,
    MacosTextInjector, MuninnEnvelopeV1, Orchestrator, PermissionPreflightStatus, PermissionStatus,
    PermissionsAdapter, PipelineOutcome, PipelineRunner, PipelineStopReason, PipelineTraceEntry,
    Platform, RecordingMode, TextInjector,
};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
#[cfg(target_os = "macos")]
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use uuid::Uuid;

const INDICATOR_ICON_SIZE_PX: u32 = 36;
const OUTPUT_INDICATOR_MIN_DURATION: Duration = Duration::from_millis(125);
const RUNTIME_EVENT_BUFFER_CAPACITY: usize = 32;
const HOTKEY_RECOVERY_DELAY: Duration = Duration::from_millis(250);
const STALE_RECORDING_MAX_AGE: Duration = Duration::from_secs(60 * 60);

fn main() -> ExitCode {
    maybe_load_dotenv();

    let args = std::env::args().collect::<Vec<_>>();
    if let Some(exit_code) = internal_tools::maybe_handle_internal_step(&args) {
        return exit_code;
    }

    match bootstrap() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("muninn failed to start: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn bootstrap() -> Result<()> {
    let config_path = resolve_config_path().context("resolving configured AppConfig path")?;
    let config = AppConfig::load().context("loading AppConfig from configured path")?;
    init_logging(&config)?;
    if let Err(error) = cleanup_stale_temp_recordings() {
        warn!(error = %error, "failed to clean up stale temporary recordings");
    }

    info!(profile = %config.app.profile, "loaded application configuration");

    let runtime = AppRuntime::new(config_path, config)?;
    runtime.run()
}

fn init_logging(config: &AppConfig) -> Result<()> {
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .compact()
        .try_init()
        .map_err(|error| anyhow!("initializing tracing subscriber: {error}"))?;

    info!(
        replay_enabled = config.logging.replay_enabled,
        replay_dir = %config.logging.replay_dir.display(),
        replay_retention_days = config.logging.replay_retention_days,
        replay_max_bytes = config.logging.replay_max_bytes,
        "logging initialized"
    );

    Ok(())
}

struct AppRuntime {
    config_path: PathBuf,
    config: AppConfig,
    platform: Platform,
    preflight: PermissionPreflightStatus,
}

impl AppRuntime {
    fn new(config_path: PathBuf, config: AppConfig) -> Result<Self> {
        let platform = detect_platform();
        ensure_supported_platform().with_context(|| {
            format!("muninn currently supports macOS only (detected: {platform:?})")
        })?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("building startup tokio runtime")?;
        let preflight = runtime
            .block_on(MacosPermissionsAdapter::new().preflight())
            .context("running macOS permission preflight")?;

        Ok(Self {
            config_path,
            config,
            platform,
            preflight,
        })
    }

    fn run(self) -> Result<()> {
        let mut event_loop_builder = EventLoopBuilder::<UserEvent>::with_user_event();
        #[cfg(target_os = "macos")]
        let mut event_loop = event_loop_builder.build();
        #[cfg(target_os = "macos")]
        {
            event_loop.set_activation_policy(ActivationPolicy::Accessory);
            event_loop.set_dock_visibility(false);
            event_loop.set_activate_ignoring_other_apps(false);
        }

        let proxy = event_loop.create_proxy();
        install_tray_event_bridge(proxy.clone());

        let profile = self.config.app.profile.clone();
        let platform = self.platform;
        let strict_step_contract = self.config.app.strict_step_contract;
        let preflight = self.preflight;
        let config_path = self.config_path.clone();
        let mut indicator_config = self.config.indicator.clone();
        let runtime_config = self.config.clone();
        let (runtime_event_tx, runtime_event_rx) =
            tokio::sync::mpsc::channel::<RuntimeMessage>(RUNTIME_EVENT_BUFFER_CAPACITY);
        let mut runtime_event_rx = Some(runtime_event_rx);
        let mut pending_config_reload: Option<Box<AppConfig>> = None;
        let tray_icon = Some(build_tray_icon(indicator_icon(
            IndicatorState::Idle,
            &indicator_config,
        ))?);
        let mut last_indicator_state = IndicatorState::Idle;

        event_loop.run(move |event, _, control_flow| {
            *control_flow = ControlFlow::Wait;
            flush_pending_config_reload(&runtime_event_tx, &mut pending_config_reload);

            match event {
                Event::NewEvents(StartCause::Init) => {
                    info!(
                        profile = %profile,
                        platform = ?platform,
                        strict_step_contract,
                        config_path = %config_path.display(),
                        preflight = ?preflight,
                        "runtime bootstrap complete"
                    );

                    let runtime_events = runtime_event_rx
                        .take()
                        .expect("runtime event receiver should only be taken once");
                    spawn_runtime_worker(
                        runtime_config.clone(),
                        preflight,
                        proxy.clone(),
                        runtime_events,
                    );
                    spawn_config_watcher(config_path.clone(), proxy.clone());
                }
                Event::UserEvent(UserEvent::TrayEvent(event)) => {
                    if let Some(app_event) = map_tray_event(&event) {
                        if runtime_event_tx
                            .try_send(RuntimeMessage::AppEvent(app_event))
                            .is_err()
                        {
                            warn!(?app_event, "dropped tray interaction while runtime queue was full");
                        }
                    }

                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state,
                        ..
                    } = event
                    {
                        info!(indicator = ?last_indicator_state, ?button_state, "menu bar icon interaction");
                    }
                }
                Event::UserEvent(UserEvent::IndicatorUpdated(state)) => {
                    last_indicator_state = state;
                    if let Some(icon) = tray_icon.as_ref() {
                        update_tray_appearance(icon, state, &indicator_config);
                    }
                }
                Event::UserEvent(UserEvent::ConfigReloaded(config)) => {
                    indicator_config = config.indicator.clone();
                    if let Some(icon) = tray_icon.as_ref() {
                        update_tray_appearance(icon, last_indicator_state, &indicator_config);
                    }
                    match runtime_event_tx.try_send(RuntimeMessage::ReloadConfig(config.clone())) {
                        Ok(()) => {
                            info!(
                                profile = %config.app.profile,
                                "applied live config reload"
                            );
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Full(
                            RuntimeMessage::ReloadConfig(config),
                        )) => {
                            pending_config_reload = Some(config);
                            info!("queued latest config reload for next available runtime slot");
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            warn!("failed to forward config reload because runtime worker channel closed");
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Full(
                            RuntimeMessage::AppEvent(_),
                        )) => unreachable!("config reload forwarding only sends reload messages"),
                    }
                }
                Event::UserEvent(UserEvent::ConfigReloadFailed(message)) => {
                    warn!(%message, "live config reload failed; keeping previous config");
                }
                Event::UserEvent(UserEvent::RuntimeFailure(message)) => {
                    error!(%message, "runtime worker failed");
                    if let Some(icon) = tray_icon.as_ref() {
                        update_tray_appearance(icon, IndicatorState::Idle, &indicator_config);
                    }
                }
                _ => {}
            }

            let _keep_alive = tray_icon.as_ref();
        });
    }
}

#[derive(Debug, Clone)]
enum UserEvent {
    TrayEvent(TrayIconEvent),
    IndicatorUpdated(IndicatorState),
    ConfigReloaded(Box<AppConfig>),
    ConfigReloadFailed(String),
    RuntimeFailure(String),
}

#[derive(Debug, Clone)]
enum RuntimeMessage {
    AppEvent(AppEvent),
    ReloadConfig(Box<AppConfig>),
}

fn install_tray_event_bridge(proxy: EventLoopProxy<UserEvent>) {
    TrayIconEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::TrayEvent(event));
    }));
}

fn spawn_config_watcher(config_path: PathBuf, proxy: EventLoopProxy<UserEvent>) {
    std::thread::spawn(move || {
        let mut last_snapshot = read_config_snapshot(&config_path);

        loop {
            std::thread::sleep(Duration::from_millis(500));

            let snapshot = read_config_snapshot(&config_path);
            if snapshot == last_snapshot {
                continue;
            }

            match &snapshot {
                ConfigSnapshot::Contents(contents) => match AppConfig::from_toml_str(contents) {
                    Ok(config) => {
                        let _ = proxy.send_event(UserEvent::ConfigReloaded(Box::new(config)));
                    }
                    Err(error) => {
                        let _ = proxy.send_event(UserEvent::ConfigReloadFailed(format!(
                            "{}: {error}",
                            config_path.display()
                        )));
                    }
                },
                ConfigSnapshot::ReadError(message) => {
                    let _ = proxy.send_event(UserEvent::ConfigReloadFailed(format!(
                        "{}: {message}",
                        config_path.display()
                    )));
                }
            }

            last_snapshot = snapshot;
        }
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ConfigSnapshot {
    Contents(String),
    ReadError(String),
}

fn read_config_snapshot(path: &Path) -> ConfigSnapshot {
    match fs::read_to_string(path) {
        Ok(contents) => ConfigSnapshot::Contents(contents),
        Err(error) => ConfigSnapshot::ReadError(error.to_string()),
    }
}

fn build_tray_icon(icon: Icon) -> Result<tray_icon::TrayIcon> {
    TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("Muninn")
        .build()
        .context("creating menu bar tray icon")
}

fn update_tray_appearance(
    tray_icon: &tray_icon::TrayIcon,
    state: IndicatorState,
    indicator_config: &IndicatorConfig,
) {
    let visible_state = visible_indicator_state(state, indicator_config);
    if let Err(error) = tray_icon.set_icon(Some(indicator_icon(visible_state, indicator_config))) {
        warn!(%error, "failed to update tray icon");
    }
    if let Err(error) = tray_icon.set_tooltip(Some(indicator_tooltip(state))) {
        warn!(%error, "failed to update tray tooltip");
    }
    tray_icon.set_title(None::<&str>);
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
        IndicatorState::Cancelled => "Muninn cancelled",
    }
}

fn indicator_icon(state: IndicatorState, indicator_config: &IndicatorConfig) -> Icon {
    let rgba = match state {
        IndicatorState::Idle => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.idle),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
        ),
        IndicatorState::Recording { .. } => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.recording),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
        ),
        IndicatorState::Transcribing => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.transcribing),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
        ),
        IndicatorState::Pipeline => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.pipeline),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
        ),
        IndicatorState::Output => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.output),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
        ),
        IndicatorState::Cancelled => menu_bar_icon_rgba(
            parse_hex_rgb(&indicator_config.colors.cancelled),
            parse_hex_rgb(&indicator_config.colors.outline),
            parse_hex_rgb(&indicator_config.colors.glyph),
        ),
    };
    Icon::from_rgba(rgba, INDICATOR_ICON_SIZE_PX, INDICATOR_ICON_SIZE_PX)
        .expect("building indicator icon")
}

fn menu_bar_icon_rgba(
    background_rgb: [u8; 3],
    outline_rgb: [u8; 3],
    glyph_rgb: [u8; 3],
) -> Vec<u8> {
    let size = INDICATOR_ICON_SIZE_PX as usize;
    let mut rgba = vec![0_u8; size * size * 4];
    let center = INDICATOR_ICON_SIZE_PX as f32 / 2.0;
    let outline_radius = center - 1.0;
    let body_radius = outline_radius - 1.25;

    for y in 0..INDICATOR_ICON_SIZE_PX {
        for x in 0..INDICATOR_ICON_SIZE_PX {
            let idx = ((y * INDICATOR_ICON_SIZE_PX + x) * 4) as usize;
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

            if is_background_disc && pixel_m_glyph(x, y) {
                write_rgba(&mut rgba, idx, glyph_rgb);
            }
        }
    }
    rgba
}

fn pixel_m_glyph(x: u32, y: u32) -> bool {
    const GLYPH: [&str; 8] = [
        "1000001", "1100011", "1010101", "1001001", "1000001", "1000001", "1000001", "1000001",
    ];
    let scale = INDICATOR_ICON_SIZE_PX as f32 / 22.0;
    let glyph_x = (7.0 * scale).round();
    let glyph_y = (6.0 * scale).round();
    let local_x = ((x as f32 - glyph_x) / scale).floor() as i32;
    let local_y = ((y as f32 - glyph_y) / scale).floor() as i32;
    if local_x < 0 || local_y < 0 {
        return false;
    }

    let Some(row) = GLYPH.get(local_y as usize) else {
        return false;
    };

    matches!(row.as_bytes().get(local_x as usize), Some(b'1'))
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

fn spawn_runtime_worker(
    config: AppConfig,
    preflight: PermissionPreflightStatus,
    proxy: EventLoopProxy<UserEvent>,
    runtime_events: tokio::sync::mpsc::Receiver<RuntimeMessage>,
) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = proxy.send_event(UserEvent::RuntimeFailure(format!(
                    "building runtime worker: {error}"
                )));
                return;
            }
        };

        let indicator = EventLoopIndicator::new(proxy.clone());
        let worker = RuntimeWorker::new(config, preflight, indicator);
        if let Err(error) = runtime.block_on(worker.run(runtime_events)) {
            let _ = proxy.send_event(UserEvent::RuntimeFailure(format!("{error:#}")));
        }
    });
}

#[derive(Clone)]
struct EventLoopIndicator {
    proxy: EventLoopProxy<UserEvent>,
    state: Arc<Mutex<IndicatorState>>,
    sequence: Arc<AtomicU64>,
}

impl EventLoopIndicator {
    fn new(proxy: EventLoopProxy<UserEvent>) -> Self {
        Self {
            proxy,
            state: Arc::new(Mutex::new(IndicatorState::Idle)),
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
        self.sequence.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut guard) = self.state.lock() {
            *guard = state;
        }
        let _ = self.proxy.send_event(UserEvent::IndicatorUpdated(state));
        Ok(())
    }

    async fn set_temporary_state(
        &mut self,
        state: IndicatorState,
        min_duration: Duration,
        fallback_state: IndicatorState,
    ) -> muninn::MacosAdapterResult<()> {
        let generation = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        if let Ok(mut guard) = self.state.lock() {
            *guard = state;
        }
        let _ = self.proxy.send_event(UserEvent::IndicatorUpdated(state));

        let proxy = self.proxy.clone();
        let state = Arc::clone(&self.state);
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
            let _ = proxy.send_event(UserEvent::IndicatorUpdated(fallback_state));
        });

        Ok(())
    }

    async fn state(&self) -> muninn::MacosAdapterResult<IndicatorState> {
        self.state.lock().map(|guard| *guard).map_err(|_| {
            muninn::MacosAdapterError::operation_failed("indicator", "state mutex poisoned")
        })
    }
}

struct RuntimeWorker<I>
where
    I: IndicatorAdapter,
{
    config: AppConfig,
    preflight: PermissionPreflightStatus,
    indicator: I,
}

struct ProcessingContext<'a> {
    config: &'a AppConfig,
    pipeline: &'a PipelineConfig,
    runner: &'a PipelineRunner,
    injector: &'a MacosTextInjector,
}

impl<I> RuntimeWorker<I>
where
    I: IndicatorAdapter,
{
    fn new(config: AppConfig, preflight: PermissionPreflightStatus, indicator: I) -> Self {
        Self {
            config,
            preflight,
            indicator,
        }
    }

    async fn run(
        mut self,
        mut runtime_events: tokio::sync::mpsc::Receiver<RuntimeMessage>,
    ) -> Result<()> {
        let mut hotkeys = MacosHotkeyEventSource::from_config(&self.config.hotkeys)
            .context("initializing hotkeys")?;
        let mut recorder = MacosAudioRecorder::new();
        let injector = MacosTextInjector::new();
        let mut pipeline = resolve_pipeline_config(&self.config)?;
        let mut runner = PipelineRunner::new(self.config.app.strict_step_contract);
        let mut state = AppState::Idle;

        self.indicator
            .initialize()
            .await
            .context("initializing indicator")?;

        if self.preflight.allows_recording() {
            let warm_up_started = std::time::Instant::now();
            match recorder.warm_up().await {
                Ok(()) => {
                    debug!(
                        elapsed_ms = warm_up_started.elapsed().as_millis(),
                        "prewarmed audio recorder"
                    );
                }
                Err(error) => {
                    warn!(error = %error, "audio recorder prewarm failed; falling back to lazy init");
                }
            }
        }

        loop {
            let app_event = tokio::select! {
                hotkey_event = hotkeys.next_event() => {
                    let hotkey_event = match hotkey_event {
                        Ok(event) => event,
                        Err(error) => {
                            warn!(error = %error, "hotkey listener failed; attempting restart");
                            tokio::time::sleep(HOTKEY_RECOVERY_DELAY).await;
                            hotkeys = MacosHotkeyEventSource::from_config(&self.config.hotkeys)
                                .context("reinitializing hotkeys after listener failure")?;
                            continue;
                        }
                    };
                    let Some(app_event) = map_hotkey_event(hotkey_event) else {
                        continue;
                    };
                    app_event
                }
                maybe_event = runtime_events.recv() => {
                    match maybe_event {
                        Some(RuntimeMessage::AppEvent(app_event)) => app_event,
                        Some(RuntimeMessage::ReloadConfig(new_config)) => {
                            apply_live_config_reload(
                                &mut self.config,
                                &mut pipeline,
                                &mut runner,
                                *new_config,
                            )?;
                            continue;
                        }
                        None => return Err(anyhow!("runtime event channel closed")),
                    }
                }
            };

            let next = state.on_event(app_event);
            if next == state {
                continue;
            }

            match (state, app_event, next) {
                (AppState::Idle, AppEvent::PttPressed, AppState::RecordingPushToTalk) => {
                    self.preflight = refresh_permissions_status()
                        .await
                        .context("refreshing permissions before push-to-talk recording")?;
                    ensure_recording_can_start(self.preflight)?;
                    self.indicator
                        .set_state(IndicatorState::Recording {
                            mode: RecordingMode::PushToTalk,
                        })
                        .await
                        .context("setting push-to-talk recording indicator")?;
                    let started = std::time::Instant::now();
                    if let Err(error) = recorder.start_recording().await {
                        let _ = self.indicator.set_state(IndicatorState::Idle).await;
                        return Err(error).context("starting push-to-talk recording");
                    }
                    debug!(
                        elapsed_ms = started.elapsed().as_millis(),
                        "push-to-talk recording started"
                    );
                    state = next;
                }
                (AppState::Idle, AppEvent::DoneTogglePressed, AppState::RecordingDone) => {
                    self.preflight = refresh_permissions_status()
                        .await
                        .context("refreshing permissions before done-mode recording")?;
                    ensure_recording_can_start(self.preflight)?;
                    self.indicator
                        .set_state(IndicatorState::Recording {
                            mode: RecordingMode::DoneMode,
                        })
                        .await
                        .context("setting done-mode recording indicator")?;
                    let started = std::time::Instant::now();
                    if let Err(error) = recorder.start_recording().await {
                        let _ = self.indicator.set_state(IndicatorState::Idle).await;
                        return Err(error).context("starting done-mode recording");
                    }
                    debug!(
                        elapsed_ms = started.elapsed().as_millis(),
                        "done-mode recording started"
                    );
                    state = next;
                }
                (
                    AppState::RecordingPushToTalk | AppState::RecordingDone,
                    AppEvent::CancelPressed,
                    AppState::Idle,
                ) => {
                    recorder
                        .cancel_recording()
                        .await
                        .context("canceling recording")?;
                    self.indicator
                        .set_temporary_state(
                            IndicatorState::Cancelled,
                            OUTPUT_INDICATOR_MIN_DURATION,
                            IndicatorState::Idle,
                        )
                        .await
                        .context("setting cancelled indicator after cancel")?;
                    state = next;
                }
                (
                    AppState::RecordingPushToTalk | AppState::RecordingDone,
                    AppEvent::PttReleased | AppEvent::DoneTogglePressed,
                    AppState::Processing,
                ) => {
                    self.indicator
                        .set_state(initial_processing_indicator(&pipeline))
                        .await
                        .context("setting initial processing indicator")?;
                    let stopped = std::time::Instant::now();
                    let recorded = match recorder.stop_recording().await {
                        Ok(recorded) => recorded,
                        Err(error) => {
                            let _ = self.indicator.set_state(IndicatorState::Idle).await;
                            return Err(error).context("stopping recording");
                        }
                    };
                    debug!(
                        elapsed_ms = stopped.elapsed().as_millis(),
                        "recording stopped and wav finalized"
                    );
                    state = next;

                    if let Err(error) = process_and_inject(
                        ProcessingContext {
                            config: &self.config,
                            pipeline: &pipeline,
                            runner: &runner,
                            injector: &injector,
                        },
                        &mut self.indicator,
                        &mut state,
                        &recorded,
                    )
                    .await
                    {
                        error!(error = %error, "processing or injection failed");
                        state = AppState::Idle;
                        let _ = self.indicator.set_state(IndicatorState::Idle).await;
                    }
                    drain_busy_input_backlog(
                        &mut hotkeys,
                        &mut runtime_events,
                        &mut self.config,
                        &mut pipeline,
                        &mut runner,
                    )?;
                }
                _ => {
                    state = next;
                }
            }
        }
    }
}

async fn process_and_inject<I>(
    context: ProcessingContext<'_>,
    indicator: &mut I,
    state: &mut AppState,
    recorded: &muninn::RecordedAudio,
) -> Result<()>
where
    I: IndicatorAdapter,
{
    let result = async {
        let envelope = build_envelope(context.config, recorded);
        let outcome = run_pipeline_with_indicator_stages(
            context.pipeline,
            context.runner,
            indicator,
            envelope.clone(),
        )
        .await?;
        let route = Orchestrator::route_injection(&outcome);
        log_pipeline_outcome_diagnostics(&outcome);

        spawn_replay_persist(
            context.config.clone(),
            envelope.clone(),
            outcome.clone(),
            route.clone(),
            recorded.clone(),
        );

        *state = state.on_event(AppEvent::ProcessingFinished);

        if let Some(text) = route.target.text() {
            ensure_injection_allowed(
                refresh_permissions_status()
                    .await
                    .context("refreshing permissions before injection")?,
            )?;
            indicator
                .set_temporary_state(
                    IndicatorState::Output,
                    OUTPUT_INDICATOR_MIN_DURATION,
                    IndicatorState::Idle,
                )
                .await
                .context("setting output indicator")?;
            context
                .injector
                .inject_checked(text)
                .await
                .context("injecting final text")?;
            info!(
                route_reason = ?route.reason,
                pipeline_stop_reason = ?route.pipeline_stop_reason,
                injected_len = text.len(),
                "injected dictation text"
            );
        } else {
            warn!(
                route_reason = ?route.reason,
                pipeline_stop_reason = ?route.pipeline_stop_reason,
                "pipeline completed without injectable text"
            );
        }

        Ok::<(), anyhow::Error>(())
    }
    .await;

    *state = state.on_event(AppEvent::InjectionFinished);
    cleanup_recording_file(&recorded.wav_path);
    result?;
    let indicator_state = indicator
        .state()
        .await
        .context("reading indicator state after processing")?;
    if matches!(
        indicator_state,
        IndicatorState::Transcribing | IndicatorState::Pipeline
    ) {
        indicator
            .set_state(IndicatorState::Idle)
            .await
            .context("restoring idle indicator after non-output flow")?;
    }
    Ok(())
}

async fn run_pipeline_with_indicator_stages<I>(
    pipeline: &PipelineConfig,
    runner: &PipelineRunner,
    indicator: &mut I,
    envelope: MuninnEnvelopeV1,
) -> Result<PipelineOutcome>
where
    I: IndicatorAdapter,
{
    let Some((transcription_pipeline, mut remaining_pipeline)) =
        split_pipeline_for_indicator(pipeline)
    else {
        indicator
            .set_state(IndicatorState::Pipeline)
            .await
            .context("setting pipeline indicator")?;
        return Ok(runner.run(envelope, pipeline).await);
    };

    let pipeline_started = Instant::now();
    indicator
        .set_state(IndicatorState::Transcribing)
        .await
        .context("setting transcribing indicator")?;
    let transcription_outcome = runner.run(envelope, &transcription_pipeline).await;

    if remaining_pipeline.steps.is_empty() {
        return Ok(transcription_outcome);
    }

    match transcription_outcome {
        PipelineOutcome::Completed {
            envelope,
            trace: mut transcription_trace,
        } => {
            let elapsed_ms = duration_to_u64_ms(pipeline_started.elapsed());
            let remaining_deadline_ms = pipeline.deadline_ms.saturating_sub(elapsed_ms);
            if remaining_deadline_ms == 0 {
                return Ok(PipelineOutcome::FallbackRaw {
                    envelope,
                    trace: transcription_trace,
                    reason: PipelineStopReason::GlobalDeadlineExceeded {
                        deadline_ms: pipeline.deadline_ms,
                        step_id: remaining_pipeline.steps.first().map(|step| step.id.clone()),
                    },
                });
            }

            remaining_pipeline.deadline_ms = remaining_deadline_ms;
            indicator
                .set_state(IndicatorState::Pipeline)
                .await
                .context("setting pipeline indicator")?;
            let pipeline_outcome = runner.run(envelope, &remaining_pipeline).await;
            Ok(merge_pipeline_outcomes(
                &mut transcription_trace,
                pipeline_outcome,
            ))
        }
        other => Ok(other),
    }
}

fn split_pipeline_for_indicator(
    pipeline: &PipelineConfig,
) -> Option<(PipelineConfig, PipelineConfig)> {
    let transcription_steps = pipeline
        .steps
        .iter()
        .take_while(|step| internal_tools::is_transcription_step(step))
        .count();
    if transcription_steps == 0 {
        return None;
    }

    let transcription_pipeline = PipelineConfig {
        deadline_ms: pipeline.deadline_ms,
        payload_format: pipeline.payload_format,
        steps: pipeline
            .steps
            .iter()
            .take(transcription_steps)
            .cloned()
            .collect(),
    };
    let remaining_pipeline = PipelineConfig {
        deadline_ms: pipeline.deadline_ms,
        payload_format: pipeline.payload_format,
        steps: pipeline
            .steps
            .iter()
            .skip(transcription_steps)
            .cloned()
            .collect(),
    };

    Some((transcription_pipeline, remaining_pipeline))
}

fn initial_processing_indicator(pipeline: &PipelineConfig) -> IndicatorState {
    match pipeline.steps.first() {
        Some(step) if internal_tools::is_transcription_step(step) => IndicatorState::Transcribing,
        _ => IndicatorState::Pipeline,
    }
}

fn merge_pipeline_outcomes(
    existing_trace: &mut Vec<PipelineTraceEntry>,
    outcome: PipelineOutcome,
) -> PipelineOutcome {
    match outcome {
        PipelineOutcome::Completed {
            envelope,
            mut trace,
        } => {
            existing_trace.append(&mut trace);
            PipelineOutcome::Completed {
                envelope,
                trace: std::mem::take(existing_trace),
            }
        }
        PipelineOutcome::FallbackRaw {
            envelope,
            mut trace,
            reason,
        } => {
            existing_trace.append(&mut trace);
            PipelineOutcome::FallbackRaw {
                envelope,
                trace: std::mem::take(existing_trace),
                reason,
            }
        }
        PipelineOutcome::Aborted { mut trace, reason } => {
            existing_trace.append(&mut trace);
            PipelineOutcome::Aborted {
                trace: std::mem::take(existing_trace),
                reason,
            }
        }
    }
}

fn duration_to_u64_ms(duration: Duration) -> u64 {
    duration
        .as_millis()
        .min(u128::from(u64::MAX))
        .try_into()
        .unwrap_or(u64::MAX)
}

fn log_pipeline_outcome_diagnostics(outcome: &PipelineOutcome) {
    let trace = match outcome {
        PipelineOutcome::Completed { trace, .. }
        | PipelineOutcome::FallbackRaw { trace, .. }
        | PipelineOutcome::Aborted { trace, .. } => trace,
    };

    let Some(last_step) = trace.last() else {
        return;
    };

    if last_step.stderr.trim().is_empty() {
        return;
    }

    warn!(
        step_id = %last_step.id,
        exit_status = ?last_step.exit_status,
        timed_out = last_step.timed_out,
        stderr_len = last_step.stderr.len(),
        "pipeline step emitted stderr (redacted)"
    );
}

fn build_envelope(_config: &AppConfig, recorded: &muninn::RecordedAudio) -> MuninnEnvelopeV1 {
    MuninnEnvelopeV1::new(Uuid::now_v7().to_string(), Utc::now().to_rfc3339()).with_audio(
        Some(recorded.wav_path.display().to_string()),
        recorded.duration_ms,
    )
}

fn resolve_pipeline_config(config: &AppConfig) -> Result<PipelineConfig> {
    let mut pipeline = config.pipeline.clone();
    for step in &mut pipeline.steps {
        if internal_tools::rewrite_internal_tool_step(step)? {
            continue;
        }
        step.cmd = resolve_step_command(&step.cmd)?;
    }
    Ok(pipeline)
}

fn apply_live_config_reload(
    current_config: &mut AppConfig,
    pipeline: &mut PipelineConfig,
    runner: &mut PipelineRunner,
    mut new_config: AppConfig,
) -> Result<()> {
    let old_profile = current_config.app.profile.clone();
    if new_config.hotkeys != current_config.hotkeys {
        warn!("hotkey changes detected in config reload; restart Muninn to apply new hotkeys");
        new_config.hotkeys = current_config.hotkeys.clone();
    }

    *pipeline = resolve_pipeline_config(&new_config)?;
    *runner = PipelineRunner::new(new_config.app.strict_step_contract);
    *current_config = new_config;
    info!(
        old_profile = %old_profile,
        new_profile = %current_config.app.profile,
        pipeline_steps = current_config.pipeline.steps.len(),
        "runtime worker applied live config reload"
    );

    Ok(())
}

fn resolve_step_command(cmd: &str) -> Result<String> {
    if Path::new(cmd).components().count() > 1 {
        return Ok(cmd.to_string());
    }

    let current_exe = std::env::current_exe().context("resolving current executable path")?;
    let bin_dir = current_exe
        .parent()
        .context("resolving executable parent directory")?;
    let sibling = bin_dir.join(cmd);
    if sibling.exists() {
        return Ok(sibling.display().to_string());
    }

    Ok(cmd.to_string())
}

fn cleanup_recording_file(path: &Path) {
    if let Err(error) = std::fs::remove_file(path) {
        warn!(path = %path.display(), %error, "failed to delete temporary recording");
    }
}

fn maybe_load_dotenv() {
    if std::env::var("MUNINN_LOAD_DOTENV")
        .ok()
        .as_deref()
        .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
    {
        let _ = dotenvy::dotenv();
    }
}

fn flush_pending_config_reload(
    runtime_event_tx: &tokio::sync::mpsc::Sender<RuntimeMessage>,
    pending_config_reload: &mut Option<Box<AppConfig>>,
) {
    let Some(config) = pending_config_reload.take() else {
        return;
    };

    match runtime_event_tx.try_send(RuntimeMessage::ReloadConfig(config)) {
        Ok(()) => {}
        Err(tokio::sync::mpsc::error::TrySendError::Full(RuntimeMessage::ReloadConfig(config))) => {
            *pending_config_reload = Some(config);
        }
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
            warn!("runtime worker channel closed before queued config reload could be forwarded");
        }
        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => unreachable!(),
    }
}

async fn refresh_permissions_status() -> Result<PermissionPreflightStatus> {
    MacosPermissionsAdapter::new()
        .preflight()
        .await
        .map_err(|error| anyhow!(error))
}

fn drain_busy_input_backlog(
    hotkeys: &mut MacosHotkeyEventSource,
    runtime_events: &mut tokio::sync::mpsc::Receiver<RuntimeMessage>,
    current_config: &mut AppConfig,
    pipeline: &mut PipelineConfig,
    runner: &mut PipelineRunner,
) -> Result<()> {
    let mut dropped_hotkeys = 0_usize;
    while let Some(result) = hotkeys.try_next_event() {
        match result {
            Ok(event) if map_hotkey_event(event).is_some() => dropped_hotkeys += 1,
            Ok(_) => {}
            Err(error) => {
                warn!(error = %error, "dropping queued hotkey listener error after busy period")
            }
        }
    }

    let mut dropped_runtime_events = 0_usize;
    let mut latest_config = None;
    loop {
        match runtime_events.try_recv() {
            Ok(RuntimeMessage::AppEvent(_)) => dropped_runtime_events += 1,
            Ok(RuntimeMessage::ReloadConfig(config)) => latest_config = Some(config),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                return Err(anyhow!("runtime event channel closed"));
            }
        }
    }

    let applied_config_reload = if let Some(config) = latest_config {
        apply_live_config_reload(current_config, pipeline, runner, *config)?;
        true
    } else {
        false
    };

    if dropped_hotkeys > 0 || dropped_runtime_events > 0 || applied_config_reload {
        info!(
            dropped_hotkeys,
            dropped_runtime_events, applied_config_reload, "drained busy-period runtime backlog"
        );
    }

    Ok(())
}

fn spawn_replay_persist(
    config: AppConfig,
    envelope: MuninnEnvelopeV1,
    outcome: PipelineOutcome,
    route: muninn::InjectionRoute,
    recorded: muninn::RecordedAudio,
) {
    drop(tokio::task::spawn_blocking(
        move || match replay::persist_replay(&config, &envelope, &outcome, &route, &recorded) {
            Ok(Some(path)) => {
                info!(replay_dir = %path.display(), "persisted replay artifact");
            }
            Ok(None) => {}
            Err(error) => {
                warn!(error = %error, "failed to persist replay artifact");
            }
        },
    ));
}

fn cleanup_stale_temp_recordings() -> Result<()> {
    let temp_dir = std::env::temp_dir();
    let now = std::time::SystemTime::now();

    for entry in fs::read_dir(&temp_dir)
        .with_context(|| format!("reading temp dir {}", temp_dir.display()))?
    {
        let entry = entry.with_context(|| format!("reading entry in {}", temp_dir.display()))?;
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !file_name.starts_with("muninn-")
            || path.extension().and_then(|value| value.to_str()) != Some("wav")
        {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                warn!(path = %path.display(), %error, "failed to inspect temporary recording metadata");
                continue;
            }
        };
        let Ok(modified_at) = metadata.modified() else {
            continue;
        };
        let Ok(age) = now.duration_since(modified_at) else {
            continue;
        };
        if age < STALE_RECORDING_MAX_AGE {
            continue;
        }

        if let Err(error) = fs::remove_file(&path) {
            warn!(path = %path.display(), %error, "failed to remove stale temporary recording");
        }
    }

    Ok(())
}

fn ensure_recording_can_start(preflight: PermissionPreflightStatus) -> Result<()> {
    if matches!(preflight.input_monitoring, PermissionStatus::Granted)
        && !matches!(
            preflight.microphone,
            PermissionStatus::Denied | PermissionStatus::Restricted | PermissionStatus::Unsupported
        )
    {
        return Ok(());
    }

    preflight
        .ensure_recording_allowed()
        .map_err(|error| anyhow!(error))
}

fn ensure_injection_allowed(preflight: PermissionPreflightStatus) -> Result<()> {
    preflight
        .ensure_injection_allowed()
        .map_err(|error| anyhow!(error))
}

fn map_hotkey_event(event: HotkeyEvent) -> Option<AppEvent> {
    match (event.action, event.kind) {
        (HotkeyAction::PushToTalk, HotkeyEventKind::Pressed) => Some(AppEvent::PttPressed),
        (HotkeyAction::PushToTalk, HotkeyEventKind::Released) => Some(AppEvent::PttReleased),
        (HotkeyAction::DoneModeToggle, HotkeyEventKind::Pressed) => {
            Some(AppEvent::DoneTogglePressed)
        }
        (HotkeyAction::CancelCurrentCapture, HotkeyEventKind::Pressed) => {
            Some(AppEvent::CancelPressed)
        }
        _ => None,
    }
}

fn map_tray_event(event: &TrayIconEvent) -> Option<AppEvent> {
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

#[cfg(test)]
mod tests {
    use super::{apply_live_config_reload, map_tray_event, resolve_pipeline_config};
    use muninn::config::{OnErrorPolicy, PipelineStepConfig, StepIoMode};
    use muninn::AppEvent;
    use muninn::PipelineRunner;
    use tray_icon::{
        dpi::{PhysicalPosition, PhysicalSize},
        MouseButton, MouseButtonState, Rect, TrayIconEvent, TrayIconId,
    };

    fn left_tray_click(button_state: MouseButtonState) -> TrayIconEvent {
        TrayIconEvent::Click {
            id: TrayIconId::new("muninn-test"),
            position: PhysicalPosition::new(0.0, 0.0),
            rect: Rect {
                size: PhysicalSize::new(22, 22),
                position: PhysicalPosition::new(0.0, 0.0),
            },
            button: MouseButton::Left,
            button_state,
        }
    }

    #[test]
    fn tray_left_mouse_down_maps_to_ptt_pressed() {
        assert_eq!(
            map_tray_event(&left_tray_click(MouseButtonState::Down)),
            Some(AppEvent::PttPressed)
        );
    }

    #[test]
    fn tray_left_mouse_up_maps_to_ptt_released() {
        assert_eq!(
            map_tray_event(&left_tray_click(MouseButtonState::Up)),
            Some(AppEvent::PttReleased)
        );
    }

    #[test]
    fn live_reload_updates_pipeline_but_keeps_existing_hotkeys() {
        let mut current = muninn::AppConfig::launchable_default();
        current.pipeline.steps = vec![PipelineStepConfig {
            id: "noop".to_string(),
            cmd: "/usr/bin/true".to_string(),
            args: Vec::new(),
            io_mode: StepIoMode::Auto,
            timeout_ms: 250,
            on_error: OnErrorPolicy::Continue,
        }];
        let original_hotkeys = current.hotkeys.clone();

        let mut pipeline =
            resolve_pipeline_config(&current).expect("initial pipeline should resolve");
        let mut runner = PipelineRunner::new(current.app.strict_step_contract);

        let mut reloaded = current.clone();
        reloaded.hotkeys.push_to_talk.chord = vec!["cmd".to_string()];
        reloaded.pipeline.steps = vec![PipelineStepConfig {
            id: "uppercase".to_string(),
            cmd: "/bin/cat".to_string(),
            args: Vec::new(),
            io_mode: StepIoMode::Auto,
            timeout_ms: 250,
            on_error: OnErrorPolicy::Continue,
        }];

        apply_live_config_reload(&mut current, &mut pipeline, &mut runner, reloaded)
            .expect("reload should succeed");

        assert_eq!(current.hotkeys, original_hotkeys);
        assert_eq!(current.pipeline.steps[0].id, "uppercase");
        assert_eq!(pipeline.steps[0].cmd, "/bin/cat");
    }
}

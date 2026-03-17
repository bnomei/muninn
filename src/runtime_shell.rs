use std::path::PathBuf;

use anyhow::{Context, Result};
use muninn::{
    capture_frontmost_target_context, detect_platform, ensure_supported_platform, AppConfig,
    IndicatorState, PermissionPreflightStatus, PermissionsAdapter, Platform,
};
use tao::event::{Event, StartCause};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
#[cfg(target_os = "macos")]
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
use tracing::{error, info, warn};
use tray_icon::{MouseButton, TrayIconEvent};

use crate::runtime_tray::{
    build_tray_icon, indicator_icon, install_tray_event_bridge, map_tray_event,
    update_tray_appearance, UserEvent,
};
use crate::runtime_worker::{spawn_runtime_worker, RuntimeMessage};
use crate::{config_watch, logging};

const CONFIG_RELOAD_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

pub(crate) struct AppRuntime {
    config_path: PathBuf,
    config: AppConfig,
    platform: Platform,
    preflight: PermissionPreflightStatus,
}

impl AppRuntime {
    pub(crate) fn new(config_path: PathBuf, config: AppConfig) -> Result<Self> {
        let platform = detect_platform();
        ensure_supported_platform().with_context(|| {
            format!("muninn currently supports macOS only (detected: {platform:?})")
        })?;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("building startup tokio runtime")?;
        let preflight = runtime
            .block_on(muninn::MacosPermissionsAdapter::new().preflight())
            .context("running macOS permission preflight")?;

        Ok(Self {
            config_path,
            config,
            platform,
            preflight,
        })
    }

    pub(crate) fn run(self) -> Result<()> {
        let mut event_loop_builder = EventLoopBuilder::<UserEvent>::with_user_event();
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
        let mut current_config = self.config.clone();
        let mut indicator_config = current_config.indicator.clone();
        let runtime_config = current_config.clone();
        let (runtime_event_tx, runtime_event_rx) =
            tokio::sync::mpsc::channel::<RuntimeMessage>(crate::RUNTIME_EVENT_BUFFER_CAPACITY);
        let mut runtime_event_rx = Some(runtime_event_rx);
        let mut pending_config_reload: Option<Box<AppConfig>> = None;
        let mut preview_context = capture_frontmost_target_context();
        let mut preview_selection = current_config.resolve_profile_selection(&preview_context);
        let tray_icon = Some(build_tray_icon(indicator_icon(
            IndicatorState::Idle,
            None,
            preview_selection.voice_glyph,
            &indicator_config,
        ))?);
        let mut last_indicator_state = IndicatorState::Idle;
        let mut last_active_glyph = None;

        event_loop.run(move |event, _, control_flow| {
            *control_flow = ControlFlow::Wait;
            crate::flush_pending_config_reload(&runtime_event_tx, &mut pending_config_reload);

            match event {
                Event::NewEvents(StartCause::Init) => {
                    info!(
                        target: logging::TARGET_RUNTIME,
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
                    config_watch::spawn_config_watcher(config_path.clone(), proxy.clone());
                    config_watch::spawn_preview_context_watcher(proxy.clone());
                }
                Event::UserEvent(UserEvent::TrayEvent(event)) => {
                    if let Some(app_event) = map_tray_event(&event) {
                        match runtime_event_tx.try_send(RuntimeMessage::AppEvent(app_event)) {
                            Ok(()) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Full(
                                RuntimeMessage::AppEvent(app_event),
                            )) => {
                                warn!(
                                    target: logging::TARGET_HOTKEY,
                                    ?app_event,
                                    "dropped tray interaction while runtime queue was full"
                                );
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(
                                RuntimeMessage::AppEvent(app_event),
                            )) => {
                                warn!(
                                    target: logging::TARGET_RUNTIME,
                                    ?app_event,
                                    "dropped tray interaction because runtime worker channel closed"
                                );
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Full(
                                RuntimeMessage::ReloadConfig(_),
                            ))
                            | Err(tokio::sync::mpsc::error::TrySendError::Closed(
                                RuntimeMessage::ReloadConfig(_),
                            )) => unreachable!("tray forwarding only sends app events"),
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
                Event::UserEvent(UserEvent::IndicatorUpdated { state, glyph }) => {
                    last_indicator_state = state;
                    last_active_glyph = glyph;
                    if let Some(icon) = tray_icon.as_ref() {
                        update_tray_appearance(
                            icon,
                            state,
                            glyph,
                            preview_selection.voice_glyph,
                            &indicator_config,
                        );
                    }
                }
                Event::UserEvent(UserEvent::PreviewContextUpdated(context)) => {
                    preview_context = context;
                    preview_selection = current_config.resolve_profile_selection(&preview_context);
                    if let Some(icon) = tray_icon.as_ref() {
                        update_tray_appearance(
                            icon,
                            last_indicator_state,
                            last_active_glyph,
                            preview_selection.voice_glyph,
                            &indicator_config,
                        );
                    }
                }
                Event::UserEvent(UserEvent::ConfigReloaded(config)) => {
                    current_config = (*config).clone();
                    indicator_config = current_config.indicator.clone();
                    preview_selection = current_config.resolve_profile_selection(&preview_context);
                    crate::sync_os_autostart(&config_path, &current_config);
                    if let Some(icon) = tray_icon.as_ref() {
                        update_tray_appearance(
                            icon,
                            last_indicator_state,
                            last_active_glyph,
                            preview_selection.voice_glyph,
                            &indicator_config,
                        );
                    }
                    match runtime_event_tx.try_send(RuntimeMessage::ReloadConfig(config.clone())) {
                        Ok(()) => {
                            info!(
                                target: logging::TARGET_CONFIG,
                                profile = %current_config.app.profile,
                                "applied live config reload"
                            );
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Full(
                            RuntimeMessage::ReloadConfig(config),
                        )) => {
                            pending_config_reload = Some(config);
                            schedule_pending_config_reload_retry(&proxy);
                            info!(
                                target: logging::TARGET_CONFIG,
                                "queued latest config reload for next available runtime slot"
                            );
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            warn!(
                                target: logging::TARGET_CONFIG,
                                "failed to forward config reload because runtime worker channel closed"
                            );
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Full(
                            RuntimeMessage::AppEvent(_),
                        )) => unreachable!("config reload forwarding only sends reload messages"),
                    }
                }
                Event::UserEvent(UserEvent::ConfigReloadFailed(message)) => {
                    warn!(
                        target: logging::TARGET_CONFIG,
                        %message,
                        "live config reload failed; keeping previous config"
                    );
                }
                Event::UserEvent(UserEvent::RetryPendingConfigReload) => {
                    if pending_config_reload.is_some() {
                        schedule_pending_config_reload_retry(&proxy);
                    }
                }
                Event::UserEvent(UserEvent::RuntimeFailure(message)) => {
                    error!(target: logging::TARGET_RUNTIME, %message, "runtime worker failed");
                    last_indicator_state = IndicatorState::Idle;
                    last_active_glyph = None;
                    if let Some(icon) = tray_icon.as_ref() {
                        update_tray_appearance(
                            icon,
                            IndicatorState::Idle,
                            None,
                            preview_selection.voice_glyph,
                            &indicator_config,
                        );
                    }
                }
                _ => {}
            }

            let _keep_alive = tray_icon.as_ref();
        });
    }
}

fn schedule_pending_config_reload_retry(proxy: &EventLoopProxy<UserEvent>) {
    let proxy = proxy.clone();
    std::thread::spawn(move || {
        std::thread::sleep(CONFIG_RELOAD_RETRY_DELAY);
        let _ = proxy.send_event(UserEvent::RetryPendingConfigReload);
    });
}

//! Localhost streamable-HTTP MCP server exposing recording-control tools.

use std::net::SocketAddr;

use rmcp::{
    model::{CallToolResult, Content, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::streamable_http_server::{
        session::local::LocalSessionManager,
        tower::{StreamableHttpServerConfig, StreamableHttpService},
    },
    ErrorData as McpError, ServerHandler,
};
use tao::event_loop::EventLoopProxy;
use tracing::{error, info, warn};

use super::{ExternalControlAction, RuntimeStatusHandle};
use crate::logging::TARGET_RUNTIME;
use crate::runtime_tray::{send_user_event, UserEvent};

/// MCP server that translates tool calls into [`UserEvent::ExternalControl`].
///
/// `#[tool_handler]` dispatches through the generated `Self::tool_router()`, so
/// the router is not stored on the struct itself.
#[derive(Clone)]
pub(crate) struct RecordingControlServer {
    proxy: EventLoopProxy<UserEvent>,
    status: RuntimeStatusHandle,
}

#[tool_router]
impl RecordingControlServer {
    fn new(proxy: EventLoopProxy<UserEvent>, status: RuntimeStatusHandle) -> Self {
        Self { proxy, status }
    }

    fn dispatch(
        &self,
        action: ExternalControlAction,
        message: &'static str,
    ) -> Result<CallToolResult, McpError> {
        send_user_event(
            &self.proxy,
            UserEvent::ExternalControl(action),
            "mcp_external_control",
        );
        Ok(CallToolResult::success(vec![Content::text(message)]))
    }

    #[tool(
        description = "Return Muninn runtime status without starting or stopping recording. \
            The response is JSON with state, recording_active, busy, permissions, \
            and optional failure fields. State is one of idle, recording_active, \
            permission_blocked, already_running, or failed.",
        annotations(
            title = "Get runtime status",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn get_status(&self) -> Result<CallToolResult, McpError> {
        let json = serde_json::to_string(&self.status.snapshot()).map_err(|error| {
            McpError::internal_error(format!("failed to serialize runtime status: {error}"), None)
        })?;
        Ok(CallToolResult::success(vec![Content::text(json)]))
    }

    #[tool(
        description = "Start Muninn dictation recording (microphone capture). \
            Recording stays active until it is stopped: call stop_recording to \
            finish and transcribe, or the user can stop it themselves by \
            clicking the menu-bar tray icon or pressing their dictation hotkey. \
            No-op if a recording is already active.",
        annotations(
            title = "Start recording",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn start_recording(&self) -> Result<CallToolResult, McpError> {
        self.dispatch(ExternalControlAction::Start, "recording start requested")
    }

    #[tool(
        description = "Stop the recording started by start_recording and run the \
            transcription pipeline; the transcribed text is typed into the \
            user's focused application. No-op if no recording is active. The \
            user can also stop from the tray icon or their hotkey.",
        annotations(
            title = "Stop recording and transcribe",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn stop_recording(&self) -> Result<CallToolResult, McpError> {
        self.dispatch(ExternalControlAction::Stop, "recording stop requested")
    }

    #[tool(
        description = "Cancel the active Muninn recording, discarding the \
            captured audio without transcribing or typing anything. Use this \
            instead of stop_recording to abandon a recording. No-op if no \
            recording is active.",
        annotations(
            title = "Cancel recording",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    fn cancel_recording(&self) -> Result<CallToolResult, McpError> {
        self.dispatch(ExternalControlAction::Cancel, "recording cancel requested")
    }
}

#[tool_handler]
impl ServerHandler for RecordingControlServer {
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.instructions = Some(
            "Control Muninn dictation (speech-to-text) recording. Call get_status \
             to inspect idle, recording, permission-blocked, busy, or failed \
             state without starting work. Typical flow: call start_recording, \
             let the user speak, then call stop_recording to transcribe and \
             type the text into their focused app (the user can also stop via \
             the menu-bar tray icon or their hotkey). Use cancel_recording to \
             discard a recording without transcribing."
                .to_string(),
        );
        info
    }
}

/// Spawn the MCP server on a dedicated thread with its own current-thread runtime.
pub(crate) fn spawn_mcp_server(
    proxy: EventLoopProxy<UserEvent>,
    bind_address: String,
    status: RuntimeStatusHandle,
) {
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                error!(target: TARGET_RUNTIME, %error, "failed to build MCP server runtime");
                return;
            }
        };
        runtime.block_on(serve(proxy, bind_address, status));
    });
}

/// Warn when the MCP server is bound to a non-loopback address.
///
/// The server exposes recording-control tools with no authentication, relying
/// entirely on the bind address staying loopback-only. A non-loopback bind
/// (for example `0.0.0.0` or a LAN IP) lets any host that can reach the address
/// start or stop recording. Hostname binds that do not parse as a socket
/// address are left for `TcpListener::bind` to resolve.
fn warn_if_exposed_bind_address(bind_address: &str) {
    if let Ok(addr) = bind_address.parse::<SocketAddr>() {
        if !addr.ip().is_loopback() {
            warn!(
                target: TARGET_RUNTIME,
                %bind_address,
                "external-control MCP server is bound to a non-loopback address and has no authentication; any host that can reach this address can start or stop recording. Bind to 127.0.0.1 unless you intend to expose it."
            );
        }
    }
}

async fn serve(
    proxy: EventLoopProxy<UserEvent>,
    bind_address: String,
    status: RuntimeStatusHandle,
) {
    warn_if_exposed_bind_address(&bind_address);
    let service = StreamableHttpService::new(
        move || Ok(RecordingControlServer::new(proxy.clone(), status.clone())),
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );
    let app = axum::Router::new().nest_service("/mcp", service);

    let listener = match tokio::net::TcpListener::bind(&bind_address).await {
        Ok(listener) => listener,
        Err(error) => {
            error!(
                target: TARGET_RUNTIME,
                %bind_address,
                %error,
                "failed to bind external-control MCP server"
            );
            return;
        }
    };
    info!(
        target: TARGET_RUNTIME,
        %bind_address,
        "external-control MCP server listening on /mcp"
    );
    if let Err(error) = axum::serve(listener, app).await {
        error!(target: TARGET_RUNTIME, %error, "external-control MCP server stopped");
    }
}

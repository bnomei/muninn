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
use tracing::{error, info};

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
    start_recording_enabled: bool,
}

#[tool_router]
impl RecordingControlServer {
    fn new(
        proxy: EventLoopProxy<UserEvent>,
        status: RuntimeStatusHandle,
        start_recording_enabled: bool,
    ) -> Self {
        Self {
            proxy,
            status,
            start_recording_enabled,
        }
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
        if !self.start_recording_enabled {
            return Ok(CallToolResult::success(vec![Content::json(
                serde_json::json!({
                    "status": "disabled",
                    "action": "start_recording",
                    "reason": "external_control.start_recording_enabled is false"
                }),
            )?]));
        }

        send_user_event(
            &self.proxy,
            UserEvent::ExternalControl(ExternalControlAction::Start),
            "mcp_external_control",
        );
        Ok(CallToolResult::success(vec![Content::json(
            serde_json::json!({
                "status": "enabled",
                "action": "start_recording",
                "message": "recording start requested"
            }),
        )?]))
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
    start_recording_enabled: bool,
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
        runtime.block_on(serve(proxy, bind_address, status, start_recording_enabled));
    });
}

/// Validate that the MCP server bind address is an explicit loopback socket address.
///
/// The server exposes recording-control tools with no authentication, so it must
/// not bind to wildcard, LAN, or other non-loopback addresses. Hostnames are
/// rejected instead of resolved so the policy is visible from configuration.
fn validate_bind_address(bind_address: &str) -> Result<SocketAddr, String> {
    let addr = bind_address.parse::<SocketAddr>().map_err(|error| {
        format!("mcp_bind_address must be an explicit loopback socket address: {error}")
    })?;

    if !addr.ip().is_loopback() {
        return Err(format!(
            "mcp_bind_address must be loopback-only; {bind_address} is not allowed"
        ));
    }

    Ok(addr)
}

async fn serve(
    proxy: EventLoopProxy<UserEvent>,
    bind_address: String,
    status: RuntimeStatusHandle,
    start_recording_enabled: bool,
) {
    let bind_addr = match validate_bind_address(&bind_address) {
        Ok(addr) => addr,
        Err(error) => {
            error!(
                target: TARGET_RUNTIME,
                %bind_address,
                %error,
                "refusing to start external-control MCP server"
            );
            return;
        }
    };
    let service = StreamableHttpService::new(
        move || {
            Ok(RecordingControlServer::new(
                proxy.clone(),
                status.clone(),
                start_recording_enabled,
            ))
        },
        LocalSessionManager::default().into(),
        StreamableHttpServerConfig::default(),
    );
    let app = axum::Router::new().nest_service("/mcp", service);

    let listener = match tokio::net::TcpListener::bind(bind_addr).await {
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

#[cfg(test)]
mod tests {
    use super::validate_bind_address;

    #[test]
    fn accepts_loopback_bind_addresses() {
        assert!(validate_bind_address("127.0.0.1:2769").is_ok());
        assert!(validate_bind_address("[::1]:2769").is_ok());
    }

    #[test]
    fn rejects_non_loopback_bind_addresses() {
        assert!(validate_bind_address("0.0.0.0:2769").is_err());
        assert!(validate_bind_address("192.168.1.10:2769").is_err());
        assert!(validate_bind_address("[::]:2769").is_err());
    }
}

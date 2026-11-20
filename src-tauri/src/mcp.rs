//! Inspect explicitly chosen Streamable HTTP MCP endpoints in the desktop host.
use std::time::Duration;

use serde::Serialize;
use tauri::{Manager, Runtime, WebviewWindow};
use uebox_mcp_client::{ClientConfig, McpClient, McpError};

#[derive(Default)]
pub(crate) struct McpHost(tauri::async_runtime::Mutex<()>);

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct McpInspection {
    protocol_version: String,
    total_tools: usize,
    tool_names: Vec<String>,
    session_cleanup_pending: bool,
}

fn error_code(error: McpError) -> &'static str {
    match error {
        McpError::InvalidEndpoint => "mcp_invalid_endpoint",
        McpError::Timeout => "mcp_timeout",
        McpError::Redirect => "mcp_redirect",
        McpError::HttpStatus(401 | 403) => "mcp_authorization_required",
        McpError::ProtocolVersionMismatch => "mcp_version",
        McpError::ToolsNotSupported => "mcp_no_tools",
        _ => "mcp_unavailable",
    }
}

#[tauri::command]
pub(crate) async fn inspect_mcp_endpoint<R: Runtime>(
    window: WebviewWindow<R>,
    endpoint: String,
) -> Result<McpInspection, &'static str> {
    if window.label() != "main" {
        return Err("mcp_unavailable");
    }
    let state = window.state::<McpHost>();
    let _busy = state.0.try_lock().map_err(|_| "mcp_busy")?;

    let mut config = ClientConfig::new(endpoint);
    config.request_timeout = Duration::from_secs(10);
    let mut client = McpClient::connect(config).await.map_err(error_code)?;
    let tools = client.list_tools().await;
    let session_cleanup_pending = client.close().await.is_err();
    let tools = tools.map_err(error_code)?;
    let total_tools = tools.len();
    let tool_names = tools
        .into_iter()
        .take(200)
        .map(|tool| {
            tool.name
                .chars()
                .filter(|character| !character.is_control())
                .collect()
        })
        .collect();
    Ok(McpInspection {
        protocol_version: client.protocol_version().to_owned(),
        total_tools,
        tool_names,
        session_cleanup_pending,
    })
}

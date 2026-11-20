//! Bounded MCP Streamable HTTP client for explicitly configured trusted endpoints.

use std::{collections::HashSet, fmt, time::Duration};

use reqwest::{header, redirect::Policy, Client, RequestBuilder, Response, StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::{timeout_at, Instant};

pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";
const SUPPORTED_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_SSE_EVENTS: usize = 1024;
const MAX_PAGES: usize = 256;
const MAX_TOOLS: usize = 10_000;
const MAX_TOTAL_TOOLS_BYTES: usize = 8 * 1024 * 1024;
const MAX_CURSOR_BYTES: usize = 8 * 1024;
const MAX_TIMEOUT: Duration = Duration::from_secs(300);

/// Explicit endpoint configuration. No ambient credentials or headers are read.
#[derive(Clone)]
pub struct ClientConfig {
    endpoint: String,
    authorization: Option<header::HeaderValue>,
    pub request_timeout: Duration,
}

impl ClientConfig {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            authorization: None,
            request_timeout: Duration::from_secs(30),
        }
    }

    /// Attach a manually provisioned bearer to this exact endpoint for every protocol request.
    /// This configuration never follows redirects or reads ambient authorization.
    pub fn with_bearer(mut self, token: &[u8]) -> Result<Self, McpError> {
        if token.is_empty()
            || token.len() > 2560
            || !token.iter().all(|byte| (0x21..=0x7e).contains(byte))
        {
            return Err(McpError::InvalidAuthorization);
        }
        let mut bytes = b"Bearer ".to_vec();
        bytes.extend_from_slice(token);
        let value = header::HeaderValue::from_maybe_shared(bytes);
        let mut value = value.map_err(|_| McpError::InvalidAuthorization)?;
        value.set_sensitive(true);
        self.authorization = Some(value);
        Ok(self)
    }
}

/// Validate and store the exact URL used to send requests.
pub fn canonical_endpoint(raw: &str) -> Result<String, McpError> {
    validate_endpoint(raw).map(Into::into)
}

/// Stable errors omit endpoint values and server-provided error messages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum McpError {
    InvalidEndpoint,
    InvalidAuthorization,
    InvalidTimeout,
    Transport,
    Timeout,
    Redirect,
    HttpStatus(u16),
    RequestTooLarge,
    ResponseTooLarge,
    UnsupportedContentType,
    InvalidSessionId,
    SessionRequired,
    SessionExpired,
    ProtocolVersionMismatch,
    ToolsNotSupported,
    InvalidProtocol,
    ServerRpc(i64),
    PaginationLoop,
    PaginationLimit,
    ToolLimit,
    Closed,
}

impl fmt::Display for McpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidEndpoint => f.write_str("invalid or disallowed MCP endpoint"),
            Self::InvalidAuthorization => f.write_str("invalid MCP authorization"),
            Self::InvalidTimeout => f.write_str("invalid MCP request timeout"),
            Self::Transport => f.write_str("MCP transport failed"),
            Self::Timeout => f.write_str("MCP request timed out"),
            Self::Redirect => f.write_str("MCP endpoint redirected the request"),
            Self::HttpStatus(status) => write!(f, "MCP endpoint returned HTTP {status}"),
            Self::RequestTooLarge => f.write_str("MCP request exceeded the size limit"),
            Self::ResponseTooLarge => f.write_str("MCP response exceeded the size limit"),
            Self::UnsupportedContentType => f.write_str("unsupported MCP response content type"),
            Self::InvalidSessionId => f.write_str("MCP endpoint returned an invalid session id"),
            Self::SessionRequired => f.write_str("MCP endpoint requires a missing session id"),
            Self::SessionExpired => f.write_str("MCP session expired"),
            Self::ProtocolVersionMismatch => f.write_str("MCP protocol version is unsupported"),
            Self::ToolsNotSupported => f.write_str("MCP endpoint does not support tools"),
            Self::InvalidProtocol => f.write_str("MCP response did not match the protocol"),
            Self::ServerRpc(code) => write!(f, "MCP endpoint returned JSON-RPC error {code}"),
            Self::PaginationLoop => f.write_str("MCP tools pagination cursor repeated"),
            Self::PaginationLimit => f.write_str("MCP tools pagination limit exceeded"),
            Self::ToolLimit => f.write_str("MCP tools count exceeded the limit"),
            Self::Closed => f.write_str("MCP client is closed"),
        }
    }
}

impl std::error::Error for McpError {}

/// Tool metadata only. There is deliberately no tool invocation API.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tool {
    pub name: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub input_schema: Value,
    #[serde(default)]
    pub output_schema: Option<Value>,
    #[serde(default)]
    pub annotations: Option<Value>,
    #[serde(default)]
    pub execution: Option<Value>,
}

/// An initialized MCP session. Optional server session IDs are held in memory only.
pub struct McpClient {
    http: Client,
    endpoint: Url,
    protocol_version: String,
    session_id: Option<header::HeaderValue>,
    timeout: Duration,
    next_id: u64,
    closed: bool,
}

impl McpClient {
    pub async fn connect(config: ClientConfig) -> Result<Self, McpError> {
        if config.request_timeout.is_zero() || config.request_timeout > MAX_TIMEOUT {
            return Err(McpError::InvalidTimeout);
        }
        let endpoint = validate_endpoint(&config.endpoint)?;
        let mut headers = header::HeaderMap::new();
        if let Some(authorization) = config.authorization {
            headers.insert(header::AUTHORIZATION, authorization);
        }
        let http = Client::builder()
            .no_proxy()
            .default_headers(headers)
            .resolve("localhost", std::net::SocketAddr::from(([127, 0, 0, 1], 0)))
            .redirect(Policy::none())
            .timeout(config.request_timeout)
            .build()
            .map_err(|_| McpError::Transport)?;
        let mut client = Self {
            http,
            endpoint,
            protocol_version: String::new(),
            session_id: None,
            timeout: config.request_timeout,
            next_id: 1,
            closed: false,
        };
        if let Err(error) = client
            .initialize(Instant::now() + config.request_timeout)
            .await
        {
            let _ = client.close().await;
            return Err(error);
        }
        Ok(client)
    }

    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    pub fn has_server_session(&self) -> bool {
        self.session_id.is_some()
    }

    /// Fetch all tools/list pages under a single deadline and bounded page/item limits.
    pub async fn list_tools(&mut self) -> Result<Vec<Tool>, McpError> {
        self.ensure_open()?;
        let deadline = Instant::now() + self.timeout;
        for generation in 0..2 {
            match self.list_tools_generation(deadline).await {
                Err(McpError::SessionExpired) if generation == 0 => {
                    self.session_id = None;
                    self.initialize(deadline).await?;
                }
                result => return result,
            }
        }
        Err(McpError::SessionExpired)
    }

    /// Explicitly end an optional session. HTTP 405 and expired sessions are accepted.
    pub async fn close(&mut self) -> Result<(), McpError> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        let Some(session_id) = self.session_id.take() else {
            return Ok(());
        };
        let response = timeout_at(
            Instant::now() + self.timeout,
            self.http
                .delete(self.endpoint.clone())
                .header("MCP-Session-Id", session_id)
                .header("MCP-Protocol-Version", &self.protocol_version)
                .send(),
        )
        .await
        .map_err(|_| McpError::Timeout)?
        .map_err(map_transport_error)?;
        if response.status().is_redirection() {
            return Err(McpError::Redirect);
        }
        if response.status().is_success()
            || response.status() == StatusCode::METHOD_NOT_ALLOWED
            || response.status() == StatusCode::NOT_FOUND
        {
            return Ok(());
        }
        Err(McpError::HttpStatus(response.status().as_u16()))
    }

    async fn initialize(&mut self, deadline: Instant) -> Result<(), McpError> {
        let id = self.take_id()?;
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": LATEST_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": { "name": "uebox-mcp-client", "version": env!("CARGO_PKG_VERSION") }
            }
        });
        let response = send_request(
            &self.http,
            &self.endpoint,
            None,
            None,
            &request,
            id,
            deadline,
        )
        .await?;
        let result = required_result(response.message)?;
        let version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or(McpError::InvalidProtocol)?;
        if !SUPPORTED_VERSIONS.contains(&version) {
            return Err(McpError::ProtocolVersionMismatch);
        }
        if result
            .get("capabilities")
            .and_then(|caps| caps.get("tools"))
            .is_none()
            || !result.get("serverInfo").is_some_and(Value::is_object)
        {
            return Err(
                if result
                    .get("capabilities")
                    .and_then(|caps| caps.get("tools"))
                    .is_none()
                {
                    McpError::ToolsNotSupported
                } else {
                    McpError::InvalidProtocol
                },
            );
        }
        self.protocol_version = version.to_owned();
        self.session_id = response.session_id;
        self.send_notification(
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
            deadline,
        )
        .await
    }

    async fn list_tools_generation(&mut self, deadline: Instant) -> Result<Vec<Tool>, McpError> {
        let mut tools = Vec::new();
        let mut total_tools_bytes = 0usize;
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();
        for _ in 0..MAX_PAGES {
            let id = self.take_id()?;
            let mut params = serde_json::Map::new();
            if let Some(value) = cursor.as_ref() {
                params.insert("cursor".to_owned(), Value::String(value.clone()));
            }
            let request = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/list",
                "params": params
            });
            let response = timeout_at(
                deadline,
                send_request(
                    &self.http,
                    &self.endpoint,
                    Some(&self.protocol_version),
                    self.session_id.as_ref(),
                    &request,
                    id,
                    deadline,
                ),
            )
            .await
            .map_err(|_| McpError::Timeout)??;
            let page: ToolsPage = serde_json::from_value(required_result(response.message)?)
                .map_err(|_| McpError::InvalidProtocol)?;
            if tools.len().saturating_add(page.tools.len()) > MAX_TOOLS {
                return Err(McpError::ToolLimit);
            }
            if page.tools.iter().any(|tool| {
                tool.name.is_empty() || tool.name.len() > 256 || !tool.input_schema.is_object()
            }) {
                return Err(McpError::InvalidProtocol);
            }
            let page_bytes =
                serde_json::to_vec(&page.tools).map_err(|_| McpError::InvalidProtocol)?;
            total_tools_bytes = total_tools_bytes.saturating_add(page_bytes.len());
            if total_tools_bytes > MAX_TOTAL_TOOLS_BYTES {
                return Err(McpError::ResponseTooLarge);
            }
            tools.extend(page.tools);
            match page.next_cursor {
                Some(next) => {
                    if next.is_empty() || next.len() > MAX_CURSOR_BYTES {
                        return Err(McpError::InvalidProtocol);
                    }
                    if !seen.insert(next.clone()) {
                        return Err(McpError::PaginationLoop);
                    }
                    cursor = Some(next);
                }
                None => return Ok(tools),
            }
        }
        Err(McpError::PaginationLimit)
    }

    async fn send_notification(&self, message: Value, deadline: Instant) -> Result<(), McpError> {
        self.ensure_open()?;
        let body = bounded_json(&message)?;
        let request = common_headers(
            self.http.post(self.endpoint.clone()),
            Some(&self.protocol_version),
            self.session_id.as_ref(),
        );
        let response = timeout_at(deadline, request.body(body).send())
            .await
            .map_err(|_| McpError::Timeout)?
            .map_err(map_transport_error)?;
        if response.status().is_redirection() {
            return Err(McpError::Redirect);
        }
        if response.status() != StatusCode::ACCEPTED {
            return Err(McpError::HttpStatus(response.status().as_u16()));
        }
        if !read_bounded(response, deadline).await?.is_empty() {
            return Err(McpError::InvalidProtocol);
        }
        Ok(())
    }

    fn ensure_open(&self) -> Result<(), McpError> {
        if self.closed {
            Err(McpError::Closed)
        } else {
            Ok(())
        }
    }

    fn take_id(&mut self) -> Result<u64, McpError> {
        let id = self.next_id;
        self.next_id = self
            .next_id
            .checked_add(1)
            .ok_or(McpError::InvalidProtocol)?;
        Ok(id)
    }
}

fn validate_endpoint(raw: &str) -> Result<Url, McpError> {
    if raw.len() > 2048 || raw.chars().any(char::is_control) {
        return Err(McpError::InvalidEndpoint);
    }
    let endpoint = Url::parse(raw).map_err(|_| McpError::InvalidEndpoint)?;
    if !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(McpError::InvalidEndpoint);
    }
    match endpoint.scheme() {
        "https" => {}
        "http" if endpoint.host_str().is_some_and(is_loopback_host) => {}
        _ => return Err(McpError::InvalidEndpoint),
    }
    Ok(endpoint)
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(host)
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn bounded_json(value: &Value) -> Result<Vec<u8>, McpError> {
    let body = serde_json::to_vec(value).map_err(|_| McpError::InvalidProtocol)?;
    if body.len() > MAX_REQUEST_BYTES {
        return Err(McpError::RequestTooLarge);
    }
    Ok(body)
}

fn common_headers(
    request: RequestBuilder,
    version: Option<&str>,
    session: Option<&header::HeaderValue>,
) -> RequestBuilder {
    let mut request = request
        .header(header::ACCEPT, "application/json, text/event-stream")
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(version) = version {
        request = request.header("MCP-Protocol-Version", version);
    }
    if let Some(session) = session {
        request = request.header("MCP-Session-Id", session.clone());
    }
    request
}

async fn send_request(
    http: &Client,
    endpoint: &Url,
    version: Option<&str>,
    session: Option<&header::HeaderValue>,
    message: &Value,
    expected_id: u64,
    deadline: Instant,
) -> Result<PostedMessage, McpError> {
    let body = bounded_json(message)?;
    let request = common_headers(http.post(endpoint.clone()), version, session);
    let response = timeout_at(deadline, request.body(body).send())
        .await
        .map_err(|_| McpError::Timeout)?
        .map_err(map_transport_error)?;
    if response.status().is_redirection() {
        return Err(McpError::Redirect);
    }
    if response.status() == StatusCode::BAD_REQUEST && session.is_none() && version.is_some() {
        return Err(McpError::SessionRequired);
    }
    if response.status() == StatusCode::NOT_FOUND && session.is_some() {
        return Err(McpError::SessionExpired);
    }
    if response.status() != StatusCode::OK {
        return Err(McpError::HttpStatus(response.status().as_u16()));
    }
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(str::trim)
        .ok_or(McpError::UnsupportedContentType)?
        .to_ascii_lowercase();
    let mut session_values = response.headers().get_all("MCP-Session-Id").iter();
    let session_header = session_values.next().cloned();
    if session_values.next().is_some() {
        return Err(McpError::InvalidSessionId);
    }
    let session_id = validate_session_id(session_header)?;
    let value = match content_type.as_str() {
        "application/json" => {
            let body = read_bounded(response, deadline).await?;
            serde_json::from_slice(&body).map_err(|_| McpError::InvalidProtocol)?
        }
        "text/event-stream" => read_sse_response(response, expected_id, deadline).await?,
        _ => return Err(McpError::UnsupportedContentType),
    };
    let message: WireResponse =
        serde_json::from_value(value).map_err(|_| McpError::InvalidProtocol)?;
    if message.jsonrpc != "2.0" || message.id != expected_id {
        return Err(McpError::InvalidProtocol);
    }
    Ok(PostedMessage {
        message,
        session_id,
    })
}

async fn read_bounded(mut response: Response, deadline: Instant) -> Result<Vec<u8>, McpError> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
    {
        return Err(McpError::ResponseTooLarge);
    }
    let mut body = Vec::new();
    loop {
        let chunk = timeout_at(deadline, response.chunk())
            .await
            .map_err(|_| McpError::Timeout)?
            .map_err(|_| McpError::Transport)?;
        let Some(chunk) = chunk else { break };
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(McpError::ResponseTooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn validate_session_id(
    id: Option<header::HeaderValue>,
) -> Result<Option<header::HeaderValue>, McpError> {
    let Some(id) = id else { return Ok(None) };
    let bytes = id.as_bytes();
    if bytes.is_empty() || bytes.iter().any(|b| !(0x21..=0x7e).contains(b)) {
        return Err(McpError::InvalidSessionId);
    }
    Ok(Some(id))
}

async fn read_sse_response(
    mut response: Response,
    expected_id: u64,
    deadline: Instant,
) -> Result<Value, McpError> {
    if response
        .content_length()
        .is_some_and(|n| n > MAX_RESPONSE_BYTES as u64)
    {
        return Err(McpError::ResponseTooLarge);
    }
    let mut decoder = SseDecoder::default();
    loop {
        let chunk = timeout_at(deadline, response.chunk())
            .await
            .map_err(|_| McpError::Timeout)?
            .map_err(map_transport_error)?;
        let Some(chunk) = chunk else {
            return decoder.finish(expected_id);
        };
        if let Some(value) = decoder.push_chunk(&chunk, expected_id)? {
            return Ok(value);
        }
    }
}

#[derive(Default)]
struct SseDecoder {
    total_bytes: usize,
    pending_line: Vec<u8>,
    event_data: Vec<u8>,
    events: usize,
}

impl SseDecoder {
    fn push_chunk(&mut self, chunk: &[u8], expected_id: u64) -> Result<Option<Value>, McpError> {
        self.total_bytes = self.total_bytes.saturating_add(chunk.len());
        if self.total_bytes > MAX_RESPONSE_BYTES {
            return Err(McpError::ResponseTooLarge);
        }
        for byte in chunk {
            if *byte == b'\n' {
                let mut line = std::mem::take(&mut self.pending_line);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if let Some(value) = self.consume_line(&line, expected_id)? {
                    return Ok(Some(value));
                }
            } else {
                self.pending_line.push(*byte);
            }
        }
        Ok(None)
    }

    fn finish(mut self, expected_id: u64) -> Result<Value, McpError> {
        if !self.pending_line.is_empty() {
            let line = std::mem::take(&mut self.pending_line);
            if let Some(value) = self.consume_line(&line, expected_id)? {
                return Ok(value);
            }
        }
        if !self.event_data.is_empty() {
            if let Some(value) = self.dispatch_event(expected_id)? {
                return Ok(value);
            }
        }
        Err(McpError::InvalidProtocol)
    }

    fn consume_line(&mut self, line: &[u8], expected_id: u64) -> Result<Option<Value>, McpError> {
        if line.is_empty() {
            return self.dispatch_event(expected_id);
        }
        if let Some(data) = line.strip_prefix(b"data:") {
            let data = data.strip_prefix(b" ").unwrap_or(data);
            self.event_data.extend_from_slice(data);
            self.event_data.push(b'\n');
        }
        Ok(None)
    }

    fn dispatch_event(&mut self, expected_id: u64) -> Result<Option<Value>, McpError> {
        if self.event_data.is_empty() {
            return Ok(None);
        }
        self.events += 1;
        if self.events > MAX_SSE_EVENTS {
            return Err(McpError::InvalidProtocol);
        }
        self.event_data.pop();
        if self.event_data.is_empty() {
            return Ok(None);
        }
        let value: Value =
            serde_json::from_slice(&self.event_data).map_err(|_| McpError::InvalidProtocol)?;
        self.event_data.clear();
        if value.get("id").and_then(Value::as_u64) == Some(expected_id) {
            return Ok(Some(value));
        }
        if value.get("method").is_some()
            && value.get("id").is_some()
            && !value
                .get("method")
                .and_then(Value::as_str)
                .is_some_and(|method| method.starts_with("notifications/"))
        {
            return Err(McpError::InvalidProtocol);
        }
        Ok(None)
    }
}

#[cfg(test)]
fn parse_sse_response(body: &[u8], expected_id: u64) -> Result<Value, McpError> {
    let mut decoder = SseDecoder::default();
    if let Some(value) = decoder.push_chunk(body, expected_id)? {
        return Ok(value);
    }
    decoder.finish(expected_id)
}

fn required_result(response: WireResponse) -> Result<Value, McpError> {
    match (response.result, response.error) {
        (Some(result), None) => Ok(result),
        (None, Some(error)) => Err(McpError::ServerRpc(error.code)),
        _ => Err(McpError::InvalidProtocol),
    }
}

fn map_transport_error(error: reqwest::Error) -> McpError {
    if error.is_timeout() {
        McpError::Timeout
    } else {
        McpError::Transport
    }
}

struct PostedMessage {
    message: WireResponse,
    session_id: Option<header::HeaderValue>,
}

#[derive(Debug, Deserialize)]
struct ToolsPage {
    tools: Vec<Tool>,
    #[serde(default, rename = "nextCursor")]
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireResponse {
    jsonrpc: String,
    id: u64,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<WireError>,
}

#[derive(Debug, Deserialize)]
struct WireError {
    code: i64,
}

#[cfg(test)]
mod tests {
    use super::{parse_sse_response, validate_endpoint, McpError, SseDecoder};

    #[test]
    fn endpoint_policy_allows_https_and_loopback_http_only() {
        assert!(validate_endpoint("https://mcp.example.test/mcp").is_ok());
        assert!(validate_endpoint("http://127.0.0.1:9000/mcp").is_ok());
        assert!(validate_endpoint("http://[::1]:9000/mcp").is_ok());
        assert!(validate_endpoint("http://localhost:9000/mcp").is_ok());
        assert_eq!(
            validate_endpoint("http://192.0.2.1/mcp"),
            Err(McpError::InvalidEndpoint)
        );
    }

    #[test]
    fn endpoint_policy_rejects_credentials_query_fragment_and_other_schemes() {
        for endpoint in [
            "https://user:password@example.test/mcp",
            "https://example.test/mcp?token=not-allowed",
            "https://example.test/mcp#fragment",
            "file:///tmp/mcp",
        ] {
            assert_eq!(validate_endpoint(endpoint), Err(McpError::InvalidEndpoint));
        }
    }

    #[test]
    fn sse_ignores_notifications_and_rejects_server_requests() {
        let body = b": ping\n\nid: prime\ndata:\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":7,\"result\":{}}\n\n";
        assert_eq!(parse_sse_response(body, 7).unwrap()["id"], 7);
        let request =
            b"data: {\"jsonrpc\":\"2.0\",\"id\":99,\"method\":\"sampling/createMessage\"}\n\n";
        assert_eq!(
            parse_sse_response(request, 7),
            Err(McpError::InvalidProtocol)
        );
    }

    #[test]
    fn sse_decoder_returns_matching_response_before_stream_eof() {
        let mut decoder = SseDecoder::default();
        let chunk =
            b"event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":9,\"result\":{\"ready\":true}}\n\n";
        let response = decoder.push_chunk(chunk, 9).unwrap();
        assert_eq!(response.unwrap()["result"]["ready"], true);
    }

    #[test]
    fn session_ids_must_be_visible_ascii_and_sse_is_bounded() {
        assert_eq!(
            super::validate_session_id(Some(reqwest::header::HeaderValue::from_static(
                "session id"
            ))),
            Err(McpError::InvalidSessionId)
        );
        let mut decoder = SseDecoder::default();
        assert_eq!(
            decoder.push_chunk(&vec![b'x'; super::MAX_RESPONSE_BYTES + 1], 1),
            Err(McpError::ResponseTooLarge)
        );
    }
}

use std::{collections::HashMap, net::SocketAddr};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use uebox_mcp_client::{ClientConfig, McpClient, McpError, LATEST_PROTOCOL_VERSION};

struct Request {
    method: String,
    headers: HashMap<String, String>,
    body: Value,
}

async fn bind_fixture() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    (listener, address)
}

async fn read_request(stream: &mut TcpStream) -> Request {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let (header_end, content_length) = loop {
        let count = stream.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0, "fixture received a truncated request");
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let header_text = std::str::from_utf8(&bytes[..end]).unwrap();
            let length = header_text
                .lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                .unwrap_or(0);
            break (end + 4, length);
        }
    };
    while bytes.len() < header_end + content_length {
        let count = stream.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0, "fixture received a truncated request body");
        bytes.extend_from_slice(&buffer[..count]);
    }
    let header_text = std::str::from_utf8(&bytes[..header_end - 4]).unwrap();
    let mut lines = header_text.lines();
    let method = lines
        .next()
        .unwrap()
        .split_whitespace()
        .next()
        .unwrap()
        .to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let body = if content_length == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&bytes[header_end..header_end + content_length]).unwrap()
    };
    Request {
        method,
        headers,
        body,
    }
}

async fn respond(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) {
    let mut response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        response.push_str(&format!("{name}: {value}\r\n"));
    }
    response.push_str("\r\n");
    stream.write_all(response.as_bytes()).await.unwrap();
    stream.write_all(body).await.unwrap();
}

async fn accept(listener: &TcpListener) -> (TcpStream, Request) {
    let (mut stream, _) = listener.accept().await.unwrap();
    let request = read_request(&mut stream).await;
    (stream, request)
}

fn json_reply(id: u64, result: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({ "jsonrpc": "2.0", "id": id, "result": result })).unwrap()
}

fn require_error<T>(result: Result<T, McpError>) -> McpError {
    match result {
        Err(error) => error,
        Ok(_) => panic!("expected the MCP operation to fail"),
    }
}

#[tokio::test]
async fn initializes_checks_version_sends_memory_session_and_lists_sse_pages() {
    let negotiated = "2025-06-18";
    let (listener, address) = bind_fixture().await;
    let server = tokio::spawn(async move {
        let (mut stream, init) = accept(&listener).await;
        assert_eq!(init.method, "POST");
        assert_eq!(init.body["method"], "initialize");
        assert_eq!(
            init.body["params"]["protocolVersion"],
            LATEST_PROTOCOL_VERSION
        );
        assert_eq!(
            init.headers["accept"],
            "application/json, text/event-stream"
        );
        respond(
            &mut stream,
            "200 OK",
            "application/json",
            &[("MCP-Session-Id", "fixture-session")],
            &json_reply(
                1,
                json!({
                    "protocolVersion": negotiated,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "fixture", "version": "1" }
                }),
            ),
        )
        .await;

        let (mut stream, initialized) = accept(&listener).await;
        assert_eq!(initialized.body["method"], "notifications/initialized");
        assert_eq!(initialized.headers["mcp-session-id"], "fixture-session");
        assert_eq!(initialized.headers["mcp-protocol-version"], negotiated);
        respond(&mut stream, "202 Accepted", "application/json", &[], b"").await;

        let (mut stream, first_page) = accept(&listener).await;
        assert_eq!(first_page.body["method"], "tools/list");
        assert_eq!(first_page.headers["mcp-session-id"], "fixture-session");
        assert!(first_page.body["params"].get("cursor").is_none());
        let first = json_reply(
            2,
            json!({
                "tools": [{
                    "name": "read_first",
                    "description": "fixture tool",
                    "inputSchema": { "type": "object" }
                }],
                "nextCursor": "page-two"
            }),
        );
        respond(&mut stream, "200 OK", "application/json", &[], &first).await;

        let (mut stream, second_page) = accept(&listener).await;
        assert_eq!(second_page.body["params"]["cursor"], "page-two");
        let sse = format!(
            "event: message\ndata: {}\n\n",
            String::from_utf8(json_reply(
                3,
                json!({
                    "tools": [{
                        "name": "read_second",
                        "inputSchema": { "type": "object" }
                    }]
                })
            ))
            .unwrap()
        );
        respond(
            &mut stream,
            "200 OK",
            "text/event-stream; charset=utf-8",
            &[],
            sse.as_bytes(),
        )
        .await;

        let (mut stream, close) = accept(&listener).await;
        assert_eq!(close.method, "DELETE");
        assert_eq!(close.headers["mcp-protocol-version"], negotiated);
        assert_eq!(close.headers["mcp-session-id"], "fixture-session");
        respond(
            &mut stream,
            "405 Method Not Allowed",
            "application/json",
            &[],
            b"",
        )
        .await;
    });

    let mut client = McpClient::connect(ClientConfig::new(format!("http://{address}/mcp")))
        .await
        .unwrap();
    assert_eq!(client.protocol_version(), negotiated);
    assert!(client.has_server_session());
    let tools = client.list_tools().await.unwrap();
    assert_eq!(
        tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["read_first", "read_second"]
    );
    client.close().await.unwrap();
    assert!(!client.has_server_session());
    server.await.unwrap();
}

#[tokio::test]
async fn refuses_redirect_without_following_it() {
    let (listener, address) = bind_fixture().await;
    let server = tokio::spawn(async move {
        let (mut stream, request) = accept(&listener).await;
        assert_eq!(request.body["method"], "initialize");
        respond(
            &mut stream,
            "302 Found",
            "text/plain",
            &[("Location", "/redirect-target")],
            b"",
        )
        .await;
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(150), listener.accept())
                .await
                .is_err()
        );
    });
    let error =
        require_error(McpClient::connect(ClientConfig::new(format!("http://{address}/mcp"))).await);
    assert_eq!(error, McpError::Redirect);
    server.await.unwrap();
}

#[tokio::test]
async fn rejects_unknown_protocol_version_and_bounded_response_before_parsing() {
    let (listener, address) = bind_fixture().await;
    let server = tokio::spawn(async move {
        let (mut stream, request) = accept(&listener).await;
        assert_eq!(request.body["method"], "initialize");
        respond(
            &mut stream,
            "200 OK",
            "application/json",
            &[],
            &json_reply(
                1,
                json!({
                    "protocolVersion": "2099-01-01",
                    "capabilities": { "tools": {} }
                }),
            ),
        )
        .await;
        let (mut stream, _) = accept(&listener).await;
        let headers = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", 1024 * 1024 + 1);
        stream.write_all(headers.as_bytes()).await.unwrap();
    });
    let error = require_error(
        McpClient::connect(ClientConfig::new(format!("http://{address}/version"))).await,
    );
    assert_eq!(error, McpError::ProtocolVersionMismatch);
    let error = require_error(
        McpClient::connect(ClientConfig::new(format!("http://{address}/large"))).await,
    );
    assert_eq!(error, McpError::ResponseTooLarge);
    server.await.unwrap();
}

#[tokio::test]
async fn expired_session_reinitializes_once_and_keeps_the_id_in_memory() {
    let (listener, address) = bind_fixture().await;
    let server = tokio::spawn(async move {
        let (mut stream, _) = accept(&listener).await;
        respond(
            &mut stream,
            "200 OK",
            "application/json",
            &[("MCP-Session-Id", "old-session")],
            &json_reply(
                1,
                json!({
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "fixture", "version": "1" }
                }),
            ),
        )
        .await;
        let (mut stream, _) = accept(&listener).await;
        respond(&mut stream, "202 Accepted", "application/json", &[], b"").await;

        let (mut stream, expired_list) = accept(&listener).await;
        assert_eq!(expired_list.headers["mcp-session-id"], "old-session");
        respond(&mut stream, "404 Not Found", "application/json", &[], b"").await;

        let (mut stream, reinitialize) = accept(&listener).await;
        assert_eq!(reinitialize.body["method"], "initialize");
        assert!(!reinitialize.headers.contains_key("mcp-session-id"));
        respond(
            &mut stream,
            "200 OK",
            "application/json",
            &[("MCP-Session-Id", "new-session")],
            &json_reply(
                3,
                json!({
                    "protocolVersion": LATEST_PROTOCOL_VERSION,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "fixture", "version": "1" }
                }),
            ),
        )
        .await;
        let (mut stream, initialized) = accept(&listener).await;
        assert_eq!(initialized.headers["mcp-session-id"], "new-session");
        respond(&mut stream, "202 Accepted", "application/json", &[], b"").await;
        let (mut stream, list_again) = accept(&listener).await;
        assert_eq!(list_again.headers["mcp-session-id"], "new-session");
        respond(
            &mut stream,
            "200 OK",
            "application/json",
            &[],
            &json_reply(4, json!({ "tools": [] })),
        )
        .await;
        let (mut stream, close) = accept(&listener).await;
        assert_eq!(close.method, "DELETE");
        assert_eq!(close.headers["mcp-session-id"], "new-session");
        respond(
            &mut stream,
            "405 Method Not Allowed",
            "application/json",
            &[],
            b"",
        )
        .await;
    });

    let mut client = McpClient::connect(ClientConfig::new(format!("http://{address}/mcp")))
        .await
        .unwrap();
    assert_eq!(client.list_tools().await.unwrap().len(), 0);
    assert!(client.has_server_session());
    client.close().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn bearer_is_on_each_protocol_request_and_never_follows_a_redirect() {
    let (listener, address) = bind_fixture().await;
    let (other, other_addr) = bind_fixture().await;
    let server = tokio::spawn(async move {
        for operation in [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "DELETE",
        ] {
            let (mut stream, request) = accept(&listener).await;
            assert_eq!(
                request.headers.get("authorization").map(String::as_str),
                Some("Bearer fixture-private")
            );
            assert!(!request.body.to_string().contains("fixture-private"));
            match operation {
                "initialize" => {
                    assert_eq!(request.body["method"], operation);
                    respond(&mut stream, "200 OK", "application/json", &[("MCP-Session-Id", "fixture")], &json_reply(1, json!({
                        "protocolVersion":LATEST_PROTOCOL_VERSION, "capabilities": {"tools": {}}, "serverInfo":{"name":"fixture","version":"1"}
                    }))).await;
                }
                "notifications/initialized" => {
                    assert_eq!(request.body["method"], operation);
                    respond(&mut stream, "202 Accepted", "application/json", &[], b"").await;
                }
                "tools/list" => {
                    assert_eq!(request.body["method"], operation);
                    respond(
                        &mut stream,
                        "302 Found",
                        "text/plain",
                        &[("Location", &format!("http://{other_addr}/mcp"))],
                        b"",
                    )
                    .await;
                }
                _ => {
                    assert_eq!(request.method, operation);
                    respond(&mut stream, "204 No Content", "text/plain", &[], b"").await;
                }
            }
        }
    });
    let config = ClientConfig::new(format!("http://{address}/mcp"))
        .with_bearer(b"fixture-private")
        .unwrap();
    let mut client = McpClient::connect(config).await.unwrap();
    assert_eq!(require_error(client.list_tools().await), McpError::Redirect);
    client.close().await.unwrap();
    server.await.unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), other.accept())
            .await
            .is_err()
    );
}

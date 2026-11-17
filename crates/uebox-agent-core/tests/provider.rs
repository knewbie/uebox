use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uebox_agent_core::{Cancellation, Message, ModelEvent, OpenAiProvider, ProviderConfig};

async fn read_request_with_headers(stream: &mut tokio::net::TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let mut buf = [0; 4096];
    let header_end;
    loop {
        let n = stream.read(&mut buf).await.unwrap();
        assert_ne!(n, 0);
        bytes.extend_from_slice(&buf[..n]);
        if let Some(pos) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            header_end = pos + 4;
            break;
        }
    }
    let headers = std::str::from_utf8(&bytes[..header_end])
        .unwrap()
        .to_string();
    let len = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap();
    while bytes.len() < header_end + len {
        let n = stream.read(&mut buf).await.unwrap();
        assert_ne!(n, 0);
        bytes.extend_from_slice(&buf[..n]);
    }
    (
        headers,
        serde_json::from_slice(&bytes[header_end..header_end + len]).unwrap(),
    )
}
async fn read_request(stream: &mut tokio::net::TcpStream) -> Value {
    read_request_with_headers(stream).await.1
}

async fn cancel_from_first_delta(
    body: &'static str,
) -> (
    Result<uebox_agent_core::AssistantMessage, uebox_agent_core::Error>,
    Vec<ModelEvent>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        // One write makes the fixture's multiple SSE events available together.
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let cancel = Cancellation::new();
    let callback_cancel = cancel.clone();
    let mut events = Vec::new();
    let result = provider
        .complete(&[Message::user("hi")], &[], &cancel, |event| {
            events.push(event);
            callback_cancel.cancel();
        })
        .await;
    server.await.unwrap();
    (result, events)
}

#[tokio::test]
async fn cancellation_in_first_callback_suppresses_later_buffered_deltas() {
    let (result, events) = cancel_from_first_delta(concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"first\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"late\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    )).await;
    assert_eq!(events, vec![ModelEvent::TextDelta("first".into())]);
    assert_eq!(result.unwrap_err(), uebox_agent_core::Error::Cancelled);
}

#[tokio::test]
async fn cancellation_in_final_callback_prevents_done_success() {
    let (result, events) = cancel_from_first_delta(concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"last\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    )).await;
    assert_eq!(events, vec![ModelEvent::TextDelta("last".into())]);
    assert_eq!(result.unwrap_err(), uebox_agent_core::Error::Cancelled);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn external_cancel_can_finish_while_an_admitted_callback_is_running() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"first\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"late\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let cancel = Cancellation::new();
    let other = cancel.clone();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let canceller = std::thread::spawn(move || {
        started_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        other.cancel();
        release_tx.send(()).unwrap();
    });
    let mut events = Vec::new();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        provider.complete(&[Message::user("hi")], &[], &cancel, |event| {
            events.push(event);
            started_tx.send(()).unwrap();
            release_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .expect("cancel must not wait for the callback");
        }),
    )
    .await
    .unwrap();
    assert_eq!(result.unwrap_err(), uebox_agent_core::Error::Cancelled);
    assert_eq!(events, vec![ModelEvent::TextDelta("first".into())]);
    canceller.join().unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn refusal_fragments_are_streamed_and_preserved_as_response_text() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":null,\"refusal\":\"I cannot \"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"refusal\":\"help with that.\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let mut events = Vec::new();
    let result = provider
        .complete(&[Message::user("hi")], &[], &Cancellation::new(), |event| {
            events.push(event)
        })
        .await
        .unwrap();
    assert_eq!(result.text, "I cannot help with that.");
    assert_eq!(
        events,
        vec![
            ModelEvent::TextDelta("I cannot ".into()),
            ModelEvent::TextDelta("help with that.".into())
        ]
    );
    server.await.unwrap();
}

#[tokio::test]
async fn empty_refusal_with_no_text_or_tools_is_a_protocol_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":null,\"refusal\":\"\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let result = provider
        .complete(&[Message::user("hi")], &[], &Cancellation::new(), |_| {})
        .await;
    assert_eq!(result.unwrap_err(), uebox_agent_core::Error::Protocol);
    server.await.unwrap();
}

#[tokio::test]
async fn empty_refusal_prefix_does_not_discard_later_nonempty_fragments() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"refusal\":\"\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"refusal\":\"不\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"refusal\":\"能\"},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let mut events = Vec::new();
    let response = provider
        .complete(&[Message::user("hi")], &[], &Cancellation::new(), |event| {
            events.push(event)
        })
        .await
        .unwrap();
    assert_eq!(response.text, "不能");
    assert_eq!(
        events,
        vec![
            ModelEvent::TextDelta("不".into()),
            ModelEvent::TextDelta("能".into())
        ]
    );
    server.await.unwrap();
}

#[tokio::test]
async fn cancellation_from_refusal_delta_suppresses_later_fragments() {
    let (result, events) = cancel_from_first_delta(concat!(
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"refusal\":\"first\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"index\":0,\"delta\":{\"refusal\":\"late\"},\"finish_reason\":\"stop\"}]}\n\n",
        "data: [DONE]\n\n"
    )).await;
    assert_eq!(events, vec![ModelEvent::TextDelta("first".into())]);
    assert_eq!(result.unwrap_err(), uebox_agent_core::Error::Cancelled);
}

#[tokio::test]
async fn a_refusal_mixed_with_tool_calls_is_not_returned_for_dispatch() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"refusal\":\"I cannot\",\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"type\":\"function\",\"function\":{\"name\":\"read_project\",\"arguments\":\"{}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    let result = provider
        .complete(&[Message::user("hi")], &[], &Cancellation::new(), |_| {})
        .await;
    assert_eq!(result.unwrap_err(), uebox_agent_core::Error::Protocol);
    server.await.unwrap();
}

#[tokio::test]
async fn explicit_auth_and_output_limit_are_sent_without_echoing_the_credential() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (headers, body) = read_request_with_headers(&mut stream).await;
        assert!(headers
            .lines()
            .any(|line| line.eq_ignore_ascii_case("authorization: Bearer fixture-only-token")));
        assert!(headers
            .lines()
            .any(|line| line.eq_ignore_ascii_case("x-provider-key: fixture-header")));
        assert_eq!(body["max_tokens"], 128);
        let body = "data: {\"error\":{\"message\":\"fixture-only-token\"}}\n\n";
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let config = ProviderConfig::new(
        format!("http://{address}/v1/chat/completions"),
        "test-model",
    )
    .with_bearer_token("fixture-only-token")
    .with_header("x-provider-key", "fixture-header")
    .with_max_output_tokens(128);
    let error = OpenAiProvider::new(config)
        .unwrap()
        .complete(&[Message::user("hello")], &[], &Cancellation::new(), |_| {})
        .await
        .unwrap_err();
    assert!(!format!("{error:?} {error}").contains("fixture-only-token"));
    assert_eq!(error, uebox_agent_core::Error::Provider);
    server.await.unwrap();
}

#[test]
fn custom_headers_cannot_override_routing_or_authorization() {
    for name in [
        "authorization",
        "host",
        "content-length",
        "transfer-encoding",
    ] {
        let config = ProviderConfig::new("http://127.0.0.1:1/completions", "model")
            .with_header(name, "fixture");
        assert!(matches!(
            OpenAiProvider::new(config),
            Err(uebox_agent_core::Error::InvalidConfig)
        ));
    }
}

#[tokio::test]
async fn credential_supplier_is_consulted_for_every_model_request() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for index in 1..=2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (headers, _) = read_request_with_headers(&mut stream).await;
            assert!(headers
                .lines()
                .any(|line| line
                    .eq_ignore_ascii_case(&format!("authorization: Bearer fixture-{index}"))));
            let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    let counter = Arc::new(AtomicUsize::new(0));
    let config = ProviderConfig::new(format!("http://{address}/completions"), "model")
        .with_token_provider(move || {
            Ok(Some(format!(
                "fixture-{}",
                counter.fetch_add(1, Ordering::SeqCst) + 1
            )))
        });
    let provider = OpenAiProvider::new(config).unwrap();
    for _ in 0..2 {
        provider
            .complete(&[Message::user("hi")], &[], &Cancellation::new(), |_| {})
            .await
            .unwrap();
    }
    server.await.unwrap();
}

#[tokio::test]
async fn unavailable_credential_fails_before_any_request() {
    let config = ProviderConfig::new("http://127.0.0.1:1/completions", "model")
        .with_token_provider(|| Err(uebox_agent_core::Error::Network));
    let error = OpenAiProvider::new(config)
        .unwrap()
        .complete(&[Message::user("hi")], &[], &Cancellation::new(), |_| {})
        .await
        .unwrap_err();
    assert_eq!(error, uebox_agent_core::Error::CredentialUnavailable);
}

#[tokio::test]
async fn redirects_are_not_followed_with_credentials() {
    let source = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let source_address = source.local_addr().unwrap();
    let destination_address = destination.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = source.accept().await.unwrap();
        read_request(&mut stream).await;
        stream.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://{destination_address}/other\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    });
    let config = ProviderConfig::new(format!("http://{source_address}/completions"), "model")
        .with_bearer_token("fixture-only-token");
    let error = OpenAiProvider::new(config)
        .unwrap()
        .complete(&[Message::user("hi")], &[], &Cancellation::new(), |_| {})
        .await
        .unwrap_err();
    assert_eq!(error, uebox_agent_core::Error::HttpStatus);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), destination.accept())
            .await
            .is_err()
    );
    server.await.unwrap();
}

#[tokio::test]
async fn request_over_explicit_byte_budget_is_rejected_before_network() {
    let config =
        ProviderConfig::new("http://127.0.0.1:1/completions", "model").with_max_request_bytes(16);
    let error = OpenAiProvider::new(config)
        .unwrap()
        .complete(
            &[Message::user("a longer message")],
            &[],
            &Cancellation::new(),
            |_| {},
        )
        .await
        .unwrap_err();
    assert_eq!(error, uebox_agent_core::Error::ContextLimit);
}

#[tokio::test]
async fn multiline_sse_data_fields_are_reassembled_as_one_event() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        let body = "data: {\"choices\":\r\ndata: [{\"index\":0,\"delta\":{\"content\":\"ok\"},\r\ndata: \"finish_reason\":\"stop\"}]}\r\n\r\ndata: [DONE]\r\n\r\n";
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/completions"),
        "model",
    ))
    .unwrap();
    assert_eq!(
        provider
            .complete(&[Message::user("hi")], &[], &Cancellation::new(), |_| {})
            .await
            .unwrap()
            .text,
        "ok"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn malformed_or_unfinished_stream_and_http_errors_are_typed() {
    let cases: Vec<(u16, Vec<u8>, uebox_agent_core::Error)> = vec![
        (
            200,
            b"data: [DONE]\n\n".to_vec(),
            uebox_agent_core::Error::Protocol,
        ),
        (
            200,
            b"data: \xff\n\n".to_vec(),
            uebox_agent_core::Error::Protocol,
        ),
        (
            200,
            b"data: {\"choices\":[{\"index\":0,\"delta\":{\"refusal\":17},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_vec(),
            uebox_agent_core::Error::Protocol,
        ),
        (
            200,
            b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"length\"}]}\n\n"
                .to_vec(),
            uebox_agent_core::Error::OutputLimit,
        ),
        (
            429,
            b"private upstream response".to_vec(),
            uebox_agent_core::Error::HttpStatus,
        ),
    ];
    for (status, body, expected) in cases {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 {status} test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            stream.write_all(&body).await.unwrap();
        });
        let provider = OpenAiProvider::new(ProviderConfig::new(
            format!("http://{address}/completions"),
            "model",
        ))
        .unwrap();
        let error = provider
            .complete(&[Message::user("hi")], &[], &Cancellation::new(), |_| {})
            .await
            .unwrap_err();
        assert_eq!(error, expected);
        assert!(!format!("{error:?} {error}").contains("private upstream response"));
        server.await.unwrap();
    }
}

#[tokio::test]
async fn provider_streams_utf8_sse_and_waits_for_done() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let (headers, request) = read_request_with_headers(&mut stream).await;
        assert!(headers
            .lines()
            .any(|line| line.eq_ignore_ascii_case("accept: text/event-stream")));
        assert_eq!(request["model"], "test-model");
        assert_eq!(request["stream"], true);
        assert_eq!(
            request["messages"],
            json!([{"role":"user", "content":"hello"}])
        );
        let body = concat!(
            ": heartbeat\r\n\r\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"你\"},\"finish_reason\":null}]}\r\n\r\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"好\"},\"finish_reason\":\"stop\"}]}\r\n\r\n",
            "data: [DONE]\r\n\r\n"
        ).as_bytes();
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
        // Deliberately split every byte, including UTF-8 code points and CRLF.
        for byte in body {
            stream.write_all(&[*byte]).await.unwrap();
            tokio::task::yield_now().await;
        }
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/v1/chat/completions"),
        "test-model",
    ))
    .unwrap();
    let mut events = Vec::new();
    let response = provider
        .complete(
            &[Message::user("hello")],
            &[],
            &Cancellation::new(),
            |event| events.push(event),
        )
        .await
        .unwrap();
    assert_eq!(response.text, "你好");
    assert_eq!(
        events,
        vec![
            ModelEvent::TextDelta("你".into()),
            ModelEvent::TextDelta("好".into())
        ]
    );
    server.await.unwrap();
}

#[tokio::test]
async fn aggregates_interleaved_tool_fragments_by_index_before_returning() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        let body = concat!(
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"call-b\",\"type\":\"function\",\"function\":{\"name\":\"scene\",\"arguments\":\"{\\\"depth\\\":\"}},{\"index\":0,\"id\":\"call-a\",\"type\":\"function\",\"function\":{\"name\":\"project_\",\"arguments\":\"{\"}}]},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-a\",\"function\":{\"name\":\"info\",\"arguments\":\"}\"}},{\"index\":1,\"id\":\"call-b\",\"function\":{\"arguments\":\"2}\"}}]},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"total_tokens\":9}}\n\n",
            "data: [DONE]\n\n"
        );
        stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).as_bytes()).await.unwrap();
    });
    let provider = OpenAiProvider::new(ProviderConfig::new(
        format!("http://{address}/v1/chat/completions"),
        "test-model",
    ))
    .unwrap();
    let response = provider
        .complete(
            &[Message::user("inspect")],
            &[],
            &Cancellation::new(),
            |_| {},
        )
        .await
        .unwrap();
    let calls = response.tool_calls;
    assert_eq!(calls.len(), 2);
    assert_eq!(
        (&*calls[0].id, &*calls[0].name, &*calls[0].arguments),
        ("call-a", "project_info", "{}")
    );
    assert_eq!(
        (&*calls[1].id, &*calls[1].name, &*calls[1].arguments),
        ("call-b", "scene", "{\"depth\":2}")
    );
    server.await.unwrap();
}

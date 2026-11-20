use std::{net::SocketAddr, time::Duration};

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
    MaybeTlsStream, WebSocketStream,
};
use uebox_engine_unreal::{BridgeError, UnrealBridge, DEFAULT_ADDR, MAX_MESSAGE_BYTES};

type Client = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn connect(addr: SocketAddr, origin: Option<&str>) -> Client {
    let mut request = format!("ws://{addr}").into_client_request().unwrap();
    if let Some(origin) = origin {
        request
            .headers_mut()
            .insert("Origin", HeaderValue::from_str(origin).unwrap());
    }
    connect_async(request).await.unwrap().0
}

async fn send(client: &mut Client, value: Value) {
    client
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

async fn announce(client: &mut Client, name: &str, path: &str, interactive: Option<bool>) {
    let mut payload = json!({"projectName": name, "projectPath": path});
    if let Some(interactive) = interactive {
        payload["interactive"] = json!(interactive);
    }
    // Real plugin's event has no time.
    send(
        client,
        json!({"ver": "1.0", "type": "evt", "method": "project.info", "payload": payload}),
    )
    .await;
}

async fn next_request(client: &mut Client) -> Value {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let message = client.next().await.unwrap().unwrap();
            if let Message::Text(text) = message {
                let value: Value = serde_json::from_str(&text).unwrap();
                if value["type"] == "req" {
                    break value;
                }
            }
        }
    })
    .await
    .expect("request did not arrive")
}

async fn wait_for_instances(bridge: &UnrealBridge, count: usize) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while bridge.instances().len() != count {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("instance count did not converge");
}

fn id(request: &Value) -> &str {
    request["id"].as_str().expect("request id")
}

#[tokio::test]
async fn project_announcement_gates_selection_and_query_uses_real_wire_format() {
    assert_eq!(DEFAULT_ADDR, "127.0.0.1:17860");
    let bridge = UnrealBridge::bind(0).await.unwrap();
    assert_eq!(bridge.local_addr().ip(), std::net::Ipv4Addr::LOCALHOST);
    let mut client = connect(bridge.local_addr(), None).await;
    assert!(bridge.instances().is_empty());
    assert!(matches!(
        bridge.select_instance("missing"),
        Err(BridgeError::UnknownInstance)
    ));

    // Unidentifiable and malformed events cannot make a socket selectable.
    send(
        &mut client,
        json!({"type":"evt","method":"project.info","payload":{}}),
    )
    .await;
    send(&mut client, json!({"type":"evt","method":"project.info","payload":{"projectName":"Broken","projectPath":"","interactive":true}})).await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(bridge.instances().is_empty());

    announce(&mut client, "Demo", "/project/demo", None).await;
    wait_for_instances(&bridge, 1).await;
    let summary = bridge.instances().pop().unwrap();
    assert_eq!(summary.project_name, "Demo");
    assert_eq!(summary.project_path, "/project/demo");
    assert!(summary.interactive);
    let selected = bridge.select_instance(&summary.connection_id).unwrap();
    let query =
        tokio::spawn(async move { selected.get_project_info(Duration::from_secs(1)).await });

    let request = next_request(&mut client).await;
    assert_eq!(request["ver"], "1.0");
    assert_eq!(request["type"], "req");
    assert_eq!(request["method"], "project.info");
    assert_eq!(request["params"], json!({}));
    assert!(!id(&request).is_empty());
    assert!(request["time"].as_u64().unwrap() > 0);

    // Real plugin response has no method and no time.
    send(
        &mut client,
        json!({"ver":"1.0","type":"res","id":id(&request),"code":200,
        "result":{"projectName":"Actual Engine Result","engineVersion":"5.5"}}),
    )
    .await;
    assert_eq!(
        query.await.unwrap().unwrap()["projectName"],
        "Actual Engine Result"
    );
    bridge.shutdown().await;
}

#[tokio::test]
async fn only_missing_or_exact_loopback_origin_is_accepted() {
    let bridge = UnrealBridge::bind(0).await.unwrap();
    let _native = connect(bridge.local_addr(), None).await;
    let _ue = connect(bridge.local_addr(), Some("http://127.0.0.1")).await;
    for origin in [
        "https://example.com",
        "http://127.0.0.1:1234",
        "http://localhost",
    ] {
        let mut request = format!("ws://{}", bridge.local_addr())
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert("Origin", HeaderValue::from_str(origin).unwrap());
        let error = connect_async(request)
            .await
            .expect_err("untrusted Origin accepted");
        match error {
            tokio_tungstenite::tungstenite::Error::Http(response) => {
                assert_eq!(response.status(), 403)
            }
            other => panic!("expected HTTP 403, got {other:?}"),
        }
    }
    bridge.shutdown().await;
}

#[tokio::test]
async fn routes_concurrent_requests_by_connection_and_id_without_cross_delivery() {
    let bridge = UnrealBridge::bind(0).await.unwrap();
    let mut a = connect(bridge.local_addr(), None).await;
    let mut b = connect(bridge.local_addr(), None).await;
    announce(&mut a, "A", "/a", Some(true)).await;
    announce(&mut b, "B", "/b", Some(false)).await;
    wait_for_instances(&bridge, 2).await;
    let instances = bridge.instances();
    let a_summary = instances.iter().find(|x| x.project_name == "A").unwrap();
    let b_summary = instances.iter().find(|x| x.project_name == "B").unwrap();
    assert!(!b_summary.interactive);
    let a_selected = bridge.select_instance(&a_summary.connection_id).unwrap();
    let b_selected = bridge.select_instance(&b_summary.connection_id).unwrap();

    let a1 = tokio::spawn({
        let s = a_selected.clone();
        async move { s.get_project_info(Duration::from_secs(2)).await }
    });
    let a2 = tokio::spawn(async move { a_selected.get_project_info(Duration::from_secs(2)).await });
    let b1 = tokio::spawn(async move { b_selected.get_project_info(Duration::from_secs(2)).await });
    let req_a1 = next_request(&mut a).await;
    let req_a2 = next_request(&mut a).await;
    let req_b = next_request(&mut b).await;
    assert_ne!(id(&req_a1), id(&req_a2));
    assert_ne!(id(&req_a1), id(&req_b));

    // A cannot answer B's id, even if it knows that id.
    send(
        &mut a,
        json!({"type":"res","id":id(&req_b),"code":200,"result":{"owner":"forged"}}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!b1.is_finished());
    send(
        &mut a,
        json!({"type":"res","id":id(&req_a2),"code":200,"result":{"owner":id(&req_a2)}}),
    )
    .await;
    send(
        &mut b,
        json!({"type":"res","id":id(&req_b),"code":200,"result":{"owner":"B"}}),
    )
    .await;
    send(
        &mut a,
        json!({"type":"res","id":id(&req_a1),"code":200,"result":{"owner":id(&req_a1)}}),
    )
    .await;

    let mut got = vec![
        a1.await.unwrap().unwrap()["owner"]
            .as_str()
            .unwrap()
            .to_owned(),
        a2.await.unwrap().unwrap()["owner"]
            .as_str()
            .unwrap()
            .to_owned(),
    ];
    got.sort();
    let mut expected = vec![id(&req_a1).to_owned(), id(&req_a2).to_owned()];
    expected.sort();
    assert_eq!(got, expected);
    assert_eq!(b1.await.unwrap().unwrap()["owner"], "B");
    bridge.shutdown().await;
}

#[tokio::test]
async fn errors_from_status_or_result_are_not_reported_as_success() {
    let bridge = UnrealBridge::bind(0).await.unwrap();
    let mut client = connect(bridge.local_addr(), None).await;
    announce(&mut client, "Demo", "/demo", None).await;
    wait_for_instances(&bridge, 1).await;
    let selected = bridge
        .select_instance(&bridge.instances()[0].connection_id)
        .unwrap();

    for (code, result) in [
        (500, json!({"message":"engine failed"})),
        (200, json!({"ok":false,"error":"not ok"})),
        (200, json!({"success":false,"message":"not successful"})),
    ] {
        let query = tokio::spawn({
            let s = selected.clone();
            async move { s.get_project_info(Duration::from_secs(1)).await }
        });
        let request = next_request(&mut client).await;
        send(
            &mut client,
            json!({"type":"res","id":id(&request),"code":code,"result":result}),
        )
        .await;
        let error = query.await.unwrap().unwrap_err();
        assert!(
            matches!(error, BridgeError::Remote { .. }),
            "unexpected: {error:?}"
        );
    }

    let query =
        tokio::spawn(async move { selected.get_project_info(Duration::from_secs(1)).await });
    let request = next_request(&mut client).await;
    send(
        &mut client,
        json!({"type":"res","id":id(&request),"code":200,"result":null}),
    )
    .await;
    assert!(matches!(
        query.await.unwrap(),
        Err(BridgeError::InvalidResponse)
    ));
    bridge.shutdown().await;
}

#[tokio::test]
async fn timeout_does_not_retry_and_late_reply_cannot_satisfy_next_query() {
    let bridge = UnrealBridge::bind(0).await.unwrap();
    let mut client = connect(bridge.local_addr(), None).await;
    announce(&mut client, "Demo", "/demo", None).await;
    wait_for_instances(&bridge, 1).await;
    let selected = bridge
        .select_instance(&bridge.instances()[0].connection_id)
        .unwrap();
    let first = tokio::spawn({
        let s = selected.clone();
        async move { s.get_project_info(Duration::from_millis(80)).await }
    });
    let old = next_request(&mut client).await;
    assert!(matches!(first.await.unwrap(), Err(BridgeError::Timeout)));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), client.next())
            .await
            .is_err(),
        "unexpected retry"
    );

    let second =
        tokio::spawn(async move { selected.get_project_info(Duration::from_secs(1)).await });
    let new = next_request(&mut client).await;
    assert_ne!(id(&old), id(&new));
    send(
        &mut client,
        json!({"type":"res","id":id(&old),"code":200,"result":{"which":"old"}}),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!second.is_finished());
    send(
        &mut client,
        json!({"type":"res","id":id(&new),"code":200,"result":{"which":"new"}}),
    )
    .await;
    assert_eq!(second.await.unwrap().unwrap()["which"], "new");
    bridge.shutdown().await;
}

#[tokio::test]
async fn disconnect_fails_pending_and_old_selection_never_follows_reconnection() {
    let bridge = UnrealBridge::bind(0).await.unwrap();
    let mut client = connect(bridge.local_addr(), None).await;
    announce(&mut client, "Demo", "/demo", None).await;
    wait_for_instances(&bridge, 1).await;
    let old_id = bridge.instances()[0].connection_id.clone();
    let selected = bridge.select_instance(&old_id).unwrap();
    let pending = tokio::spawn({
        let s = selected.clone();
        async move { s.get_project_info(Duration::from_secs(10)).await }
    });
    let _request = next_request(&mut client).await;
    client.close(None).await.unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .unwrap()
            .unwrap(),
        Err(BridgeError::Disconnected)
    ));
    wait_for_instances(&bridge, 0).await;
    assert!(matches!(
        bridge.select_instance(&old_id),
        Err(BridgeError::UnknownInstance)
    ));

    let mut replacement = connect(bridge.local_addr(), None).await;
    announce(&mut replacement, "Demo", "/demo", None).await;
    wait_for_instances(&bridge, 1).await;
    assert_ne!(bridge.instances()[0].connection_id, old_id);
    assert!(matches!(
        selected.get_project_info(Duration::from_millis(50)).await,
        Err(BridgeError::Disconnected)
    ));
    bridge.shutdown().await;
}

#[tokio::test]
async fn accepts_32_mib_message_and_disconnects_oversized_frame() {
    let bridge = UnrealBridge::bind(0).await.unwrap();
    let mut client = connect(bridge.local_addr(), None).await;
    let prefix = r#"{"type":"evt","method":"project.info","payload":{"projectName":"Big","projectPath":"/big","padding":""#;
    let suffix = r#""}}"#;
    let padding = "a".repeat(MAX_MESSAGE_BYTES - prefix.len() - suffix.len());
    let message = format!("{prefix}{padding}{suffix}");
    assert_eq!(message.len(), 32 * 1024 * 1024);
    client.send(Message::Text(message.into())).await.unwrap();
    wait_for_instances(&bridge, 1).await;
    let _ = client
        .send(Message::Text("x".repeat(MAX_MESSAGE_BYTES + 1).into()))
        .await;
    wait_for_instances(&bridge, 0).await;
    bridge.shutdown().await;
}

#[tokio::test]
async fn project_change_invalidates_selection_and_outstanding_queries() {
    let bridge = UnrealBridge::bind(0).await.unwrap();
    let mut client = connect(bridge.local_addr(), None).await;
    announce(&mut client, "First", "/first", None).await;
    wait_for_instances(&bridge, 1).await;
    let selected = bridge
        .select_instance(&bridge.instances()[0].connection_id)
        .unwrap();
    let pending = tokio::spawn({
        let s = selected.clone();
        async move { s.get_project_info(Duration::from_secs(2)).await }
    });
    let old_request = next_request(&mut client).await;
    announce(&mut client, "Second", "/second", None).await;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .unwrap()
            .unwrap(),
        Err(BridgeError::StaleSelection)
    ));
    assert!(matches!(
        selected.get_project_info(Duration::from_secs(1)).await,
        Err(BridgeError::StaleSelection)
    ));
    assert_eq!(bridge.instances()[0].project_name, "Second");

    let fresh = bridge
        .select_instance(&bridge.instances()[0].connection_id)
        .unwrap();
    let query = tokio::spawn(async move { fresh.get_project_info(Duration::from_secs(1)).await });
    let new_request = next_request(&mut client).await;
    send(
        &mut client,
        json!({"type":"res","id":id(&old_request),"code":200,"result":{"which":"old"}}),
    )
    .await;
    send(
        &mut client,
        json!({"type":"res","id":id(&new_request),"code":200,"result":{"which":"second"}}),
    )
    .await;
    assert_eq!(query.await.unwrap().unwrap()["which"], "second");
    bridge.shutdown().await;
}

#[tokio::test]
async fn shutdown_fails_pending_queries_and_releases_the_listener() {
    let bridge = std::sync::Arc::new(UnrealBridge::bind(0).await.unwrap());
    let addr = bridge.local_addr();
    let mut client = connect(addr, None).await;
    announce(&mut client, "Demo", "/demo", None).await;
    wait_for_instances(&bridge, 1).await;
    let selected = bridge
        .select_instance(&bridge.instances()[0].connection_id)
        .unwrap();
    let pending =
        tokio::spawn(async move { selected.get_project_info(Duration::from_secs(10)).await });
    let _request = next_request(&mut client).await;
    let second_owner = bridge.clone();
    tokio::join!(bridge.shutdown(), second_owner.shutdown());
    assert!(second_owner.instances().is_empty());
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .unwrap()
            .unwrap(),
        Err(BridgeError::Disconnected)
    ));
    let replacement = UnrealBridge::bind(addr.port()).await.unwrap();
    replacement.shutdown().await;
}

#[tokio::test]
async fn project_closed_removes_instance_and_fails_pending_without_waiting_for_socket_close() {
    let bridge = UnrealBridge::bind(0).await.unwrap();
    let mut client = connect(bridge.local_addr(), None).await;
    announce(&mut client, "Demo", "/demo", None).await;
    wait_for_instances(&bridge, 1).await;
    let selected = bridge
        .select_instance(&bridge.instances()[0].connection_id)
        .unwrap();
    let pending =
        tokio::spawn(async move { selected.get_project_info(Duration::from_secs(10)).await });
    let _request = next_request(&mut client).await;
    send(
        &mut client,
        json!({"type":"evt","method":"project.closed","payload":{}}),
    )
    .await;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), pending)
            .await
            .unwrap()
            .unwrap(),
        Err(BridgeError::Disconnected)
    ));
    wait_for_instances(&bridge, 0).await;
    bridge.shutdown().await;
}

#[tokio::test]
async fn restarting_bridge_never_reuses_a_previous_connection_id_in_the_process() {
    let bridge = UnrealBridge::bind(0).await.unwrap();
    let mut old_client = connect(bridge.local_addr(), None).await;
    announce(&mut old_client, "Old", "/old", None).await;
    wait_for_instances(&bridge, 1).await;
    let old_id = bridge.instances()[0].connection_id.clone();
    bridge.shutdown().await;

    let replacement = UnrealBridge::bind(0).await.unwrap();
    let mut new_client = connect(replacement.local_addr(), None).await;
    announce(&mut new_client, "New", "/new", None).await;
    wait_for_instances(&replacement, 1).await;
    assert_ne!(replacement.instances()[0].connection_id, old_id);
    assert!(matches!(
        replacement.select_instance(&old_id),
        Err(BridgeError::UnknownInstance)
    ));
    replacement.shutdown().await;
}

#[test]
fn public_types_are_usable_as_tauri_managed_state_and_serialized_replies() {
    fn assert_send_sync<T: Send + Sync>() {}
    fn assert_serialize<T: serde::Serialize>() {}
    assert_send_sync::<UnrealBridge>();
    assert_send_sync::<uebox_engine_unreal::SelectedInstance>();
    assert_serialize::<uebox_engine_unreal::InstanceSummary>();
    assert_serialize::<BridgeError>();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_list_snapshot_cannot_select_a_changed_project_on_the_same_connection() {
    let bridge = UnrealBridge::bind(0).await.unwrap();
    let mut client = connect(bridge.local_addr(), None).await;
    announce(&mut client, "First", "/first", None).await;
    wait_for_instances(&bridge, 1).await;
    let first = bridge.instances().pop().unwrap();
    let retained_id = first.connection_id.clone();
    let selected_before_change = bridge.select_instance(&retained_id).unwrap();

    announce(&mut client, "Second", "/second", None).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while bridge
            .instances()
            .first()
            .is_none_or(|item| item.project_name != "Second")
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("new identity was not announced");

    // The list item and any previously retained credential both refer to First.
    assert!(matches!(
        bridge.select_instance(&first.connection_id),
        Err(BridgeError::UnknownInstance | BridgeError::StaleSelection)
    ));
    assert!(matches!(
        bridge.select_instance(&retained_id),
        Err(BridgeError::UnknownInstance | BridgeError::StaleSelection)
    ));
    assert!(matches!(
        selected_before_change
            .get_project_info(Duration::from_secs(1))
            .await,
        Err(BridgeError::StaleSelection)
    ));

    let second = bridge.instances().pop().unwrap();
    let selected = bridge.select_instance(&second.connection_id).unwrap();
    let query =
        tokio::spawn(async move { selected.get_project_info(Duration::from_secs(1)).await });
    let request = next_request(&mut client).await;
    send(
        &mut client,
        json!({
            "type": "res", "id": id(&request), "code": 200,
            "result": {"projectName": "Second"}
        }),
    )
    .await;
    assert_eq!(query.await.unwrap().unwrap()["projectName"], "Second");

    // Even when the same project name/path reappears, the original snapshot is stale.
    announce(&mut client, "First", "/first", None).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        while bridge
            .instances()
            .first()
            .is_none_or(|item| item.project_name != "First")
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("original identity was not re-announced");
    assert!(matches!(
        bridge.select_instance(&first.connection_id),
        Err(BridgeError::UnknownInstance | BridgeError::StaleSelection)
    ));
    assert!(bridge
        .select_instance(&bridge.instances()[0].connection_id)
        .is_ok());
    bridge.shutdown().await;
}

fn short_liveness() -> uebox_engine_unreal::LivenessConfig {
    uebox_engine_unreal::LivenessConfig {
        ping_interval: Duration::from_millis(40),
        inactivity_timeout: Duration::from_millis(250),
        registration_timeout: Duration::from_millis(400),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn silent_registered_connection_expires_and_fails_pending_without_a_close_frame() {
    let bridge = UnrealBridge::bind_with_liveness(0, short_liveness())
        .await
        .unwrap();
    let mut client = connect(bridge.local_addr(), None).await;
    announce(&mut client, "Silent", "/silent", None).await;
    wait_for_instances(&bridge, 1).await;
    let old_id = bridge.instances()[0].connection_id.clone();
    let selected = bridge.select_instance(&old_id).unwrap();
    let query = tokio::spawn({
        let selected = selected.clone();
        async move { selected.get_project_info(Duration::from_secs(5)).await }
    });
    let _request = next_request(&mut client).await;
    // Keep the socket open but stop reading: it cannot receive and answer ping frames.
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), query)
            .await
            .unwrap()
            .unwrap(),
        Err(BridgeError::Disconnected)
    ));
    wait_for_instances(&bridge, 0).await;
    assert!(matches!(
        bridge.select_instance(&old_id),
        Err(BridgeError::UnknownInstance)
    ));
    assert!(matches!(
        selected.get_project_info(Duration::from_secs(1)).await,
        Err(BridgeError::Disconnected)
    ));
    bridge.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heartbeat_events_keep_a_slow_read_only_query_alive() {
    let bridge = UnrealBridge::bind_with_liveness(0, short_liveness())
        .await
        .unwrap();
    let mut client = connect(bridge.local_addr(), None).await;
    announce(&mut client, "Active", "/active", None).await;
    wait_for_instances(&bridge, 1).await;
    let selected = bridge
        .select_instance(&bridge.instances()[0].connection_id)
        .unwrap();
    let query = tokio::spawn({
        let selected = selected.clone();
        async move { selected.get_project_info(Duration::from_secs(2)).await }
    });
    let request = next_request(&mut client).await;
    // Longer than the idle limit, with actual plugin-style heartbeats.
    for _ in 0..5 {
        tokio::time::sleep(Duration::from_millis(75)).await;
        send(
            &mut client,
            json!({
                "ver":"1.0", "type":"evt", "method":"system.heartbeat", "payload":{}
            }),
        )
        .await;
    }
    assert_eq!(bridge.instances().len(), 1);
    send(
        &mut client,
        json!({
            "type":"res", "id":id(&request), "code":200, "result":{"projectName":"Active"}
        }),
    )
    .await;
    assert_eq!(query.await.unwrap().unwrap()["projectName"], "Active");
    bridge.shutdown().await;
}

#[tokio::test]
async fn responding_to_server_ping_keeps_an_identified_instance_alive() {
    let bridge = UnrealBridge::bind_with_liveness(0, short_liveness())
        .await
        .unwrap();
    let mut client = connect(bridge.local_addr(), None).await;
    announce(&mut client, "Pong", "/pong", None).await;
    wait_for_instances(&bridge, 1).await;
    let started = tokio::time::Instant::now();
    let mut pings = 0;
    tokio::time::timeout(Duration::from_secs(2), async {
        while started.elapsed() < Duration::from_millis(450) {
            match client
                .next()
                .await
                .expect("connection closed")
                .expect("socket error")
            {
                Message::Ping(payload) => {
                    pings += 1;
                    client.send(Message::Pong(payload)).await.unwrap();
                }
                Message::Close(_) => panic!("connection closed despite pong activity"),
                _ => {}
            }
        }
    })
    .await
    .expect("server did not send periodic pings");
    assert!(pings >= 3);
    assert_eq!(bridge.instances().len(), 1);
    let selected = bridge
        .select_instance(&bridge.instances()[0].connection_id)
        .unwrap();
    let query =
        tokio::spawn(async move { selected.get_project_info(Duration::from_secs(1)).await });
    let request = next_request(&mut client).await;
    send(
        &mut client,
        json!({
            "type":"res", "id":id(&request), "code":200, "result":{"projectName":"Pong"}
        }),
    )
    .await;
    assert_eq!(query.await.unwrap().unwrap()["projectName"], "Pong");
    bridge.shutdown().await;
}

#[tokio::test]
async fn unregistered_connection_has_a_deadline_even_if_it_answers_pings() {
    let bridge = UnrealBridge::bind_with_liveness(0, short_liveness())
        .await
        .unwrap();
    let mut client = connect(bridge.local_addr(), None).await;
    let mut pings = 0;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match client.next().await {
                Some(Ok(Message::Ping(payload))) => {
                    pings += 1;
                    client.send(Message::Pong(payload)).await.unwrap();
                }
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                _ => {}
            }
        }
    })
    .await
    .expect("unregistered connection was not closed");
    assert!(pings >= 1, "test must observe and answer a ping");
    assert!(bridge.instances().is_empty());
    bridge.shutdown().await;
}

#[tokio::test]
async fn one_query_timeout_does_not_disconnect_a_still_live_instance() {
    let bridge = UnrealBridge::bind_with_liveness(0, short_liveness())
        .await
        .unwrap();
    let mut client = connect(bridge.local_addr(), None).await;
    announce(&mut client, "Slow", "/slow", None).await;
    wait_for_instances(&bridge, 1).await;
    let instance = bridge.instances()[0].clone();
    let selected = bridge.select_instance(&instance.connection_id).unwrap();
    let query =
        tokio::spawn(async move { selected.get_project_info(Duration::from_millis(60)).await });
    let _request = next_request(&mut client).await;
    assert!(matches!(query.await.unwrap(), Err(BridgeError::Timeout)));
    assert_eq!(bridge.instances().len(), 1);
    assert!(bridge.select_instance(&instance.connection_id).is_ok());
    bridge.shutdown().await;
}

#[tokio::test]
async fn invalid_liveness_durations_are_rejected() {
    let bad = uebox_engine_unreal::LivenessConfig {
        ping_interval: Duration::ZERO,
        ..short_liveness()
    };
    assert!(matches!(
        UnrealBridge::bind_with_liveness(0, bad).await,
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput
    ));
    let bad = uebox_engine_unreal::LivenessConfig {
        inactivity_timeout: Duration::from_millis(20),
        ..short_liveness()
    };
    assert!(matches!(
        UnrealBridge::bind_with_liveness(0, bad).await,
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn activity_on_one_connection_does_not_keep_another_connection_alive() {
    let bridge = UnrealBridge::bind_with_liveness(0, short_liveness())
        .await
        .unwrap();
    let mut active = connect(bridge.local_addr(), None).await;
    let mut silent = connect(bridge.local_addr(), None).await;
    announce(&mut active, "Active", "/active", None).await;
    announce(&mut silent, "Silent", "/silent", None).await;
    wait_for_instances(&bridge, 2).await;
    let silent_id = bridge
        .instances()
        .iter()
        .find(|item| item.project_name == "Silent")
        .unwrap()
        .connection_id
        .clone();
    let selected = bridge.select_instance(&silent_id).unwrap();
    let pending =
        tokio::spawn(async move { selected.get_project_info(Duration::from_secs(2)).await });
    let _request = next_request(&mut silent).await;
    for _ in 0..5 {
        tokio::time::sleep(Duration::from_millis(75)).await;
        send(
            &mut active,
            json!({"type":"evt","method":"system.heartbeat","payload":{}}),
        )
        .await;
    }
    assert!(matches!(
        pending.await.unwrap(),
        Err(BridgeError::Disconnected)
    ));
    let instances = bridge.instances();
    assert_eq!(instances.len(), 1);
    assert_eq!(instances[0].project_name, "Active");
    assert!(matches!(
        bridge.select_instance(&silent_id),
        Err(BridgeError::UnknownInstance)
    ));
    bridge.shutdown().await;
}

#[tokio::test]
async fn invalid_announcement_invalidates_old_snapshot_and_requires_new_registration() {
    let bridge = UnrealBridge::bind_with_liveness(0, short_liveness())
        .await
        .unwrap();
    let mut client = connect(bridge.local_addr(), None).await;
    announce(&mut client, "Original", "/original", None).await;
    wait_for_instances(&bridge, 1).await;
    let old_id = bridge.instances()[0].connection_id.clone();

    send(
        &mut client,
        json!({"type":"evt","method":"project.info","payload":{}}),
    )
    .await;
    wait_for_instances(&bridge, 0).await;
    assert!(matches!(
        bridge.select_instance(&old_id),
        Err(BridgeError::UnknownInstance)
    ));
    announce(&mut client, "Original", "/original", None).await;
    wait_for_instances(&bridge, 1).await;
    assert_ne!(bridge.instances()[0].connection_id, old_id);
    assert!(matches!(
        bridge.select_instance(&old_id),
        Err(BridgeError::UnknownInstance)
    ));
    assert!(bridge
        .select_instance(&bridge.instances()[0].connection_id)
        .is_ok());
    bridge.shutdown().await;
}

#[test]
fn default_liveness_timings_match_the_existing_service() {
    let config = uebox_engine_unreal::LivenessConfig::default();
    assert_eq!(config.ping_interval, Duration::from_secs(15));
    assert_eq!(config.inactivity_timeout, Duration::from_secs(45));
    assert_eq!(config.registration_timeout, Duration::from_secs(45));
}

#[tokio::test]
async fn losing_a_valid_project_starts_a_fresh_registration_deadline() {
    let bridge = UnrealBridge::bind_with_liveness(0, short_liveness())
        .await
        .unwrap();
    let mut client = connect(bridge.local_addr(), None).await;
    announce(&mut client, "First", "/first", None).await;
    wait_for_instances(&bridge, 1).await;
    let old_id = bridge.instances()[0].connection_id.clone();

    // Stay registered longer than the original registration deadline.
    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(75)).await;
        send(
            &mut client,
            json!({"type":"evt","method":"system.heartbeat","payload":{}}),
        )
        .await;
    }
    assert_eq!(bridge.instances().len(), 1);

    send(
        &mut client,
        json!({"type":"evt","method":"project.info","payload":{}}),
    )
    .await;
    wait_for_instances(&bridge, 0).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    send(
        &mut client,
        json!({"type":"evt","method":"system.heartbeat","payload":{}}),
    )
    .await;
    announce(&mut client, "Second", "/second", None).await;
    wait_for_instances(&bridge, 1).await;
    assert!(matches!(
        bridge.select_instance(&old_id),
        Err(BridgeError::UnknownInstance)
    ));
    assert!(bridge
        .select_instance(&bridge.instances()[0].connection_id)
        .is_ok());
    bridge.shutdown().await;
}

#[tokio::test]
async fn unregistered_connection_expires_even_under_continuous_activity() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    let bridge = UnrealBridge::bind_with_liveness(0, short_liveness())
        .await
        .unwrap();
    let client = connect(bridge.local_addr(), None).await;
    let (mut writer, mut reader) = client.split();
    let sent = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&sent);
    let spammer = tokio::spawn(async move {
        let heartbeat = json!({"type":"evt","method":"system.heartbeat","payload":{}}).to_string();
        loop {
            if writer
                .send(Message::Text(heartbeat.clone().into()))
                .await
                .is_err()
            {
                break;
            }
            counter.fetch_add(1, Ordering::Relaxed);
        }
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while let Some(Ok(message)) = reader.next().await {
            if matches!(message, Message::Close(_)) {
                break;
            }
        }
    })
    .await
    .expect("unregistered active connection was not closed");
    assert!(sent.load(Ordering::Relaxed) > 10);
    spammer.abort();
    let _ = spammer.await;
    assert!(bridge.instances().is_empty());
    bridge.shutdown().await;
}

const ACTOR_LABEL_TICKET: &str = "0123456789abcdef0123456789abcdef";
const ACTOR_OBJECT_PATH: &str = "/Game/Test.Test:PersistentLevel.FixtureActor";
const ACTOR_LABEL_RPC: Duration = Duration::from_secs(2);

async fn prepare_label(
    client: &mut Client,
    selected: &uebox_engine_unreal::SelectedInstance,
) -> uebox_engine_unreal::PreparedActorLabel {
    let prepare = tokio::spawn({
        let selected = selected.clone();
        async move {
            selected
                .prepare_actor_label("Requested", ACTOR_LABEL_RPC)
                .await
        }
    });
    let frame = next_request(client).await;
    assert_eq!(frame["method"], "actor.prepare_label");
    assert_eq!(frame["params"], json!({"new_label":"Requested"}));
    send(
        client,
        json!({"type":"res","id":id(&frame),"code":200,"result":{
            "protocol":"actor_label_v1","status":"prepared","ticket":ACTOR_LABEL_TICKET,
            "actor_path":ACTOR_OBJECT_PATH,"old_label":"Before"
        }}),
    )
    .await;
    let preview = prepare.await.unwrap().unwrap();
    assert_eq!(preview.actor_path(), ACTOR_OBJECT_PATH);
    assert_eq!(preview.old_label(), "Before");
    assert_eq!(preview.new_label(), "Requested");
    preview
}

#[tokio::test]
async fn actor_label_commit_uses_only_guarded_params_and_reports_actual_readback() {
    use uebox_engine_unreal::ActorLabelOutcome;

    let bridge = UnrealBridge::bind(0).await.unwrap();
    let mut editor = connect(bridge.local_addr(), None).await;
    announce(&mut editor, "Fixture", "/fixture/project", Some(true)).await;
    wait_for_instances(&bridge, 1).await;
    let selected = bridge
        .select_instance(&bridge.instances()[0].connection_id)
        .unwrap();
    for (status, actual) in [
        ("applied", "Requested"),
        ("readback_mismatch", "Requested_1"),
    ] {
        let preview = prepare_label(&mut editor, &selected).await;
        let write = tokio::spawn({
            let selected = selected.clone();
            async move { selected.commit_actor_label(preview, ACTOR_LABEL_RPC).await }
        });
        let frame = next_request(&mut editor).await;
        assert_eq!(frame["method"], "actor.set_property");
        assert_eq!(
            frame["params"],
            json!({"guarded_actor_label":{"ticket":ACTOR_LABEL_TICKET}})
        );
        send(
            &mut editor,
            json!({"type":"res","id":id(&frame),"code":200,"result":{
                "protocol":"actor_label_v1","status":status,"ticket":ACTOR_LABEL_TICKET,
                "actor_path":ACTOR_OBJECT_PATH,"actual_label":actual
            }}),
        )
        .await;
        let outcome = write.await.unwrap();
        assert_eq!(
            outcome,
            if status == "applied" {
                ActorLabelOutcome::Applied {
                    actual_label: actual.into(),
                }
            } else {
                ActorLabelOutcome::ReadbackMismatch {
                    actual_label: actual.into(),
                }
            }
        );
    }
}

#[tokio::test]
async fn actor_label_preview_cannot_move_to_another_socket_or_project_generation() {
    use uebox_engine_unreal::{ActorLabelOutcome, ActorLabelRejection};

    let bridge = UnrealBridge::bind(0).await.unwrap();
    let mut first = connect(bridge.local_addr(), None).await;
    let mut second = connect(bridge.local_addr(), None).await;
    announce(&mut first, "First", "/fixture/first", Some(true)).await;
    announce(&mut second, "Second", "/fixture/second", Some(true)).await;
    wait_for_instances(&bridge, 2).await;
    let select = |name: &str| {
        let item = bridge
            .instances()
            .into_iter()
            .find(|item| item.project_name == name)
            .unwrap();
        bridge.select_instance(&item.connection_id).unwrap()
    };
    let a = select("First");
    let b = select("Second");
    let preview = prepare_label(&mut first, &a).await;
    assert_eq!(
        b.commit_actor_label(preview, ACTOR_LABEL_RPC).await,
        ActorLabelOutcome::NotExecuted(ActorLabelRejection::WrongTarget)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(60), next_request(&mut second))
            .await
            .is_err()
    );

    let preview = prepare_label(&mut first, &a).await;
    announce(&mut first, "Rotated", "/fixture/rotated", Some(true)).await;
    tokio::time::timeout(ACTOR_LABEL_RPC, a.wait_until_stale())
        .await
        .unwrap();
    assert_eq!(
        a.commit_actor_label(preview, ACTOR_LABEL_RPC).await,
        ActorLabelOutcome::NotExecuted(ActorLabelRejection::StaleSelection)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(60), next_request(&mut first))
            .await
            .is_err()
    );

    let preview = prepare_label(&mut first, &select("Rotated")).await;
    announce(&mut first, "First", "/fixture/first", Some(true)).await;
    let fresh = tokio::time::timeout(ACTOR_LABEL_RPC, async {
        loop {
            if bridge
                .instances()
                .iter()
                .any(|item| item.project_name == "First")
            {
                break select("First");
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        fresh.commit_actor_label(preview, ACTOR_LABEL_RPC).await,
        ActorLabelOutcome::NotExecuted(ActorLabelRejection::WrongTarget)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(60), next_request(&mut first))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn lost_actor_label_reply_is_unknown_and_the_bridge_never_retries() {
    use uebox_engine_unreal::ActorLabelOutcome;

    let bridge = UnrealBridge::bind(0).await.unwrap();
    let mut editor = connect(bridge.local_addr(), None).await;
    announce(&mut editor, "Fixture", "/fixture/project", Some(true)).await;
    wait_for_instances(&bridge, 1).await;
    let selected = bridge
        .select_instance(&bridge.instances()[0].connection_id)
        .unwrap();
    let preview = prepare_label(&mut editor, &selected).await;
    let write = tokio::spawn(async move {
        selected
            .commit_actor_label(preview, Duration::from_millis(180))
            .await
    });
    let frame = next_request(&mut editor).await;
    assert_eq!(frame["method"], "actor.set_property");
    assert_eq!(write.await.unwrap(), ActorLabelOutcome::OutcomeUnknown);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), next_request(&mut editor))
            .await
            .is_err()
    );
}

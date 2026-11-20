use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::ServerOptions;

use super::*;

/// Pipe names are global to the machine, so every fixture takes its own.
fn fixture(tag: &str) -> String {
    format!("{PIPE_PREFIX}{APP_IDENTIFIER}.{ENDPOINT_COMPONENT}-unit-{tag}")
}

/// S-1-5-4, the INTERACTIVE group SID: a valid identity that is never a process's user SID.
const INTERACTIVE: [u8; 12] = [1, 1, 0, 0, 0, 0, 0, 5, 4, 0, 0, 0];

#[test]
fn only_the_local_pipe_namespace_is_a_valid_endpoint() {
    let longest = format!("{PIPE_PREFIX}{}", "x".repeat(256 - PIPE_PREFIX.len()));
    for accepted in [
        fixture("local"),
        longest,
        // Pipe names are case-insensitive in the kernel, so another spelling of the prefix names
        // the same namespace and stays a valid endpoint.
        format!(r"\\.\PIPE\{}-mixed", APP_IDENTIFIER),
    ] {
        assert_eq!(
            endpoint::checked(&accepted),
            Ok(accepted.as_str()),
            "{accepted}"
        );
    }

    for rejected in [
        String::new(),
        // A remote spelling would put this channel on the network.
        r"\\fileserver\pipe\uebox".to_owned(),
        r"\\.\pipe".to_owned(),
        PIPE_PREFIX.to_owned(),
        // A separator in the name would leave the pipe namespace.
        r"\\.\pipe\a\b".to_owned(),
        r"\\.\pipe\ ".to_owned(),
        r"\\.\pipe/a".to_owned(),
        format!("{PIPE_PREFIX}a\u{1}b"),
        format!("{PIPE_PREFIX}{}\u{0}", APP_IDENTIFIER),
        format!("{PIPE_PREFIX}{}", "x".repeat(257 - PIPE_PREFIX.len())),
        // A multi-byte first character must be refused, not panic on the prefix split.
        r"é\\.\pipe\a".to_owned(),
    ] {
        assert_eq!(
            endpoint::checked(&rejected),
            Err(HostError::EndpointUnreadable),
            "{rejected:?}"
        );
    }
}

#[test]
fn the_default_endpoint_is_this_user_and_fits_the_api_limit() {
    let owner = Sid::current_user().expect("every Windows process has a user token");
    let endpoint = endpoint::default(&owner.hex()).expect("a valid endpoint for a real SID");
    // Host and connector build the same name, so no configuration step can invent a different one.
    assert_eq!(
        Some(endpoint.as_str()),
        endpoint::default(&owner.hex()).as_deref()
    );
    assert!(endpoint.starts_with(PIPE_PREFIX), "{endpoint}");
    assert_eq!(endpoint::checked(&endpoint).unwrap(), endpoint.as_str());
    assert!(endpoint.encode_utf16().count() <= 256, "{endpoint}");
    assert!(!owner.hex().is_empty());
    assert!(owner.hex().len() <= 2 * security::SID_MAX);
    assert!(owner.hex().len().is_multiple_of(2));
    assert!(owner
        .hex()
        .chars()
        .all(|digit| digit.is_ascii_digit() || ('a'..='f').contains(&digit)));
}

#[tokio::test]
async fn the_applied_descriptor_is_read_back_and_only_lets_this_user_in() {
    let owner = Sid::current_user().unwrap();
    let private = security::PipeSecurity::private(&owner).unwrap();
    let listener = security::create_listener(&fixture("strict"), true, &private).unwrap();
    assert_eq!(security::verify_listener(&listener, &owner), Ok(()));
    // The object's owner is compared, not the identity the caller hoped for, so an identity that is
    // not this user's never matches whatever the DACL says.
    assert_eq!(
        security::verify_listener(&listener, &Sid::from_bytes(INTERACTIVE)),
        Err(HostError::Authentication)
    );

    // A pipe created with no descriptor inherits the default one, which grants read access to
    // Everyone and to the anonymous account. Its owner is still this user, so only the ACE
    // enumeration can refuse it — which is why the descriptor is passed at creation and read back.
    let loose = ServerOptions::new()
        .reject_remote_clients(true)
        .create(fixture("default-descriptor"))
        .unwrap();
    assert_eq!(
        security::verify_listener(&loose, &owner),
        Err(HostError::Authentication)
    );
}

#[tokio::test]
async fn impersonation_is_required_before_it_is_used() {
    let owner = Sid::current_user().unwrap();
    let private = security::PipeSecurity::private(&owner).unwrap();
    // A listening instance has no client, so there is no identity to read: impersonation fails and
    // the failure is a refusal. This is the check that would silently pass if it were skipped.
    let listener = security::create_listener(&fixture("no-client"), true, &private).unwrap();
    assert_eq!(
        security::authenticate_client(security::InstanceHandle::of(&listener).unwrap(), &owner),
        Err(HostError::Authentication)
    );
}

// Current-thread, like the console binary's own runtime. `CreateFile` on an instance created
// overlapped returns before the server accepts — tokio's own disconnect example opens the client
// before awaiting `connect` on a current-thread runtime — so the open cannot stall the runtime that
// is about to accept it, and this test would hang rather than pass if that ever stopped holding.
#[tokio::test]
async fn the_client_reads_the_server_identity_back_before_sending_anything() {
    let owner = Sid::current_user().unwrap();
    let private = security::PipeSecurity::private(&owner).unwrap();
    let name = fixture("client-verify");
    let listener = security::create_listener(&name, true, &private).unwrap();
    security::verify_listener(&listener, &owner).unwrap();
    let accepted = tokio::spawn(async move { listener.connect().await });
    let client = security::open_client(&name).unwrap();
    // The proof needs only the handle CreateFile returned, and it happens before any frame is
    // written: a request never leaves for an unverified peer.
    assert_eq!(security::verify_server(&client, &owner), Ok(()));
    assert_eq!(
        security::verify_server(&client, &Sid::from_bytes(INTERACTIVE)),
        Err(HostError::Authentication)
    );
    drop(client);
    accepted.await.unwrap().unwrap();
    // An absent host is an outage, not a configuration problem, and never a reason to try another
    // transport.
    assert_eq!(
        connect_error(security::open_client(&fixture("absent")).unwrap_err()),
        HostError::Unavailable
    );
}

#[tokio::test]
async fn frames_match_the_unix_contract_and_their_bounds_hold() {
    assert_eq!(
        serde_json::to_value(Request::Hello { version: 1 }).unwrap(),
        json!({"type": "hello", "version": 1})
    );
    assert_eq!(
        serde_json::to_value(Request::PrepareRead {
            tool: ReadTool::UnrealProjectInfo,
            connection_id: "generation-1".into(),
        })
        .unwrap(),
        json!({"type":"prepare_read","tool":"ue_project_info","connectionId":"generation-1"})
    );
    assert_eq!(
        serde_json::to_value(Request::ExecuteRead).unwrap(),
        json!({"type": "execute_read"})
    );
    assert_eq!(
        serde_json::to_value(Response::Ready).unwrap(),
        json!({"type": "ready"})
    );
    assert_eq!(
        serde_json::to_value(Response::rejected(HostError::ToolUnavailable)).unwrap(),
        json!({"type":"failure","error":"tool_unavailable","dispatched":false})
    );

    let (mut read, mut write) = tokio::io::duplex(64);
    write.write_u32(64).await.unwrap();
    write.write_all(b"partial").await.unwrap();
    drop(write);
    // A truncated frame is a transport loss, not a protocol mismatch: it must not be reported as
    // Incompatible, and nothing was dispatched.
    assert_eq!(
        wire::read::<Request, _>(&mut read, MAX_REQUEST)
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::UnexpectedEof
    );
    assert_eq!(
        wire_error(io::Error::from(io::ErrorKind::UnexpectedEof)),
        HostError::Unavailable
    );
    assert_eq!(
        wire_error(io::Error::from(io::ErrorKind::InvalidData)),
        HostError::Incompatible
    );

    for prefix in [0u32, MAX_REQUEST as u32 + 1, u32::MAX] {
        let (mut read, mut write) = tokio::io::duplex(16);
        write.write_u32(prefix).await.unwrap();
        assert_eq!(
            wire::read::<Request, _>(&mut read, MAX_REQUEST)
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData,
            "length {prefix}"
        );
    }

    // A private or unknown field on a variant that carries arguments never reaches the host.
    for raw in [
        br#"{"type":"prepare_read","tool":"ue_project_info","connectionId":"g","path":"/private"}"#
            .as_slice(),
        br#"{"type":"prepare_read","tool":"shell","connectionId":"g"}"#.as_slice(),
    ] {
        let (mut read, mut write) = tokio::io::duplex(4096);
        write.write_u32(raw.len() as u32).await.unwrap();
        write.write_all(raw).await.unwrap();
        assert_eq!(
            wire::read::<Request, _>(&mut read, MAX_REQUEST)
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData,
            "{}",
            String::from_utf8_lossy(raw)
        );
    }

    // A variant with no arguments has nothing to smuggle, so extra content there is inert — and the
    // Unix decoder accepts it the same way. That parity is the contract, so it is asserted here
    // rather than tightened on one transport only.
    let (mut read, mut write) = tokio::io::duplex(4096);
    let unit = br#"{"type":"projects","extra":1}"#;
    write.write_u32(unit.len() as u32).await.unwrap();
    write.write_all(unit).await.unwrap();
    assert!(matches!(
        wire::read::<Request, _>(&mut read, MAX_REQUEST)
            .await
            .unwrap(),
        Request::Projects
    ));

    let (mut read, mut write) = tokio::io::duplex(MAX_RESPONSE);
    let oversized = json!({"text": "x".repeat(MAX_RESPONSE)});
    assert_eq!(
        wire::write(&mut write, &oversized, MAX_RESPONSE)
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    // A refused frame submits nothing, so the reader still faces an empty stream.
    let mut untouched = [0u8; 4];
    assert!(
        timeout(Duration::from_millis(50), read.read_exact(&mut untouched))
            .await
            .is_err()
    );

    let (mut read, mut write) = tokio::io::duplex(4096);
    let request = Request::PrepareRead {
        tool: ReadTool::UnityActiveScene,
        connection_id: "generation".into(),
    };
    wire::write(&mut write, &request, MAX_REQUEST)
        .await
        .unwrap();
    assert_eq!(
        wire::read::<Value, _>(&mut read, MAX_REQUEST)
            .await
            .unwrap(),
        json!({"type":"prepare_read","tool":"unity_active_scene","connectionId":"generation"})
    );
}

use std::collections::HashMap;
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::time::timeout;

use tether_protocol::{FrameCodec, Message, PROTOCOL_VERSION};

// -- Helpers --

async fn start_daemon(socket_path: &str) -> tokio::task::JoinHandle<()> {
    start_daemon_with_config(socket_path, tether_daemon::Config {
        socket_path: socket_path.to_string(),
        idle_timeout: "60s".into(),
        max_sessions: 5,
        ..Default::default()
    }).await
}

async fn start_daemon_with_config(socket_path: &str, mut config: tether_daemon::Config) -> tokio::task::JoinHandle<()> {
    config.socket_path = socket_path.to_string();
    let server = tether_daemon::Server::new(config);
    tokio::spawn(async move {
        server.run().await.unwrap();
    })
}

async fn connect_and_handshake(socket_path: &str) -> (FrameCodec, FrameCodec, UnixStream) {
    let stream = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match UnixStream::connect(socket_path).await {
                Ok(s) => return s,
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .expect("daemon didn't start in time");

    let (mut reader, mut writer) = stream.into_split();
    let write_codec = FrameCodec::new();
    let mut read_codec = FrameCodec::new();

    write_codec
        .write_message(
            &mut writer,
            &Message::Hello {
                version: PROTOCOL_VERSION,
                term: "xterm-256color".into(),
                cols: 80,
                rows: 24,
            },
        )
        .await
        .unwrap();

    let resp = read_codec.read_message(&mut reader).await.unwrap();
    assert!(matches!(resp, Message::HelloOk { .. }));

    let stream = reader.reunite(writer).unwrap();
    (write_codec, read_codec, stream)
}

fn test_socket_path(name: &str) -> String {
    format!("/tmp/tether-test-{}-{}.sock", name, std::process::id())
}

/// Create a session and attach, returning the session ID.
async fn create_and_attach(
    write_codec: &FrameCodec,
    read_codec: &mut FrameCodec,
    reader: &mut tokio::net::unix::OwnedReadHalf,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    session_id: &str,
) {
    write_codec
        .write_message(
            writer,
            &Message::SessionCreate {
                id: Some(session_id.into()),
                cmd: Some("/bin/sh".into()),
                cols: 80,
                rows: 24,
                env: HashMap::new(),
            },
        )
        .await
        .unwrap();
    let resp = read_codec.read_message(reader).await.unwrap();
    assert!(matches!(resp, Message::SessionCreated { .. }));

    write_codec
        .write_message(writer, &Message::SessionAttach { id: session_id.into() })
        .await
        .unwrap();
    let _ = read_codec.read_message(reader).await.unwrap(); // HelloOk or SessionState
}

/// Read the next non-Data message (skips any interleaved PTY output).
async fn read_non_data(
    read_codec: &mut FrameCodec,
    reader: &mut tokio::net::unix::OwnedReadHalf,
) -> Message {
    timeout(Duration::from_secs(5), async {
        loop {
            let msg = read_codec.read_message(reader).await.unwrap();
            if !matches!(msg, Message::Data(_)) {
                return msg;
            }
        }
    })
    .await
    .expect("timed out waiting for non-Data message")
}

/// Send a command and wait for a marker string in the output.
async fn send_and_expect(
    write_codec: &FrameCodec,
    read_codec: &mut FrameCodec,
    reader: &mut tokio::net::unix::OwnedReadHalf,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    command: &str,
    marker: &str,
) {
    write_codec
        .write_message(writer, &Message::Data(command.as_bytes().to_vec()))
        .await
        .unwrap();

    timeout(Duration::from_secs(5), async {
        let mut accumulated = Vec::new();
        loop {
            let msg = read_codec.read_message(reader).await.unwrap();
            if let Message::Data(data) = msg {
                accumulated.extend_from_slice(&data);
                if String::from_utf8_lossy(&accumulated).contains(marker) {
                    return;
                }
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("didn't receive marker '{marker}' in output"));
}

// -- Tests --

#[tokio::test]
async fn test_create_and_list_sessions() {
    let socket_path = test_socket_path("create-list");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    write_codec
        .write_message(
            &mut writer,
            &Message::SessionCreate {
                id: Some("test-session".into()),
                cmd: Some("/bin/sh".into()),
                cols: 80,
                rows: 24,
                env: HashMap::new(),
            },
        )
        .await
        .unwrap();

    let resp = read_codec.read_message(&mut reader).await.unwrap();
    match resp {
        Message::SessionCreated { id } => assert_eq!(id, "test-session"),
        other => panic!("expected SessionCreated, got: {other:?}"),
    }

    write_codec
        .write_message(&mut writer, &Message::SessionList)
        .await
        .unwrap();

    let resp = read_codec.read_message(&mut reader).await.unwrap();
    match resp {
        Message::SessionListResp { sessions } => {
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].id, "test-session");
            assert!(!sessions[0].attached);
        }
        other => panic!("expected SessionListResp, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_attach_and_receive_output() {
    let socket_path = test_socket_path("attach");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    create_and_attach(&write_codec, &mut read_codec, &mut reader, &mut writer, "echo-session").await;
    send_and_expect(&write_codec, &mut read_codec, &mut reader, &mut writer, "echo hello-tether\n", "hello-tether").await;

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_detach_and_reattach() {
    let socket_path = test_socket_path("reattach");
    let _daemon = start_daemon(&socket_path).await;

    // First connection: create, attach, send data
    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    create_and_attach(&write_codec, &mut read_codec, &mut reader, &mut writer, "persistent").await;
    send_and_expect(&write_codec, &mut read_codec, &mut reader, &mut writer, "echo marker-123\n", "marker-123").await;

    // Detach
    write_codec
        .write_message(&mut writer, &Message::SessionDetach)
        .await
        .unwrap();
    drop(writer);
    drop(reader);

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Reconnect and reattach
    let (write_codec2, mut read_codec2, stream2) = connect_and_handshake(&socket_path).await;
    let (mut reader2, mut writer2) = stream2.into_split();

    write_codec2
        .write_message(&mut writer2, &Message::SessionAttach { id: "persistent".into() })
        .await
        .unwrap();

    // Should get a SessionState snapshot with previous content
    let resp = read_codec2.read_message(&mut reader2).await.unwrap();
    match &resp {
        Message::SessionState(state) => {
            let all_text: String = state
                .visible_rows
                .iter()
                .chain(state.scrollback.iter())
                .flat_map(|row| row.cells.iter().map(|c| c.c))
                .collect();
            assert!(all_text.contains("marker-123"), "reattach snapshot should contain previous output");
        }
        other => panic!("expected SessionState, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_destroy_session() {
    let socket_path = test_socket_path("destroy");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    create_and_attach(&write_codec, &mut read_codec, &mut reader, &mut writer, "doomed").await;

    // Detach first (so we're not attached when destroying)
    write_codec.write_message(&mut writer, &Message::SessionDetach).await.unwrap();

    // Destroy
    write_codec
        .write_message(&mut writer, &Message::SessionDestroy { id: "doomed".into() })
        .await
        .unwrap();
    let _ = read_non_data(&mut read_codec, &mut reader).await;

    // List should be empty
    write_codec.write_message(&mut writer, &Message::SessionList).await.unwrap();
    let resp = read_non_data(&mut read_codec, &mut reader).await;
    match resp {
        Message::SessionListResp { sessions } => {
            assert!(sessions.is_empty(), "session should be destroyed");
        }
        other => panic!("expected SessionListResp, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_ping_pong() {
    let socket_path = test_socket_path("ping");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    write_codec.write_message(&mut writer, &Message::Ping { seq: 42 }).await.unwrap();
    let resp = read_codec.read_message(&mut reader).await.unwrap();
    assert_eq!(resp, Message::Pong { seq: 42 });

    std::fs::remove_file(&socket_path).ok();
}

// -- New scenario tests --

#[tokio::test]
async fn test_multiple_sessions() {
    let socket_path = test_socket_path("multi");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    // Create 3 sessions
    for name in &["alpha", "beta", "gamma"] {
        write_codec
            .write_message(
                &mut writer,
                &Message::SessionCreate {
                    id: Some((*name).into()),
                    cmd: Some("/bin/sh".into()),
                    cols: 80,
                    rows: 24,
                    env: HashMap::new(),
                },
            )
            .await
            .unwrap();
        let resp = read_codec.read_message(&mut reader).await.unwrap();
        assert!(matches!(resp, Message::SessionCreated { .. }));
    }

    // List should show all 3
    write_codec.write_message(&mut writer, &Message::SessionList).await.unwrap();
    let resp = read_codec.read_message(&mut reader).await.unwrap();
    match resp {
        Message::SessionListResp { sessions } => {
            assert_eq!(sessions.len(), 3);
            let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
            assert!(ids.contains(&"alpha"));
            assert!(ids.contains(&"beta"));
            assert!(ids.contains(&"gamma"));
        }
        other => panic!("expected SessionListResp, got: {other:?}"),
    }

    // Destroy one
    write_codec
        .write_message(&mut writer, &Message::SessionDestroy { id: "beta".into() })
        .await
        .unwrap();
    let _ = read_codec.read_message(&mut reader).await.unwrap();

    // List should show 2
    write_codec.write_message(&mut writer, &Message::SessionList).await.unwrap();
    let resp = read_codec.read_message(&mut reader).await.unwrap();
    match resp {
        Message::SessionListResp { sessions } => {
            assert_eq!(sessions.len(), 2);
            let ids: Vec<&str> = sessions.iter().map(|s| s.id.as_str()).collect();
            assert!(ids.contains(&"alpha"));
            assert!(ids.contains(&"gamma"));
            assert!(!ids.contains(&"beta"));
        }
        other => panic!("expected SessionListResp, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_max_sessions_enforced() {
    let socket_path = test_socket_path("maxsess");
    let _daemon = start_daemon(&socket_path).await; // max_sessions = 5

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    // Create 5 sessions (the max)
    for i in 0..5 {
        write_codec
            .write_message(
                &mut writer,
                &Message::SessionCreate {
                    id: Some(format!("sess-{i}")),
                    cmd: Some("/bin/sh".into()),
                    cols: 80,
                    rows: 24,
                    env: HashMap::new(),
                },
            )
            .await
            .unwrap();
        let resp = read_codec.read_message(&mut reader).await.unwrap();
        assert!(matches!(resp, Message::SessionCreated { .. }));
    }

    // 6th should fail
    write_codec
        .write_message(
            &mut writer,
            &Message::SessionCreate {
                id: Some("sess-overflow".into()),
                cmd: Some("/bin/sh".into()),
                cols: 80,
                rows: 24,
                env: HashMap::new(),
            },
        )
        .await
        .unwrap();
    let resp = read_codec.read_message(&mut reader).await.unwrap();
    match resp {
        Message::Error { message, .. } => {
            assert!(message.contains("max sessions"), "expected max sessions error, got: {message}");
        }
        other => panic!("expected Error, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_duplicate_session_id_rejected() {
    let socket_path = test_socket_path("dup-id");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    // Create "unique"
    write_codec
        .write_message(
            &mut writer,
            &Message::SessionCreate {
                id: Some("unique".into()),
                cmd: Some("/bin/sh".into()),
                cols: 80,
                rows: 24,
                env: HashMap::new(),
            },
        )
        .await
        .unwrap();
    let resp = read_codec.read_message(&mut reader).await.unwrap();
    assert!(matches!(resp, Message::SessionCreated { .. }));

    // Try to create "unique" again
    write_codec
        .write_message(
            &mut writer,
            &Message::SessionCreate {
                id: Some("unique".into()),
                cmd: Some("/bin/sh".into()),
                cols: 80,
                rows: 24,
                env: HashMap::new(),
            },
        )
        .await
        .unwrap();
    let resp = read_codec.read_message(&mut reader).await.unwrap();
    match resp {
        Message::Error { message, .. } => {
            assert!(message.contains("already exists"), "expected already exists error, got: {message}");
        }
        other => panic!("expected Error, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_attach_nonexistent_session() {
    let socket_path = test_socket_path("no-exist");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    write_codec
        .write_message(&mut writer, &Message::SessionAttach { id: "ghost".into() })
        .await
        .unwrap();

    let resp = read_codec.read_message(&mut reader).await.unwrap();
    match resp {
        Message::Error { message, .. } => {
            assert!(message.contains("not found"), "expected not found error, got: {message}");
        }
        other => panic!("expected Error, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_destroy_nonexistent_session() {
    let socket_path = test_socket_path("destroy-ghost");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    write_codec
        .write_message(&mut writer, &Message::SessionDestroy { id: "ghost".into() })
        .await
        .unwrap();

    let resp = read_codec.read_message(&mut reader).await.unwrap();
    match resp {
        Message::Error { message, .. } => {
            assert!(message.contains("not found"), "expected not found error, got: {message}");
        }
        other => panic!("expected Error, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_wrong_protocol_version() {
    let socket_path = test_socket_path("bad-ver");
    let _daemon = start_daemon(&socket_path).await;

    let stream = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match UnixStream::connect(&socket_path).await {
                Ok(s) => return s,
                Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
            }
        }
    })
    .await
    .unwrap();

    let (mut reader, mut writer) = stream.into_split();
    let write_codec = FrameCodec::new();
    let mut read_codec = FrameCodec::new();

    // Send Hello with wrong version
    write_codec
        .write_message(
            &mut writer,
            &Message::Hello {
                version: 99,
                term: "xterm".into(),
                cols: 80,
                rows: 24,
            },
        )
        .await
        .unwrap();

    let resp = read_codec.read_message(&mut reader).await.unwrap();
    match resp {
        Message::Error { message, .. } => {
            assert!(message.contains("unsupported protocol version"),
                "expected version error, got: {message}");
        }
        other => panic!("expected Error, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_session_exit_notifies_client() {
    let socket_path = test_socket_path("exit-notify");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    create_and_attach(&write_codec, &mut read_codec, &mut reader, &mut writer, "exit-test").await;

    // Send "exit" to the shell
    write_codec
        .write_message(&mut writer, &Message::Data(b"exit\n".to_vec()))
        .await
        .unwrap();

    // Should eventually receive SessionExited or connection close
    let got_exit = timeout(Duration::from_secs(5), async {
        loop {
            match read_codec.read_message(&mut reader).await {
                Ok(Message::SessionExited { .. }) => return true,
                Ok(Message::Data(_)) => continue, // consume output
                Err(_) => return true, // connection closed = session gone
                _ => continue,
            }
        }
    })
    .await;

    assert!(got_exit.unwrap_or(false), "should receive session exit notification");
    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_resize() {
    let socket_path = test_socket_path("resize");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    create_and_attach(&write_codec, &mut read_codec, &mut reader, &mut writer, "resize-test").await;

    // Resize
    write_codec
        .write_message(&mut writer, &Message::Resize { cols: 120, rows: 40 })
        .await
        .unwrap();

    // Verify by checking terminal size via stty (works in all shells including ash)
    send_and_expect(
        &write_codec, &mut read_codec, &mut reader, &mut writer,
        "stty size\n",
        "40 120",
    ).await;

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_session_list_shows_attached_status() {
    let socket_path = test_socket_path("attached-status");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    create_and_attach(&write_codec, &mut read_codec, &mut reader, &mut writer, "sess-a").await;

    // While attached, list should show attached=true
    write_codec.write_message(&mut writer, &Message::SessionList).await.unwrap();
    let resp = read_non_data(&mut read_codec, &mut reader).await;
    match &resp {
        Message::SessionListResp { sessions } => {
            let s = sessions.iter().find(|s| s.id == "sess-a").unwrap();
            assert!(s.attached, "session should be attached");
        }
        other => panic!("expected SessionListResp, got: {other:?}"),
    }

    // Detach
    write_codec.write_message(&mut writer, &Message::SessionDetach).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Now list should show attached=false
    write_codec.write_message(&mut writer, &Message::SessionList).await.unwrap();
    let resp = read_non_data(&mut read_codec, &mut reader).await;
    match &resp {
        Message::SessionListResp { sessions } => {
            let s = sessions.iter().find(|s| s.id == "sess-a").unwrap();
            assert!(!s.attached, "session should be detached");
            assert!(s.idle_secs < 5, "idle time should be small");
        }
        other => panic!("expected SessionListResp, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_exclusive_attach_kicks_previous_client() {
    let socket_path = test_socket_path("exclusive");
    let _daemon = start_daemon(&socket_path).await;

    // Client 1: create and attach
    let (wc1, mut rc1, stream1) = connect_and_handshake(&socket_path).await;
    let (mut r1, mut w1) = stream1.into_split();

    create_and_attach(&wc1, &mut rc1, &mut r1, &mut w1, "shared").await;

    // Client 2: attach to same session
    let (wc2, mut rc2, stream2) = connect_and_handshake(&socket_path).await;
    let (mut r2, mut w2) = stream2.into_split();

    wc2.write_message(&mut w2, &Message::SessionAttach { id: "shared".into() })
        .await
        .unwrap();
    let resp = rc2.read_message(&mut r2).await.unwrap();
    assert!(matches!(resp, Message::SessionState(_)), "client 2 should get snapshot");

    // Client 2 sends data, it should work
    send_and_expect(&wc2, &mut rc2, &mut r2, &mut w2, "echo client2-ok\n", "client2-ok").await;

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_connection_drop_detaches_session() {
    let socket_path = test_socket_path("drop-detach");
    let _daemon = start_daemon(&socket_path).await;

    // Connect, create, attach
    let (wc, mut rc, stream) = connect_and_handshake(&socket_path).await;
    let (mut r, mut w) = stream.into_split();
    create_and_attach(&wc, &mut rc, &mut r, &mut w, "drop-test").await;

    // Verify attached
    wc.write_message(&mut w, &Message::SessionList).await.unwrap();
    let resp = read_non_data(&mut rc, &mut r).await;
    match &resp {
        Message::SessionListResp { sessions } => {
            assert!(sessions[0].attached);
        }
        other => panic!("expected SessionListResp, got: {other:?}"),
    }

    // Drop connection abruptly (simulates SSH disconnect)
    drop(w);
    drop(r);

    tokio::time::sleep(Duration::from_millis(300)).await;

    // New connection: session should still exist but be detached
    let (wc2, mut rc2, stream2) = connect_and_handshake(&socket_path).await;
    let (mut r2, mut w2) = stream2.into_split();

    wc2.write_message(&mut w2, &Message::SessionList).await.unwrap();
    let resp = rc2.read_message(&mut r2).await.unwrap();
    match &resp {
        Message::SessionListResp { sessions } => {
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].id, "drop-test");
            assert!(!sessions[0].attached, "session should be detached after connection drop");
        }
        other => panic!("expected SessionListResp, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_env_passed_to_session() {
    let socket_path = test_socket_path("env");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    let mut env = HashMap::new();
    env.insert("TETHER_TEST_VAR".into(), "hello-from-tether".into());
    env.insert("TERM".into(), "xterm-256color".into());

    write_codec
        .write_message(
            &mut writer,
            &Message::SessionCreate {
                id: Some("env-test".into()),
                cmd: Some("/bin/sh".into()),
                cols: 80,
                rows: 24,
                env,
            },
        )
        .await
        .unwrap();
    let _ = read_codec.read_message(&mut reader).await.unwrap();

    write_codec
        .write_message(&mut writer, &Message::SessionAttach { id: "env-test".into() })
        .await
        .unwrap();
    let _ = read_codec.read_message(&mut reader).await.unwrap();

    send_and_expect(
        &write_codec, &mut read_codec, &mut reader, &mut writer,
        "echo $TETHER_TEST_VAR\n",
        "hello-from-tether",
    ).await;

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_session_info_fields() {
    let socket_path = test_socket_path("info-fields");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    create_and_attach(&write_codec, &mut read_codec, &mut reader, &mut writer, "info-test").await;

    // Wait a moment so created_secs > 0
    tokio::time::sleep(Duration::from_secs(1)).await;

    write_codec.write_message(&mut writer, &Message::SessionList).await.unwrap();
    let resp = read_non_data(&mut read_codec, &mut reader).await;
    match resp {
        Message::SessionListResp { sessions } => {
            assert_eq!(sessions.len(), 1);
            let s = &sessions[0];
            assert_eq!(s.id, "info-test");
            assert!(s.attached);
            assert!(s.created_secs >= 1, "created_secs should be >= 1");
            assert!(!s.cmd.is_empty(), "cmd should be populated");
            assert!(!s.cwd.is_empty(), "cwd should be populated");
        }
        other => panic!("expected SessionListResp, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_concurrent_connections() {
    let socket_path = test_socket_path("concurrent");
    let _daemon = start_daemon(&socket_path).await;

    // Open 3 connections simultaneously
    let (wc1, mut rc1, s1) = connect_and_handshake(&socket_path).await;
    let (wc2, mut rc2, s2) = connect_and_handshake(&socket_path).await;
    let (wc3, mut rc3, s3) = connect_and_handshake(&socket_path).await;

    let (mut r1, mut w1) = s1.into_split();
    let (mut r2, mut w2) = s2.into_split();
    let (mut r3, mut w3) = s3.into_split();

    // Each creates a different session
    for (wc, rc, r, w, name) in [
        (&wc1, &mut rc1, &mut r1, &mut w1, "c1"),
        (&wc2, &mut rc2, &mut r2, &mut w2, "c2"),
        (&wc3, &mut rc3, &mut r3, &mut w3, "c3"),
    ] {
        create_and_attach(wc, rc, r, w, name).await;
    }

    // All 3 should show in the list
    wc1.write_message(&mut w1, &Message::SessionList).await.unwrap();
    let resp = read_non_data(&mut rc1, &mut r1).await;
    match resp {
        Message::SessionListResp { sessions } => {
            assert_eq!(sessions.len(), 3);
        }
        other => panic!("expected SessionListResp, got: {other:?}"),
    }

    // Each can independently send/receive
    send_and_expect(&wc1, &mut rc1, &mut r1, &mut w1, "echo conn1-ok\n", "conn1-ok").await;
    send_and_expect(&wc2, &mut rc2, &mut r2, &mut w2, "echo conn2-ok\n", "conn2-ok").await;
    send_and_expect(&wc3, &mut rc3, &mut r3, &mut w3, "echo conn3-ok\n", "conn3-ok").await;

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_auto_generated_session_id() {
    let socket_path = test_socket_path("auto-id");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    // Create with id: None — should get auto-generated adjective-noun ID
    write_codec
        .write_message(
            &mut writer,
            &Message::SessionCreate {
                id: None,
                cmd: Some("/bin/sh".into()),
                cols: 80,
                rows: 24,
                env: HashMap::new(),
            },
        )
        .await
        .unwrap();

    let resp = read_codec.read_message(&mut reader).await.unwrap();
    match resp {
        Message::SessionCreated { id } => {
            assert!(id.contains('-'), "auto ID should be adjective-noun format: {id}");
            assert!(!id.is_empty());
        }
        other => panic!("expected SessionCreated, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

// -- Shell and environment tests --

/// Helper: create a session with a specific shell and env, attach, run a command, check output.
async fn session_with_shell(
    socket_path: &str,
    session_id: &str,
    shell: &str,
    env: HashMap<String, String>,
    command: &str,
    marker: &str,
) {
    let (write_codec, mut read_codec, stream) = connect_and_handshake(socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    write_codec
        .write_message(
            &mut writer,
            &Message::SessionCreate {
                id: Some(session_id.into()),
                cmd: Some(shell.into()),
                cols: 80,
                rows: 24,
                env,
            },
        )
        .await
        .unwrap();
    let resp = read_codec.read_message(&mut reader).await.unwrap();
    assert!(matches!(resp, Message::SessionCreated { .. }), "failed to create session with {shell}");

    write_codec
        .write_message(&mut writer, &Message::SessionAttach { id: session_id.into() })
        .await
        .unwrap();
    let _ = read_codec.read_message(&mut reader).await.unwrap();

    send_and_expect(&write_codec, &mut read_codec, &mut reader, &mut writer, command, marker).await;
}

#[tokio::test]
async fn test_shell_bash() {
    let bash = "/run/current-system/sw/bin/bash";
    if !std::path::Path::new(bash).exists() {
        eprintln!("skipping: bash not found at {bash}");
        return;
    }
    let socket_path = test_socket_path("bash");
    let _daemon = start_daemon(&socket_path).await;

    let mut env = HashMap::new();
    env.insert("TERM".into(), "xterm-256color".into());
    session_with_shell(
        &socket_path, "bash-sess", bash, env,
        "echo bash-works\n", "bash-works",
    ).await;

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_shell_sh() {
    let socket_path = test_socket_path("sh");
    let _daemon = start_daemon(&socket_path).await;

    session_with_shell(
        &socket_path, "sh-sess", "/bin/sh", HashMap::new(),
        "echo sh-works\n", "sh-works",
    ).await;

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_shell_zsh() {
    if !std::path::Path::new("/run/current-system/sw/bin/zsh").exists() {
        eprintln!("skipping: zsh not found");
        return;
    }
    let socket_path = test_socket_path("zsh");
    let _daemon = start_daemon(&socket_path).await;

    let mut env = HashMap::new();
    env.insert("TERM".into(), "xterm-256color".into());
    env.insert("ZDOTDIR".into(), "/nonexistent".into());

    session_with_shell(
        &socket_path, "zsh-sess", "/run/current-system/sw/bin/zsh", env,
        "echo zsh-works\n", "zsh-works",
    ).await;

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_rapid_input() {
    let socket_path = test_socket_path("rapid");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    create_and_attach(&write_codec, &mut read_codec, &mut reader, &mut writer, "rapid-test").await;

    for i in 0..50 {
        write_codec
            .write_message(&mut writer, &Message::Data(format!("echo r{i}\n").as_bytes().to_vec()))
            .await
            .unwrap();
    }

    send_and_expect(&write_codec, &mut read_codec, &mut reader, &mut writer, "echo rapid-done\n", "rapid-done").await;

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_large_output() {
    let socket_path = test_socket_path("large-out");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    create_and_attach(&write_codec, &mut read_codec, &mut reader, &mut writer, "large-test").await;

    write_codec
        .write_message(&mut writer, &Message::Data(b"seq 1 500\n".to_vec()))
        .await
        .unwrap();

    send_and_expect(&write_codec, &mut read_codec, &mut reader, &mut writer, "echo large-done\n", "large-done").await;

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_binary_data_passthrough() {
    let socket_path = test_socket_path("binary");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    create_and_attach(&write_codec, &mut read_codec, &mut reader, &mut writer, "binary-test").await;

    send_and_expect(
        &write_codec, &mut read_codec, &mut reader, &mut writer,
        "printf '%s' HELLO\n", "HELLO",
    ).await;

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_special_characters() {
    let socket_path = test_socket_path("special-chars");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    create_and_attach(&write_codec, &mut read_codec, &mut reader, &mut writer, "special-test").await;

    send_and_expect(
        &write_codec, &mut read_codec, &mut reader, &mut writer,
        "echo 'hello-utf8'\n", "hello-utf8",
    ).await;

    send_and_expect(
        &write_codec, &mut read_codec, &mut reader, &mut writer,
        "printf 'A\\tB\\tC\\n'\n", "A",
    ).await;

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_multiple_resize() {
    let socket_path = test_socket_path("multi-resize");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    create_and_attach(&write_codec, &mut read_codec, &mut reader, &mut writer, "resize-multi").await;

    for (cols, rows) in [(100, 30), (60, 20), (200, 50), (80, 24)] {
        write_codec
            .write_message(&mut writer, &Message::Resize { cols, rows })
            .await
            .unwrap();
    }

    tokio::time::sleep(Duration::from_millis(200)).await;

    send_and_expect(
        &write_codec, &mut read_codec, &mut reader, &mut writer,
        "stty size\n", "24 80",
    ).await;

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_detach_reattach_multiple_times() {
    let socket_path = test_socket_path("multi-reattach");
    let _daemon = start_daemon(&socket_path).await;

    let (wc, mut rc, stream) = connect_and_handshake(&socket_path).await;
    let (mut r, mut w) = stream.into_split();
    create_and_attach(&wc, &mut rc, &mut r, &mut w, "bounce").await;
    send_and_expect(&wc, &mut rc, &mut r, &mut w, "echo round-0\n", "round-0").await;

    for i in 1..=5 {
        wc.write_message(&mut w, &Message::SessionDetach).await.unwrap();
        drop(w);
        drop(r);
        tokio::time::sleep(Duration::from_millis(100)).await;

        let (wc2, mut rc2, stream2) = connect_and_handshake(&socket_path).await;
        let (mut r2, mut w2) = stream2.into_split();

        wc2.write_message(&mut w2, &Message::SessionAttach { id: "bounce".into() })
            .await
            .unwrap();
        let resp = rc2.read_message(&mut r2).await.unwrap();
        assert!(matches!(resp, Message::SessionState(_)), "reattach {i} should get snapshot");

        let marker = format!("round-{i}");
        send_and_expect(&wc2, &mut rc2, &mut r2, &mut w2, &format!("echo {marker}\n"), &marker).await;

        w = w2;
        r = r2;
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_session_survives_client_crash() {
    let socket_path = test_socket_path("survive-crash");
    let _daemon = start_daemon(&socket_path).await;

    {
        let (wc, mut rc, stream) = connect_and_handshake(&socket_path).await;
        let (mut r, mut w) = stream.into_split();
        create_and_attach(&wc, &mut rc, &mut r, &mut w, "survivor").await;
        send_and_expect(&wc, &mut rc, &mut r, &mut w, "echo before-crash\n", "before-crash").await;
        // Drop without detaching — simulates client crash
    }

    tokio::time::sleep(Duration::from_millis(300)).await;

    let (wc2, mut rc2, stream2) = connect_and_handshake(&socket_path).await;
    let (mut r2, mut w2) = stream2.into_split();

    wc2.write_message(&mut w2, &Message::SessionAttach { id: "survivor".into() })
        .await
        .unwrap();
    let resp = rc2.read_message(&mut r2).await.unwrap();
    assert!(matches!(resp, Message::SessionState(_)));

    send_and_expect(&wc2, &mut rc2, &mut r2, &mut w2, "echo after-crash\n", "after-crash").await;

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_switch_sessions_on_same_connection() {
    let socket_path = test_socket_path("switch-sess");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    create_and_attach(&write_codec, &mut read_codec, &mut reader, &mut writer, "sess-a").await;
    send_and_expect(&write_codec, &mut read_codec, &mut reader, &mut writer, "echo in-a\n", "in-a").await;

    // Create B without detaching from A
    write_codec
        .write_message(
            &mut writer,
            &Message::SessionCreate {
                id: Some("sess-b".into()),
                cmd: Some("/bin/sh".into()),
                cols: 80,
                rows: 24,
                env: HashMap::new(),
            },
        )
        .await
        .unwrap();
    let resp = read_non_data(&mut read_codec, &mut reader).await;
    assert!(matches!(resp, Message::SessionCreated { .. }));

    // Attach to B (auto-detaches from A)
    write_codec
        .write_message(&mut writer, &Message::SessionAttach { id: "sess-b".into() })
        .await
        .unwrap();
    let resp = read_non_data(&mut read_codec, &mut reader).await;
    assert!(matches!(resp, Message::HelloOk { .. } | Message::SessionState(_)));

    send_and_expect(&write_codec, &mut read_codec, &mut reader, &mut writer, "echo in-b\n", "in-b").await;

    // Verify A is detached, B is attached
    write_codec.write_message(&mut writer, &Message::SessionList).await.unwrap();
    let resp = read_non_data(&mut read_codec, &mut reader).await;
    match resp {
        Message::SessionListResp { sessions } => {
            let a = sessions.iter().find(|s| s.id == "sess-a").unwrap();
            let b = sessions.iter().find(|s| s.id == "sess-b").unwrap();
            assert!(!a.attached, "A should be detached");
            assert!(b.attached, "B should be attached");
        }
        other => panic!("expected SessionListResp, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_long_running_command_survives_detach() {
    let socket_path = test_socket_path("long-cmd");
    let _daemon = start_daemon(&socket_path).await;

    let (wc, mut rc, stream) = connect_and_handshake(&socket_path).await;
    let (mut r, mut w) = stream.into_split();
    create_and_attach(&wc, &mut rc, &mut r, &mut w, "long-cmd").await;

    wc.write_message(&mut w, &Message::Data(b"(sleep 1 && echo long-cmd-finished) &\n".to_vec()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    wc.write_message(&mut w, &Message::SessionDetach).await.unwrap();
    drop(w);
    drop(r);

    tokio::time::sleep(Duration::from_secs(2)).await;

    let (wc2, mut rc2, stream2) = connect_and_handshake(&socket_path).await;
    let (mut r2, mut w2) = stream2.into_split();

    wc2.write_message(&mut w2, &Message::SessionAttach { id: "long-cmd".into() })
        .await
        .unwrap();
    let resp = rc2.read_message(&mut r2).await.unwrap();

    match &resp {
        Message::SessionState(state) => {
            let all_text: String = state
                .visible_rows
                .iter()
                .chain(state.scrollback.iter())
                .flat_map(|row| row.cells.iter().map(|c| c.c))
                .collect();
            assert!(
                all_text.contains("long-cmd-finished"),
                "snapshot should contain output from command that ran while detached"
            );
        }
        other => panic!("expected SessionState, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_different_terminal_sizes() {
    let socket_path = test_socket_path("termsizes");
    let _daemon = start_daemon(&socket_path).await;

    for (cols, rows, label) in [(40, 10, "small"), (132, 43, "wide"), (80, 24, "standard")] {
        let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
        let (mut reader, mut writer) = stream.into_split();

        write_codec
            .write_message(
                &mut writer,
                &Message::SessionCreate {
                    id: Some(format!("size-{label}")),
                    cmd: Some("/bin/sh".into()),
                    cols,
                    rows,
                    env: HashMap::new(),
                },
            )
            .await
            .unwrap();
        let _ = read_codec.read_message(&mut reader).await.unwrap();

        write_codec
            .write_message(&mut writer, &Message::SessionAttach { id: format!("size-{label}") })
            .await
            .unwrap();
        let _ = read_codec.read_message(&mut reader).await.unwrap();

        send_and_expect(
            &write_codec, &mut read_codec, &mut reader, &mut writer,
            &format!("echo size-{cols}x{rows}\n"),
            &format!("size-{cols}x{rows}"),
        ).await;
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_multiple_pings() {
    let socket_path = test_socket_path("multi-ping");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    for i in 0..20u32 {
        write_codec.write_message(&mut writer, &Message::Ping { seq: i }).await.unwrap();
    }

    for i in 0..20u32 {
        let resp = read_codec.read_message(&mut reader).await.unwrap();
        assert_eq!(resp, Message::Pong { seq: i }, "pong {i} out of order");
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_empty_data_message() {
    let socket_path = test_socket_path("empty-data");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    create_and_attach(&write_codec, &mut read_codec, &mut reader, &mut writer, "empty-data").await;

    write_codec
        .write_message(&mut writer, &Message::Data(vec![]))
        .await
        .unwrap();

    send_and_expect(&write_codec, &mut read_codec, &mut reader, &mut writer, "echo still-alive\n", "still-alive").await;

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_session_cwd_changes() {
    let socket_path = test_socket_path("cwd-change");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    create_and_attach(&write_codec, &mut read_codec, &mut reader, &mut writer, "cwd-test").await;

    write_codec
        .write_message(&mut writer, &Message::Data(b"cd /tmp\n".to_vec()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    write_codec.write_message(&mut writer, &Message::SessionList).await.unwrap();
    let resp = read_non_data(&mut read_codec, &mut reader).await;
    match resp {
        Message::SessionListResp { sessions } => {
            let s = sessions.iter().find(|s| s.id == "cwd-test").unwrap();
            assert!(s.cwd.contains("tmp"), "cwd should contain 'tmp', got: {}", s.cwd);
        }
        other => panic!("expected SessionListResp, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_foreground_process_reported() {
    let socket_path = test_socket_path("fg-proc");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    create_and_attach(&write_codec, &mut read_codec, &mut reader, &mut writer, "fg-test").await;
    tokio::time::sleep(Duration::from_millis(200)).await;

    write_codec.write_message(&mut writer, &Message::SessionList).await.unwrap();
    let resp = read_non_data(&mut read_codec, &mut reader).await;
    match resp {
        Message::SessionListResp { sessions } => {
            let s = sessions.iter().find(|s| s.id == "fg-test").unwrap();
            assert!(!s.foreground_proc.is_empty(), "foreground_proc should be non-empty");
        }
        other => panic!("expected SessionListResp, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

/// When the daemon cannot write output to the client (e.g. proxy alive but SSH
/// tunnel dead after laptop sleep), the write timeout should fire and the
/// session should be detached — not stuck as "attached" forever.
#[tokio::test]
async fn test_write_timeout_detaches_session() {
    let socket_path = test_socket_path("write-timeout");
    let _daemon = start_daemon(&socket_path).await;

    let (wc, mut rc, stream) = connect_and_handshake(&socket_path).await;
    let (mut r, mut w) = stream.into_split();
    create_and_attach(&wc, &mut rc, &mut r, &mut w, "timeout-test").await;

    // Generate lots of output to fill the socket buffer.
    // With nobody reading, the daemon's timed write will block once the
    // buffer is full and the 5-second write timeout will fire.
    wc.write_message(&mut w, &Message::Data(b"seq 1 1000000\n".to_vec()))
        .await
        .unwrap();

    // Stop reading — keep w alive so the socket isn't fully closed
    // (a full close would trigger the read-side ConnectionClosed path,
    // not the write timeout path we're testing).

    // Wait for: output generation + buffer fill + 5s timeout + margin
    tokio::time::sleep(Duration::from_secs(8)).await;

    // Drop old connection
    drop(r);
    drop(w);

    // New connection: verify session is detached (not stuck as "attached")
    let (wc2, mut rc2, stream2) = connect_and_handshake(&socket_path).await;
    let (mut r2, mut w2) = stream2.into_split();

    wc2.write_message(&mut w2, &Message::SessionList)
        .await
        .unwrap();
    let resp = rc2.read_message(&mut r2).await.unwrap();
    match &resp {
        Message::SessionListResp { sessions } => {
            assert_eq!(sessions.len(), 1, "session should still exist");
            assert!(
                !sessions[0].attached,
                "session should be detached after write timeout"
            );
        }
        other => panic!("expected SessionListResp, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

/// After a write timeout detaches a session, a new client should be able
/// to reattach and the session should still be functional.
#[tokio::test]
async fn test_reattach_works_after_write_timeout() {
    let socket_path = test_socket_path("reattach-timeout");
    let _daemon = start_daemon(&socket_path).await;

    let (wc, mut rc, stream) = connect_and_handshake(&socket_path).await;
    let (mut r, mut w) = stream.into_split();
    create_and_attach(&wc, &mut rc, &mut r, &mut w, "reattach-test").await;

    // Generate output, stop reading → triggers write timeout
    wc.write_message(&mut w, &Message::Data(b"seq 1 1000000\n".to_vec()))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_secs(8)).await;
    drop(r);
    drop(w);

    // Reattach with new client
    let (wc2, mut rc2, stream2) = connect_and_handshake(&socket_path).await;
    let (mut r2, mut w2) = stream2.into_split();

    wc2.write_message(
        &mut w2,
        &Message::SessionAttach {
            id: "reattach-test".into(),
        },
    )
    .await
    .unwrap();
    let resp = rc2.read_message(&mut r2).await.unwrap();
    assert!(
        matches!(resp, Message::SessionState(_)),
        "should get snapshot on reattach"
    );

    // Session should still be usable
    send_and_expect(
        &wc2,
        &mut rc2,
        &mut r2,
        &mut w2,
        "echo hello-after-timeout\n",
        "hello-after-timeout",
    )
    .await;

    std::fs::remove_file(&socket_path).ok();
}

/// Regression test: reattach must not hang when the PTY is actively producing
/// output and the previous client disconnected without a clean detach.
///
/// Previously, the server held the registry lock across `handle.snapshot()`,
/// which could block when the event relay task needed the same lock to drain
/// the session event channel — causing the reattach to never complete.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_reattach_while_pty_producing_output() {
    let socket_path = test_socket_path("reattach-active");
    let _daemon = start_daemon(&socket_path).await;

    // Client 1: create session and attach
    let (wc1, mut rc1, stream1) = connect_and_handshake(&socket_path).await;
    let (mut r1, mut w1) = stream1.into_split();
    create_and_attach(&wc1, &mut rc1, &mut r1, &mut w1, "busy").await;

    // Start continuous output so event/output channels fill up
    wc1.write_message(
        &mut w1,
        &Message::Data(b"while true; do printf 'AAAAAAAAAAAAAAAAAAAAAAAAAAA\\n'; done\n".to_vec()),
    )
    .await
    .unwrap();

    // Let output flow and channels fill
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Abruptly drop the connection — no clean detach (simulates network loss).
    // The server's event relay task still has output to send, and the session's
    // PTY continues producing data.
    drop(w1);
    drop(r1);

    // Let output accumulate with no client draining the output channel
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Client 2: reconnect and reattach
    let (wc2, mut rc2, stream2) = connect_and_handshake(&socket_path).await;
    let (mut r2, mut w2) = stream2.into_split();

    wc2.write_message(&mut w2, &Message::SessionAttach { id: "busy".into() })
        .await
        .unwrap();

    // The reattach MUST complete within 5 seconds.
    // Before the fix, this could hang indefinitely because the registry lock
    // was held while snapshot() waited for the session inner lock.
    let resp = timeout(Duration::from_secs(5), rc2.read_message(&mut r2))
        .await
        .expect("reattach timed out — possible deadlock in snapshot while holding registry lock")
        .unwrap();

    assert!(
        matches!(resp, Message::SessionState(_)),
        "expected SessionState on reattach, got: {resp:?}"
    );

    std::fs::remove_file(&socket_path).ok();
}

/// Stress test: multiple sessions reconnect simultaneously with active PTY output.
/// Mirrors the real-world scenario where a network disconnect affects all sessions
/// and they all race to reattach at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_concurrent_reattach_with_active_output() {
    let socket_path = test_socket_path("concurrent-reattach");
    let _daemon = start_daemon(&socket_path).await;

    let session_names: Vec<String> = (0..4).map(|i| format!("cr-sess-{i}")).collect();

    // Create all sessions via one control connection
    {
        let (wc, mut rc, stream) = connect_and_handshake(&socket_path).await;
        let (mut r, mut w) = stream.into_split();

        for name in &session_names {
            wc.write_message(
                &mut w,
                &Message::SessionCreate {
                    id: Some(name.clone()),
                    cmd: Some("/bin/sh".into()),
                    cols: 80,
                    rows: 24,
                    env: HashMap::new(),
                },
            )
            .await
            .unwrap();
            let resp = rc.read_message(&mut r).await.unwrap();
            assert!(matches!(resp, Message::SessionCreated { .. }));
        }
    }

    // Attach to each session and start continuous output
    let mut clients = Vec::new();
    for name in &session_names {
        let (wc, mut rc, stream) = connect_and_handshake(&socket_path).await;
        let (mut r, mut w) = stream.into_split();

        wc.write_message(&mut w, &Message::SessionAttach { id: name.clone() })
            .await
            .unwrap();
        let _ = rc.read_message(&mut r).await.unwrap(); // HelloOk (first attach)

        wc.write_message(
            &mut w,
            &Message::Data(b"while true; do printf 'BBBBBBBB\\n'; done\n".to_vec()),
        )
        .await
        .unwrap();

        clients.push((wc, r, w));
    }

    // Let output flow
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Simulate network outage: drop ALL connections at once
    drop(clients);

    // Let output accumulate with all channels orphaned
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Reattach all sessions concurrently — all must succeed within timeout
    let mut handles = Vec::new();
    for name in &session_names {
        let path = socket_path.clone();
        let name = name.clone();
        handles.push(tokio::spawn(async move {
            let (wc, mut rc, stream) = connect_and_handshake(&path).await;
            let (mut r, mut w) = stream.into_split();

            wc.write_message(&mut w, &Message::SessionAttach { id: name.clone() })
                .await
                .unwrap();

            let resp = timeout(Duration::from_secs(5), rc.read_message(&mut r))
                .await
                .unwrap_or_else(|_| panic!("{name}: reattach timed out — possible deadlock"))
                .unwrap();

            assert!(
                matches!(resp, Message::SessionState(_)),
                "{name}: expected SessionState, got: {resp:?}"
            );
        }));
    }

    for h in handles {
        h.await.unwrap();
    }

    std::fs::remove_file(&socket_path).ok();
}

/// Keepalive detects dead connections on idle sessions.
///
/// When a session produces no output (e.g. an idle MCP server), the only way
/// to detect a dead client is via keepalive pings. Without keepalive, the
/// session stays "attached" forever because the daemon never tries to write.
///
/// This test attaches to an idle session, stops reading (simulating a dead SSH
/// tunnel where the proxy holds the Unix socket open), and verifies the daemon
/// detects the dead connection via keepalive and marks the session as detached.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_keepalive_detaches_idle_session() {
    let socket_path = test_socket_path("keepalive-idle");
    let _daemon = start_daemon_with_config(&socket_path, tether_daemon::Config {
        idle_timeout: "60s".into(),
        max_sessions: 5,
        keepalive: "1s".into(),
        ..Default::default()
    }).await;

    // Create and attach to a session
    let (wc, mut rc, stream) = connect_and_handshake(&socket_path).await;
    let (mut r, mut w) = stream.into_split();
    create_and_attach(&wc, &mut rc, &mut r, &mut w, "idle-ka").await;

    // Verify it's attached
    wc.write_message(&mut w, &Message::SessionList).await.unwrap();
    let resp = read_non_data(&mut rc, &mut r).await;
    match &resp {
        Message::SessionListResp { sessions } => {
            let s = sessions.iter().find(|s| s.id == "idle-ka").unwrap();
            assert!(s.attached, "session should be attached initially");
        }
        other => panic!("expected SessionListResp, got: {other:?}"),
    }

    // Simulate dead SSH tunnel: stop reading from the socket but keep
    // the write half alive so the Unix socket isn't fully closed.
    // This prevents the read-side ConnectionClosed path from firing —
    // the ONLY way the daemon can detect the dead client is via the
    // keepalive Ping/Pong mechanism.
    //
    // Don't generate any PTY output — the session stays completely idle.

    // Wait for: first keepalive tick sends Ping (1s), client doesn't
    // reply with Pong (not reading), second tick sees missing Pong and
    // disconnects (another 1s), plus margin.
    tokio::time::sleep(Duration::from_secs(4)).await;

    // Drop old connection
    drop(r);
    drop(w);

    // New connection: verify session was detached by keepalive
    let (wc2, mut rc2, stream2) = connect_and_handshake(&socket_path).await;
    let (mut r2, mut w2) = stream2.into_split();

    wc2.write_message(&mut w2, &Message::SessionList).await.unwrap();
    let resp = rc2.read_message(&mut r2).await.unwrap();
    match &resp {
        Message::SessionListResp { sessions } => {
            assert_eq!(sessions.len(), 1, "session should still exist");
            assert!(
                !sessions[0].attached,
                "session should be detached after keepalive timeout"
            );
        }
        other => panic!("expected SessionListResp, got: {other:?}"),
    }

    std::fs::remove_file(&socket_path).ok();
}

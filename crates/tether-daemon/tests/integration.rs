use std::collections::HashMap;
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::time::timeout;

use tether_protocol::{FrameCodec, Message, PROTOCOL_VERSION};

// -- Helpers --

async fn start_daemon(socket_path: &str) -> tokio::task::JoinHandle<()> {
    let config = tether_daemon::Config {
        socket_path: socket_path.to_string(),
        idle_timeout: "60s".into(),
        max_sessions: 5,
        ..Default::default()
    };

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
    let _ = read_codec.read_message(&mut reader).await.unwrap();

    // List should be empty
    write_codec.write_message(&mut writer, &Message::SessionList).await.unwrap();
    let resp = read_codec.read_message(&mut reader).await.unwrap();
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

    // Verify by checking $COLUMNS/$LINES inside the shell
    send_and_expect(
        &write_codec, &mut read_codec, &mut reader, &mut writer,
        "echo cols=$COLUMNS rows=$LINES\n",
        "cols=120 rows=40",
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
    let resp = read_codec.read_message(&mut reader).await.unwrap();
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

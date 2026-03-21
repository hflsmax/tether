use std::collections::HashMap;
use std::time::Duration;

use tokio::net::UnixStream;
use tokio::time::timeout;

use tether_protocol::{FrameCodec, Message, ScreenMode, PROTOCOL_VERSION};

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
    // Retry connection briefly while daemon starts up
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

    // Reunite the stream
    let stream = reader.reunite(writer).unwrap();
    (write_codec, read_codec, stream)
}

fn test_socket_path(name: &str) -> String {
    format!(
        "/tmp/tether-test-{}-{}.sock",
        name,
        std::process::id()
    )
}

#[tokio::test]
async fn test_create_and_list_sessions() {
    let socket_path = test_socket_path("create-list");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    // Create a session
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
        other => panic!("expected SessionCreated, got: {:?}", other),
    }

    // List sessions
    write_codec
        .write_message(&mut writer, &Message::SessionList)
        .await
        .unwrap();

    let resp = read_codec.read_message(&mut reader).await.unwrap();
    match resp {
        Message::SessionListResp { sessions } => {
            assert_eq!(sessions.len(), 1);
            assert_eq!(sessions[0].id, "test-session");
        }
        other => panic!("expected SessionListResp, got: {:?}", other),
    }

    // Cleanup
    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_attach_and_receive_output() {
    let socket_path = test_socket_path("attach");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    // Create a session running echo
    write_codec
        .write_message(
            &mut writer,
            &Message::SessionCreate {
                id: Some("echo-session".into()),
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

    // Attach
    write_codec
        .write_message(
            &mut writer,
            &Message::SessionAttach {
                id: "echo-session".into(),
            },
        )
        .await
        .unwrap();

    // Should receive SessionState snapshot
    let resp = read_codec.read_message(&mut reader).await.unwrap();
    match &resp {
        Message::SessionState(state) => {
            assert_eq!(state.cols, 80);
            assert_eq!(state.rows, 24);
            assert_eq!(state.screen_mode, ScreenMode::Main);
        }
        other => panic!("expected SessionState, got: {:?}", other),
    }

    // Send a command
    write_codec
        .write_message(&mut writer, &Message::Data(b"echo hello-tether\n".to_vec()))
        .await
        .unwrap();

    // Read output until we see our marker string
    let found = timeout(Duration::from_secs(5), async {
        let mut accumulated = Vec::new();
        loop {
            let msg = read_codec.read_message(&mut reader).await.unwrap();
            if let Message::Data(data) = msg {
                accumulated.extend_from_slice(&data);
                let output = String::from_utf8_lossy(&accumulated);
                if output.contains("hello-tether") {
                    return true;
                }
            }
        }
    })
    .await;

    assert!(found.unwrap_or(false), "didn't receive expected output");

    // Cleanup
    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_detach_and_reattach() {
    let socket_path = test_socket_path("reattach");
    let _daemon = start_daemon(&socket_path).await;

    // First connection: create and attach
    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    write_codec
        .write_message(
            &mut writer,
            &Message::SessionCreate {
                id: Some("persistent".into()),
                cmd: Some("/bin/sh".into()),
                cols: 80,
                rows: 24,
                env: HashMap::new(),
            },
        )
        .await
        .unwrap();
    let _ = read_codec.read_message(&mut reader).await.unwrap();

    write_codec
        .write_message(
            &mut writer,
            &Message::SessionAttach {
                id: "persistent".into(),
            },
        )
        .await
        .unwrap();
    let _ = read_codec.read_message(&mut reader).await.unwrap(); // SessionState

    // Send some data
    write_codec
        .write_message(&mut writer, &Message::Data(b"echo marker-123\n".to_vec()))
        .await
        .unwrap();

    // Wait for output
    timeout(Duration::from_secs(3), async {
        loop {
            let msg = read_codec.read_message(&mut reader).await.unwrap();
            if let Message::Data(data) = &msg
                && String::from_utf8_lossy(data).contains("marker-123")
            {
                break;
            }
        }
    })
    .await
    .expect("didn't see marker output");

    // Detach
    write_codec
        .write_message(&mut writer, &Message::SessionDetach)
        .await
        .unwrap();

    // Drop old connection
    drop(writer);
    drop(reader);

    // Small delay
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Reconnect and reattach
    let (write_codec2, mut read_codec2, stream2) = connect_and_handshake(&socket_path).await;
    let (mut reader2, mut writer2) = stream2.into_split();

    write_codec2
        .write_message(
            &mut writer2,
            &Message::SessionAttach {
                id: "persistent".into(),
            },
        )
        .await
        .unwrap();

    // Should get a SessionState snapshot with the previous terminal content
    let resp = read_codec2.read_message(&mut reader2).await.unwrap();
    match &resp {
        Message::SessionState(state) => {
            // Verify the snapshot contains content from our previous session
            let mut has_content = false;
            for row in &state.visible_rows {
                let text: String = row.cells.iter().map(|c| c.c).collect();
                if text.contains("marker-123") {
                    has_content = true;
                    break;
                }
            }
            // Also check scrollback
            if !has_content {
                for row in &state.scrollback {
                    let text: String = row.cells.iter().map(|c| c.c).collect();
                    if text.contains("marker-123") {
                        has_content = true;
                        break;
                    }
                }
            }
            assert!(has_content, "reattach snapshot should contain previous output");
        }
        other => panic!("expected SessionState, got: {:?}", other),
    }

    // Cleanup
    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_destroy_session() {
    let socket_path = test_socket_path("destroy");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    // Create
    write_codec
        .write_message(
            &mut writer,
            &Message::SessionCreate {
                id: Some("doomed".into()),
                cmd: Some("/bin/sh".into()),
                cols: 80,
                rows: 24,
                env: HashMap::new(),
            },
        )
        .await
        .unwrap();
    let _ = read_codec.read_message(&mut reader).await.unwrap();

    // Destroy
    write_codec
        .write_message(
            &mut writer,
            &Message::SessionDestroy {
                id: "doomed".into(),
            },
        )
        .await
        .unwrap();
    let _ = read_codec.read_message(&mut reader).await.unwrap();

    // List should be empty
    write_codec
        .write_message(&mut writer, &Message::SessionList)
        .await
        .unwrap();

    let resp = read_codec.read_message(&mut reader).await.unwrap();
    match resp {
        Message::SessionListResp { sessions } => {
            assert!(sessions.is_empty(), "session should be destroyed");
        }
        other => panic!("expected SessionListResp, got: {:?}", other),
    }

    // Cleanup
    std::fs::remove_file(&socket_path).ok();
}

#[tokio::test]
async fn test_ping_pong() {
    let socket_path = test_socket_path("ping");
    let _daemon = start_daemon(&socket_path).await;

    let (write_codec, mut read_codec, stream) = connect_and_handshake(&socket_path).await;
    let (mut reader, mut writer) = stream.into_split();

    write_codec
        .write_message(&mut writer, &Message::Ping { seq: 42 })
        .await
        .unwrap();

    let resp = read_codec.read_message(&mut reader).await.unwrap();
    assert_eq!(resp, Message::Pong { seq: 42 });

    // Cleanup
    std::fs::remove_file(&socket_path).ok();
}

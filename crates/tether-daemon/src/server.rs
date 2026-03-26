use std::sync::Arc;

use tokio::net::UnixListener;
use tokio::sync::Mutex;
use tokio::time::{self, Duration};
use tracing::{debug, info, warn};

use tether_protocol::{FrameCodec, Message, PROTOCOL_VERSION};
use tether_session::{SessionEvent, SessionHandle};

use crate::config::Config;
use crate::registry::Registry;

pub struct Server {
    config: Config,
    registry: Arc<Mutex<Registry>>,
}

impl Server {
    pub fn new(config: Config) -> Self {
        let registry = Arc::new(Mutex::new(Registry::new(config.clone())));
        Self { config, registry }
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        let socket_path = self.config.socket_path();

        // Remove stale socket
        if socket_path.exists() {
            std::fs::remove_file(&socket_path)?;
        }

        // Ensure parent directory exists
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let listener = UnixListener::bind(&socket_path)?;
        info!("listening on {}", socket_path.display());

        // Spawn idle timeout checker
        let registry_timeout = self.registry.clone();
        tokio::spawn(async move {
            let mut interval = time::interval(Duration::from_secs(3600));
            loop {
                interval.tick().await;
                let mut reg = registry_timeout.lock().await;
                let expired = reg.check_idle_timeouts();
                for id in expired {
                    warn!(session = %id, "session expired due to idle timeout");
                    reg.destroy(&id).ok();
                }
            }
        });

        loop {
            let (stream, _addr) = listener.accept().await?;
            let registry = self.registry.clone();
            let config = self.config.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection(stream, registry, config).await {
                    debug!("connection handler error: {e}");
                }
            });
        }
    }
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    registry: Arc<Mutex<Registry>>,
    config: Config,
) -> anyhow::Result<()> {
    let (reader, writer) = stream.into_split();
    let mut reader = reader;
    let mut writer = writer;
    let mut codec = FrameCodec::new();
    let write_codec = FrameCodec::new();

    // Timeout for writes — if the proxy/SSH is dead but the Unix socket hasn't
    // closed, writes will block until the socket buffer fills. Detect it early.
    let write_timeout = Duration::from_secs(5);

    // Wait for Hello
    let msg = codec.read_message(&mut reader).await?;
    match msg {
        Message::Hello { version, .. } => {
            if version != PROTOCOL_VERSION {
                write_codec
                    .write_message(
                        &mut writer,
                        &Message::Error {
                            code: 1,
                            message: format!("unsupported protocol version: {version}"),
                        },
                    )
                    .await?;
                return Ok(());
            }
            write_codec
                .write_message(&mut writer, &Message::HelloOk { version: PROTOCOL_VERSION })
                .await?;
        }
        _ => {
            write_codec
                .write_message(
                    &mut writer,
                    &Message::Error {
                        code: 2,
                        message: "expected Hello".into(),
                    },
                )
                .await?;
            return Ok(());
        }
    };

    // Attached state: session handle + output channel, held OUTSIDE the registry lock.
    // This eliminates lock contention on the hot path (every keystroke / output chunk).
    struct AttachState {
        id: String,
        handle: Arc<SessionHandle>,
        output_rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
    }
    let mut attached: Option<AttachState> = None;

    // Write with timeout — prevents blocking forever on dead connections.
    macro_rules! timed_write {
        ($msg:expr) => {
            match tokio::time::timeout(write_timeout, write_codec.write_message(&mut writer, $msg)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => return Err(e.into()),
                Err(_) => anyhow::bail!("write timeout"),
            }
        };
    }

    // Run the main loop inside an async block so that `?` and `return` exit
    // the block — not the function — letting us always detach below.
    let result: anyhow::Result<()> = async {
        loop {
            tokio::select! {
                // Read message from client
                result = codec.read_message(&mut reader) => {
                    let msg = match result {
                        Ok(msg) => msg,
                        Err(tether_protocol::codec::CodecError::ConnectionClosed) => {
                            return Ok(());
                        }
                        Err(e) => return Err(e.into()),
                    };

                    match msg {
                        Message::SessionCreate { id, cmd, cols, rows, env } => {
                            let env_vec: Vec<(String, String)> = env.into_iter().collect();
                            let mut reg = registry.lock().await;
                            match reg.create_session(id, cmd, cols, rows, env_vec) {
                                Ok(id) => {
                                    timed_write!(&Message::SessionCreated { id });
                                }
                                Err(e) => {
                                    timed_write!(
                                        &Message::Error { code: 10, message: e }
                                    );
                                }
                            }
                        }

                        Message::SessionAttach { id } => {
                            // Detach from current session if any
                            if let Some(ref prev) = attached {
                                let mut reg = registry.lock().await;
                                reg.detach(&prev.id);
                            }
                            attached = None;

                            let mut reg = registry.lock().await;
                            match reg.attach(&id) {
                                Ok((rx, event_rx_opt)) => {
                                    let handle = match reg.take_handle(&id) {
                                        Some(h) => h,
                                        None => {
                                            drop(reg);
                                            timed_write!(
                                                &Message::Error { code: 11, message: "session handle missing".into() }
                                            );
                                            continue;
                                        }
                                    };

                                    let first_attach = event_rx_opt.is_some();

                                    if first_attach {
                                        // First attach: skip snapshot — shell just started
                                        // and raw output with proper colors will follow.
                                        drop(reg);
                                        timed_write!(&Message::HelloOk { version: PROTOCOL_VERSION });
                                    } else {
                                        // Reattach: send snapshot to restore screen state.
                                        // Drop registry lock BEFORE snapshot to avoid deadlock:
                                        // snapshot() acquires session inner lock, but the PTY
                                        // reader holds inner lock while sending to the event
                                        // channel, and the event relay needs the registry lock
                                        // to drain that channel.
                                        drop(reg);
                                        let snapshot = handle.snapshot(config.scrollback_lines).await;
                                        timed_write!(&Message::SessionState(snapshot));
                                    }

                                    let reattach = !first_attach;
                                    info!(session = %id, reattach, "client attached");

                                    attached = Some(AttachState {
                                        id: id.clone(),
                                        handle: handle.clone(),
                                        output_rx: rx,
                                    });

                                    if let Some(mut erx) = event_rx_opt {
                                        let reg_clone = registry.clone();
                                        let session_id = id.clone();
                                        tokio::spawn(async move {
                                            while let Some(event) = erx.recv().await {
                                                match event {
                                                    SessionEvent::Output(data) => {
                                                        // Get sender without holding lock across send
                                                        let tx = {
                                                            let reg = reg_clone.lock().await;
                                                            reg.get_output_tx(&session_id).cloned()
                                                        };
                                                        if let Some(tx) = tx {
                                                            // Lock is already released, so this
                                                            // send can't deadlock even if it blocks.
                                                            let _ = tx.send(data).await;
                                                        }
                                                    }
                                                    SessionEvent::Exited(code) => {
                                                        info!(session = %session_id, exit_code = code, "session process exited");
                                                        let mut reg = reg_clone.lock().await;
                                                        reg.mark_exited(&session_id);
                                                        break;
                                                    }
                                                }
                                            }
                                        });
                                    }
                                }
                                Err(e) => {
                                    drop(reg);
                                    timed_write!(
                                        &Message::Error { code: 11, message: e }
                                    );
                                }
                            }
                        }

                        Message::SessionDetach => {
                            if let Some(ref state) = attached {
                                let mut reg = registry.lock().await;
                                reg.detach(&state.id);
                            }
                            attached = None;
                        }

                        Message::SessionDestroy { id } => {
                            let mut reg = registry.lock().await;
                            match reg.destroy(&id) {
                                Ok(()) => {
                                    if attached.as_ref().is_some_and(|s| s.id == id) {
                                        attached = None;
                                    }
                                    timed_write!(&Message::SessionCreated { id });
                                }
                                Err(e) => {
                                    timed_write!(
                                        &Message::Error { code: 12, message: e }
                                    );
                                }
                            }
                        }

                        Message::SessionList => {
                            let reg = registry.lock().await;
                            let sessions = reg.list();
                            drop(reg);
                            timed_write!(&Message::SessionListResp { sessions });
                        }

                        // Hot path: no registry lock needed — we have the handle directly
                        Message::Data(data) => {
                            if let Some(ref state) = attached
                                && let Err(e) = state.handle.write_input(&data)
                            {
                                warn!(session = %state.id, "failed to write to pty: {e}");
                            }
                        }

                        Message::Resize { cols, rows } => {
                            if let Some(ref state) = attached
                                && let Err(e) = state.handle.resize(cols, rows).await
                            {
                                warn!(session = %state.id, "failed to resize pty: {e}");
                            }
                        }

                        Message::Ping { seq } => {
                            timed_write!(&Message::Pong { seq });
                        }

                        _ => {
                            debug!("unexpected message type: 0x{:02x}", msg.type_id());
                        }
                    }
                }

                // Forward PTY output to client — no lock needed
                result = async {
                    if let Some(ref mut state) = attached {
                        state.output_rx.recv().await
                    } else {
                        std::future::pending::<Option<Vec<u8>>>().await
                    }
                } => {
                    match result {
                        Some(data) => {
                            timed_write!(&Message::Data(data));
                        }
                        None => {
                            // Channel closed — session exited. Notify client.
                            if let Some(state) = attached.take() {
                                timed_write!(
                                    &Message::SessionExited { id: state.id, exit_code: 0 }
                                );
                            }
                        }
                    }
                }
            }
        }
    }.await;

    // Always detach on exit, regardless of how we got here.
    // This ensures the session becomes available for reattachment even if
    // the connection was lost without a clean SessionDetach or ConnectionClosed.
    if let Some(ref state) = attached {
        info!(session = %state.id, "client disconnected");
        let mut reg = registry.lock().await;
        reg.detach(&state.id);
    }

    result
}

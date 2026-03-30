mod render;

use std::collections::HashMap;
use std::io::Write;
use std::process::Stdio;

use clap::Parser;
use crossterm::{cursor, execute, terminal};
use tokio::process::Command;
use tracing::{debug, info, warn};

use tether_protocol::{FrameCodec, Message, SessionInfo, PROTOCOL_VERSION};

const DETACH_BYTE: u8 = 0x1c; // Ctrl-backslash

struct RawModeGuard {
    enabled: bool,
}

impl RawModeGuard {
    fn enable() -> std::io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self { enabled: true })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.enabled {
            let _ = terminal::disable_raw_mode();
            let _ = execute!(std::io::stdout(), cursor::Show);
            let _ = std::io::stdout().write_all(b"\r\n");
            let _ = std::io::stdout().flush();
        }
    }
}

/// Persistent terminal sessions over SSH
#[derive(Parser)]
#[command(
    name = "tether",
    version = concat!(env!("CARGO_PKG_VERSION"), " (", env!("GIT_COMMIT_HASH"), " ", env!("GIT_COMMIT_DATE"), ")"),
    override_usage = "tether <user@host>\n       tether --socket <path>",
)]
struct Cli {
    /// Remote host (user@host)
    host: Option<String>,

    /// Use direct Unix socket connection (no SSH)
    #[arg(long)]
    socket: Option<String>,

    /// Enable verbose logging (-v info, -vv debug, -vvv trace).
    /// Logs to platform data dir (macOS: ~/Library/Application Support/tether/tether.log)
    #[arg(short, long, action = clap::ArgAction::Count)]
    verbose: u8,

    /// Override log file path
    #[arg(long)]
    log_file: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Restore terminal on panic so the user's shell isn't left in raw mode
    let default_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(std::io::stdout(), cursor::Show);
        let _ = std::io::stdout().write_all(b"\r\n");
        let _ = std::io::stdout().flush();
        default_panic(info);
    }));

    let cli = Cli::parse();

    // Set up logging: if -v is given or RUST_LOG is set, log to a file
    // (not stderr, which would corrupt the terminal in raw mode).
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().ok();
    let level = match (env_filter, cli.verbose) {
        (Some(f), _) => Some(f),
        (None, 1) => Some(tracing_subscriber::EnvFilter::new("info")),
        (None, 2) => Some(tracing_subscriber::EnvFilter::new("debug")),
        (None, v) if v >= 3 => Some(tracing_subscriber::EnvFilter::new("trace")),
        _ => None,
    };
    if let Some(filter) = level {
        let log_path = cli.log_file.clone().unwrap_or_else(|| {
            let dir = dirs::data_local_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("tether");
            let _ = std::fs::create_dir_all(&dir);
            dir.join("tether.log").to_string_lossy().into_owned()
        });
        // LineWriter flushes on every newline — critical so events just
        // before laptop sleep are persisted to disk.
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .expect("failed to open log file");
        let writer = std::io::LineWriter::new(file);
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::sync::Mutex::new(writer))
            .with_ansi(false)
            .init();
        info!(path = %log_path, "logging started");
    } else {
        // No logging requested — silently drop everything
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::new("off"))
            .with_writer(std::io::stderr)
            .init();
    }

    if cli.host.is_none() && cli.socket.is_none() {
        // Use clap's built-in help instead of a manual message
        use clap::CommandFactory;
        Cli::command().print_help()?;
        println!();
        std::process::exit(1);
    }

    auto_connect(&cli.host, &cli.socket).await
}

// -- Session picker --

enum PickerAction {
    Resume(String),
    New,
}

fn run_picker(sessions: &mut Vec<SessionInfo>, host: &Option<String>, socket: &Option<String>) -> anyhow::Result<PickerAction> {
    use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    let mut sel: usize = 0;
    let mut out = std::io::stderr();
    terminal::enable_raw_mode()?;
    execute!(out, terminal::EnterAlternateScreen, cursor::Hide)?;

    let kill_session = |id: &str, host: &Option<String>, socket: &Option<String>| -> anyhow::Result<()> {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let (mut r, mut w, _) = connect(host, socket, false).await?;
                let wc = FrameCodec::new();
                let mut rc = FrameCodec::new();
                handshake(&wc, &mut rc, w.as_mut(), r.as_mut()).await?;
                wc.write_message(w.as_mut(), &Message::SessionDestroy { id: id.into() }).await?;
                let _ = rc.read_message(r.as_mut()).await;
                Ok(())
            })
        })
    };

    let leave = |out: &mut std::io::Stderr| -> std::io::Result<()> {
        execute!(out, terminal::LeaveAlternateScreen, cursor::Show)?;
        terminal::disable_raw_mode()?;
        Ok(())
    };

    loop {
        let total = sessions.len() + 1;

        // Full redraw on clean screen
        execute!(out, cursor::MoveTo(0, 0), terminal::Clear(terminal::ClearType::All))?;

        write!(out, "  {:<18} {:<12} {:<24} {:<8} IDLE\r\n",
            "NAME", "RUNNING", "CWD", "AGE")?;

        if sel == 0 { write!(out, "\x1b[7m")?; }
        write!(out, "{} [new session]\x1b[0m\r\n", if sel == 0 { ">" } else { " " })?;

        for (i, s) in sessions.iter().enumerate() {
            let idx = i + 1;
            let proc_name = if s.foreground_proc.is_empty() { "-" } else { &s.foreground_proc };
            let age = format_duration(s.created_secs);
            let idle = format_duration(s.idle_secs);
            let cwd = shorten_path(&s.cwd);

            if s.attached {
                // Dim attached sessions
                write!(out, "\x1b[2m")?;
            } else if sel == idx {
                write!(out, "\x1b[7m")?;
            }
            let suffix = if s.attached { " (attached)" } else { "" };
            write!(out, "{} {:<18} {:<12} {:<24} {:<8} {}{}\x1b[0m\r\n",
                if sel == idx && !s.attached { ">" } else { " " }, s.id, proc_name, cwd, age, idle, suffix)?;
        }

        write!(out, "\r\n")?;
        write!(out, "  enter: select  x: kill  q: quit\r\n")?;
        out.flush()?;

        if sessions.is_empty() {
            leave(&mut out)?;
            return Ok(PickerAction::New);
        }

        if let Event::Key(ev @ KeyEvent { kind: KeyEventKind::Press, .. }) = event::read()? {
            let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);

            let is_selectable = |idx: usize| -> bool {
                idx == 0 || !sessions[idx - 1].attached
            };

            match ev.code {
                KeyCode::Up | KeyCode::Char('k') if !ctrl => {
                    let mut next = sel;
                    while next > 0 {
                        next -= 1;
                        if is_selectable(next) { break; }
                    }
                    if is_selectable(next) { sel = next; }
                }
                KeyCode::Down | KeyCode::Char('j') if !ctrl => {
                    let mut next = sel;
                    while next + 1 < total {
                        next += 1;
                        if is_selectable(next) { break; }
                    }
                    if is_selectable(next) { sel = next; }
                }
                KeyCode::Enter => {
                    if sel == 0 {
                        leave(&mut out)?;
                        return Ok(PickerAction::New);
                    }
                    leave(&mut out)?;
                    return Ok(PickerAction::Resume(sessions[sel - 1].id.clone()));
                }
                KeyCode::Char('x') if !ctrl && sel > 0 && !sessions[sel - 1].attached => {
                    let id = sessions[sel - 1].id.clone();
                    kill_session(&id, host, socket)?;
                    sessions.remove(sel - 1);
                    if sel > sessions.len() {
                        sel = sessions.len();
                    }
                }
                KeyCode::Char('q') | KeyCode::Esc => {
                    leave(&mut out)?;
                    std::process::exit(0);
                }
                KeyCode::Char('c' | 'd') if ctrl => {
                    leave(&mut out)?;
                    std::process::exit(0);
                }
                _ => {}
            }
        }
    }
}

// -- Core logic --

async fn auto_connect(
    host: &Option<String>,
    socket: &Option<String>,
) -> anyhow::Result<()> {
    let (mut reader, mut writer, _child) = connect(host, socket, false).await?;
    let write_codec = FrameCodec::new();
    let mut read_codec = FrameCodec::new();

    handshake(&write_codec, &mut read_codec, writer.as_mut(), reader.as_mut()).await?;

    write_codec
        .write_message(writer.as_mut(), &Message::SessionList)
        .await?;

    let mut sessions = match read_codec.read_message(reader.as_mut()).await? {
        Message::SessionListResp { sessions } => sessions,
        _ => vec![],
    };

    drop(reader);
    drop(writer);

    if sessions.is_empty() {
        return run_session(host, socket, None, None, false).await;
    }

    match run_picker(&mut sessions, host, socket)? {
        PickerAction::Resume(id) => {
            run_session(host, socket, Some(id), None, true).await
        }
        PickerAction::New => {
            run_session(host, socket, None, None, false).await
        }
    }
}

async fn connect(
    host: &Option<String>,
    socket: &Option<String>,
    reconnecting: bool,
) -> anyhow::Result<(
    Box<dyn tokio::io::AsyncRead + Unpin + Send>,
    Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
    Option<tokio::process::Child>,
)> {
    if let Some(socket_path) = socket {
        info!(socket = %socket_path, "connecting via unix socket");
        let stream = tokio::net::UnixStream::connect(socket_path).await.map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => {
                anyhow::anyhow!("daemon not running (socket not found: {socket_path})")
            }
            std::io::ErrorKind::ConnectionRefused => {
                anyhow::anyhow!("daemon not accepting connections ({socket_path})")
            }
            _ => anyhow::anyhow!("failed to connect to daemon at {socket_path}: {e}"),
        })?;
        let (r, w) = stream.into_split();
        Ok((Box::new(r), Box::new(w), None))
    } else if let Some(host) = host {
        info!(host = %host, "connecting via ssh");
        let mut cmd = Command::new("ssh");
        cmd.arg("-o").arg("ServerAliveInterval=5")
            .arg("-o").arg("ServerAliveCountMax=2");
        if reconnecting {
            // Single attempt with a tight timeout to avoid rapid-fire SSH errors
            cmd.arg("-o").arg("ConnectTimeout=5")
                .arg("-o").arg("ConnectionAttempts=1");
        }
        let mut child = cmd
            .arg(host)
            .arg("tether-proxy")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if reconnecting { Stdio::null() } else { Stdio::inherit() })
            .spawn()?;
        let pid = child.id();
        info!(ssh_pid = ?pid, "ssh process spawned");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        Ok((Box::new(stdout), Box::new(stdin), Some(child)))
    } else {
        anyhow::bail!("either host or --socket must be specified");
    }
}

async fn handshake(
    codec: &FrameCodec,
    read_codec: &mut FrameCodec,
    writer: &mut (dyn tokio::io::AsyncWrite + Unpin + Send),
    reader: &mut (dyn tokio::io::AsyncRead + Unpin + Send),
) -> anyhow::Result<()> {
    let (cols, rows) = terminal::size()?;
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into());

    debug!(version = PROTOCOL_VERSION, %term, cols, rows, "sending handshake");
    codec
        .write_message(
            writer,
            &Message::Hello {
                version: PROTOCOL_VERSION,
                term,
                cols,
                rows,
            },
        )
        .await?;

    match read_codec.read_message(reader).await {
        Ok(Message::HelloOk { .. }) => {
            info!("handshake ok");
            Ok(())
        }
        Ok(Message::Error { message, .. }) => anyhow::bail!("server error: {message}"),
        Ok(_) => anyhow::bail!("unexpected response to Hello"),
        Err(tether_protocol::codec::CodecError::ConnectionClosed) => {
            anyhow::bail!("daemon not reachable — is tetherd running on the remote host?")
        }
        Err(e) => Err(e.into()),
    }
}

async fn run_session(
    host: &Option<String>,
    socket: &Option<String>,
    name: Option<String>,
    cmd: Option<String>,
    attach_only: bool,
) -> anyhow::Result<()> {
    let (mut reader, mut writer, mut _child) = connect(host, socket, false).await?;
    let write_codec = FrameCodec::new();
    let mut read_codec = FrameCodec::new();

    handshake(
        &write_codec,
        &mut read_codec,
        writer.as_mut(),
        reader.as_mut(),
    )
    .await?;

    let (cols, rows) = terminal::size()?;

    let session_id = if attach_only {
        let id = name.unwrap();
        info!(session = %id, "attaching to existing session");
        write_codec
            .write_message(
                writer.as_mut(),
                &Message::SessionAttach { id: id.clone() },
            )
            .await?;
        id
    } else {
        write_codec
            .write_message(
                writer.as_mut(),
                &Message::SessionCreate {
                    id: name,
                    cmd,
                    cols,
                    rows,
                    env: session_env(),
                },
            )
            .await?;

        info!("creating new session");
        let id = match read_codec.read_message(reader.as_mut()).await? {
            Message::SessionCreated { id } => {
                info!(session = %id, "session created");
                id
            }
            Message::Error { message, .. } => anyhow::bail!("failed to create session: {message}"),
            _ => anyhow::bail!("unexpected response"),
        };

        write_codec
            .write_message(
                writer.as_mut(),
                &Message::SessionAttach { id: id.clone() },
            )
            .await?;
        id
    };

    let _guard = match read_codec.read_message(reader.as_mut()).await? {
        Message::SessionState(state) => {
            let guard = RawModeGuard::enable()?;
            let mut stdout = std::io::stdout();
            execute!(
                stdout,
                terminal::Clear(terminal::ClearType::All),
                cursor::MoveTo(0, 0)
            )?;
            render::render_snapshot(&state, &mut stdout)?;
            stdout.write_all(b"\x1b[0m")?;
            stdout.flush()?;
            guard
        }
        Message::HelloOk { .. } => {
            let guard = RawModeGuard::enable()?;
            std::io::stdout().write_all(b"\x1b[2J\x1b[H")?;
            std::io::stdout().flush()?;
            guard
        }
        Message::Error { message, .. } => anyhow::bail!("attach failed: {message}"),
        _ => RawModeGuard::enable()?,
    };

    // Spawn a stdin reader thread — shared across io_loop and reconnection
    let (stdin_tx, mut stdin_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe {
                libc::read(libc::STDIN_FILENO, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
            };
            if n <= 0 {
                break;
            }
            if stdin_tx.blocking_send(buf[..n as usize].to_vec()).is_err() {
                break;
            }
        }
    });

    // I/O loop with automatic reconnection on connection loss
    let result = io_loop(
        &write_codec,
        &mut read_codec,
        writer.as_mut(),
        reader.as_mut(),
        &session_id,
        &mut stdin_rx,
    )
    .await;

    match result {
        Ok(()) => Ok(()),
        Err(ref e) => {
            // Check if this was a connection loss (not a deliberate detach/exit)
            let err_msg = format!("{e}");
            let is_connection_loss = err_msg.contains("connection closed")
                || err_msg.contains("Broken pipe")
                || err_msg.contains("Connection reset");
            if !is_connection_loss {
                warn!(error = %err_msg, "io_loop exited with non-connection error");
                return result;
            }

            warn!(error = %err_msg, session = %session_id, "connection lost");

            // Kill the old SSH process so buffered keystrokes aren't
            // delivered to the remote when the network recovers.
            if let Some(ref mut child) = _child {
                info!("killing old ssh process");
                let _ = child.kill().await;
            }
            drop(writer);
            drop(reader);

            // Reconnect loop with exponential backoff.
            // Schedule: 1s, 2s, 4s, 8s, 15s, 15s, 15s, ...
            let mut delay_secs: u64 = 1;
            let max_delay_secs: u64 = 15;
            let mut attempt = 0u32;

            {
                let mut out = std::io::stdout();
                let _ = write!(out, "\r\n");
                let _ = out.flush();
            }

            loop {
                attempt += 1;

                // Ticking countdown so the user sees progress
                for remaining in (1..=delay_secs).rev() {
                    {
                        let mut out = std::io::stdout();
                        let _ = write!(out, "\r\x1b[2K[connection lost, retrying in {remaining}s... ctrl-c to quit]");
                        let _ = out.flush();
                    }
                    let sleep = tokio::time::sleep(std::time::Duration::from_secs(1));
                    tokio::pin!(sleep);
                    let interrupted = loop {
                        tokio::select! {
                            _ = &mut sleep => break false,
                            Some(data) = stdin_rx.recv() => {
                                if data.contains(&0x03) || data.contains(&DETACH_BYTE) || data.contains(&0x04) {
                                    break true;
                                }
                                // Non-interrupt keypress — ignore and keep waiting
                            }
                        }
                    };
                    if interrupted {
                        info!("user interrupted reconnect loop");
                        return Ok(());
                    }
                }

                {
                    let mut out = std::io::stdout();
                    let _ = write!(out, "\r\x1b[2K[reconnecting... attempt {attempt}]");
                    let _ = out.flush();
                }

                // Reconnect attempt — interruptible via stdin, with timeout.
                info!(session = %session_id, attempt, "attempting reconnect");
                let reconnect_timeout = std::time::Duration::from_secs(10);
                let result = tokio::select! {
                    r = tokio::time::timeout(reconnect_timeout, try_reconnect(host, socket, &session_id)) => {
                        match r {
                            Ok(inner) => inner,
                            Err(_) => Err(anyhow::anyhow!("reconnect timed out")),
                        }
                    }
                    Some(data) = stdin_rx.recv() => {
                        if data.contains(&0x03) || data.contains(&DETACH_BYTE) || data.contains(&0x04) {
                            return Ok(());
                        }
                        Err(anyhow::anyhow!("interrupted"))
                    }
                };
                match result {
                    Ok((reader, writer, read_codec, write_codec, _child)) => {
                        info!(session = %session_id, "reconnected successfully");
                        let mut reader = reader;
                        let mut writer = writer;
                        let mut read_codec = read_codec;
                        let result = io_loop(
                            &write_codec,
                            &mut read_codec,
                            writer.as_mut(),
                            reader.as_mut(),
                            &session_id,
                            &mut stdin_rx,
                        ).await;
                        match result {
                            Ok(()) => return Ok(()),
                            Err(ref e) => {
                                warn!(error = %e, "connection lost again after reconnect");
                                delay_secs = 1;
                                attempt = 0;
                            }
                        }
                    }
                    Err(e) => {
                        let msg = format!("{e}");
                        warn!(error = %msg, "reconnect attempt failed");
                        if msg.contains("reattach failed") {
                            let mut out = std::io::stdout();
                            let _ = write!(out, "\r\x1b[2K[session no longer exists]\r\n");
                            let _ = out.flush();
                            return Ok(());
                        }
                        delay_secs = (delay_secs * 2).min(max_delay_secs);
                    }
                }
            }
        }
    }
}

/// Attempt to reconnect and reattach to a session. Returns the connection
/// components on success so the caller can enter io_loop with stdin_rx.
async fn try_reconnect(
    host: &Option<String>,
    socket: &Option<String>,
    session_id: &str,
) -> anyhow::Result<(
    Box<dyn tokio::io::AsyncRead + Unpin + Send>,
    Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
    FrameCodec,
    FrameCodec,
    Option<tokio::process::Child>,
)> {
    debug!(session = %session_id, "try_reconnect: connecting");
    let (mut reader, mut writer, _child) = connect(host, socket, true).await?;
    debug!("try_reconnect: connected, starting handshake");
    let write_codec = FrameCodec::new();
    let mut read_codec = FrameCodec::new();

    handshake(
        &write_codec,
        &mut read_codec,
        writer.as_mut(),
        reader.as_mut(),
    )
    .await?;

    debug!(session = %session_id, "try_reconnect: handshake ok, attaching");
    write_codec
        .write_message(
            writer.as_mut(),
            &Message::SessionAttach { id: session_id.to_string() },
        )
        .await?;

    match read_codec.read_message(reader.as_mut()).await? {
        Message::SessionState(state) => {
            debug!("try_reconnect: got session state snapshot");
            let mut stdout = std::io::stdout();
            execute!(
                stdout,
                terminal::Clear(terminal::ClearType::All),
                cursor::MoveTo(0, 0)
            )?;
            render::render_snapshot(&state, &mut stdout)?;
            stdout.write_all(b"\x1b[0m")?;
            stdout.flush()?;
        }
        Message::HelloOk { .. } => {}
        Message::Error { message, .. } => anyhow::bail!("reattach failed: {message}"),
        _ => {}
    }

    Ok((reader, writer, read_codec, write_codec, _child))
}

async fn io_loop(
    write_codec: &FrameCodec,
    read_codec: &mut FrameCodec,
    writer: &mut (dyn tokio::io::AsyncWrite + Unpin + Send),
    reader: &mut (dyn tokio::io::AsyncRead + Unpin + Send),
    session_id: &str,
    stdin_rx: &mut tokio::sync::mpsc::Receiver<Vec<u8>>,
) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout();

    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;
    let mut sigint =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    // Timeout for writes — if SSH is dead but hasn't closed the pipe,
    // writes will block until the pipe buffer fills. This detects it early.
    let write_timeout = std::time::Duration::from_secs(5);

    loop {
        tokio::select! {
            result = read_codec.read_message(reader) => {
                match result {
                    Ok(Message::Data(data)) => {
                        stdout.write_all(&data)?;
                        stdout.flush()?;
                    }
                    Ok(Message::SessionExited { id, exit_code }) => {
                        info!(session = %id, exit_code, "session exited");
                        return Ok(());
                    }
                    Ok(Message::Ping { seq }) => {
                        // Reply to daemon keepalive
                        let _ = write_codec.write_message(writer, &Message::Pong { seq }).await;
                    }
                    Ok(Message::Pong { .. }) => {}
                    Ok(msg) => {
                        debug!("unexpected message: {:?}", msg.type_id());
                    }
                    Err(tether_protocol::codec::CodecError::ConnectionClosed) => {
                        warn!("io_loop: read returned connection closed");
                        anyhow::bail!("connection closed");
                    }
                    Err(e) => {
                        warn!(error = %e, "io_loop: read error");
                        return Err(e.into());
                    }
                }
            }

            Some(data) = stdin_rx.recv() => {
                // Ctrl-\ detaches immediately — don't wait for the write
                if data.contains(&DETACH_BYTE) {
                    info!(session = %session_id, "detach requested (ctrl-\\)");
                    // Best-effort send, don't block on dead connection
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_millis(500),
                        write_codec.write_message(writer, &Message::SessionDetach),
                    ).await;
                    eprintln!("\r\n[detached from {}]", session_id);
                    return Ok(());
                }

                // Normal data — timeout protects against dead connection
                match tokio::time::timeout(
                    write_timeout,
                    write_codec.write_message(writer, &Message::Data(data)),
                ).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => {
                        warn!(error = %e, "io_loop: write error");
                        anyhow::bail!("connection closed");
                    }
                    Err(_) => {
                        warn!("io_loop: write timed out after {}s", write_timeout.as_secs());
                        anyhow::bail!("connection closed");
                    }
                }
            }

            _ = sigwinch.recv() => {
                if let Ok((cols, rows)) = terminal::size() {
                    let _ = tokio::time::timeout(
                        write_timeout,
                        write_codec.write_message(writer, &Message::Resize { cols, rows }),
                    ).await;
                }
            }

            _ = sigint.recv() => {
                let _ = tokio::time::timeout(
                    write_timeout,
                    write_codec.write_message(writer, &Message::Data(vec![0x03])),
                ).await;
            }
        }
    }
}

fn session_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    env.insert("TERM".into(), "xterm-256color".into());
    if let Ok(val) = std::env::var("COLORTERM") {
        env.insert("COLORTERM".into(), val);
    }
    env
}

fn format_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

fn shorten_path(path: &str) -> String {
    if let Ok(home) = std::env::var("HOME")
        && let Some(rest) = path.strip_prefix(&home)
    {
        return format!("~{rest}");
    }
    path.to_string()
}

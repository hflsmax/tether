mod render;

use std::collections::HashMap;
use std::io::Write;
use std::process::Stdio;

use clap::Parser;
use crossterm::{cursor, execute, terminal};
use tokio::process::Command;
use tracing::{debug, info};

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
#[command(name = "tether", version, override_usage = "tether <user@host>\n       tether --socket <path>")]
struct Cli {
    /// Remote host (user@host)
    host: Option<String>,

    /// Use direct Unix socket connection (no SSH)
    #[arg(long)]
    socket: Option<String>,
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

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

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
                let (mut r, mut w, _) = connect(host, socket).await?;
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
                write!(out, "\x1b[2m  {:<18} {:<12} {:<24} {:<8} {} (attached)\x1b[0m\r\n",
                    s.id, proc_name, cwd, age, idle)?;
            } else {
                if sel == idx { write!(out, "\x1b[7m")?; }
                write!(out, "{} {:<18} {:<12} {:<24} {:<8} {}\x1b[0m\r\n",
                    if sel == idx { ">" } else { " " }, s.id, proc_name, cwd, age, idle)?;
            }
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
                    let mut next = sel as i32 - 1;
                    while next >= 0 {
                        if is_selectable(next as usize) { sel = next as usize; break; }
                        next -= 1;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') if !ctrl => {
                    let mut next = sel + 1;
                    while next < total {
                        if is_selectable(next) { sel = next; break; }
                        next += 1;
                    }
                }
                KeyCode::Enter => {
                    leave(&mut out)?;
                    if sel == 0 {
                        return Ok(PickerAction::New);
                    }
                    return Ok(PickerAction::Resume(sessions[sel - 1].id.clone()));
                }
                KeyCode::Char('x') if !ctrl && sel > 0 && is_selectable(sel) => {
                    let id = sessions[sel - 1].id.clone();
                    kill_session(&id, host, socket)?;
                    sessions.remove(sel - 1);
                    if sel > sessions.len() {
                        sel = sessions.len();
                    }
                    while sel > 0 && sel <= sessions.len() && sessions[sel - 1].attached {
                        sel -= 1;
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
    let (mut reader, mut writer, _child) = connect(host, socket).await?;
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

    let has_detached = sessions.iter().any(|s| !s.attached);

    drop(reader);
    drop(writer);

    if sessions.is_empty() || !has_detached {
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
) -> anyhow::Result<(
    Box<dyn tokio::io::AsyncRead + Unpin + Send>,
    Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
    Option<tokio::process::Child>,
)> {
    if let Some(socket_path) = socket {
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
        let mut child = Command::new("ssh")
            .arg("-o").arg("ServerAliveInterval=5")
            .arg("-o").arg("ServerAliveCountMax=2")
            .arg(host)
            .arg("tether-proxy")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
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
        Ok(Message::HelloOk { .. }) => Ok(()),
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
    let (mut reader, mut writer, mut _child) = connect(host, socket).await?;
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

        let id = match read_codec.read_message(reader.as_mut()).await? {
            Message::SessionCreated { id } => id,
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

    // I/O loop with automatic reconnection on connection loss
    let result = io_loop(
        &write_codec,
        &mut read_codec,
        writer.as_mut(),
        reader.as_mut(),
        &session_id,
    )
    .await;

    match result {
        Ok(()) => Ok(()),
        Err(ref e) => {
            // Check if this was a connection loss (not a deliberate detach/exit)
            let is_connection_loss = format!("{e}").contains("connection closed")
                || format!("{e}").contains("Broken pipe")
                || format!("{e}").contains("Connection reset");
            if !is_connection_loss {
                return result;
            }

            // Kill the old SSH process so buffered keystrokes aren't
            // delivered to the remote when the network recovers.
            if let Some(ref mut child) = _child {
                let _ = child.kill().await;
            }
            drop(writer);
            drop(reader);

            // Reconnect loop with exponential backoff
            let mut delay = std::time::Duration::from_millis(100);
            let max_delay = std::time::Duration::from_secs(30);
            loop {
                {
                    let mut out = std::io::stdout();
                    let _ = write!(out, "\r\n[connection lost, reconnecting in {}s...]\r\n",
                        delay.as_secs().max(1));
                    let _ = out.flush();
                }
                tokio::time::sleep(delay).await;

                match reconnect_session(host, socket, &session_id).await {
                    Ok(()) => return Ok(()),
                    Err(_) => {
                        delay = (delay * 2).min(max_delay);
                    }
                }
            }
        }
    }
}

async fn reconnect_session(
    host: &Option<String>,
    socket: &Option<String>,
    session_id: &str,
) -> anyhow::Result<()> {
    let (mut reader, mut writer, mut _child) = connect(host, socket).await?;
    let write_codec = FrameCodec::new();
    let mut read_codec = FrameCodec::new();

    handshake(
        &write_codec,
        &mut read_codec,
        writer.as_mut(),
        reader.as_mut(),
    )
    .await?;

    write_codec
        .write_message(
            writer.as_mut(),
            &Message::SessionAttach { id: session_id.to_string() },
        )
        .await?;

    match read_codec.read_message(reader.as_mut()).await? {
        Message::SessionState(state) => {
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

    {
        let mut out = std::io::stdout();
        let _ = write!(out, "\r\n[reconnected]\r\n");
        let _ = out.flush();
    }

    io_loop(
        &write_codec,
        &mut read_codec,
        writer.as_mut(),
        reader.as_mut(),
        session_id,
    )
    .await
}

async fn io_loop(
    write_codec: &FrameCodec,
    read_codec: &mut FrameCodec,
    writer: &mut (dyn tokio::io::AsyncWrite + Unpin + Send),
    reader: &mut (dyn tokio::io::AsyncRead + Unpin + Send),
    session_id: &str,
) -> anyhow::Result<()> {
    let mut stdout = std::io::stdout();

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
                    Ok(Message::Pong { .. }) => {}
                    Ok(msg) => {
                        debug!("unexpected message: {:?}", msg.type_id());
                    }
                    Err(tether_protocol::codec::CodecError::ConnectionClosed) => {
                        anyhow::bail!("connection closed");
                    }
                    Err(e) => return Err(e.into()),
                }
            }

            Some(data) = stdin_rx.recv() => {
                // Ctrl-\ detaches immediately — don't wait for the write
                if data.contains(&DETACH_BYTE) {
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
                    _ => anyhow::bail!("connection closed"),
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

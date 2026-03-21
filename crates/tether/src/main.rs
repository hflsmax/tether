mod render;

use std::collections::HashMap;
use std::io::Write;
use std::process::Stdio;

use clap::{Parser, Subcommand};
use crossterm::{cursor, execute, terminal};
use tokio::process::Command;
use tracing::{debug, info};

use tether_protocol::{FrameCodec, Message, PROTOCOL_VERSION};

const DETACH_BYTE: u8 = 0x1c; // Ctrl-backslash

/// Guard that restores terminal state on drop (panic, early return, etc.)
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
            // Print a newline so the shell prompt starts on a fresh line
            let _ = std::io::stdout().write_all(b"\r\n");
            let _ = std::io::stdout().flush();
        }
    }
}

#[derive(Parser)]
#[command(name = "tether", about = "Tether — persistent terminal sessions")]
struct Cli {
    /// Remote host (user@host)
    #[arg(short = 'H', long, env = "TETHER_HOST")]
    host: Option<String>,

    /// Use direct Unix socket connection (no SSH)
    #[arg(long)]
    socket: Option<String>,

    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create and attach to a new session
    New {
        /// Session name (auto-generated if omitted)
        #[arg(short, long)]
        name: Option<String>,
        /// Shell command to run
        #[arg(short, long)]
        cmd: Option<String>,
    },
    /// Attach to an existing session
    Attach {
        /// Session name
        name: String,
    },
    /// List sessions
    #[command(alias = "ls")]
    List,
    /// Destroy a session
    Destroy {
        /// Session name
        name: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Cmd::New { name, cmd } => {
            run_session(&cli.host, &cli.socket, name, cmd, false).await?;
        }
        Cmd::Attach { name } => {
            run_session(&cli.host, &cli.socket, Some(name), None, true).await?;
        }
        Cmd::List => {
            list_sessions(&cli.host, &cli.socket).await?;
        }
        Cmd::Destroy { name } => {
            destroy_session(&cli.host, &cli.socket, &name).await?;
        }
    }

    Ok(())
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
        let stream = tokio::net::UnixStream::connect(socket_path).await?;
        let (r, w) = stream.into_split();
        Ok((Box::new(r), Box::new(w), None))
    } else if let Some(host) = host {
        let mut child = Command::new("ssh")
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
        anyhow::bail!("either --host or --socket must be specified");
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

    match read_codec.read_message(reader).await? {
        Message::HelloOk { .. } => Ok(()),
        Message::Error { message, .. } => anyhow::bail!("server error: {message}"),
        _ => anyhow::bail!("unexpected response to Hello"),
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
        // Create session
        write_codec
            .write_message(
                writer.as_mut(),
                &Message::SessionCreate {
                    id: name,
                    cmd,
                    cols,
                    rows,
                    env: HashMap::new(),
                },
            )
            .await?;

        let id = match read_codec.read_message(reader.as_mut()).await? {
            Message::SessionCreated { id } => id,
            Message::Error { message, .. } => anyhow::bail!("failed to create session: {message}"),
            _ => anyhow::bail!("unexpected response"),
        };

        // Now attach
        write_codec
            .write_message(
                writer.as_mut(),
                &Message::SessionAttach { id: id.clone() },
            )
            .await?;
        id
    };

    // Wait for SessionState snapshot, then enter raw mode with cleanup guard
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
            guard
        }
        Message::Error { message, .. } => {
            anyhow::bail!("attach failed: {message}");
        }
        _ => RawModeGuard::enable()?,
    };

    // Main I/O loop — _guard ensures terminal is restored on any exit path
    io_loop(
        &write_codec,
        &mut read_codec,
        writer.as_mut(),
        reader.as_mut(),
        &session_id,
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

    // Dedicated thread reading raw bytes from fd 0 via direct syscall.
    // Bypasses std::io::Stdin's BufReader to avoid any buffering issues
    // in raw terminal mode. Cannot use tokio::io::stdin() in a select!
    // loop either — its internal spawn_blocking loses data on cancellation.
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

    // Listen for SIGWINCH (terminal resize)
    let mut sigwinch =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::window_change())?;

    // Catch SIGINT so it doesn't kill the process.
    // In raw mode the terminal won't generate SIGINT from Ctrl-C (ISIG is off),
    // but catch it anyway for robustness (e.g. `kill -INT` from another shell).
    // Forward it as byte 0x03 to the PTY.
    let mut sigint =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    loop {
        tokio::select! {
            // Read from server
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
                        return Ok(());
                    }
                    Err(e) => return Err(e.into()),
                }
            }

            // Read raw bytes from dedicated stdin thread
            Some(data) = stdin_rx.recv() => {
                // Check for detach key (Ctrl-\, 0x1c) anywhere in the chunk
                if let Some(pos) = data.iter().position(|&b| b == DETACH_BYTE) {
                    if pos > 0 {
                        write_codec
                            .write_message(writer, &Message::Data(data[..pos].to_vec()))
                            .await?;
                    }
                    write_codec.write_message(writer, &Message::SessionDetach).await?;
                    eprintln!("\r\n[detached from {}]", session_id);
                    return Ok(());
                }

                write_codec
                    .write_message(writer, &Message::Data(data))
                    .await?;
            }

            // Handle terminal resize
            _ = sigwinch.recv() => {
                if let Ok((cols, rows)) = terminal::size() {
                    write_codec
                        .write_message(writer, &Message::Resize { cols, rows })
                        .await?;
                }
            }

            // Handle SIGINT — forward as Ctrl-C byte to PTY
            _ = sigint.recv() => {
                write_codec
                    .write_message(writer, &Message::Data(vec![0x03]))
                    .await?;
            }
        }
    }
}

async fn list_sessions(
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

    match read_codec.read_message(reader.as_mut()).await? {
        Message::SessionListResp { sessions } => {
            if sessions.is_empty() {
                println!("no sessions");
            } else {
                println!("{:<20} {:<10} {:<10}", "NAME", "STATUS", "IDLE");
                for s in sessions {
                    let status = if s.attached { "attached" } else { "detached" };
                    let idle = format_duration(s.idle_secs);
                    println!("{:<20} {:<10} {:<10}", s.id, status, idle);
                }
            }
        }
        Message::Error { message, .. } => {
            anyhow::bail!("error: {message}");
        }
        _ => anyhow::bail!("unexpected response"),
    }

    Ok(())
}

async fn destroy_session(
    host: &Option<String>,
    socket: &Option<String>,
    name: &str,
) -> anyhow::Result<()> {
    let (mut reader, mut writer, _child) = connect(host, socket).await?;
    let write_codec = FrameCodec::new();
    let mut read_codec = FrameCodec::new();

    handshake(&write_codec, &mut read_codec, writer.as_mut(), reader.as_mut()).await?;

    write_codec
        .write_message(
            writer.as_mut(),
            &Message::SessionDestroy { id: name.into() },
        )
        .await?;

    match read_codec.read_message(reader.as_mut()).await? {
        Message::SessionCreated { id } => {
            println!("destroyed session: {id}");
        }
        Message::Error { message, .. } => {
            anyhow::bail!("error: {message}");
        }
        _ => anyhow::bail!("unexpected response"),
    }

    Ok(())
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

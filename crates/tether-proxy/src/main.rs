use std::path::PathBuf;

use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tracing::debug;

/// tether-proxy: stdin/stdout ↔ Unix socket bridge.
///
/// This is the ProxyCommand invoked over SSH. It connects to the daemon's
/// unix socket and bidirectionally copies framed protocol data between
/// stdin/stdout and the socket.
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("tether-proxy {} ({} {})", env!("CARGO_PKG_VERSION"), env!("GIT_COMMIT_HASH"), env!("GIT_COMMIT_DATE"));
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let socket_path = socket_path();
    debug!("connecting to {}", socket_path.display());

    let stream = UnixStream::connect(&socket_path).await.map_err(|e| {
        match e.kind() {
            std::io::ErrorKind::NotFound => {
                anyhow::anyhow!("daemon not running (socket not found: {})", socket_path.display())
            }
            std::io::ErrorKind::ConnectionRefused => {
                anyhow::anyhow!("daemon not accepting connections ({})", socket_path.display())
            }
            _ => anyhow::anyhow!("failed to connect to daemon at {}: {e}", socket_path.display()),
        }
    })?;
    let (mut sock_reader, mut sock_writer) = stream.into_split();
    let mut stdin = io::stdin();
    let mut stdout = io::stdout();

    // Bidirectional copy: stdin → socket, socket → stdout
    let stdin_to_sock = async {
        let mut buf = vec![0u8; 8192];
        loop {
            let n = stdin.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            sock_writer.write_all(&buf[..n]).await?;
            sock_writer.flush().await?;
        }
        sock_writer.shutdown().await?;
        Ok::<_, anyhow::Error>(())
    };

    let sock_to_stdout = async {
        let mut buf = vec![0u8; 8192];
        loop {
            let n = sock_reader.read(&mut buf).await?;
            if n == 0 {
                break;
            }
            stdout.write_all(&buf[..n]).await?;
            stdout.flush().await?;
        }
        Ok::<_, anyhow::Error>(())
    };

    tokio::select! {
        r = stdin_to_sock => { r?; }
        r = sock_to_stdout => { r?; }
    }

    Ok(())
}

fn socket_path() -> PathBuf {
    if let Ok(path) = std::env::var("TETHER_SOCKET") {
        return PathBuf::from(path);
    }
    // Try XDG_RUNTIME_DIR first (Home Manager / manual setup)
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let path = PathBuf::from(&runtime_dir).join("tether.sock");
        if path.exists() {
            return path;
        }
    }
    // Try /run/tether/<user>/ (NixOS system module)
    let user = std::env::var("USER").unwrap_or_default();
    let system_path = PathBuf::from(format!("/run/tether/{user}/tether.sock"));
    if system_path.exists() {
        return system_path;
    }
    // Default fallback
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", nix::unistd::getuid()));
    PathBuf::from(runtime_dir).join("tether.sock")
}

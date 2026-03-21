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
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .with_writer(std::io::stderr)
        .init();

    let socket_path = socket_path();
    debug!("connecting to {}", socket_path.display());

    let stream = UnixStream::connect(&socket_path).await?;
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
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR")
        .unwrap_or_else(|_| format!("/run/user/{}", nix::unistd::getuid()));
    PathBuf::from(runtime_dir).join("tether.sock")
}

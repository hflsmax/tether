use clap::Parser;
use std::path::PathBuf;
use tracing::info;

use tether_daemon::{Config, Server};

#[derive(Parser)]
#[command(name = "tetherd", about = "Tether daemon — persistent PTY session manager")]
struct Cli {
    /// Path to config file
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Override socket path
    #[arg(long)]
    socket: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();

    let mut config = if let Some(path) = cli.config {
        Config::load(&path)?
    } else {
        // Try default config locations
        let config_path = dirs_config_path();
        if config_path.exists() {
            Config::load(&config_path)?
        } else {
            Config::default()
        }
    };

    if let Some(socket) = cli.socket {
        config.socket_path = socket;
    }

    info!("starting tetherd");
    let server = Server::new(config);
    server.run().await
}

fn dirs_config_path() -> PathBuf {
    let config_dir = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        });
    config_dir.join("tether").join("config.toml")
}

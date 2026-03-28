# tether

Persistent terminal sessions over SSH. Survive disconnections, resume instantly.

## Why

AI coding agents running on remote machines need stable terminal sessions. SSH alone drops your session on any network hiccup. tmux works but adds complexity — extra keybindings, nested terminal emulation, and configuration that fights with your local setup. Eternal Terminal solves reconnection but runs its own TCP daemon, exposing another port.

tether takes a different approach: all traffic goes through SSH. The daemon listens on a Unix socket, and a lightweight proxy bridges SSH's stdio to that socket. Your existing SSH config, keys, and firewall rules work unchanged. Nothing new is exposed to the network.

## How it works

```
Local Machine                     Remote Machine
┌──────────┐    SSH tunnel    ┌──────────────────────────┐
│  tether  │ ──────────────>  │  tether-proxy            │
│ (client) │  stdin/stdout    │  (stdio ↔ unix socket)   │
│          │  framed protocol │         │                 │
└──────────┘                  │         ▼                 │
                              │  tetherd (user daemon)    │
                              │  ├─ Session "bright-fox"  │
                              │  │  ├─ PTY (zsh)          │
                              │  │  └─ Terminal model     │
                              │  └─ Session "calm-river"  │
                              │     ├─ PTY (vim)          │
                              │     └─ Terminal model     │
                              └──────────────────────────┘
```

Three binaries, each with a single job:

- **`tether`** (client, runs on your laptop) — connects to the remote host over SSH, displays the session picker, and handles terminal I/O. Automatically reconnects on network loss.
- **`tether-proxy`** (remote) — invoked by SSH on the remote host. Bridges SSH's stdin/stdout to the daemon's Unix socket. Stateless and lightweight.
- **`tetherd`** (remote, long-running daemon) — manages persistent PTY sessions. Tracks terminal state with [alacritty_terminal](https://github.com/alacritty/alacritty) so reconnections get a structured screen snapshot — not a raw escape sequence replay. Sessions persist until you close them or they idle out.

## Install

### Client (macOS)

```sh
brew install hflsmax/tether/tether
```

Or download manually from [releases](https://github.com/hflsmax/tether/releases):

```sh
curl -L https://github.com/hflsmax/tether/releases/latest/download/tether-aarch64-apple-darwin -o /usr/local/bin/tether
chmod +x /usr/local/bin/tether
```

### Client (Linux)

```sh
sudo curl -fSL https://github.com/hflsmax/tether/releases/latest/download/tether-x86_64-unknown-linux-gnu -o /usr/local/bin/tether
sudo chmod +x /usr/local/bin/tether
```

### Server (Ubuntu / Debian)

Download and install the `.deb` package from [releases](https://github.com/hflsmax/tether/releases):

```sh
curl -fSLO https://github.com/hflsmax/tether/releases/latest/download/tether_0.1.3_amd64.deb
sudo dpkg -i tether_0.1.3_amd64.deb
```

Enable for your user:

```sh
sudo systemctl enable --now tetherd@$USER
```

Or use the install script:

```sh
curl -fsSL https://raw.githubusercontent.com/hflsmax/tether/main/dist/install.sh | sudo bash
sudo systemctl enable --now tetherd@$USER
```

#### Per-user install (no root)

If you don't have root access, install as a systemd user service. Requires `loginctl enable-linger` (ask an admin to run it once):

```sh
curl -fsSL https://raw.githubusercontent.com/hflsmax/tether/main/dist/install.sh | bash -s -- --user
systemctl --user enable --now tetherd
```

Binaries go to `~/.local/bin` and the service to `~/.config/systemd/user/`. The script will tell you how to add `~/.local/bin` to your PATH if it isn't already.

### Server (NixOS)

Add tether to your flake inputs and import the module:

```nix
# flake.nix
inputs.tether.url = "github:hflsmax/tether";

# In your nixosSystem modules:
modules = [
  tether.nixosModules.default
  ./configuration.nix
];
```

Then in `configuration.nix`:

```nix
services.tether.enable = true;
```

This starts a `tetherd` service per user at boot (no login required), with the socket at `/run/tether/<user>/tether.sock`.

Optional settings:

```nix
services.tether.settings = {
  idleTimeout = "48h";
  scrollbackLines = 20000;
  maxSessions = 50;
};

# Restrict to specific users (defaults to all normal users)
services.tether.users = [ "alice" "bob" ];
```

### Server (other Linux)

Download the binaries and systemd service:

```sh
sudo curl -fSL https://github.com/hflsmax/tether/releases/latest/download/tetherd-x86_64-unknown-linux-gnu -o /usr/local/bin/tetherd
sudo curl -fSL https://github.com/hflsmax/tether/releases/latest/download/tether-proxy-x86_64-unknown-linux-gnu -o /usr/local/bin/tether-proxy
sudo chmod +x /usr/local/bin/tetherd /usr/local/bin/tether-proxy
sudo curl -fSL https://raw.githubusercontent.com/hflsmax/tether/main/dist/tetherd@.service -o /etc/systemd/system/tetherd@.service
sudo systemctl daemon-reload
sudo systemctl enable --now tetherd@$USER
```

Without root, use the [per-user install](#per-user-install-no-root) instead.

Or run manually:

```sh
tetherd  # listens on $XDG_RUNTIME_DIR/tether.sock
```

Make sure `tether-proxy` is in your PATH so SSH can find it.

## Usage

```sh
tether user@host
```

That's it. If no sessions exist, one is created. If detached sessions exist, a picker appears:

```
  NAME               RUNNING      CWD                      AGE      IDLE
> [new session]
  bright-fox         vim          ~/src/project            2h       15m
  calm-river         cargo        ~/src/tether             30m      5m

  enter: select  x: kill  q: quit
```

### Inside a session

- **Ctrl-\\** — detach (session keeps running)
- **Ctrl-D** — exit shell (session is destroyed)
- Everything else works normally — vim, htop, cargo watch, AI agents, etc.

### Reconnecting

Just run `tether user@host` again. If you had one detached session, it resumes automatically. The terminal is restored to where you left off.

If the network drops (laptop sleep, Wi-Fi change), the client reconnects automatically with exponential backoff — no manual action needed.

## Configuration

For manual installs and `.deb`, the daemon reads `~/.config/tether/config.toml`:

```toml
idle_timeout = "24h"       # destroy detached sessions after this
scrollback_lines = 10000   # per-session scrollback buffer
max_sessions = 20          # max concurrent sessions per user
```

All settings are optional — the defaults above apply if the file doesn't exist.

On NixOS, configuration is managed via `services.tether.settings` in your NixOS config — the daemon receives a generated config file via `--config`.

## Comparison

| | tether | [tmux](https://github.com/tmux/tmux) | [Eternal Terminal](https://github.com/MystenLabs/EternalTerminal) | [mosh](https://github.com/mobile-shell/mosh) |
|---|---|---|---|---|
| Transport | SSH (no extra ports) | local only | Custom TCP (port 2022) | Custom UDP (port 60000+) |
| Reconnect | automatic | manual reattach | automatic | automatic |
| Terminal emulation | passthrough | nested (double-escape issues) | passthrough | custom (SSP) |
| Session persistence | daemon keeps PTY alive | server keeps PTY alive | server keeps PTY alive | server keeps PTY alive |
| Setup | daemon + proxy in PATH | install tmux | install etserver, open port | install mosh-server, open UDP ports |

## Building from source

Requires Rust 1.85+.

```sh
# All binaries (Linux)
cargo build --release

# Client only (macOS cross-compile from Linux)
cargo install cargo-zigbuild
rustup target add aarch64-apple-darwin
cargo zigbuild -p tether --target aarch64-apple-darwin --release
```

## License

MIT

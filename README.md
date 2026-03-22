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

The daemon owns the PTY sessions and tracks terminal state with [alacritty_terminal](https://github.com/alacritty/alacritty). When you reconnect, it sends a structured snapshot of the screen — not a raw escape sequence replay. The client renders the snapshot and resumes live I/O. Sessions persist until you close them or they idle out.

## Install

### Client (macOS)

Download from [releases](https://github.com/hflsmax/tether/releases):

```sh
mkdir -p ~/.local/bin
curl -L https://github.com/hflsmax/tether/releases/latest/download/tether-aarch64-apple-darwin -o ~/.local/bin/tether
chmod +x ~/.local/bin/tether
codesign -s - ~/.local/bin/tether
```

Add `~/.local/bin` to your PATH if needed (add to `~/.zshrc`):

```sh
export PATH="$HOME/.local/bin:$PATH"
```

### Server (NixOS)

Add to your flake inputs:

```nix
inputs.tether.url = "github:hflsmax/tether";
```

System module (makes binaries available):

```nix
imports = [ tether.nixosModules.default ];
services.tether.enable = true;
```

Home Manager module (starts daemon per user):

```nix
imports = [ tether.homeManagerModules.default ];
services.tether.enable = true;
```

### Server (other Linux)

Download the server binaries from [releases](https://github.com/hflsmax/tether/releases) and put `tetherd` and `tether-proxy` in your PATH:

```sh
curl -L https://github.com/hflsmax/tether/releases/latest/download/tetherd-x86_64-unknown-linux-gnu -o ~/.local/bin/tetherd
curl -L https://github.com/hflsmax/tether/releases/latest/download/tether-proxy-x86_64-unknown-linux-gnu -o ~/.local/bin/tether-proxy
chmod +x ~/.local/bin/tetherd ~/.local/bin/tether-proxy
```

Start the daemon:

```sh
tetherd  # listens on $XDG_RUNTIME_DIR/tether.sock
```

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

## Configuration

The daemon reads `~/.config/tether/config.toml`:

```toml
idle_timeout = "24h"       # destroy detached sessions after this
scrollback_lines = 10000   # per-session scrollback buffer
max_sessions = 20          # max concurrent sessions per user
```

All settings are optional — the defaults above apply if the file doesn't exist.

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

## Disclaimer

This project was mostly generated by AI (Claude). Use with caution — there may be bugs, edge cases, or security issues that haven't been caught. Review the code before using in any sensitive environment.

## License

MIT

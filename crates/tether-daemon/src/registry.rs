use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::mpsc;
use tracing::{debug, info};

use tether_protocol::SessionInfo;
use tether_session::{Session, SessionEvent, SessionHandle};

use crate::config::Config;

struct SessionEntry {
    handle: Arc<SessionHandle>,
    event_rx: Option<mpsc::Receiver<SessionEvent>>,
    /// Channel to forward PTY output to the attached client
    output_tx: Option<mpsc::Sender<Vec<u8>>>,
    detached_at: Option<Instant>,
    created_at: Instant,
    cmd: String,
}

pub struct Registry {
    sessions: HashMap<String, SessionEntry>,
    config: Config,
}

impl Registry {
    pub fn new(config: Config) -> Self {
        Self {
            sessions: HashMap::new(),
            config,
        }
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    pub fn create_session(
        &mut self,
        id: Option<String>,
        cmd: Option<String>,
        cols: u16,
        rows: u16,
        env: Vec<(String, String)>,
    ) -> Result<String, String> {
        if self.sessions.len() >= self.config.max_sessions {
            return Err("max sessions reached".into());
        }

        let id = id.unwrap_or_else(tether_session::id_gen::generate_id);

        if self.sessions.contains_key(&id) {
            return Err(format!("session '{id}' already exists"));
        }

        let cmd = cmd.as_deref().unwrap_or(&self.config.default_shell);

        let (handle, event_rx) = Session::spawn(
            id.clone(),
            cmd,
            cols,
            rows,
            env,
            self.config.scrollback_lines,
            self.config.raw_log_size,
        )
        .map_err(|e| e.to_string())?;

        self.sessions.insert(
            id.clone(),
            SessionEntry {
                handle: Arc::new(handle),
                event_rx: Some(event_rx),
                output_tx: None,
                detached_at: Some(Instant::now()),
                created_at: Instant::now(),
                cmd: cmd.to_string(),
            },
        );

        info!(session = %id, "session created");
        Ok(id)
    }

    #[allow(clippy::type_complexity)]
    pub fn attach(
        &mut self,
        id: &str,
    ) -> Result<(mpsc::Receiver<Vec<u8>>, Option<mpsc::Receiver<SessionEvent>>), String> {
        let entry = self.sessions.get_mut(id).ok_or_else(|| format!("session '{id}' not found"))?;

        if entry.output_tx.is_some() {
            return Err(format!("session '{id}' is already attached"));
        }

        let (tx, rx) = mpsc::channel(256);
        entry.output_tx = Some(tx);
        entry.detached_at = None;

        let event_rx = entry.event_rx.take();

        Ok((rx, event_rx))
    }

    /// Get an Arc clone of the session handle (cheap, no lock needed after this).
    pub fn take_handle(&self, id: &str) -> Option<Arc<SessionHandle>> {
        self.sessions.get(id).map(|e| e.handle.clone())
    }

    pub fn detach(&mut self, id: &str) {
        if let Some(entry) = self.sessions.get_mut(id) {
            entry.output_tx = None;
            entry.detached_at = Some(Instant::now());
            debug!(session = %id, "session detached");
        }
    }

    pub fn destroy(&mut self, id: &str) -> Result<(), String> {
        if self.sessions.remove(id).is_some() {
            info!(session = %id, "session destroyed");
            Ok(())
        } else {
            Err(format!("session '{id}' not found"))
        }
    }

    pub fn list(&self) -> Vec<SessionInfo> {
        self.sessions
            .iter()
            .map(|(id, entry)| {
                let pid = entry.handle.child_pid();
                let idle_secs = entry
                    .detached_at
                    .map(|t| t.elapsed().as_secs())
                    .unwrap_or(0);
                let cwd = std::fs::read_link(format!("/proc/{pid}/cwd"))
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let foreground_proc = read_foreground_proc(pid);
                SessionInfo {
                    id: id.clone(),
                    cols: 0,
                    rows: 0,
                    attached: entry.output_tx.is_some(),
                    idle_secs,
                    created_secs: entry.created_at.elapsed().as_secs(),
                    cmd: entry.cmd.clone(),
                    cwd,
                    foreground_proc,
                }
            })
            .collect()
    }

    pub fn get_output_tx(&self, id: &str) -> Option<&mpsc::Sender<Vec<u8>>> {
        self.sessions.get(id).and_then(|e| e.output_tx.as_ref())
    }

    pub fn check_idle_timeouts(&self) -> Vec<String> {
        let timeout = self.config.idle_timeout_duration();
        self.sessions
            .iter()
            .filter_map(|(id, entry)| {
                if let Some(detached_at) = entry.detached_at
                    && detached_at.elapsed() >= timeout
                {
                    return Some(id.clone());
                }
                None
            })
            .collect()
    }

    pub fn mark_exited(&mut self, id: &str) {
        self.sessions.remove(id);
        info!(session = %id, "session removed (exited)");
    }
}

/// Read the foreground process name for a shell PID.
/// Walks the child tree to find the leaf process (the one the user is interacting with).
fn read_foreground_proc(shell_pid: i32) -> String {
    // Find the deepest child — that's usually the foreground command.
    // Walk: shell_pid → child → grandchild → ...
    let mut pid = shell_pid;
    loop {
        // Read /proc/<pid>/task/<pid>/children to find child PIDs
        let children_path = format!("/proc/{pid}/task/{pid}/children");
        match std::fs::read_to_string(&children_path) {
            Ok(content) => {
                let child: Option<i32> = content
                    .split_whitespace()
                    .filter_map(|s| s.parse().ok())
                    .next();
                match child {
                    Some(c) => pid = c,
                    None => break,
                }
            }
            Err(_) => break,
        }
    }
    // Read the comm (process name) of the leaf process
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

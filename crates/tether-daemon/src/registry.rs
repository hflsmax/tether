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

        // Detach previous client if any
        if entry.output_tx.is_some() {
            debug!(session = %id, "detaching previous client");
            entry.output_tx = None;
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
                let idle_secs = entry
                    .detached_at
                    .map(|t| t.elapsed().as_secs())
                    .unwrap_or(0);
                SessionInfo {
                    id: id.clone(),
                    cols: 0,
                    rows: 0,
                    attached: entry.output_tx.is_some(),
                    idle_secs,
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

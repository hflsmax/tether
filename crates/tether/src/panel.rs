use std::io::Write;

use tether_protocol::SessionInfo;

pub enum PanelAction {
    /// Close panel, return to current session
    Cancel,
    /// Detach from session
    Detach,
    /// Switch to a different session
    SwitchTo(String),
    /// Create a new session
    NewSession,
    /// Kill a session (stays in panel)
    KillSession(String),
}

pub struct PanelState {
    current_session: String,
    sessions: Option<Vec<SessionInfo>>,
    selected: usize,
    cols: u16,
    rows: u16,
}

impl PanelState {
    pub fn new(current_session: String) -> Self {
        let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        Self {
            current_session,
            sessions: None,
            selected: 0,
            cols,
            rows,
        }
    }

    pub fn update_sessions(&mut self, mut sessions: Vec<SessionInfo>) {
        // Sort: current session first, then alphabetical
        sessions.sort_by(|a, b| {
            let a_current = a.id == self.current_session;
            let b_current = b.id == self.current_session;
            b_current.cmp(&a_current).then(a.id.cmp(&b.id))
        });
        // Default selection to current session (index 0 after sort)
        self.selected = 0;
        self.sessions = Some(sessions);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.cols = cols;
        self.rows = rows;
    }

    /// Total selectable items: sessions + [new session]
    fn item_count(&self) -> usize {
        match &self.sessions {
            Some(s) => s.len() + 1, // sessions + [new session]
            None => 0,
        }
    }

    fn is_selectable(&self, idx: usize) -> bool {
        let sessions = match &self.sessions {
            Some(s) => s,
            None => return false,
        };
        if idx < sessions.len() {
            let s = &sessions[idx];
            // Current session and detached sessions are selectable
            s.id == self.current_session || !s.attached
        } else {
            // [new session] is always selectable
            true
        }
    }

    pub fn render(&self, out: &mut impl Write) -> std::io::Result<()> {
        write!(out, "\x1b[H\x1b[2J")?; // clear screen, cursor to top

        write!(out, "\r\n  \x1b[1mtether\x1b[0m \u{2500} ctrl-\\ control panel\r\n\r\n")?;

        match &self.sessions {
            None => {
                write!(out, "  loading...\r\n")?;
            }
            Some(sessions) => {
                // Header
                write!(
                    out,
                    "  {:<20} {:<12} {:<24} {:<8} IDLE\r\n",
                    "NAME", "RUNNING", "CWD", "AGE"
                )?;

                for (i, s) in sessions.iter().enumerate() {
                    let is_current = s.id == self.current_session;
                    let proc_name = if s.foreground_proc.is_empty() {
                        "-"
                    } else {
                        &s.foreground_proc
                    };
                    let age = format_duration(s.created_secs);
                    let idle = format_duration(s.idle_secs);
                    let cwd = shorten_path(&s.cwd);

                    let marker = if is_current { "*" } else { " " };
                    let suffix = if s.attached && !is_current {
                        " (attached)"
                    } else {
                        ""
                    };

                    if s.attached && !is_current {
                        // Dim non-current attached sessions
                        write!(out, "\x1b[2m")?;
                    } else if self.selected == i {
                        write!(out, "\x1b[7m")?;
                    }

                    let arrow = if self.selected == i && self.is_selectable(i) {
                        ">"
                    } else {
                        " "
                    };
                    write!(
                        out,
                        "{} {:<18}{} {:<12} {:<24} {:<8} {}{}\x1b[0m\r\n",
                        arrow, s.id, marker, proc_name, cwd, age, idle, suffix
                    )?;
                }

                // [new session] entry
                let new_idx = sessions.len();
                if self.selected == new_idx {
                    write!(out, "\x1b[7m")?;
                }
                let arrow = if self.selected == new_idx { ">" } else { " " };
                write!(out, "{} [new session]\x1b[0m\r\n", arrow)?;
            }
        }

        write!(out, "\r\n")?;
        write!(
            out,
            "  enter: select  x: kill  d: detach  esc: back\r\n"
        )?;
        out.flush()?;
        Ok(())
    }

    /// Process raw stdin bytes. Returns Some(action) for terminal actions,
    /// None if the panel just needs a re-render (navigation).
    pub fn handle_input(&mut self, raw: &[u8]) -> Option<PanelAction> {
        // Parse escape sequences and single bytes from the raw input
        let mut i = 0;
        while i < raw.len() {
            let key = if raw[i] == 0x1b {
                // Escape sequence
                if i + 2 < raw.len() && raw[i + 1] == b'[' {
                    let code = raw[i + 2];
                    i += 3;
                    match code {
                        b'A' => Key::Up,
                        b'B' => Key::Down,
                        _ => Key::Unknown,
                    }
                } else {
                    i += 1;
                    // Lone ESC
                    Key::Esc
                }
            } else {
                let b = raw[i];
                i += 1;
                match b {
                    0x1c => Key::CtrlBackslash,
                    0x03 | 0x04 => Key::CtrlC,
                    b'\r' | b'\n' => Key::Enter,
                    b'j' => Key::Down,
                    b'k' => Key::Up,
                    b'n' => Key::Char('n'),
                    b'x' => Key::Char('x'),
                    b'd' => Key::Char('d'),
                    b'q' => Key::Esc,
                    _ => Key::Unknown,
                }
            };

            if let Some(action) = self.process_key(key) {
                return Some(action);
            }
        }
        None
    }

    fn process_key(&mut self, key: Key) -> Option<PanelAction> {
        let total = self.item_count();
        if total == 0 {
            // Still loading — only allow closing
            return match key {
                Key::Esc | Key::CtrlC => Some(PanelAction::Cancel),
                Key::CtrlBackslash | Key::Char('d') => Some(PanelAction::Detach),
                _ => None,
            };
        }

        match key {
            Key::Up => {
                let mut next = self.selected;
                while next > 0 {
                    next -= 1;
                    if self.is_selectable(next) {
                        break;
                    }
                }
                if self.is_selectable(next) {
                    self.selected = next;
                }
                None
            }
            Key::Down => {
                let mut next = self.selected;
                while next + 1 < total {
                    next += 1;
                    if self.is_selectable(next) {
                        break;
                    }
                }
                if self.is_selectable(next) {
                    self.selected = next;
                }
                None
            }
            Key::Enter => {
                let sessions = self.sessions.as_ref().unwrap();
                if self.selected < sessions.len() {
                    let id = &sessions[self.selected].id;
                    if *id == self.current_session {
                        Some(PanelAction::Cancel)
                    } else {
                        Some(PanelAction::SwitchTo(id.clone()))
                    }
                } else {
                    Some(PanelAction::NewSession)
                }
            }
            Key::Char('n') => Some(PanelAction::NewSession),
            Key::Char('x') => {
                let sessions = self.sessions.as_ref()?;
                if self.selected < sessions.len() {
                    let s = &sessions[self.selected];
                    // Can only kill detached, non-current sessions
                    if !s.attached && s.id != self.current_session {
                        return Some(PanelAction::KillSession(s.id.clone()));
                    }
                }
                None
            }
            Key::Char('d') | Key::CtrlBackslash => Some(PanelAction::Detach),
            Key::Esc | Key::CtrlC => Some(PanelAction::Cancel),
            Key::Unknown | Key::Char(_) => None,
        }
    }

    /// Remove a killed session from the list and fix selection.
    pub fn remove_session(&mut self, id: &str) {
        if let Some(ref mut sessions) = self.sessions {
            sessions.retain(|s| s.id != id);
            let total = self.item_count();
            if self.selected >= total && total > 0 {
                self.selected = total - 1;
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Key {
    Up,
    Down,
    Enter,
    Esc,
    CtrlBackslash,
    CtrlC,
    Char(char),
    Unknown,
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

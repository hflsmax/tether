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

        write!(out, "\r\n  \x1b[1mtether sessions\x1b[0m\r\n\r\n")?;

        match &self.sessions {
            None => {
                write!(out, "  loading...\r\n")?;
            }
            Some(sessions) => {
                // 4 columns split equally across available width
                // Reserve: arrow(2) + suffix(12) = 14 chars for non-column content
                let avail = (self.cols as usize).saturating_sub(14);
                let col_w = (avail / 4).max(6).min(30);

                // Header
                write!(
                    out,
                    "  {:<col_w$} {:<col_w$} {:<col_w$} {}\r\n",
                    "RUNNING", "CWD", "AGE", "IDLE"
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
                    let cwd = truncate_str(&shorten_path(&s.cwd), col_w - 1);
                    let proc_name = truncate_str(proc_name, col_w - 1);

                    let suffix = if is_current {
                        " (current)"
                    } else if s.attached {
                        " (attached)"
                    } else {
                        ""
                    };

                    if s.attached && !is_current {
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
                        "{} {:<col_w$} {:<col_w$} {:<col_w$} {}{}\x1b[0m\r\n",
                        arrow, proc_name, cwd, age, idle, suffix
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
                if i + 1 >= raw.len() {
                    // \x1b at end of buffer — could be ESC or start
                    // of a split escape sequence. Treat as unknown;
                    // user can press q instead.
                    i += 1;
                    Key::Unknown
                } else if raw[i + 1] == b'[' || raw[i + 1] == b'O' {
                    // CSI (\x1b[) or SS3 (\x1bO) — both used for arrows
                    if i + 2 < raw.len() {
                        let code = raw[i + 2];
                        i += 3;
                        // Skip any remaining CSI parameters (e.g. \x1b[1;5A)
                        while i < raw.len() && raw[i - 1] != code {
                            // already consumed the final byte above
                            break;
                        }
                        match code {
                            b'A' => Key::Up,
                            b'B' => Key::Down,
                            _ => Key::Unknown,
                        }
                    } else {
                        // Incomplete sequence — skip
                        i += 2;
                        Key::Unknown
                    }
                } else {
                    // \x1b followed by something else — skip both bytes
                    // to avoid misinterpreting escape sequences as Esc
                    i += 2;
                    Key::Unknown
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

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else if max <= 3 {
        s[..max].to_string()
    } else {
        format!("{}...", &s[..max - 3])
    }
}

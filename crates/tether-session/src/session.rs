use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::Arc;

use tokio::io::unix::AsyncFd;
use tokio::io::Interest;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, warn};

use tether_protocol::SessionState;

use crate::pty::{self, PtyHandle};
use crate::terminal::TerminalModel;

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("pty error: {0}")]
    Pty(#[from] pty::PtyError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// PTY produced output bytes
    Output(Vec<u8>),
    /// PTY process exited
    Exited(i32),
}

pub struct SessionHandle {
    pub id: String,
    inner: Arc<Mutex<SessionInner>>,
    pty_handle: PtyHandle,
}

struct SessionInner {
    terminal: TerminalModel,
    raw_log: RingLog,
}

/// A simple ring buffer for raw PTY byte logging (debug aid).
struct RingLog {
    buf: Vec<u8>,
    pos: usize,
    capacity: usize,
}

impl RingLog {
    fn new(capacity: usize) -> Self {
        Self {
            buf: vec![0u8; capacity],
            pos: 0,
            capacity,
        }
    }

    fn write(&mut self, data: &[u8]) {
        for &byte in data {
            self.buf[self.pos % self.capacity] = byte;
            self.pos += 1;
        }
    }
}

pub struct Session;

impl Session {
    pub fn spawn(
        id: String,
        cmd: &str,
        cols: u16,
        rows: u16,
        env_vars: Vec<(String, String)>,
        scrollback_lines: usize,
        raw_log_size: usize,
    ) -> Result<(SessionHandle, mpsc::Receiver<SessionEvent>), SessionError> {
        let pty_handle = pty::spawn_pty(cmd, cols, rows, &env_vars)?;

        let terminal = TerminalModel::new(cols, rows, scrollback_lines);
        let raw_log = RingLog::new(raw_log_size);
        let inner = Arc::new(Mutex::new(SessionInner { terminal, raw_log }));

        let (event_tx, event_rx) = mpsc::channel(256);

        // Spawn PTY reader task
        let reader_inner = inner.clone();
        let reader_fd = pty_handle.master.raw_fd();
        let child_pid = pty_handle.child_pid;
        let session_id = id.clone();

        // We need to dup the fd so the reader task has its own handle
        let reader_owned_fd = unsafe {
            let duped = libc::dup(reader_fd);
            if duped < 0 {
                return Err(SessionError::Io(std::io::Error::last_os_error()));
            }
            // Set non-blocking
            let flags = libc::fcntl(duped, libc::F_GETFL);
            libc::fcntl(duped, libc::F_SETFL, flags | libc::O_NONBLOCK);
            std::os::fd::OwnedFd::from_raw_fd(duped)
        };
        let async_reader_fd = AsyncFd::new(reader_owned_fd)?;

        tokio::spawn(async move {
            let mut buf = vec![0u8; 4096];
            loop {
                let result: Result<usize, std::io::Error> =
                    async_reader_fd.async_io(Interest::READABLE, |fd| {
                        let n = unsafe {
                            libc::read(
                                fd.as_raw_fd(),
                                buf.as_mut_ptr() as *mut libc::c_void,
                                buf.len(),
                            )
                        };
                        if n < 0 {
                            Err(std::io::Error::last_os_error())
                        } else if n == 0 {
                            Err(std::io::Error::new(
                                std::io::ErrorKind::UnexpectedEof,
                                "pty closed",
                            ))
                        } else {
                            Ok(n as usize)
                        }
                    }).await;

                match result {
                    Ok(n) => {
                        let data = buf[..n].to_vec();

                        // Feed into terminal model and raw log
                        {
                            let mut inner = reader_inner.lock().await;
                            inner.terminal.process(&data);
                            inner.raw_log.write(&data);
                        }

                        // Send output event
                        if event_tx.send(SessionEvent::Output(data)).await.is_err() {
                            debug!(session = %session_id, "event receiver dropped");
                            break;
                        }
                    }
                    Err(e) => {
                        if e.kind() == std::io::ErrorKind::UnexpectedEof {
                            debug!(session = %session_id, "pty closed");
                        } else {
                            warn!(session = %session_id, "pty read error: {e}");
                        }
                        // Wait for child to exit and report
                        let status = tokio::task::spawn_blocking(move || {
                            nix::sys::wait::waitpid(child_pid, None)
                        })
                        .await;

                        let exit_code = match status {
                            Ok(Ok(nix::sys::wait::WaitStatus::Exited(_, code))) => code,
                            _ => -1,
                        };
                        let _ = event_tx.send(SessionEvent::Exited(exit_code)).await;
                        break;
                    }
                }
            }
        });

        let handle = SessionHandle {
            id,
            inner,
            pty_handle,
        };

        Ok((handle, event_rx))
    }
}

impl SessionHandle {
    /// Get the child process PID.
    pub fn child_pid(&self) -> i32 {
        self.pty_handle.child_pid.as_raw()
    }

    /// Write input data to the PTY.
    pub fn write_input(&self, data: &[u8]) -> Result<(), SessionError> {
        let fd = self.pty_handle.master.raw_fd();
        let n = unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
        if n < 0 {
            return Err(SessionError::Io(std::io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Resize the PTY and terminal model.
    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), SessionError> {
        pty::resize_pty(self.pty_handle.master.raw_fd(), cols, rows)?;
        let mut inner = self.inner.lock().await;
        inner.terminal.resize(cols, rows);
        Ok(())
    }

    /// Get a structured snapshot of the terminal state.
    pub async fn snapshot(&self, max_scrollback_rows: usize) -> SessionState {
        let inner = self.inner.lock().await;
        inner.terminal.snapshot(max_scrollback_rows)
    }

    /// Set the viewport offset (called on detach).
    pub async fn set_viewport_offset(&self, offset: u32) {
        let mut inner = self.inner.lock().await;
        inner.terminal.set_viewport_offset(offset);
    }
}

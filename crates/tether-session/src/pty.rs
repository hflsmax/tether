use nix::pty::{openpty, OpenptyResult, Winsize};
use nix::unistd::{close, dup2, execvp, fork, setsid, ForkResult};
use std::ffi::CString;
use std::os::fd::{AsRawFd, OwnedFd};
use tokio::io::unix::AsyncFd;

#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("nix error: {0}")]
    Nix(#[from] nix::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("null byte in string")]
    NulError(#[from] std::ffi::NulError),
}

pub struct PtyMaster {
    fd: AsyncFd<OwnedFd>,
}

impl PtyMaster {
    pub fn fd(&self) -> &AsyncFd<OwnedFd> {
        &self.fd
    }

    pub fn raw_fd(&self) -> std::os::fd::RawFd {
        self.fd.as_raw_fd()
    }
}

pub struct PtyHandle {
    pub master: PtyMaster,
    pub child_pid: nix::unistd::Pid,
}

/// Spawn a child process in a new PTY.
pub fn spawn_pty(
    cmd: &str,
    cols: u16,
    rows: u16,
    env_vars: &[(String, String)],
) -> Result<PtyHandle, PtyError> {
    let winsize = Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    let OpenptyResult { master, slave } = openpty(&winsize, None)?;

    // Set master to non-blocking for async I/O
    let master_raw = master.as_raw_fd();
    let flags = nix::fcntl::fcntl(master_raw, nix::fcntl::FcntlArg::F_GETFL)?;
    let mut oflags = nix::fcntl::OFlag::from_bits_truncate(flags);
    oflags |= nix::fcntl::OFlag::O_NONBLOCK;
    nix::fcntl::fcntl(master_raw, nix::fcntl::FcntlArg::F_SETFL(oflags))?;

    // Fork
    match unsafe { fork()? } {
        ForkResult::Parent { child } => {
            // Close slave in parent
            drop(slave);
            let async_fd = AsyncFd::new(master)?;
            Ok(PtyHandle {
                master: PtyMaster { fd: async_fd },
                child_pid: child,
            })
        }
        ForkResult::Child => {
            // Close master in child
            let _ = close(master.as_raw_fd());
            std::mem::forget(master); // don't double-close

            // Create new session
            setsid().ok();

            // Set controlling terminal
            unsafe {
                libc::ioctl(slave.as_raw_fd(), libc::TIOCSCTTY, 0);
            }

            // Dup slave to stdin/stdout/stderr
            let slave_raw = slave.as_raw_fd();
            dup2(slave_raw, 0).ok();
            dup2(slave_raw, 1).ok();
            dup2(slave_raw, 2).ok();
            if slave_raw > 2 {
                let _ = close(slave_raw);
            }
            std::mem::forget(slave);

            // Set environment variables
            // Safety: we are in a forked child process, single-threaded.
            for (key, val) in env_vars {
                unsafe { std::env::set_var(key, val) };
            }

            // Exec the command
            let cmd_cstr = CString::new(cmd)?;
            let args = [cmd_cstr.clone()];
            execvp(&cmd_cstr, &args).ok();
            std::process::exit(127);
        }
    }
}

/// Resize a PTY.
pub fn resize_pty(fd: std::os::fd::RawFd, cols: u16, rows: u16) -> Result<(), PtyError> {
    let winsize = Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        let ret = libc::ioctl(fd, libc::TIOCSWINSZ, &winsize as *const _);
        if ret < 0 {
            return Err(PtyError::Nix(nix::Error::last()));
        }
    }
    Ok(())
}

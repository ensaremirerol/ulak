//! A process on a pseudo-terminal: the tmux client that streams an agent's terminal (Linux).
use std::{
    fs::File,
    io::{self, Write},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    os::unix::process::CommandExt,
    process::{Child, Command, Stdio},
};

/// The child is killed and reaped on drop; the master closes with the last `File` (the reader's
/// clone ends on EOF/EIO once the child is gone).
pub struct Pty {
    master: File,
    child: Child,
}

impl std::fmt::Debug for Pty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Pty(pid {})", self.child.id())
    }
}

fn winsize(cols: u16, rows: u16) -> libc::winsize {
    libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    }
}

impl Pty {
    /// Runs `argv` with a new PTY of `cols` x `rows` as its controlling terminal and stdio.
    pub fn spawn(argv: &[String], env: &[(&str, &str)], cols: u16, rows: u16) -> io::Result<Self> {
        let (mut m, mut s) = (0, 0);
        let mut ws = winsize(cols, rows);
        // SAFETY: openpty writes two fds into the out-params; name and termios may be null.
        if unsafe {
            libc::openpty(
                &mut m,
                &mut s,
                std::ptr::null_mut(),
                // const on Linux, mut on macOS
                std::ptr::null_mut::<libc::termios>() as _,
                &mut ws as *mut libc::winsize as _,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: both fds were just returned by openpty and are owned by nothing else.
        let (master, slave) = unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
        for fd in [&master, &slave] {
            // SAFETY: fcntl on a valid fd we own; keeps the fds out of every other child we spawn.
            if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        let slave = File::from(slave);
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .envs(env.iter().copied())
            .stdin(Stdio::from(slave.try_clone()?))
            .stdout(Stdio::from(slave.try_clone()?))
            .stderr(Stdio::from(slave));
        // SAFETY: only async-signal-safe calls between fork and exec: a new session, then the PTY
        // (now fd 0) as its controlling terminal.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn()?; // the parent's slave copies drop with `cmd`
        Ok(Self {
            master: File::from(master),
            child,
        })
    }

    /// A second handle for a blocking reader thread.
    pub fn reader(&self) -> io::Result<File> {
        self.master.try_clone()
    }

    // ponytail: blocking write on the async side; keystrokes are tiny and the PTY buffer is ~4 KiB+.
    pub fn write(&self, data: &[u8]) -> io::Result<()> {
        (&self.master).write_all(data)
    }

    /// The kernel sends SIGWINCH to the client, which redraws at the new size.
    pub fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        let ws = winsize(cols, rows);
        // SAFETY: TIOCSWINSZ on our master fd with a valid winsize.
        if unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ as _, &ws) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn read_until(r: &mut File, needle: &str) -> String {
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        while !String::from_utf8_lossy(&out).contains(needle) {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => out.extend_from_slice(&buf[..n]),
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn runs_on_a_tty_with_its_size_echoes_input_and_resizes() {
        let argv: Vec<String> = [
            "sh",
            "-c",
            "stty size; read l; echo got:$l; sleep 0.3; stty size; sleep 5",
        ]
        .map(String::from)
        .to_vec();
        let mut p = Pty::spawn(&argv, &[("TERM", "xterm-256color")], 100, 30).unwrap();
        let mut r = p.reader().unwrap();
        assert!(read_until(&mut r, "30 100").contains("30 100"));
        p.write(b"hello\n").unwrap();
        p.resize(80, 20).unwrap();
        let out = read_until(&mut r, "20 80");
        assert!(
            out.contains("got:hello") && out.contains("20 80"),
            "{out:?}"
        );
        assert!(p.alive());
        let pid = p.child.id();
        drop(p);
        // reaped: no zombie left with that pid
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
    }
}

// SPDX-License-Identifier: MIT-0
// Copyright (C) 2026 Simplebooks Foundation
// Copyright (C) 2026 Josh Rodd

//! Bounded capture of a command's output through its own Unix PTY.
//!
//! The command starts a new session and process group. Cleanup signals only that
//! group, while keeping its leader unreaped so its ID cannot be recycled. This
//! contains ordinary descendants, including ones that keep the slave open after
//! the command exits. A descendant that deliberately creates another session or
//! process group escapes this containment; this is not a sandbox. Callers must
//! not independently reap this function's child (for example with waitpid(-1)).
//! Termination escalates to SIGKILL after a finite grace period, closes the PTY
//! master, then reaps the leader. As with any Unix process supervisor, an
//! uninterruptible kernel wait can delay reaping even after SIGKILL.

use serde::Serialize;
use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const EXIT_DRAIN_GRACE: Duration = Duration::from_millis(100);
const TERMINATION_GRACE: Duration = Duration::from_millis(100);

#[derive(Clone, Debug)]
pub struct CaptureOptions {
    pub timeout: Duration,
    pub max_bytes: usize,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            max_bytes: 64 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CaptureOutcome {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub output_limit_reached: bool,
    pub bytes_captured: usize,
}

impl CaptureOutcome {
    /// Supervisor limits take precedence over the status caused by cleanup.
    /// Spawn and I/O failures are returned as errors, not as capture outcomes;
    /// their CLI status is 2. A successful child's analysis status is separate.
    pub fn shell_status(&self) -> u8 {
        if self.timed_out {
            124
        } else if self.output_limit_reached {
            125
        } else if let Some(signal) = self.signal {
            (128 + signal).clamp(0, 255) as u8
        } else {
            self.exit_code.unwrap_or(0).clamp(0, 255) as u8
        }
    }
}

/// Run exactly `argv`, without an implicit shell, on a 320-column, 80-row PTY.
///
/// The transcript is created/truncated before spawning and written incrementally
/// without text decoding. It remains available on every subsequent error. PTY
/// line discipline applies (normally, an emitted LF is captured as CR LF).
/// Stdin is the same PTY but no input is sent. The byte limit is inclusive:
/// reaching it stops capture, even if the child would otherwise exit then; zero
/// permits no output. The timeout starts immediately before spawning.
///
/// Output already available when the leader exits is drained for at most 100 ms
/// before descendant cleanup. Output produced during termination is not captured.
/// The returned exit/signal describes the leader, including termination by this
/// supervisor when a limit is reached.
pub fn capture_child(
    argv: &[OsString],
    transcript: &Path,
    options: &CaptureOptions,
) -> io::Result<CaptureOutcome> {
    let mut output = File::create(transcript)?;
    let program = argv
        .first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "capture requires a command"))?;
    let (master, slave) = open_pty()?;
    let mut command = Command::new(program);
    command
        .args(&argv[1..])
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor")
        .stdin(Stdio::from(slave.try_clone()?))
        .stdout(Stdio::from(slave.try_clone()?))
        .stderr(Stdio::from(slave));
    // Only async-signal-safe operations belong in the post-fork callback.
    // A successful spawn confirms these operations succeeded before exec.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::ioctl(libc::STDIN_FILENO, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let started = Instant::now();
    let child = command.spawn()?;
    let mut child = ChildSession {
        child,
        master: Some(File::from(master)),
        reaped: false,
    };
    // Command owns the parent's Stdio handles, even after spawn. Release all
    // slave descriptors so only the child/descendants can keep the PTY open.
    drop(command);
    let mut outcome = CaptureOutcome {
        exit_code: None,
        signal: None,
        timed_out: false,
        output_limit_reached: false,
        bytes_captured: 0,
    };
    let mut exited_at: Option<Instant> = None;
    let mut eof = false;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        if options.max_bytes == outcome.bytes_captured {
            outcome.output_limit_reached = true;
            break;
        }
        if exited_at.is_none() && child.observe_exit()? {
            exited_at = Some(Instant::now());
        }
        if started.elapsed() >= options.timeout {
            outcome.timed_out = exited_at.is_none();
            break;
        }
        if let Some(exited) = exited_at {
            if eof || exited.elapsed() >= EXIT_DRAIN_GRACE {
                break;
            }
        }
        if !eof {
            let available = buffer.len().min(options.max_bytes - outcome.bytes_captured);
            match child
                .master
                .as_mut()
                .unwrap()
                .read(&mut buffer[..available])
            {
                Ok(0) => eof = true,
                Ok(count) => {
                    output.write_all(&buffer[..count])?;
                    outcome.bytes_captured += count;
                    // Recheck both limits and leader status on every chunk, even
                    // when a writer continuously fills the nonblocking master.
                    continue;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    if exited_at.is_some() {
                        break;
                    }
                }
                // Linux returns EIO after the final slave closes; macOS may
                // instead return EOF. Neither means the transcript failed.
                Err(error) if error.raw_os_error() == Some(libc::EIO) => eof = true,
                Err(error) => return Err(error),
            }
        }
        let remaining = options.timeout.saturating_sub(started.elapsed());
        let pause = POLL_INTERVAL.min(remaining);
        if eof {
            std::thread::sleep(pause);
        } else {
            wait_readable(child.master.as_ref().unwrap().as_raw_fd(), pause)?;
        }
    }
    let status = child.stop_and_reap()?;
    outcome.exit_code = status.code();
    outcome.signal = status.signal();
    Ok(outcome)
}

fn open_pty() -> io::Result<(OwnedFd, OwnedFd)> {
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_row: 80,
        ws_col: 320,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    if unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut::<libc::termios>(),
            &mut size,
        )
    } == -1
    {
        return Err(io::Error::last_os_error());
    }
    // Own both descriptors before any further fallible operation.
    let master = unsafe { OwnedFd::from_raw_fd(master) };
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let master = private_fd(master)?;
    let slave = private_fd(slave)?;
    let flags = unsafe { libc::fcntl(master.as_raw_fd(), libc::F_GETFL) };
    if flags == -1
        || unsafe { libc::fcntl(master.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1
    {
        return Err(io::Error::last_os_error());
    }
    Ok((master, slave))
}

fn private_fd(fd: OwnedFd) -> io::Result<OwnedFd> {
    // Keep internal descriptors out of 0..=2 even if the caller has closed a
    // standard stream. CLOEXEC prevents leakage into the executed command.
    let duplicate = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
}

fn wait_readable(fd: RawFd, duration: Duration) -> io::Result<()> {
    let mut descriptor = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // Round up sub-millisecond durations; never exceed the small poll interval.
    let milliseconds =
        duration.as_millis() as i32 + i32::from(!duration.subsec_nanos().is_multiple_of(1_000_000));
    let result = unsafe { libc::poll(&mut descriptor, 1, milliseconds) };
    if result == -1 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    } else if descriptor.revents & libc::POLLNVAL != 0 {
        return Err(io::Error::from_raw_os_error(libc::EBADF));
    }
    Ok(())
}

struct ChildSession {
    child: Child,
    master: Option<File>,
    reaped: bool,
}

impl ChildSession {
    fn observe_exit(&mut self) -> io::Result<bool> {
        self.wait_for_exit(libc::WNOHANG)
    }

    fn wait_for_exit(&mut self, flags: libc::c_int) -> io::Result<bool> {
        if self.reaped {
            return Err(io::Error::from_raw_os_error(libc::ECHILD));
        }
        loop {
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.child.id() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT | flags,
                )
            };
            if result == 0 {
                return Ok(unsafe { info.si_pid() } != 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if error.raw_os_error() == Some(libc::ECHILD) {
                // An external reaper violated ownership. Never signal a PID
                // that may now identify an unrelated process or process group.
                self.reaped = true;
            }
            return Err(error);
        }
    }

    fn signal_group(&mut self, signal: libc::c_int) -> io::Result<bool> {
        // WNOWAIT pins the original leader even after its exit. Its PID is also
        // the process-group/session ID established in the successful pre_exec.
        self.observe_exit()?;
        let result = unsafe { libc::kill(-(self.child.id() as libc::pid_t), signal) };
        if result == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(false)
        } else {
            Err(error)
        }
    }

    fn signal_result(
        &self,
        signal: libc::c_int,
        result: io::Result<bool>,
        no_live_members: bool,
    ) -> io::Result<bool> {
        let error = match result {
            Ok(found) => return Ok(found),
            Err(error) => error,
        };
        // Darwin skips zombies in kill(-pgid, sig), reporting EPERM when no
        // live target remains. Never infer this from the leader alone: a live
        // privileged descendant must still cause the original error.
        if no_live_members && error.raw_os_error() == Some(libc::EPERM) {
            return Ok(false);
        }
        Err(io::Error::new(
            error.kind(),
            format!(
                "signal {signal} to owned group {}: {error}",
                self.child.id()
            ),
        ))
    }

    fn stop_and_reap(&mut self) -> io::Result<ExitStatus> {
        let term = self.signal_group(libc::SIGTERM);
        // Resume stopped members so they can handle TERM during the grace period.
        let resume = self.signal_group(libc::SIGCONT);
        if !self.reaped && !matches!(&term, Ok(false)) {
            std::thread::sleep(TERMINATION_GRACE);
        }
        let kill = self.signal_group(libc::SIGKILL);
        // Darwin can block exit in tty drain after TERM, making subsequent
        // group signals report EPERM before waitid can see an exited leader.
        // Release the master only after escalation (avoiding an earlier HUP),
        // and before waiting, on both success and error/Drop paths.
        drop(self.master.take());
        if self.reaped {
            return Err(io::Error::from_raw_os_error(libc::ECHILD));
        }
        // Keep the leader pinned while classifying Darwin's zombie-only EPERM.
        // Closing the master above lets any pending terminal teardown finish.
        self.wait_for_exit(0)?;
        #[cfg(target_os = "macos")]
        let no_live_members = [&term, &resume, &kill].iter().any(|result| {
            result.as_ref().err().and_then(io::Error::raw_os_error) == Some(libc::EPERM)
        }) && matches!(
            group_has_live_members(self.child.id() as libc::pid_t),
            Ok(false)
        );
        #[cfg(not(target_os = "macos"))]
        let no_live_members = false;
        let term = self.signal_result(libc::SIGTERM, term, no_live_members);
        let resume = self.signal_result(libc::SIGCONT, resume, no_live_members);
        let kill = self.signal_result(libc::SIGKILL, kill, no_live_members);
        let status = loop {
            match self.child.wait() {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => break result,
            }
        };
        if status.is_ok()
            || status.as_ref().err().and_then(io::Error::raw_os_error) == Some(libc::ECHILD)
        {
            self.reaped = true;
        }
        // Always attempt escalation and reaping, even when an earlier operation
        // failed. Preserve the first error rather than disguising it as success.
        term?;
        resume?;
        kill?;
        status
    }
}

impl Drop for ChildSession {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.stop_and_reap();
        }
    }
}

#[cfg(target_os = "macos")]
fn group_has_live_members(group: libc::pid_t) -> io::Result<bool> {
    // The size query is an upper bound for the system, not just this group.
    // A full snapshot may have been truncated by concurrent forks; it cannot
    // establish absence of live members, so conservatively retain EPERM.
    let capacity = unsafe { libc::proc_listpgrppids(group, std::ptr::null_mut(), 0) };
    if capacity <= 0 {
        return Err(io::Error::last_os_error());
    }
    let mut pids = vec![0 as libc::pid_t; capacity as usize];
    let bytes = std::mem::size_of_val(pids.as_slice());
    let bytes =
        libc::c_int::try_from(bytes).map_err(|_| io::Error::from_raw_os_error(libc::EOVERFLOW))?;
    let count = unsafe { libc::proc_listpgrppids(group, pids.as_mut_ptr().cast(), bytes) };
    // Our unreaped leader must appear even in a zombie-only group. Zero also
    // represents a libproc error; neither case is evidence to dismiss EPERM.
    if count <= 0 {
        return Err(io::Error::last_os_error());
    }
    if count >= capacity {
        return Ok(true);
    }
    for &pid in &pids[..count as usize] {
        let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of_val(&info) as libc::c_int;
        // arg=1 includes zombies. Inspect only; never signal enumerated PIDs,
        // whose identities (unlike the owned group ID) are not pinned.
        let read = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDTBSDINFO,
                1,
                (&mut info as *mut libc::proc_bsdinfo).cast(),
                size,
            )
        };
        if read != size {
            let error = io::Error::last_os_error();
            if read == 0 && error.raw_os_error() == Some(libc::ESRCH) {
                continue;
            }
            return Err(error);
        }
        if info.pbi_pgid == group as u32 && info.pbi_status != libc::SZOMB {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(script: &str) -> Vec<OsString> {
        vec!["/bin/sh".into(), "-c".into(), script.into()]
    }

    fn capture(script: &str, options: CaptureOptions) -> (CaptureOutcome, Vec<u8>) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("transcript.pty");
        let outcome = capture_child(&shell(script), &path, &options).unwrap();
        (outcome, std::fs::read(path).unwrap())
    }

    #[test]
    fn raw_output_and_nonzero_status_survive_capture() {
        let (outcome, bytes) = capture(
            "printf '\\033[31mred\\000'; printf 'error\\n' >&2; exit 7",
            CaptureOptions::default(),
        );
        assert_eq!(bytes, b"\x1b[31mred\0error\r\n");
        assert_eq!(outcome.bytes_captured, bytes.len());
        assert_eq!(outcome.exit_code, Some(7));
        assert_eq!(outcome.signal, None);
        assert_eq!(outcome.shell_status(), 7);
    }

    #[test]
    fn child_has_terminal_geometry_and_color_environment() {
        let (outcome, bytes) = capture(
            "[ -t 0 ] && [ -t 1 ] && [ -t 2 ] || exit 9; stty size; printf '%s/%s' \"$TERM\" \"$COLORTERM\"",
            CaptureOptions::default(),
        );
        assert_eq!(outcome.shell_status(), 0);
        assert_eq!(bytes, b"80 320\r\nxterm-256color/truecolor");
    }

    #[test]
    fn signal_exit_is_not_flattened_into_an_exit_code() {
        let (outcome, _) = capture("kill -TERM $$", CaptureOptions::default());
        assert_eq!(outcome.exit_code, None);
        assert_eq!(outcome.signal, Some(libc::SIGTERM));
        assert_eq!(outcome.shell_status(), 128 + libc::SIGTERM as u8);
    }

    #[test]
    fn timeout_escalates_past_ignored_term() {
        let started = Instant::now();
        let (outcome, bytes) = capture(
            "trap '' TERM; printf ready; while :; do :; done",
            CaptureOptions {
                timeout: Duration::from_millis(500),
                max_bytes: 1024,
            },
        );
        assert!(outcome.timed_out);
        assert!(!outcome.output_limit_reached);
        assert_eq!(outcome.shell_status(), 124);
        assert_eq!(outcome.signal, Some(libc::SIGKILL));
        assert_eq!(bytes, b"ready");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn closed_terminal_is_not_mistaken_for_child_exit() {
        let (outcome, bytes) = capture(
            "exec 0<&- 1>&- 2>&-; while :; do :; done",
            CaptureOptions {
                timeout: Duration::from_millis(500),
                max_bytes: 1024,
            },
        );
        assert!(bytes.is_empty());
        assert!(outcome.timed_out);
        assert_eq!(outcome.shell_status(), 124);
    }

    #[test]
    fn continuous_output_is_cut_at_the_exact_byte_limit() {
        let (outcome, bytes) = capture(
            "while :; do printf 0123456789; done",
            CaptureOptions {
                timeout: Duration::from_secs(5),
                max_bytes: 37,
            },
        );
        assert_eq!(bytes, b"0123456789012345678901234567890123456");
        assert_eq!(outcome.bytes_captured, 37);
        assert!(outcome.output_limit_reached);
        assert!(!outcome.timed_out);
        assert_eq!(outcome.shell_status(), 125);
    }

    #[test]
    fn zero_byte_limit_preserves_an_empty_transcript() {
        let (outcome, bytes) = capture(
            "printf forbidden",
            CaptureOptions {
                timeout: Duration::from_secs(5),
                max_bytes: 0,
            },
        );
        assert!(bytes.is_empty());
        assert_eq!(outcome.bytes_captured, 0);
        assert_eq!(outcome.shell_status(), 125);
    }

    #[test]
    fn inherited_slave_does_not_delay_completed_leader_until_timeout() {
        let started = Instant::now();
        let (outcome, bytes) = capture(
            "trap '' HUP; sleep 30 & printf complete; exit 3",
            CaptureOptions {
                timeout: Duration::from_secs(5),
                max_bytes: 1024,
            },
        );
        assert_eq!(bytes, b"complete");
        assert_eq!(outcome.exit_code, Some(3));
        assert!(!outcome.timed_out);
        assert_eq!(outcome.shell_status(), 3);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn spawn_failure_keeps_the_created_transcript() {
        let directory = tempfile::tempdir().unwrap();
        let transcript = directory.path().join("transcript.pty");
        let missing = directory.path().join("missing-command").into_os_string();
        let error = capture_child(&[missing], &transcript, &CaptureOptions::default()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        assert_eq!(std::fs::read(transcript).unwrap(), b"");
    }

    #[test]
    fn empty_argv_is_an_error_with_a_preserved_transcript() {
        let directory = tempfile::tempdir().unwrap();
        let transcript = directory.path().join("transcript.pty");
        let error = capture_child(&[], &transcript, &CaptureOptions::default()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(std::fs::read(transcript).unwrap(), b"");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn transcript_write_failure_terminates_the_child() {
        let started = Instant::now();
        let directory = tempfile::tempdir().unwrap();
        let pid_file = directory.path().join("child.pid");
        let mut argv = shell("printf '%s' \"$$\" > \"$1\"; printf output; while :; do :; done");
        argv.push("capture-test".into());
        argv.push(pid_file.clone().into_os_string());
        let error =
            capture_child(&argv, Path::new("/dev/full"), &CaptureOptions::default()).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
        assert!(started.elapsed() < Duration::from_secs(3));
        let pid: libc::pid_t = std::fs::read_to_string(pid_file).unwrap().parse().unwrap();
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
            -1
        );
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
}

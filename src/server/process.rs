//! `slutty server`: the server as a process of its own.

use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::ExitCode;

use super::run::run;
use super::socket::Listener;
use crate::config;

/// Serves on `socket`. With `daemon`, goes to the background first, says on
/// stdout when it listens, then writes its log to
/// `~/Library/Logs/slutty/server.log`.
pub fn main(socket: PathBuf, daemon: bool) -> ExitCode {
    if daemon && let Err(err) = daemonize() {
        log::error!("going to the background failed: {err}");
        return ExitCode::FAILURE;
    }
    let listener = match Listener::bind(&socket) {
        Ok(Some(listener)) => listener,
        Ok(None) => {
            // Another server won a race to start; whoever started this one
            // can use that one.
            log::info!("a server already runs on {}", socket.display());
            ready(daemon);
            return ExitCode::SUCCESS;
        }
        Err(err) => {
            log::error!("listening on {} failed: {err}", socket.display());
            return ExitCode::FAILURE;
        }
    };
    log::info!("serving on {}, pid {}", socket.display(), std::process::id());
    match run(listener, config::path(), true, || ready(daemon)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            log::error!("the server failed: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Forks while the process still has one thread. The parent exits at once,
/// so whoever started it can reap it, and the child leads a session of its
/// own, so it lives on when the app that started it quits.
fn daemonize() -> io::Result<()> {
    // SAFETY: no thread has started yet, so the child is a whole copy.
    match unsafe { libc::fork() } {
        -1 => Err(io::Error::last_os_error()),
        // SAFETY: plain calls in the child.
        0 if unsafe { libc::setsid() } < 0 => Err(io::Error::last_os_error()),
        0 => Ok(()),
        // SAFETY: ends the parent without running anything the child owns.
        _ => unsafe { libc::_exit(0) },
    }
}

/// Tells whoever started a daemon that it listens, with one byte on
/// stdout, then sends stdout and stderr to the log.
fn ready(daemon: bool) {
    if !daemon {
        return;
    }
    let mut stdout = io::stdout().lock();
    if let Err(err) = stdout.write_all(b"\n").and_then(|()| stdout.flush()) {
        log::warn!("saying the server is ready failed: {err}");
    }
    let log = open_log().or_else(|err| {
        log::warn!("opening the log failed: {err}");
        File::options().write(true).open("/dev/null")
    });
    match log {
        // SAFETY: dup2 onto the standard descriptors, which nothing else
        // holds as an owned descriptor.
        Ok(log) => unsafe {
            libc::dup2(log.as_raw_fd(), libc::STDOUT_FILENO);
            libc::dup2(log.as_raw_fd(), libc::STDERR_FILENO);
        },
        Err(err) => log::warn!("leaving the output as it is: {err}"),
    }
}

fn open_log() -> io::Result<File> {
    let home = std::env::home_dir().ok_or_else(|| io::Error::other("no home directory"))?;
    let dir = home.join("Library/Logs/slutty");
    fs::create_dir_all(&dir)?;
    File::options().create(true).append(true).open(dir.join("server.log"))
}

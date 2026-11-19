//! The program in the foreground of a terminal, read from the kernel when
//! asked. Nothing here polls.

use std::ffi::{CStr, OsStr};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Programs that are only a prompt: closing them loses nothing, so closing
/// their pane needs no confirmation. WezTerm's list.
const SHELLS: [&str; 6] = ["bash", "sh", "zsh", "fish", "tmux", "nu"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Process {
    /// The executable.
    pub path: PathBuf,
    pub cwd: Option<PathBuf>,
}

impl Process {
    pub fn name(&self) -> &str {
        self.path.file_name().and_then(OsStr::to_str).unwrap_or_default()
    }

    /// Whether the process is a shell waiting at its prompt.
    pub fn is_shell(&self) -> bool {
        // A login shell may be named `-zsh`.
        SHELLS.contains(&self.name().trim_start_matches('-'))
    }
}

/// The foreground process group's leader on the terminal `master`.
pub fn foreground(master: BorrowedFd<'_>) -> Option<Process> {
    // SAFETY: a valid descriptor.
    let pid = unsafe { libc::tcgetpgrp(master.as_raw_fd()) };
    if pid <= 0 {
        return None;
    }
    Some(Process { path: path(pid)?, cwd: cwd(pid) })
}

fn path(pid: libc::pid_t) -> Option<PathBuf> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: `buf` has the length given.
    let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let len = usize::try_from(len).ok().filter(|&len| len > 0)?;
    Some(Path::new(OsStr::from_bytes(&buf[..len])).to_path_buf())
}

fn cwd(pid: libc::pid_t) -> Option<PathBuf> {
    // SAFETY: plain data, valid when zeroed.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: `info` has the size given.
    let written = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, (&raw mut info).cast(), size) };
    if written != size {
        return None;
    }
    // SAFETY: the path is a NUL-terminated string inside the array.
    let path = unsafe { CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()) };
    let path = Path::new(OsStr::from_bytes(path.to_bytes()));
    path.is_absolute().then(|| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_own_process_has_a_path_and_cwd() {
        let pid = std::process::id().cast_signed();
        assert!(path(pid).is_some_and(|p| p.is_absolute()));
        assert_eq!(cwd(pid), std::env::current_dir().ok());
    }

    #[test]
    fn shells_are_known_by_name() {
        let process = |path: &str| Process { path: path.into(), cwd: None };
        assert!(process("/bin/zsh").is_shell());
        assert!(process("/opt/homebrew/bin/fish").is_shell());
        assert!(!process("/usr/bin/vim").is_shell());
        assert!(!process("/Users/me/.local/bin/rho").is_shell());
    }
}

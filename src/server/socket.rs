//! The socket the server listens on, held by one server at a time.

use std::fs::{self, DirBuilder, File};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

/// A bound socket. Dropping it removes the socket file.
pub struct Listener {
    listener: UnixListener,
    path: PathBuf,
    /// Held while the server runs: `flock` on `<socket>.lock`.
    _lock: File,
}

impl Listener {
    /// Binds `path`, making its directory private to the user when it does
    /// not exist. `None` when another server holds it.
    pub fn bind(path: &Path) -> io::Result<Option<Self>> {
        if let Some(dir) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        }
        let lock = File::options().create(true).truncate(false).write(true).open(path.with_extension("lock"))?;
        // SAFETY: a plain call on a descriptor `lock` owns.
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let err = io::Error::last_os_error();
            return if err.kind() == io::ErrorKind::WouldBlock { Ok(None) } else { Err(err) };
        }
        // Whoever made it is gone, since the lock was free.
        match fs::remove_file(path) {
            Err(err) if err.kind() != io::ErrorKind::NotFound => return Err(err),
            _ => {}
        }
        let listener = UnixListener::bind(path)?;
        Ok(Some(Self { listener, path: path.to_owned(), _lock: lock }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn accept(&self) -> io::Result<UnixStream> {
        self.listener.accept().map(|(stream, _)| stream)
    }

    /// Wakes a blocked [`Listener::accept`] with a connection of its own.
    pub fn wake(&self) {
        // Failing means nothing is blocked there any more.
        let _ = UnixStream::connect(&self.path);
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.path) {
            log::warn!("removing {}: {err}", self.path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_server_per_socket_and_a_stale_socket_is_replaced() {
        let dir = std::env::temp_dir().join(format!("slutty-socket-{}", std::process::id()));
        let path = dir.join("mux.sock");
        fs::create_dir_all(&dir).expect("made");
        // A crashed server leaves its socket file behind.
        drop(UnixListener::bind(&path).expect("binds"));
        assert!(path.exists());
        let first = Listener::bind(&path).expect("binds").expect("free");
        assert!(Listener::bind(&path).expect("checks").is_none(), "taken");
        drop(first);
        assert!(!path.exists());
        fs::remove_dir_all(dir).expect("cleaned up");
    }
}

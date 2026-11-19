//! How messages cross the socket. Each side first sends a raw header, `SLTY`
//! then the protocol version and its pid, so two versions can always tell
//! they differ. Then each message is a `u32` LE length and a postcard body.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// Any change to a message bumps this; both sides must match exactly.
pub const VERSION: u32 = 1;

const MAGIC: [u8; 4] = *b"SLTY";

/// The largest body either side accepts.
const MAX_FRAME: usize = 16 << 20;

/// The server's socket: `explicit`, else `$SLUTTY_SOCKET`, else one in
/// Application Support, where macOS does not clean old files as it does in
/// the temporary directories.
pub fn socket_path(explicit: Option<PathBuf>) -> Option<PathBuf> {
    explicit
        .or_else(|| std::env::var_os("SLUTTY_SOCKET").filter(|path| !path.is_empty()).map(PathBuf::from))
        .or_else(|| Some(std::env::home_dir()?.join("Library/Application Support/slutty/mux.sock")))
}

/// What the other side says it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u32,
    pub pid: u32,
}

pub fn write_header(w: &mut impl Write) -> io::Result<()> {
    let mut bytes = [0; 12];
    bytes[..4].copy_from_slice(&MAGIC);
    bytes[4..8].copy_from_slice(&VERSION.to_le_bytes());
    bytes[8..].copy_from_slice(&std::process::id().to_le_bytes());
    w.write_all(&bytes)?;
    w.flush()
}

/// Reads the other side's header. Not speaking the protocol at all is an
/// error; another version is not, so the caller can say which.
pub fn read_header(r: &mut impl Read) -> io::Result<Header> {
    let mut bytes = [0; 12];
    r.read_exact(&mut bytes)?;
    if bytes[..4] != MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "not a slutty socket"));
    }
    let word = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    Ok(Header { version: word(4), pid: word(8) })
}

pub fn send<T: Serialize>(w: &mut impl Write, message: &T) -> io::Result<()> {
    let body = postcard::to_stdvec(message).map_err(io::Error::other)?;
    if body.len() > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("a {} byte message", body.len())));
    }
    let len = u32::try_from(body.len()).map_err(io::Error::other)?;
    w.write_all(&len.to_le_bytes())?;
    w.write_all(&body)?;
    w.flush()
}

/// The next message; `None` when the other side closed between messages.
pub fn receive<T: DeserializeOwned>(r: &mut impl Read) -> io::Result<Option<T>> {
    let mut len = [0; 4];
    loop {
        match r.read(&mut len[..1]) {
            Ok(0) => return Ok(None),
            Ok(_) => break,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    r.read_exact(&mut len[1..])?;
    let len = usize::try_from(u32::from_le_bytes(len)).map_err(io::Error::other)?;
    if len > MAX_FRAME {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("a {len} byte message")));
    }
    let mut body = vec![0; len];
    r.read_exact(&mut body)?;
    postcard::from_bytes(&body).map(Some).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_names_the_version_and_the_process() {
        let mut bytes = Vec::new();
        write_header(&mut bytes).expect("written");
        let header = read_header(&mut bytes.as_slice()).expect("read");
        assert_eq!(header, Header { version: VERSION, pid: std::process::id() });
        bytes[4] ^= 0xff;
        assert_ne!(read_header(&mut bytes.as_slice()).expect("read").version, VERSION);
        assert!(read_header(&mut &b"HTTP/1.1 200"[..]).is_err());
    }

    #[test]
    fn messages_follow_each_other_until_a_clean_end() {
        let mut bytes = Vec::new();
        send(&mut bytes, &"one").expect("sent");
        send(&mut bytes, &2u32).expect("sent");
        let mut r = bytes.as_slice();
        assert_eq!(receive::<String>(&mut r).expect("read").as_deref(), Some("one"));
        assert_eq!(receive::<u32>(&mut r).expect("read"), Some(2));
        assert_eq!(receive::<u32>(&mut r).expect("read"), None);
    }

    #[test]
    fn a_cut_or_oversized_message_is_an_error() {
        let mut bytes = Vec::new();
        send(&mut bytes, &"a message").expect("sent");
        assert!(receive::<String>(&mut &bytes[..bytes.len() - 1]).is_err());
        assert!(receive::<String>(&mut &bytes[..2]).is_err());
        let huge = u32::try_from(MAX_FRAME + 1).expect("fits").to_le_bytes();
        assert!(receive::<String>(&mut &huge[..]).is_err());
    }
}

//! iTerm2's user variables (`OSC 1337 ; SetUserVar=name=base64 ST`), which
//! rho uses to report its state. libghostty parses OSC 1337 itself and drops
//! this command, so the output is scanned for it before libghostty sees it.

/// Longer sequences are not user variables worth keeping.
const MAX_SEQUENCE: usize = 64 * 1024;

const PREFIX: &[u8] = b"1337;SetUserVar=";

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// After ESC.
    Escape,
    /// Inside an OSC, collecting it.
    Osc,
    /// Inside an OSC that is not a user variable, waiting for its end.
    Skip,
    /// After ESC inside an OSC: `\` ends it.
    OscEscape,
}

/// Finds user variables in output that may arrive in pieces.
#[derive(Debug, Default)]
pub struct Scanner {
    state: State,
    osc: Vec<u8>,
}

impl Scanner {
    /// Scans `data` and returns the variables it set, in order.
    pub fn feed(&mut self, data: &[u8]) -> Vec<(String, String)> {
        let mut found = Vec::new();
        let mut rest = data;
        while !rest.is_empty() {
            if self.state == State::Ground {
                // Most output has no escapes worth a look.
                match rest.iter().position(|&b| b == 0x1b) {
                    Some(i) => {
                        self.state = State::Escape;
                        rest = &rest[i + 1..];
                    }
                    None => break,
                }
                continue;
            }
            let b = rest[0];
            rest = &rest[1..];
            self.state = match (self.state, b) {
                (State::Escape, b']') => {
                    self.osc.clear();
                    State::Osc
                }
                (State::Escape, 0x1b) => State::Escape,
                (State::Escape, _) => State::Ground,
                (State::Osc | State::Skip, 0x07) | (State::OscEscape, b'\\') => {
                    found.extend(parse(&self.osc));
                    State::Ground
                }
                (State::Osc | State::Skip, 0x1b) => State::OscEscape,
                // CAN and SUB cancel a sequence.
                (State::Osc | State::Skip, 0x18 | 0x1a) => State::Ground,
                (State::Skip, _) => State::Skip,
                (State::Osc, b) => {
                    let other = self.osc.len() < PREFIX.len() && PREFIX[self.osc.len()] != b;
                    if other || self.osc.len() >= MAX_SEQUENCE {
                        self.osc.clear();
                        State::Skip
                    } else {
                        self.osc.push(b);
                        State::Osc
                    }
                }
                // ESC then anything but `\` ends the OSC and starts anew.
                (State::OscEscape, b']') => {
                    self.osc.clear();
                    State::Osc
                }
                (State::OscEscape, 0x1b) => State::Escape,
                (State::OscEscape, _) => State::Ground,
                (State::Ground, _) => State::Ground,
            };
        }
        found
    }
}

fn parse(osc: &[u8]) -> Option<(String, String)> {
    let (name, value) = osc.strip_prefix(PREFIX)?.split_at(osc[PREFIX.len()..].iter().position(|&b| b == b'=')?);
    let name = std::str::from_utf8(name).ok()?;
    let value = String::from_utf8(decode_base64(&value[1..])?).ok()?;
    Some((name.to_owned(), value))
}

fn decode_base64(text: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0);
    for &c in text.iter().take_while(|&&c| c != b'=') {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = acc << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn var(name: &str, value: &str) -> (String, String) {
        (name.to_owned(), value.to_owned())
    }

    #[test]
    fn finds_variables_with_either_terminator() {
        let mut s = Scanner::default();
        let found = s.feed(b"hi\x1b]1337;SetUserVar=rho_activity=d29ya2luZw==\x07x\x1b]1337;SetUserVar=a=MQ==\x1b\\");
        assert_eq!(found, vec![var("rho_activity", "working"), var("a", "1")]);
    }

    #[test]
    fn sequences_may_arrive_in_pieces() {
        let mut s = Scanner::default();
        let data = b"\x1b]1337;SetUserVar=focus_pane=MTIz\x1b\\";
        let mut found = Vec::new();
        for byte in data {
            found.extend(s.feed(&[*byte]));
        }
        assert_eq!(found, vec![var("focus_pane", "123")]);
    }

    #[test]
    fn other_sequences_are_ignored() {
        let mut s = Scanner::default();
        assert!(s.feed(b"\x1b]0;title\x07\x1b]1337;SetMark\x07\x1b[31m").is_empty());
        assert!(s.feed(b"\x1b]1337;SetUserVar=x=\x18").is_empty(), "cancelled");
        assert_eq!(s.feed(b"\x1b]1337;SetUserVar=empty=\x07"), vec![var("empty", "")]);
    }

    #[test]
    fn base64_without_padding_decodes() {
        assert_eq!(decode_base64(b"aGk").as_deref(), Some(&b"hi"[..]));
        assert_eq!(decode_base64(b"a?"), None);
    }
}

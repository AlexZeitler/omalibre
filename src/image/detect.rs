//! Finding out what the terminal can draw.
//!
//! The user picks the terminal, not us, so the reader asks instead of assuming.
//! Two questions go out and the answers decide: the Kitty graphics query and the
//! primary device attributes, whose answer lists Sixel support as feature 4.
//!
//! Anything unanswered means no. A terminal that stays silent must not hold up
//! the start, so the wait is short and failure is ordinary.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// Two pixels per cell. Works in every terminal and through tmux.
    HalfBlocks,
    /// Real pixels, supported by foot among others.
    Sixel,
    /// Real pixels, supported by Ghostty and kitty.
    Kitty,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::HalfBlocks => "half blocks",
            Backend::Sixel => "sixel",
            Backend::Kitty => "kitty graphics",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text.trim().to_ascii_lowercase().as_str() {
            "halfblocks" | "half-blocks" | "half_blocks" | "blocks" => Some(Backend::HalfBlocks),
            "sixel" => Some(Backend::Sixel),
            "kitty" => Some(Backend::Kitty),
            _ => None,
        }
    }
}

/// How long to wait for a terminal to answer a query.
const REPLY_TIMEOUT: Duration = Duration::from_millis(250);

/// Picks the best backend the terminal admits to supporting.
///
/// Must run before the alternate screen is entered and while the terminal is in
/// raw mode, because the answers arrive on stdin as escape sequences.
pub fn detect() -> Backend {
    // Inside a multiplexer the pixel protocols are out of reach. tmux manages the
    // screen itself and knows nothing about the pixels a passthrough would put
    // there, so a picture would survive scrolling and cover the text. Half blocks
    // are ordinary cells and behave.
    if std::env::var_os("TMUX").is_some() || is_screen() {
        return Backend::HalfBlocks;
    }

    match query() {
        Some(reply) => classify(&reply),
        None => Backend::HalfBlocks,
    }
}

fn is_screen() -> bool {
    std::env::var("TERM")
        .map(|term| term.starts_with("screen"))
        .unwrap_or(false)
}

/// Sends both queries at once, followed by a primary device attributes request
/// that every terminal answers. That last answer marks the end of the replies,
/// so there is no need to wait out the timeout when the terminal is quick.
fn query() -> Option<String> {
    // A one-pixel image, so the query costs nothing even if it is displayed by a
    // terminal that misunderstands it.
    const KITTY: &str = "\x1b_Gi=1,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";
    const DEVICE_ATTRIBUTES: &str = "\x1b[c";

    let mut out = std::io::stdout();
    out.write_all(KITTY.as_bytes()).ok()?;
    out.write_all(DEVICE_ATTRIBUTES.as_bytes()).ok()?;
    out.flush().ok()?;

    let deadline = Instant::now() + REPLY_TIMEOUT;
    let mut reply = String::new();
    let mut buffer = [0u8; 256];
    let mut stdin = std::io::stdin();

    while Instant::now() < deadline {
        if !readable(&deadline) {
            continue;
        }
        match stdin.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                reply.push_str(&String::from_utf8_lossy(&buffer[..read]));
                // The device attributes answer ends in `c` and comes last.
                if reply.contains('c') && reply.contains("\x1b[?") {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    if reply.is_empty() { None } else { Some(reply) }
}

/// Waits for input without spinning, using poll on the raw file descriptor.
fn readable(deadline: &Instant) -> bool {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return false;
    }
    let mut fds = [libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    }];
    let millis = remaining.as_millis().min(i32::MAX as u128) as i32;
    // Safety: the descriptor is stdin and the array is valid for the call.
    let ready = unsafe { libc::poll(fds.as_mut_ptr(), 1, millis) };
    ready > 0 && fds[0].revents & libc::POLLIN != 0
}

/// Reads a terminal's answers. Kitty support outranks Sixel, because it carries
/// full colour while Sixel is limited to a palette.
pub fn classify(reply: &str) -> Backend {
    if reply.contains("_Gi=1;OK") || reply.contains("_Gi=1;ok") {
        return Backend::Kitty;
    }
    if supports_sixel(reply) {
        return Backend::Sixel;
    }
    Backend::HalfBlocks
}

/// Looks for feature 4 in a primary device attributes answer, which is Sixel.
///
/// The answer looks like `ESC [ ? 62 ; 4 ; 22 c`, so the features are the
/// semicolon-separated numbers between `?` and `c`.
fn supports_sixel(reply: &str) -> bool {
    let Some(start) = reply.find("\x1b[?") else {
        return false;
    };
    let rest = &reply[start + 3..];
    let Some(end) = rest.find('c') else {
        return false;
    };
    rest[..end].split(';').any(|feature| feature.trim() == "4")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_a_kitty_answer() {
        assert_eq!(classify("\x1b_Gi=1;OK\x1b\\\x1b[?62;c"), Backend::Kitty);
    }

    #[test]
    fn recognises_sixel_in_device_attributes() {
        // foot answers like this.
        assert_eq!(classify("\x1b[?62;4;22c"), Backend::Sixel);
        assert!(supports_sixel("\x1b[?62;4c"));
        assert!(supports_sixel("\x1b[?64;1;2;4;6;9;15;22c"));
    }

    #[test]
    fn a_terminal_without_feature_four_gets_half_blocks() {
        // alacritty answers without Sixel.
        assert_eq!(classify("\x1b[?6c"), Backend::HalfBlocks);
        assert_eq!(classify("\x1b[?62;22c"), Backend::HalfBlocks);
        assert!(!supports_sixel("\x1b[?62;40c"), "40 must not match 4");
        assert!(!supports_sixel("\x1b[?14c"));
    }

    #[test]
    fn silence_means_half_blocks() {
        assert_eq!(classify(""), Backend::HalfBlocks);
        assert_eq!(classify("nonsense"), Backend::HalfBlocks);
    }

    #[test]
    fn kitty_outranks_sixel() {
        assert_eq!(classify("\x1b_Gi=1;OK\x1b\\\x1b[?62;4;22c"), Backend::Kitty);
    }

    #[test]
    fn names_round_trip_through_the_setting() {
        for backend in [Backend::HalfBlocks, Backend::Sixel, Backend::Kitty] {
            let written = match backend {
                Backend::HalfBlocks => "half-blocks",
                Backend::Sixel => "sixel",
                Backend::Kitty => "kitty",
            };
            assert_eq!(Backend::parse(written), Some(backend));
        }
        assert_eq!(Backend::parse("nonsense"), None);
    }
}

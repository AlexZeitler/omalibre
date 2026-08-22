//! Encoding pictures for the Kitty graphics protocol.
//!
//! The protocol takes PNG data as it is, so a book's own file can travel
//! unchanged and the terminal does the scaling. That keeps the full resolution
//! and avoids decoding altogether.
//!
//! Payloads have to be split into chunks of at most 4096 base64 characters, each
//! carrying `m=1` while more follows and `m=0` on the last one.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

const CHUNK: usize = 4096;

/// Builds the escape sequence that places a PNG at the cursor, scaled into
/// `cols` by `rows` cells.
pub fn encode_png(png: &[u8], cols: u16, rows: u16, id: u32) -> String {
    let payload = STANDARD.encode(png);
    let mut out = String::with_capacity(payload.len() + 256);
    let mut chunks = payload.as_bytes().chunks(CHUNK).peekable();
    let mut first = true;

    while let Some(chunk) = chunks.next() {
        let more = u8::from(chunks.peek().is_some());
        out.push_str("\x1b_G");
        if first {
            // f=100 announces PNG, a=T places it, c and r give the cell box.
            // C=1 keeps the cursor where it is, so the text layout is untouched.
            out.push_str(&format!(
                "i={id},f=100,a=T,c={cols},r={rows},C=1,q=2,m={more}"
            ));
            first = false;
        } else {
            out.push_str(&format!("m={more}"));
        }
        out.push(';');
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push_str("\x1b\\");
    }
    out
}

/// Removes every picture this reader placed.
pub fn delete_all() -> String {
    "\x1b_Ga=d,d=A,q=2\x1b\\".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_image_becomes_one_chunk() {
        let escape = encode_png(b"tiny", 10, 5, 7);
        assert_eq!(escape.matches("\x1b_G").count(), 1);
        assert!(escape.contains("i=7,f=100,a=T,c=10,r=5"));
        assert!(escape.contains("m=0"));
        assert!(escape.ends_with("\x1b\\"));
    }

    #[test]
    fn a_large_image_is_split_and_marked() {
        // Three chunks' worth of base64.
        let png = vec![0u8; CHUNK * 3];
        let escape = encode_png(&png, 40, 20, 1);
        let parts = escape.matches("\x1b_G").count();
        assert!(parts >= 3, "{parts} chunks");
        // Only the first chunk carries the parameters.
        assert_eq!(escape.matches("f=100").count(), 1);
        // Continuations announce more data, the last one does not.
        assert!(escape.contains("m=1"));
        assert_eq!(escape.matches("m=0").count(), 1);
    }

    #[test]
    fn the_payload_is_recoverable() {
        let png = b"\x89PNG\r\n\x1a\nsome data";
        let escape = encode_png(png, 4, 2, 3);
        let payload: String = escape
            .split("\x1b_G")
            .filter(|part| !part.is_empty())
            .map(|part| {
                let body = part.split(';').nth(1).unwrap_or("");
                body.trim_end_matches("\x1b\\").to_string()
            })
            .collect();
        assert_eq!(STANDARD.decode(payload).unwrap(), png);
    }

    #[test]
    fn deletion_targets_every_picture() {
        assert!(delete_all().contains("d=A"));
    }
}

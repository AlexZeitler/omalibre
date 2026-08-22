//! Identifying a book by its content.
//!
//! The path must not be the key. A book that moves keeps its reading position,
//! and the same file sits at different paths on different machines, so the
//! identity is a hash over the file's bytes.

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::Path;

/// A book's content hash, rendered as `sha256:<hex>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BookId(String);

impl BookId {
    pub fn of_file(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
        let mut reader = BufReader::new(file);
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        // sha2 0.11 returns a byte array without hex formatting of its own.
        let digest = hasher.finalize();
        let mut hex = String::with_capacity(7 + digest.len() * 2);
        hex.push_str("sha256:");
        for byte in digest {
            hex.push_str(&format!("{byte:02x}"));
        }
        Ok(Self(hex))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BookId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<String> for BookId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(name: &str, contents: &[u8]) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("omalibre-test-{name}"));
        let mut file = File::create(&path).unwrap();
        file.write_all(contents).unwrap();
        path
    }

    #[test]
    fn same_content_yields_the_same_id() {
        let a = temp_file("id-a", b"content of a book");
        let b = temp_file("id-b", b"content of a book");
        assert_eq!(BookId::of_file(&a).unwrap(), BookId::of_file(&b).unwrap());
        std::fs::remove_file(a).ok();
        std::fs::remove_file(b).ok();
    }

    #[test]
    fn different_content_yields_different_ids() {
        let a = temp_file("id-c", b"one book");
        let b = temp_file("id-d", b"another book");
        assert_ne!(BookId::of_file(&a).unwrap(), BookId::of_file(&b).unwrap());
        std::fs::remove_file(a).ok();
        std::fs::remove_file(b).ok();
    }

    #[test]
    fn matches_the_reference_digest() {
        let path = temp_file("id-e", b"abc");
        // Known SHA-256 of "abc".
        assert_eq!(
            BookId::of_file(&path).unwrap().as_str(),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_file(path).ok();
    }
}

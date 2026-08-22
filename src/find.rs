//! Searching the whole library and opening a hit.
//!
//! One command, one word to type: `omalibre find accessory`. The hits appear as a
//! list, and Enter opens the passage in the reader. Nobody assembles a shell
//! pipeline to look something up.
//!
//! Where qmd is installed, its index answers: fast, and by meaning as well as by
//! wording. Where it is not, the library is searched directly - slower, because
//! every book has to be unpacked, but the command works either way rather than
//! demanding a second tool.

use crate::identity::BookId;
use crate::journal::State;
use crate::{export, library, search};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::PathBuf;

/// One hit, whatever found it.
#[derive(Debug, Clone)]
pub struct Hit {
    /// Book title, for the list.
    pub book_title: String,
    pub chapter_title: String,
    /// What to open.
    pub book: BookId,
    pub chapter_href: Option<String>,
    /// Passage to jump to inside the chapter.
    pub passage: Option<String>,
    /// Text to show in the list.
    pub snippet: String,
}

/// Where the hits came from, for the status line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// qmd's index answered.
    Index,
    /// The books were read directly.
    Direct,
}

pub struct Results {
    pub hits: Vec<Hit>,
    pub source: Source,
    pub query: String,
}

/// Searches the library, preferring qmd where it can answer.
pub fn find(
    query: &str,
    state: &State,
    limit: usize,
    report: &mut dyn FnMut(&str),
) -> Result<Results> {
    if let Some(hits) = try_qmd(query, state, limit) {
        if !hits.is_empty() {
            return Ok(Results {
                hits,
                source: Source::Index,
                query: query.to_string(),
            });
        }
    }
    let hits = search_directly(query, state, limit, report)?;
    Ok(Results {
        hits,
        source: Source::Direct,
        query: query.to_string(),
    })
}

// ----- via qmd -----

#[derive(Debug, Deserialize)]
struct QmdHit {
    file: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    snippet: String,
}

/// Asks qmd. `None` when it is absent or answers with nothing usable, so the
/// caller can fall back without having to tell the two cases apart.
fn try_qmd(query: &str, state: &State, limit: usize) -> Option<Vec<Hit>> {
    let output = std::process::Command::new("qmd")
        .args(["search", query, "-n", &limit.to_string(), "--json"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    // qmd writes progress markers to the same stream; the payload is the JSON
    // array that follows them.
    let start = text.find('[')?;
    let parsed: Vec<QmdHit> = serde_json::from_str(&text[start..]).ok()?;

    let export_dir = export::default_dir().ok()?;
    let mut hits = Vec::new();
    for hit in parsed {
        let (path, line) = split_qmd_reference(&hit.file, &export_dir);
        let Ok(origin) = export::origin_of(&path) else {
            continue;
        };
        let book = BookId::from(origin.book);
        // A hit for a book no longer in the library cannot be opened.
        let Some(record) = state.book(&book) else {
            continue;
        };
        let chapter_title = strip_book_prefix(&hit.title, &record.display_title());
        let mut snippet = tidy(&hit.snippet);
        let passage = passage_of(&hit.snippet).or_else(|| line.and_then(|l| line_text(&path, l)));
        // The match sat in the front matter, so the file holds the text to show.
        if snippet.is_empty() {
            snippet = first_prose(&path).unwrap_or_else(|| chapter_title.clone());
        }
        hits.push(Hit {
            book_title: record.display_title(),
            // The exported heading names the book as well, which a search engine
            // needs but a list that already shows the book does not.
            chapter_title,
            book,
            chapter_href: origin.chapter,
            passage,
            snippet,
        });
    }
    Some(hits)
}

/// Removes a leading book title from a chapter heading.
fn strip_book_prefix(title: &str, book: &str) -> String {
    let separator = format!("{book} · ");
    title
        .strip_prefix(&separator)
        .unwrap_or(title)
        .trim()
        .to_string()
}

/// Turns `qmd://collection/relative/path.md:12` into a path and a line.
fn split_qmd_reference(reference: &str, export_dir: &std::path::Path) -> (PathBuf, Option<usize>) {
    let (body, line) = match reference.rsplit_once(':') {
        Some((body, tail)) => match tail.parse::<usize>() {
            Ok(line) => (body, Some(line)),
            Err(_) => (reference, None),
        },
        None => (reference, None),
    };
    let path = match body.strip_prefix("qmd://") {
        Some(rest) => {
            // Drop the collection name; the rest is relative to the export.
            let relative = rest.split_once('/').map(|(_, rest)| rest).unwrap_or(rest);
            export_dir.join(relative)
        }
        None => PathBuf::from(body),
    };
    (path, line)
}

/// True for lines that belong to the exported file rather than to the book: the
/// front matter naming book and chapter, and qmd's own diff-style header.
///
/// Both are indexed along with the text, so a search for words that appear in a
/// chapter title finds them there. Showing that as the passage would put file
/// bookkeeping where the reader expects a sentence from the book.
fn is_bookkeeping(line: &str) -> bool {
    let line = line.trim();
    if line.starts_with("@@") || line == "---" {
        return true;
    }
    // A front matter entry: a bare key, a colon, then a quoted value.
    match line.split_once(':') {
        Some((key, value)) => {
            !key.is_empty()
                && key.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                && value.trim_start().starts_with('"')
        }
        None => false,
    }
}

/// The first real sentence of a snippet, as something to search for in the book.
fn passage_of(snippet: &str) -> Option<String> {
    let body = snippet
        .lines()
        .filter(|line| !is_bookkeeping(line) && !line.trim().is_empty())
        .find(|line| !line.starts_with('#'))?;
    let words: Vec<&str> = body.split_whitespace().take(8).collect();
    if words.len() < 2 {
        None
    } else {
        Some(words.join(" "))
    }
}

/// The first sentence of an exported chapter, for a hit that matched its title.
fn first_prose(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    // Skip the front matter, then the title line.
    let body = text.split("\n---").nth(1).unwrap_or(&text);
    let line = body
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#') && !is_bookkeeping(line))?;
    Some(line.to_string())
}

fn line_text(path: &std::path::Path, line: usize) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let raw = text.lines().nth(line.saturating_sub(1))?.trim();
    let cleaned = raw.trim_start_matches(['#', '>', '-', '*', ' ']).trim();
    let words: Vec<&str> = cleaned.split_whitespace().take(8).collect();
    if words.len() < 2 {
        None
    } else {
        Some(words.join(" "))
    }
}

/// Reduces a snippet to one line of book text for the list.
fn tidy(snippet: &str) -> String {
    let body: Vec<&str> = snippet
        .lines()
        .filter(|line| !is_bookkeeping(line) && !line.trim().is_empty())
        .collect();
    body.join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

// ----- without qmd -----

/// Reads every book and searches it. Reports progress, because unpacking hundreds
/// of books is not instant.
fn search_directly(
    query: &str,
    state: &State,
    limit: usize,
    report: &mut dyn FnMut(&str),
) -> Result<Vec<Hit>> {
    let mut entries = library::entries(state);
    // Recently read books first: what you are looking for is more likely there.
    library::sort(&mut entries, library::Order::Recent);

    let mut hits = Vec::new();
    for entry in entries {
        if hits.len() >= limit {
            break;
        }
        let Some(path) = entry.record.path() else {
            continue;
        };
        report(&entry.record.display_title());
        let Ok(mut book) = crate::epub::Book::open(path) else {
            continue;
        };
        for index in 0..book.spine.len() {
            if hits.len() >= limit {
                break;
            }
            let Ok(chapter) = book.chapter(index) else {
                continue;
            };
            let found = search::find_all(&chapter, query);
            if found.is_empty() {
                continue;
            }
            let chapter_title = book
                .spine
                .get(index)
                .and_then(|item| item.title.clone())
                .unwrap_or_else(|| format!("Chapter {}", index + 1));

            // One hit per chapter: a list of forty hits from one chapter would
            // bury the other books.
            let (block, offset) = found[0];
            let text = chapter
                .blocks
                .get(block)
                .map(|b| b.plain_text())
                .unwrap_or_default();
            hits.push(Hit {
                book_title: entry.record.display_title(),
                chapter_title,
                book: entry.id.clone(),
                chapter_href: Some(chapter.href.clone()),
                passage: Some(query.to_string()),
                snippet: around(&text, offset, query.chars().count()),
            });
        }
    }
    Ok(hits)
}

fn around(text: &str, offset: usize, length: usize) -> String {
    const BEFORE: usize = 40;
    const AFTER: usize = 140;
    let chars: Vec<char> = text.chars().collect();
    let start = offset.saturating_sub(BEFORE);
    let end = (offset + length + AFTER).min(chars.len());
    let mut out = String::new();
    if start > 0 {
        out.push('…');
    }
    out.extend(&chars[start..end]);
    if end < chars.len() {
        out.push('…');
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The file to open for a hit.
pub fn file_of(hit: &Hit, state: &State) -> Result<PathBuf> {
    state
        .book(&hit.book)
        .and_then(|record| record.path().cloned())
        .with_context(|| format!("no file recorded for {}", hit.book_title))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn a_qmd_reference_becomes_a_path_and_a_line() {
        let (path, line) = split_qmd_reference(
            "qmd://books/some-book-1234abcd/019-15-accessories.md:13",
            Path::new("/export"),
        );
        assert_eq!(
            path,
            PathBuf::from("/export/some-book-1234abcd/019-15-accessories.md")
        );
        assert_eq!(line, Some(13));
    }

    #[test]
    fn a_reference_without_a_line_still_resolves() {
        let (path, line) = split_qmd_reference("qmd://books/b/001-x.md", Path::new("/export"));
        assert_eq!(path, PathBuf::from("/export/b/001-x.md"));
        assert_eq!(line, None);
    }

    #[test]
    fn a_plain_path_is_left_alone() {
        let (path, line) = split_qmd_reference("/tmp/a/001-x.md:7", Path::new("/export"));
        assert_eq!(path, PathBuf::from("/tmp/a/001-x.md"));
        assert_eq!(line, Some(7));
    }

    #[test]
    fn a_chapter_heading_loses_the_book_it_repeats() {
        assert_eq!(
            strip_book_prefix("Kamal Handbook · 15. Accessories", "Kamal Handbook"),
            "15. Accessories"
        );
        // A heading that does not repeat it stays whole.
        assert_eq!(
            strip_book_prefix("15. Accessories", "Kamal Handbook"),
            "15. Accessories"
        );
    }

    #[test]
    fn a_passage_skips_the_diff_header_and_headings() {
        let snippet = "@@ -12,4 @@ (11 before, 459 after)\n\n# Some Book · 15. Accessories\n\nAn accessory is Kamal's answer to running a permanent service.";
        assert_eq!(
            passage_of(snippet).as_deref(),
            Some("An accessory is Kamal's answer to running a")
        );
    }

    #[test]
    fn a_snippet_without_prose_yields_no_passage() {
        assert_eq!(passage_of("@@ -1,1 @@\n\n# Only a heading"), None);
        assert_eq!(passage_of(""), None);
    }

    #[test]
    fn front_matter_is_not_mistaken_for_book_text() {
        assert!(is_bookkeeping("chapter: \"OEBPS/f_0034.xhtml\""));
        assert!(is_bookkeeping("chapter_title: \"How to Take Notes\""));
        assert!(is_bookkeeping("---"));
        assert!(is_bookkeeping("@@ -12,4 @@"));
        // A sentence with a colon is book text, not bookkeeping.
        assert!(!is_bookkeeping("There is one rule: keep it simple."));
        assert!(!is_bookkeeping("Chapter 4: Concurrency"));
        assert!(!is_bookkeeping("plain prose"));
    }

    #[test]
    fn a_snippet_of_pure_front_matter_yields_nothing() {
        let snippet = "@@ -1,4 @@\n\n---\nbook: \"sha256:abc\"\nchapter_title: \"Notes\"\n---";
        assert_eq!(tidy(snippet), "");
        assert_eq!(passage_of(snippet), None);
    }

    #[test]
    fn a_snippet_becomes_one_line() {
        let tidied = tidy("@@ -12,4 @@\n\nFirst line\nsecond   line\n");
        assert_eq!(tidied, "First line second line");
    }

    #[test]
    fn context_marks_where_it_was_cut() {
        let text = "word ".repeat(80);
        let window = around(&text, 200, 4);
        assert!(window.starts_with('…'));
        assert!(window.ends_with('…'));
    }
}

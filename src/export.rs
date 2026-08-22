//! Writing books and annotations out as Markdown.
//!
//! The point is not to convert books for their own sake, but to feed a search
//! engine. qmd indexes Markdown collections with BM25 and embeddings; giving it
//! one file per chapter saves building an index here, and it makes the whole
//! library searchable by meaning rather than only by substring.
//!
//! Every file carries front matter naming the book and the chapter it came from,
//! so a hit can be turned back into a place in the reader. Without that the
//! search would be a dead end: it would tell you that something exists, but not
//! where to read it.

use crate::doc::{BlockKind, Chapter};
use crate::epub::Book;
use crate::identity::BookId;
use crate::journal::{BookRecord, State};
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

#[derive(Debug, Default)]
pub struct Report {
    pub books: usize,
    pub chapters: usize,
    pub annotations: usize,
    pub skipped: Vec<(String, String)>,
    /// Books left alone because nothing changed since the last export.
    pub unchanged: usize,
}

/// Writes the library to `dir`, one directory per book.
///
/// `force` writes even where the export is newer than the book, which is
/// otherwise skipped: re-exporting a whole library on every run would make the
/// command useless in a loop.
pub fn export(dir: &Path, state: &State, force: bool, journal_dir: &Path) -> Result<Report> {
    let mut report = Report::default();
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    // Annotations follow the journal, not the book file, so they need their own
    // yardstick for what counts as current.
    let journal_changed = newest_in(journal_dir);

    let mut books: Vec<(BookId, BookRecord)> = state
        .books()
        .map(|(id, record)| (id.clone(), record.clone()))
        .collect();
    // A stable order keeps the output diffable.
    books.sort_by_key(|(_, record)| record.display_title().to_lowercase());

    for (id, record) in books {
        let Some(path) = record.path().cloned() else {
            continue;
        };
        let slug = slug_of(&record, &id);
        let book_dir = dir.join(&slug);

        let annotations = state.annotations(&id);
        let chapters_current = !force && is_current(&book_dir, &path);
        let notes_file = book_dir.join("000-annotations.md");
        let notes_current = !force
            && (annotations.is_empty() && !notes_file.exists()
                || match (newest(&notes_file), journal_changed) {
                    (Some(written), Some(journal)) => written >= journal,
                    _ => false,
                });

        if chapters_current && notes_current {
            report.unchanged += 1;
            continue;
        }

        // Only the annotations changed: rewriting 300 chapters for one note would
        // make the incremental export pointless.
        if chapters_current {
            std::fs::create_dir_all(&book_dir)?;
            if annotations.is_empty() {
                std::fs::remove_file(&notes_file).ok();
            } else {
                std::fs::write(
                    &notes_file,
                    annotations_markdown(&id, &record, &annotations),
                )?;
                report.annotations += annotations.len();
            }
            report.books += 1;
            continue;
        }

        let mut book = match Book::open(&path) {
            Ok(book) => book,
            Err(err) => {
                report.skipped.push((slug, format!("{err:#}")));
                continue;
            }
        };
        std::fs::create_dir_all(&book_dir)?;

        // A stale export must not linger: chapters can vanish when a book is
        // replaced by another edition. The annotations file is written below and
        // is not a chapter, so it stays.
        clear_chapters(&book_dir)?;

        let count = book.spine.len();
        for index in 0..count {
            let title = book
                .spine
                .get(index)
                .and_then(|item| item.title.clone())
                .unwrap_or_else(|| format!("Chapter {}", index + 1));
            let Ok(chapter) = book.chapter(index) else {
                continue;
            };
            let body =
                chapter_markdown_titled(&chapter, Some(&record.display_title()), Some(&title));
            if body.trim().is_empty() {
                continue;
            }
            let file = book_dir.join(format!("{:03}-{}.md", index + 1, slugify(&title)));
            let text = format!(
                "{}\n{body}",
                front_matter(&[
                    ("book", id.as_str()),
                    ("title", &record.display_title()),
                    ("author", &record.display_authors()),
                    ("chapter", &chapter.href),
                    ("chapter_title", &title),
                    ("chapter_number", &(index + 1).to_string()),
                ])
            );
            std::fs::write(&file, text)
                .with_context(|| format!("cannot write {}", file.display()))?;
            report.chapters += 1;
        }

        // Annotations go into a file of their own, so a search can find what you
        // wrote about a book as well as what it says.
        if !annotations.is_empty() {
            std::fs::write(
                &notes_file,
                annotations_markdown(&id, &record, &annotations),
            )?;
            report.annotations += annotations.len();
        }

        report.books += 1;
    }

    Ok(report)
}

/// Modification time of a path, if it can be read.
fn newest(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Newest modification time anywhere in a directory.
fn newest_in(dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| newest(&entry.path()))
        .max()
}

/// True when the exported chapters are at least as new as the book file.
fn is_current(book_dir: &Path, book: &Path) -> bool {
    let Some(source) = newest(book) else {
        return false;
    };
    // The annotations file follows the journal, so it must not vouch for the
    // chapters being current.
    let Ok(entries) = std::fs::read_dir(book_dir) else {
        return false;
    };
    entries
        .flatten()
        .filter(|entry| entry.file_name() != "000-annotations.md")
        .filter_map(|entry| newest(&entry.path()))
        .max()
        .is_some_and(|exported| exported >= source)
}

fn clear_chapters(dir: &Path) -> Result<()> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name() == "000-annotations.md" {
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) == Some("md") {
            std::fs::remove_file(path).ok();
        }
    }
    Ok(())
}

/// YAML front matter, as qmd and most Markdown tools expect it.
fn front_matter(fields: &[(&str, &str)]) -> String {
    let mut out = String::from("---\n");
    for (key, value) in fields {
        out.push_str(&format!("{key}: {}\n", quote(value)));
    }
    out.push_str("---\n");
    out
}

/// Quotes a YAML scalar. Colons and leading characters would otherwise change
/// the meaning of a line, and book titles are full of colons.
fn quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{}\"", escaped.replace('\n', " "))
}

/// Turns a chapter into Markdown.
///
/// The aim is searchable prose, not a faithful copy: headings keep their level so
/// a hit can be placed in the book's structure, code stays fenced so it is not
/// mistaken for text, and images become their alt text.
#[cfg(test)]
pub fn chapter_markdown(chapter: &Chapter) -> String {
    chapter_markdown_titled(chapter, None, None)
}

/// As `chapter_markdown`, with a title line naming book and chapter.
///
/// Search engines show a document's first heading as the title of a hit, and
/// "34" or "Pattern: Snapshots" alone says nothing about the book. The chapter's
/// own headings move down one level so the document has a single title with the
/// chapter's structure beneath it. Replacing the first heading instead would go
/// wrong wherever a book splits number and title across two of them.
pub fn chapter_markdown_titled(
    chapter: &Chapter,
    book_title: Option<&str>,
    chapter_title: Option<&str>,
) -> String {
    let mut out = String::new();
    let mut in_code = false;
    // Only shift when a title of our own is added, or the levels would be wrong
    // for anyone reading the file on its own.
    let shift = u8::from(book_title.is_some() || chapter_title.is_some());

    if shift == 1 {
        let title = match (book_title, chapter_title) {
            (Some(book), Some(chapter)) => format!("{book} · {chapter}"),
            (Some(book), None) => book.to_string(),
            (None, Some(chapter)) => chapter.to_string(),
            (None, None) => unreachable!("shift is set only when one is given"),
        };
        out.push_str(&format!("# {title}\n\n"));
    }

    for block in &chapter.blocks {
        let text = block.plain_text();
        let is_code = block.kind == BlockKind::Code;

        if is_code && !in_code {
            out.push_str("\n```\n");
            in_code = true;
        } else if !is_code && in_code {
            out.push_str("```\n\n");
            in_code = false;
        }

        match &block.kind {
            BlockKind::Heading(level) => {
                let hashes = "#".repeat(level.saturating_add(shift).clamp(1, 6) as usize);
                out.push_str(&format!("\n{hashes} {text}\n\n"));
            }
            BlockKind::Code => {
                out.push_str(&text);
                out.push('\n');
            }
            BlockKind::Quote => out.push_str(&format!("> {text}\n\n")),
            BlockKind::ListItem { depth, ordinal } => {
                let indent = "  ".repeat(*depth as usize);
                match ordinal {
                    Some(n) => out.push_str(&format!("{indent}{n}. {text}\n")),
                    None => out.push_str(&format!("{indent}- {text}\n")),
                }
            }
            BlockKind::Rule => out.push_str("\n---\n\n"),
            BlockKind::Image { .. } => {
                if !text.trim().is_empty() {
                    out.push_str(&format!("![{text}]()\n\n"));
                }
            }
            BlockKind::Paragraph => {
                if !text.trim().is_empty() {
                    out.push_str(&format!("{text}\n\n"));
                }
            }
        }
    }
    if in_code {
        out.push_str("```\n");
    }
    out
}

/// Annotations of one book as Markdown, quote and comment together.
fn annotations_markdown(
    id: &BookId,
    record: &BookRecord,
    annotations: &[crate::annotation::Annotation],
) -> String {
    let mut out = front_matter(&[
        ("book", id.as_str()),
        ("title", &record.display_title()),
        ("author", &record.display_authors()),
        ("kind", "annotations"),
    ]);
    out.push_str(&format!("\n# Notes on {}\n\n", record.display_title()));

    for annotation in annotations {
        out.push_str(&format!(
            "## {} · {}\n\n",
            annotation.color.label(),
            annotation.href
        ));
        out.push_str(&format!("> {}\n\n", annotation.quote.replace('\n', " ")));
        if let Some(note) = &annotation.note {
            out.push_str(&format!("{note}\n\n"));
        }
        // The anchor, so `omalibre open` can land on the passage itself.
        if let Some(slice) = annotation.slices.first() {
            out.push_str(&format!(
                "<!-- at chapter={} block={} offset={} -->\n\n",
                annotation.href, slice.block, slice.start
            ));
        }
    }
    out
}

/// Directory name for a book: its title, plus enough of the hash to keep two
/// books of the same title apart.
fn slug_of(record: &BookRecord, id: &BookId) -> String {
    let short: String = id.as_str().chars().skip("sha256:".len()).take(8).collect();
    format!("{}-{short}", slugify(&record.display_title()))
}

/// Lowercase, ASCII, hyphens. A file name that survives every filesystem.
pub fn slugify(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_dash = true;
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    // Long titles would make unwieldy paths.
    let cut: String = trimmed.chars().take(60).collect();
    if cut.is_empty() {
        "untitled".to_string()
    } else {
        cut.trim_matches('-').to_string()
    }
}

/// What an exported file says about where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    pub book: String,
    pub chapter: Option<String>,
}

/// Reads the front matter of an exported file. This is what turns a search hit
/// back into a place in a book.
pub fn origin_of(path: &Path) -> Result<Origin> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    parse_origin(&text).with_context(|| format!("{} carries no book reference", path.display()))
}

fn parse_origin(text: &str) -> Option<Origin> {
    let body = text.strip_prefix("---")?;
    let end = body.find("\n---")?;
    let mut book = None;
    let mut chapter = None;
    for line in body[..end].lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_string();
        match key.trim() {
            "book" => book = Some(value),
            "chapter" => chapter = Some(value),
            _ => {}
        }
    }
    Some(Origin {
        book: book?,
        chapter,
    })
}

/// Default place for the export, beside the read model.
pub fn default_dir() -> Result<PathBuf> {
    Ok(crate::paths::data_dir()?.join("export"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Block, Run, RunStyle};

    fn block(kind: BlockKind, text: &str) -> Block {
        Block {
            kind,
            runs: vec![Run {
                text: text.into(),
                style: RunStyle::default(),
            }],
        }
    }

    fn chapter(blocks: Vec<Block>) -> Chapter {
        Chapter {
            href: "OEBPS/ch01.xhtml".into(),
            blocks,
            ..Chapter::default()
        }
    }

    #[test]
    fn one_title_line_names_book_and_chapter() {
        let md = chapter_markdown_titled(
            &chapter(vec![
                block(BlockKind::Heading(1), "34"),
                block(BlockKind::Heading(1), "Pattern: Snapshots"),
            ]),
            Some("Understanding Eventsourcing"),
            Some("34. Pattern: Snapshots"),
        );
        assert!(
            md.starts_with("# Understanding Eventsourcing · 34. Pattern: Snapshots"),
            "{md}"
        );
        // Exactly one first-level heading: the title.
        assert_eq!(
            md.lines().filter(|l| l.starts_with("# ")).count(),
            1,
            "{md}"
        );
        // The book's own headings sit below it.
        assert!(md.contains("## 34"));
        assert!(md.contains("## Pattern: Snapshots"));
    }

    #[test]
    fn a_chapter_without_a_heading_still_has_a_title() {
        let md = chapter_markdown_titled(
            &chapter(vec![block(BlockKind::Paragraph, "just prose")]),
            Some("A Book"),
            Some("1. Beginning"),
        );
        assert!(md.starts_with("# A Book · 1. Beginning"), "{md}");
    }

    #[test]
    fn without_a_title_the_levels_are_untouched() {
        let md = chapter_markdown(&chapter(vec![block(BlockKind::Heading(1), "Title")]));
        assert!(md.contains("# Title"));
        assert!(!md.contains("## Title"));
    }

    #[test]
    fn headings_keep_their_level() {
        let md = chapter_markdown(&chapter(vec![
            block(BlockKind::Heading(1), "Title"),
            block(BlockKind::Heading(3), "Detail"),
        ]));
        assert!(md.contains("# Title"));
        assert!(md.contains("### Detail"));
    }

    #[test]
    fn code_is_fenced_once_per_run() {
        let md = chapter_markdown(&chapter(vec![
            block(BlockKind::Paragraph, "before"),
            block(BlockKind::Code, "line one"),
            block(BlockKind::Code, "line two"),
            block(BlockKind::Paragraph, "after"),
        ]));
        // One fence open, one close, not one pair per line.
        assert_eq!(md.matches("```").count(), 2, "{md}");
        assert!(md.contains("line one\nline two"));
    }

    #[test]
    fn an_unclosed_fence_is_closed_at_the_end() {
        let md = chapter_markdown(&chapter(vec![block(BlockKind::Code, "last")]));
        assert_eq!(md.matches("```").count(), 2, "{md}");
    }

    #[test]
    fn lists_keep_their_shape() {
        let md = chapter_markdown(&chapter(vec![
            block(
                BlockKind::ListItem {
                    depth: 0,
                    ordinal: Some(1),
                },
                "first",
            ),
            block(
                BlockKind::ListItem {
                    depth: 1,
                    ordinal: None,
                },
                "nested",
            ),
        ]));
        assert!(md.contains("1. first"));
        assert!(md.contains("  - nested"));
    }

    #[test]
    fn front_matter_survives_a_colon_in_the_title() {
        let text = format!(
            "{}\nbody",
            front_matter(&[
                ("book", "sha256:abc"),
                ("title", "Kamal Handbook: The missing manual"),
            ])
        );
        let origin = parse_origin(&text).expect("front matter");
        assert_eq!(origin.book, "sha256:abc");
    }

    #[test]
    fn an_origin_is_read_back() {
        let text = format!(
            "{}\n# Body\n",
            front_matter(&[
                ("book", "sha256:abc"),
                ("title", "A Book"),
                ("chapter", "OEBPS/ch02.xhtml"),
            ])
        );
        assert_eq!(
            parse_origin(&text),
            Some(Origin {
                book: "sha256:abc".into(),
                chapter: Some("OEBPS/ch02.xhtml".into()),
            })
        );
    }

    #[test]
    fn a_file_without_front_matter_has_no_origin() {
        assert!(parse_origin("# Just a heading\n").is_none());
        assert!(parse_origin("").is_none());
        // Front matter without a book reference is not an origin either.
        assert!(parse_origin("---\ntitle: \"x\"\n---\n").is_none());
    }

    #[test]
    fn slugs_are_safe_file_names() {
        assert_eq!(
            slugify("Kamal Handbook: The missing manual"),
            "kamal-handbook-the-missing-manual"
        );
        assert_eq!(slugify("C++ für Anfänger"), "c-f-r-anf-nger");
        assert_eq!(slugify("!!!"), "untitled");
        assert!(slugify(&"x".repeat(200)).chars().count() <= 60);
    }
}

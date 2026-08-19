//! Live Markdown mirror of annotations for an Obsidian vault.
//!
//! The journal stays the source of truth. When `notes_dir` is set, every
//! annotation change also rewrites one Markdown file per book so the vault
//! stays current without a separate export step.

use crate::annotation::Annotation;
use crate::export;
use crate::identity::BookId;
use crate::journal::BookRecord;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Path of the notes file for one book.
pub fn path_for(notes_dir: &Path, record: &BookRecord, id: &BookId) -> PathBuf {
    notes_dir.join(format!("{}.md", slug_of(record, id)))
}

/// Rewrites the book's notes file from the current annotations.
///
/// An empty annotation list removes the file so the vault does not keep stale
/// notes after the last highlight is deleted.
pub fn write_book(
    notes_dir: &Path,
    id: &BookId,
    record: &BookRecord,
    annotations: &[Annotation],
) -> Result<()> {
    std::fs::create_dir_all(notes_dir)
        .with_context(|| format!("cannot create {}", notes_dir.display()))?;
    let path = path_for(notes_dir, record, id);
    if annotations.is_empty() {
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("cannot remove {}", path.display()))?;
        }
        return Ok(());
    }
    let body = annotations_markdown(id, record, annotations);
    atomic_write(&path, &body)
        .with_context(|| format!("cannot write {}", path.display()))?;
    Ok(())
}

/// Writes every book that has annotations into `notes_dir`.
///
/// Used by `omalibre --sync-notes` to backfill a vault from the journal.
pub fn sync_all(notes_dir: &Path, state: &crate::journal::State) -> Result<usize> {
    let mut written = 0usize;
    let mut books: Vec<(BookId, BookRecord)> = state
        .books()
        .map(|(id, record)| (id.clone(), record.clone()))
        .collect();
    books.sort_by_key(|(_, record)| record.display_title().to_lowercase());

    for (id, record) in books {
        let annotations = state.annotations(&id);
        if annotations.is_empty() {
            let path = path_for(notes_dir, &record, &id);
            if path.exists() {
                std::fs::remove_file(&path).ok();
            }
            continue;
        }
        write_book(notes_dir, &id, &record, &annotations)?;
        written += 1;
    }
    Ok(written)
}

fn annotations_markdown(id: &BookId, record: &BookRecord, annotations: &[Annotation]) -> String {
    let mut out = front_matter(&[
        ("book", id.as_str()),
        ("title", &record.display_title()),
        ("author", &record.display_authors()),
        ("kind", "annotations"),
        ("omalibre", "1"),
    ]);
    out.push_str(&format!("\n# Notes on {}\n\n", record.display_title()));

    for annotation in annotations {
        out.push_str(&format!(
            "## {} · {}\n",
            annotation.color.label(),
            annotation.href
        ));
        out.push_str(&format!("<!-- id: {} -->\n", annotation.id));
        if let Some(slice) = annotation.slices.first() {
            let end = annotation
                .slices
                .last()
                .map(|s| s.end)
                .unwrap_or(slice.end);
            out.push_str(&format!(
                "<!-- at chapter={} block={} offset={} end={} -->\n\n",
                annotation.href, slice.block, slice.start, end
            ));
        } else {
            out.push('\n');
        }
        out.push_str(&format!("> {}\n\n", annotation.quote.replace('\n', " ")));
        if let Some(note) = &annotation.note {
            out.push_str(note.trim());
            out.push_str("\n\n");
        }
    }
    out
}

fn slug_of(record: &BookRecord, id: &BookId) -> String {
    let short: String = id.as_str().chars().skip("sha256:".len()).take(8).collect();
    format!("{}-{short}", export::slugify(&record.display_title()))
}

fn front_matter(fields: &[(&str, &str)]) -> String {
    let mut out = String::from("---\n");
    for (key, value) in fields {
        out.push_str(&format!("{key}: {}\n", yaml_quote(value)));
    }
    out.push_str("---\n");
    out
}

fn yaml_quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{}\"", escaped.replace('\n', " "))
}

fn atomic_write(path: &Path, body: &str) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = parent.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("omalibre-note")
    ));
    std::fs::write(&tmp, body)
        .with_context(|| format!("cannot write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("cannot rename {} to {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::{Color, Slice};
    use std::path::PathBuf;

    fn sample_record() -> BookRecord {
        let mut record = BookRecord::new(PathBuf::from("/books/demo.epub"));
        record.title = Some("They Say / I Say".into());
        record.authors = vec!["Graff".into()];
        record
    }

    fn sample_annotation(note: Option<&str>) -> Annotation {
        Annotation {
            id: "host-1".into(),
            href: "EPUB/ch1.xhtml".into(),
            slices: vec![Slice {
                block: 2,
                start: 10,
                end: 20,
            }],
            color: Color::Purple,
            quote: "quoted text".into(),
            note: note.map(str::to_string),
        }
    }

    #[test]
    fn writes_one_file_per_book() {
        let dir = tempfile();
        let id = BookId::from(
            "sha256:7e4ca47200a437e2104ef607eb6c50c3ed4bfe5f7e9cefd1d81fd106cf6ed7d0".to_string(),
        );
        let record = sample_record();
        let annotations = vec![sample_annotation(Some("a thought"))];

        write_book(&dir, &id, &record, &annotations).unwrap();
        let path = path_for(&dir, &record, &id);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("kind: \"annotations\""));
        assert!(text.contains("omalibre: \"1\""));
        assert!(text.contains("<!-- id: host-1 -->"));
        assert!(text.contains("> quoted text"));
        assert!(text.contains("a thought"));
        assert!(text.contains("purple · EPUB/ch1.xhtml"));
    }

    #[test]
    fn empty_annotations_remove_the_file() {
        let dir = tempfile();
        let id = BookId::from(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string(),
        );
        let record = sample_record();
        write_book(&dir, &id, &record, &[sample_annotation(None)]).unwrap();
        let path = path_for(&dir, &record, &id);
        assert!(path.exists());
        write_book(&dir, &id, &record, &[]).unwrap();
        assert!(!path.exists());
    }

    fn tempfile() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "omalibre-vault-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

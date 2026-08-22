//! The library: every book, with its metadata.
//!
//! This is a read model, folded out of the journal on startup. It is not stored:
//! the journal is the truth, and a few thousand events fold in milliseconds. A
//! database would add a schema and migrations for no gain at this size; it can
//! come when a full-text index over every book calls for one.
//!
//! A book is identified by the hash of its contents, never by its path, so
//! moving or renaming a file keeps its metadata, reading position and
//! annotations. See `identity`.

use crate::epub::Book;
use crate::identity::BookId;
use crate::journal::{BookRecord, Journal, Payload, State};
use anyhow::Result;
use std::path::{Path, PathBuf};

/// One row of the library.
#[derive(Debug, Clone)]
pub struct Entry {
    pub id: BookId,
    pub record: BookRecord,
    /// Share of the book read, when a position is known.
    pub progress: Option<u16>,
    /// When the book was last read, from the journal's own timestamps.
    pub last_read: Option<chrono::DateTime<chrono::Utc>>,
}

/// How the list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    Title,
    Author,
    /// Series first, then position within it; books without a series last.
    Series,
    /// Most recently read first.
    Recent,
}

impl Order {
    pub fn label(self) -> &'static str {
        match self {
            Order::Title => "title",
            Order::Author => "author",
            Order::Series => "series",
            Order::Recent => "recent",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Order::Title => Order::Author,
            Order::Author => Order::Series,
            Order::Series => Order::Recent,
            Order::Recent => Order::Title,
        }
    }
}

/// Builds the library from a replayed journal.
pub fn entries(state: &State) -> Vec<Entry> {
    state
        .books()
        .map(|(id, record)| Entry {
            id: id.clone(),
            record: record.clone(),
            progress: state.position(id).map(|_| 0),
            last_read: state.last_read(id),
        })
        .collect()
}

/// Sorts a list in place.
pub fn sort(entries: &mut [Entry], order: Order) {
    match order {
        Order::Title => entries.sort_by_key(|e| sortable(&e.record.display_title())),
        Order::Author => entries.sort_by(|a, b| {
            // A book without an author belongs at the end, not in front of
            // everything: its placeholder would otherwise sort before the letters.
            let key = |e: &Entry| {
                e.record
                    .authors
                    .first()
                    .filter(|name| !name.trim().is_empty())
                    .map(|name| sortable(name))
            };
            match (key(a), key(b)) {
                (Some(x), Some(y)) => x.cmp(&y).then_with(|| {
                    sortable(&a.record.display_title()).cmp(&sortable(&b.record.display_title()))
                }),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => {
                    sortable(&a.record.display_title()).cmp(&sortable(&b.record.display_title()))
                }
            }
        }),
        Order::Series => entries.sort_by(|a, b| {
            // Books outside a series come last rather than clumping under an
            // empty heading.
            let key = |e: &Entry| e.record.series.as_ref().map(|s| sortable(s));
            match (key(a), key(b)) {
                (Some(x), Some(y)) => x.cmp(&y).then_with(|| {
                    a.record
                        .series_index
                        .unwrap_or(f32::MAX)
                        .total_cmp(&b.record.series_index.unwrap_or(f32::MAX))
                }),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => {
                    sortable(&a.record.display_title()).cmp(&sortable(&b.record.display_title()))
                }
            }
        }),
        // Most recently read first. A book never opened has no timestamp and
        // belongs at the end rather than among the fresh ones.
        Order::Recent => entries.sort_by(|a, b| match (a.last_read, b.last_read) {
            (Some(x), Some(y)) => y.cmp(&x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => {
                sortable(&a.record.display_title()).cmp(&sortable(&b.record.display_title()))
            }
        }),
    }
}

/// Sort key: lowercase, and a leading article dropped so "The Hobbit" files
/// under H.
fn sortable(text: &str) -> String {
    let lower = text.trim().to_lowercase();
    for article in ["the ", "a ", "an ", "der ", "die ", "das ", "ein ", "eine "] {
        if let Some(rest) = lower.strip_prefix(article) {
            return rest.to_string();
        }
    }
    lower
}

/// Keeps the entries whose title, author, series or tags match the needle.
///
/// A match at the start of a word counts first: searching for "event" should find
/// "Event Sourcing" and "Eventsourcing", not "Practical Fraud Prevention". Only
/// when nothing matches that way does a match inside a word count, so a partial
/// word still finds something rather than nothing.
pub fn filter(entries: &[Entry], needle: &str) -> Vec<Entry> {
    if needle.trim().is_empty() {
        return entries.to_vec();
    }
    let needle = needle.trim().to_lowercase();

    let at_word_start: Vec<Entry> = entries
        .iter()
        .filter(|entry| {
            fields_of(entry)
                .iter()
                .any(|text| starts_a_word(text, &needle))
        })
        .cloned()
        .collect();
    if !at_word_start.is_empty() {
        return at_word_start;
    }

    entries
        .iter()
        .filter(|entry| fields_of(entry).iter().any(|text| text.contains(&needle)))
        .cloned()
        .collect()
}

/// The fields a filter looks at, lowercased.
fn fields_of(entry: &Entry) -> [String; 4] {
    let record = &entry.record;
    [
        record.display_title().to_lowercase(),
        record.display_authors().to_lowercase(),
        record.series.clone().unwrap_or_default().to_lowercase(),
        record.tags.join(" ").to_lowercase(),
    ]
}

/// True when the needle appears at the start of a word.
fn starts_a_word(text: &str, needle: &str) -> bool {
    let mut from = 0;
    while let Some(found) = text[from..].find(needle) {
        let at = from + found;
        let before = text[..at].chars().next_back();
        if before.is_none_or(|c| !c.is_alphanumeric()) {
            return true;
        }
        from = at + needle.len();
    }
    false
}

/// What a scan did, for the report afterwards.
#[derive(Debug, Default)]
pub struct ScanReport {
    pub seen: usize,
    pub added: usize,
    pub moved: usize,
    pub unreadable: Vec<(PathBuf, String)>,
}

/// Walks a directory and records every book it can read.
///
/// A file already known by its hash is not added again; if it turned up
/// elsewhere, only its path is corrected. Metadata from the book itself is
/// written once, on first sight, so a later correction of yours is never
/// overwritten by a re-scan.
pub fn scan(
    dir: &Path,
    journal: &mut Journal,
    state: &State,
    report_progress: &mut dyn FnMut(&Path),
) -> Result<ScanReport> {
    let mut report = ScanReport::default();

    for path in collect_books(dir) {
        report_progress(&path);
        report.seen += 1;

        let id = match BookId::of_file(&path) {
            Ok(id) => id,
            Err(err) => {
                report.unreadable.push((path, format!("{err:#}")));
                continue;
            }
        };

        let known = state.book(&id);
        let here = path.canonicalize().unwrap_or_else(|_| path.clone());
        if let Some(record) = known {
            if record.paths.contains(&here) {
                continue;
            }
            // Same contents at a path we did not know: either the file moved or
            // there is a second copy. Both mean one more file for this book.
            journal.append(
                &id,
                Payload::BookSeen {
                    title: record.title.clone(),
                    authors: record.authors.clone(),
                    path: here,
                },
            )?;
            report.moved += 1;
            continue;
        }

        // New book: read what the file says about itself.
        let mut book = match Book::open(&path) {
            Ok(book) => book,
            Err(err) => {
                report.unreadable.push((path, format!("{err:#}")));
                continue;
            }
        };
        let metadata = std::mem::take(&mut book.metadata);
        journal.append(
            &id,
            Payload::BookSeen {
                title: metadata.title.clone(),
                authors: metadata.authors.clone(),
                path: here,
            },
        )?;
        // Fields that BookSeen does not carry.
        if metadata.language.is_some() {
            journal.append(
                &id,
                Payload::MetadataSet {
                    title: None,
                    authors: None,
                    series: None,
                    series_index: None,
                    tags: None,
                    rating: None,
                    publisher: None,
                    year: None,
                    language: metadata.language.clone(),
                },
            )?;
        }
        report.added += 1;
    }

    Ok(report)
}

/// Every readable book file below a directory, in a stable order.
fn collect_books(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    walk(dir, &mut found, 0);
    found.sort();
    found
}

/// Depth limit, so a symlink loop cannot run away with the scan.
const MAX_DEPTH: usize = 12;

fn walk(dir: &Path, found: &mut Vec<PathBuf>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // Hidden directories hold caches and version control, not books.
        if name.starts_with('.') {
            continue;
        }
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => walk(&path, found, depth + 1),
            Ok(kind) if kind.is_file() => {
                if is_book(&path) {
                    found.push(path);
                }
            }
            _ => {}
        }
    }
}

fn is_book(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .as_deref(),
        Some("epub")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(title: &str, author: &str, series: Option<&str>, index: Option<f32>) -> Entry {
        let mut record = BookRecord::new(PathBuf::from("/tmp/x.epub"));
        record.title = Some(title.into());
        record.authors = vec![author.into()];
        record.series = series.map(String::from);
        record.series_index = index;
        Entry {
            id: BookId::from(format!("sha256:{title}")),
            record,
            progress: None,
            last_read: None,
        }
    }

    #[test]
    fn a_leading_article_does_not_decide_the_order() {
        assert_eq!(sortable("The Hobbit"), "hobbit");
        assert_eq!(sortable("Die Verwandlung"), "verwandlung");
        assert_eq!(sortable("Anathem"), "anathem");
    }

    #[test]
    fn sorts_by_title() {
        let mut list = vec![
            entry("Zero to Sold", "Bechtel", None, None),
            entry("The Hobbit", "Tolkien", None, None),
        ];
        sort(&mut list, Order::Title);
        assert_eq!(list[0].record.title.as_deref(), Some("The Hobbit"));
    }

    #[test]
    fn sorts_a_series_by_its_position() {
        let mut list = vec![
            entry("Third", "A", Some("Saga"), Some(3.0)),
            entry("Interlude", "A", Some("Saga"), Some(1.5)),
            entry("First", "A", Some("Saga"), Some(1.0)),
        ];
        sort(&mut list, Order::Series);
        let titles: Vec<_> = list
            .iter()
            .map(|e| e.record.title.clone().unwrap())
            .collect();
        assert_eq!(titles, ["First", "Interlude", "Third"]);
    }

    #[test]
    fn books_without_an_author_come_last() {
        let mut nameless = entry("Zzz Anonymous", "", None, None);
        nameless.record.authors.clear();
        let mut list = vec![nameless, entry("Anathem", "Stephenson", None, None)];
        sort(&mut list, Order::Author);
        assert_eq!(
            list[0].record.authors.first().map(String::as_str),
            Some("Stephenson")
        );
    }

    #[test]
    fn books_without_a_series_come_last() {
        let mut list = vec![
            entry("Loose", "A", None, None),
            entry("In a series", "A", Some("Saga"), Some(1.0)),
        ];
        sort(&mut list, Order::Series);
        assert_eq!(list[0].record.series.as_deref(), Some("Saga"));
    }

    #[test]
    fn the_most_recently_read_comes_first() {
        use chrono::{TimeZone, Utc};
        let mut old = entry("Older", "A", None, None);
        old.last_read = Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
        let mut fresh = entry("Fresher", "B", None, None);
        fresh.last_read = Some(Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap());
        let never = entry("Never opened", "C", None, None);

        let mut list = vec![old, never, fresh];
        sort(&mut list, Order::Recent);
        let titles: Vec<_> = list
            .iter()
            .map(|e| e.record.title.clone().unwrap())
            .collect();
        assert_eq!(titles, ["Fresher", "Older", "Never opened"]);
    }

    #[test]
    fn a_match_inside_a_word_yields_to_one_at_its_start() {
        let list = vec![
            entry("Practical Fraud Prevention", "Saporta", None, None),
            entry("Understanding Eventsourcing", "Dilger", None, None),
        ];
        // "event" hides inside "Prevention", but starts a word in the other title.
        let found = filter(&list, "event");
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].record.title.as_deref(),
            Some("Understanding Eventsourcing")
        );
    }

    #[test]
    fn a_partial_word_still_finds_something() {
        let list = vec![entry("Understanding Eventsourcing", "Dilger", None, None)];
        // Nothing starts with "sourcing", so the inside-word match counts.
        assert_eq!(filter(&list, "sourcing").len(), 1);
    }

    #[test]
    fn filters_across_title_author_series_and_tags() {
        let mut tagged = entry("Anathem", "Stephenson", None, None);
        tagged.record.tags = vec!["science fiction".into()];
        let list = vec![
            entry("The Hobbit", "Tolkien", Some("Middle-earth"), Some(1.0)),
            tagged,
        ];
        assert_eq!(filter(&list, "hobbit").len(), 1);
        assert_eq!(filter(&list, "tolkien").len(), 1);
        assert_eq!(filter(&list, "middle").len(), 1);
        assert_eq!(filter(&list, "science").len(), 1);
        assert_eq!(filter(&list, "").len(), 2);
        assert_eq!(filter(&list, "nothing here").len(), 0);
    }

    #[test]
    fn a_title_falls_back_to_the_file_name() {
        let record = BookRecord::new(PathBuf::from("/books/some-book.epub"));
        assert_eq!(record.display_title(), "some-book");
        assert_eq!(record.display_authors(), "-");
    }

    #[test]
    fn a_book_can_sit_in_several_files() {
        let mut record = BookRecord::new(PathBuf::from("/a/book.epub"));
        record.paths.push(PathBuf::from("/b/book.epub"));
        // The first one recorded is the one to open.
        assert_eq!(record.path(), Some(&PathBuf::from("/a/book.epub")));
        assert_eq!(record.paths.len(), 2);
    }

    #[test]
    fn the_order_cycles_through_all_four() {
        let mut order = Order::Title;
        let mut seen = vec![order];
        for _ in 0..3 {
            order = order.next();
            seen.push(order);
        }
        assert_eq!(order.next(), Order::Title, "the cycle must close");
        assert_eq!(seen.len(), 4);
    }

    #[test]
    fn only_epub_files_count_as_books() {
        assert!(is_book(Path::new("a.epub")));
        assert!(is_book(Path::new("a.EPUB")));
        assert!(!is_book(Path::new("a.pdf")));
        assert!(!is_book(Path::new("a.mobi")));
        assert!(!is_book(Path::new("noextension")));
    }
}

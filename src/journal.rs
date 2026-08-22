//! The journal: append-only event log, one file per machine.
//!
//! The journal is the source of truth. Everything else, including the reading
//! position the reader shows, is folded out of it. Each machine writes only its
//! own file, so two machines syncing through a shared folder can never write the
//! same file and no conflict copies appear.
//!
//! Events are JSON, one per line, so a partly written last line costs at most
//! one event and never the file.

use crate::annotation::{Annotation, Color, Slice};
use crate::doc::Locator;
use crate::identity::BookId;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Payload {
    /// A book entered the library, or was seen again at a path.
    BookSeen {
        title: Option<String>,
        authors: Vec<String>,
        path: PathBuf,
    },
    /// The reader moved to a position.
    PositionSet {
        href: String,
        block: usize,
        offset: usize,
    },
    /// A passage was annotated.
    HighlightAdded {
        id: String,
        href: String,
        slices: Vec<Slice>,
        color: Color,
        quote: String,
    },
    /// A comment was written or rewritten. An empty text removes the comment
    /// but keeps the highlight.
    NoteSet { id: String, text: String },
    /// The colour of an existing annotation changed.
    ColorSet { id: String, color: Color },
    /// A highlight and its comment were deleted.
    AnnotationRemoved { id: String },
    /// Metadata was set or corrected.
    ///
    /// One event carries a patch, not the whole record: only the fields that
    /// changed are written. That keeps the journal readable and makes a later
    /// correction obvious in the log. An empty string clears a field.
    MetadataSet {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        authors: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        series: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        series_index: Option<f32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tags: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        rating: Option<u8>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        publisher: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        year: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
    },
    /// A file is no longer where it was recorded.
    FileMissing { path: PathBuf },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub at: DateTime<Utc>,
    pub host: String,
    pub book: String,
    #[serde(flatten)]
    pub payload: Payload,
}

/// What the events add up to. Rebuilt on startup, never stored.
#[derive(Debug, Default)]
pub struct State {
    positions: HashMap<BookId, StoredPosition>,
    books: HashMap<BookId, BookRecord>,
    /// Annotations per book, keyed by id so later events can amend them.
    annotations: HashMap<BookId, HashMap<String, Annotation>>,
}

#[derive(Debug, Clone)]
struct StoredPosition {
    at: DateTime<Utc>,
    locator: Locator,
}

/// What is known about a book without opening its file.
///
/// `BookSeen` fills in what the file itself says; `MetadataSet` carries
/// corrections and everything a book file cannot express, such as series and
/// tags. A correction wins over the file.
#[derive(Debug, Clone, PartialEq)]
pub struct BookRecord {
    pub title: Option<String>,
    pub authors: Vec<String>,
    /// Every file holding this book. The same book can sit at two paths, as a
    /// copy or in two formats, and both belong to one entry rather than making a
    /// second book of it.
    pub paths: Vec<PathBuf>,
    pub series: Option<String>,
    pub series_index: Option<f32>,
    pub tags: Vec<String>,
    pub rating: Option<u8>,
    pub publisher: Option<String>,
    pub year: Option<i32>,
    pub language: Option<String>,
    /// True when the file was not found where it was last recorded.
    pub missing: bool,
}

impl BookRecord {
    pub fn new(path: PathBuf) -> Self {
        Self {
            title: None,
            authors: Vec::new(),
            paths: if path.as_os_str().is_empty() {
                Vec::new()
            } else {
                vec![path]
            },
            series: None,
            series_index: None,
            tags: Vec::new(),
            rating: None,
            publisher: None,
            year: None,
            language: None,
            missing: false,
        }
    }

    /// The file to open. The first recorded one, which is where the book was
    /// first found.
    pub fn path(&self) -> Option<&PathBuf> {
        self.paths.first()
    }

    /// Title to show, falling back to the file name so a book is never nameless.
    pub fn display_title(&self) -> String {
        self.title.clone().unwrap_or_else(|| {
            self.path()
                .and_then(|p| p.file_stem())
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Untitled".into())
        })
    }

    pub fn display_authors(&self) -> String {
        if self.authors.is_empty() {
            "-".to_string()
        } else {
            self.authors.join(", ")
        }
    }
}

impl State {
    /// The last known position of a book. Where two machines disagree, the newer
    /// timestamp wins.
    pub fn position(&self, book: &BookId) -> Option<&Locator> {
        self.positions.get(book).map(|p| &p.locator)
    }

    /// When a position was last recorded for a book, which is when it was last
    /// read. Taken from the event's own timestamp, so it holds across machines.
    pub fn last_read(&self, book: &BookId) -> Option<DateTime<Utc>> {
        self.positions.get(book).map(|p| p.at)
    }

    pub fn book(&self, book: &BookId) -> Option<&BookRecord> {
        self.books.get(book)
    }

    /// Every book in the library, with its identity.
    pub fn books(&self) -> impl Iterator<Item = (&BookId, &BookRecord)> {
        self.books.iter()
    }

    /// A book's annotations, ordered by where they start in the text.
    pub fn annotations(&self, book: &BookId) -> Vec<Annotation> {
        let mut all: Vec<Annotation> = self
            .annotations
            .get(book)
            .map(|by_id| by_id.values().cloned().collect())
            .unwrap_or_default();
        all.sort_by(|a, b| a.href.cmp(&b.href).then_with(|| a.start().cmp(&b.start())));
        all
    }

    fn apply(&mut self, event: Event) {
        let book = BookId::from(event.book);
        match event.payload {
            Payload::PositionSet {
                href,
                block,
                offset,
            } => {
                let candidate = StoredPosition {
                    at: event.at,
                    locator: Locator {
                        href,
                        block,
                        offset,
                    },
                };
                match self.positions.get(&book) {
                    Some(existing) if existing.at > candidate.at => {}
                    _ => {
                        self.positions.insert(book, candidate);
                    }
                }
            }
            Payload::BookSeen {
                title,
                authors,
                path,
            } => {
                let record = self
                    .books
                    .entry(book)
                    .or_insert_with(|| BookRecord::new(path.clone()));
                if !record.paths.contains(&path) {
                    record.paths.push(path);
                }
                record.missing = false;
                // A correction made later must not be undone by seeing the file
                // again, so the file's own values only fill what is still empty.
                if record.title.is_none() {
                    record.title = title;
                }
                if record.authors.is_empty() {
                    record.authors = authors;
                }
            }
            Payload::HighlightAdded {
                id,
                href,
                slices,
                color,
                quote,
            } => {
                self.annotations.entry(book).or_default().insert(
                    id.clone(),
                    Annotation {
                        id,
                        href,
                        slices,
                        color,
                        quote,
                        note: None,
                    },
                );
            }
            Payload::NoteSet { id, text } => {
                if let Some(annotation) = self.annotations.entry(book).or_default().get_mut(&id) {
                    annotation.note = if text.trim().is_empty() {
                        None
                    } else {
                        Some(text)
                    };
                }
            }
            Payload::ColorSet { id, color } => {
                if let Some(annotation) = self.annotations.entry(book).or_default().get_mut(&id) {
                    annotation.color = color;
                }
            }
            Payload::AnnotationRemoved { id } => {
                self.annotations.entry(book).or_default().remove(&id);
            }
            Payload::MetadataSet {
                title,
                authors,
                series,
                series_index,
                tags,
                rating,
                publisher,
                year,
                language,
            } => {
                let record = self
                    .books
                    .entry(book)
                    .or_insert_with(|| BookRecord::new(PathBuf::new()));
                // An empty string clears a field; a missing field leaves it be.
                if let Some(value) = title {
                    record.title = non_empty(value);
                }
                if let Some(value) = authors {
                    record.authors = value.into_iter().filter_map(non_empty).collect();
                }
                if let Some(value) = series {
                    record.series = non_empty(value);
                }
                if let Some(value) = series_index {
                    record.series_index = Some(value);
                }
                if let Some(value) = tags {
                    record.tags = value.into_iter().filter_map(non_empty).collect();
                }
                if let Some(value) = rating {
                    record.rating = if value == 0 { None } else { Some(value.min(5)) };
                }
                if let Some(value) = publisher {
                    record.publisher = non_empty(value);
                }
                if let Some(value) = year {
                    record.year = if value == 0 { None } else { Some(value) };
                }
                if let Some(value) = language {
                    record.language = non_empty(value);
                }
            }
            Payload::FileMissing { path } => {
                if let Some(record) = self.books.get_mut(&book) {
                    record.paths.retain(|known| *known != path);
                    // Only a book without any file left counts as missing.
                    record.missing = record.paths.is_empty();
                }
            }
        }
    }
}

/// Trims a value and turns an empty one into `None`, so a cleared field is
/// really cleared rather than an empty string.
fn non_empty(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub struct Journal {
    /// This machine's file. The only one written to.
    own_file: PathBuf,
    host: String,
    /// The last position written, to avoid recording every keystroke.
    last_written: Option<Locator>,
    /// Distinguishes ids created within the same nanosecond.
    id_counter: u64,
}

impl Journal {
    /// Opens the journal directory, creating it when missing.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let host = crate::paths::hostname();
        Ok(Self {
            own_file: dir.join(format!("journal-{host}.jsonl")),
            host,
            last_written: None,
            id_counter: 0,
        })
    }

    /// Folds every journal in the directory into a state. Unreadable lines are
    /// skipped: one damaged event must not hide the rest.
    pub fn replay(dir: &Path) -> Result<State> {
        let mut events = Vec::new();
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            // No directory yet means nothing has been read so far.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
            Err(err) => return Err(err).with_context(|| format!("cannot read {}", dir.display())),
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(file) = File::open(&path) else {
                continue;
            };
            for line in BufReader::new(file).lines().map_while(Result::ok) {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(event) = serde_json::from_str::<Event>(&line) {
                    events.push(event);
                }
            }
        }

        // Apply in time order so the newest position wins regardless of which
        // file it came from.
        events.sort_by(|a, b| a.at.cmp(&b.at));
        let mut state = State::default();
        for event in events {
            state.apply(event);
        }
        Ok(state)
    }

    pub fn append(&mut self, book: &BookId, payload: Payload) -> Result<()> {
        let event = Event {
            at: Utc::now(),
            host: self.host.clone(),
            book: book.as_str().to_string(),
            payload,
        };
        let line = serde_json::to_string(&event)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.own_file)
            .with_context(|| format!("cannot append to {}", self.own_file.display()))?;
        writeln!(file, "{line}")?;
        Ok(())
    }

    /// Treats a position as already recorded. Called at startup with the
    /// position from the journal, so reopening a book without moving adds no
    /// event.
    pub fn assume_written(&mut self, locator: Option<Locator>) {
        self.last_written = locator;
    }

    /// A fresh annotation id. The hostname keeps ids from two machines apart,
    /// the timestamp keeps them apart within one machine, and the counter covers
    /// the case of two annotations inside the same nanosecond.
    pub fn next_id(&mut self) -> String {
        self.id_counter += 1;
        format!(
            "{}-{}-{}",
            self.host,
            Utc::now().timestamp_nanos_opt().unwrap_or_default(),
            self.id_counter
        )
    }

    /// Records a position, unless it is the one already written.
    pub fn record_position(&mut self, book: &BookId, locator: &Locator) -> Result<()> {
        if self.last_written.as_ref() == Some(locator) {
            return Ok(());
        }
        self.append(
            book,
            Payload::PositionSet {
                href: locator.href.clone(),
                block: locator.block,
                offset: locator.offset,
            },
        )?;
        self.last_written = Some(locator.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("omalibre-journal-{name}"));
        std::fs::remove_dir_all(&dir).ok();
        dir
    }

    fn locator(block: usize, offset: usize) -> Locator {
        Locator {
            href: "OEBPS/ch01.xhtml".into(),
            block,
            offset,
        }
    }

    #[test]
    fn a_recorded_position_survives_a_replay() {
        let dir = scratch("roundtrip");
        let book = BookId::from("sha256:abc".to_string());
        let mut journal = Journal::open(&dir).unwrap();
        journal.record_position(&book, &locator(7, 12)).unwrap();

        let state = Journal::replay(&dir).unwrap();
        assert_eq!(state.position(&book), Some(&locator(7, 12)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_newest_position_wins() {
        let dir = scratch("newest");
        let book = BookId::from("sha256:abc".to_string());
        let mut journal = Journal::open(&dir).unwrap();
        journal.record_position(&book, &locator(1, 0)).unwrap();
        journal.record_position(&book, &locator(40, 5)).unwrap();

        let state = Journal::replay(&dir).unwrap();
        assert_eq!(state.position(&book), Some(&locator(40, 5)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn positions_from_another_machine_are_merged() {
        let dir = scratch("merge");
        std::fs::create_dir_all(&dir).unwrap();
        let other = dir.join("journal-otherbox.jsonl");
        std::fs::write(
            &other,
            "{\"at\":\"2099-01-01T00:00:00Z\",\"host\":\"otherbox\",\"book\":\"sha256:abc\",\
             \"type\":\"position_set\",\"href\":\"OEBPS/ch01.xhtml\",\"block\":99,\"offset\":3}\n",
        )
        .unwrap();

        let book = BookId::from("sha256:abc".to_string());
        let mut journal = Journal::open(&dir).unwrap();
        journal.record_position(&book, &locator(1, 0)).unwrap();

        // The other machine's event is dated later, so it wins.
        let state = Journal::replay(&dir).unwrap();
        assert_eq!(state.position(&book).map(|l| l.block), Some(99));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_damaged_line_does_not_hide_the_others() {
        let dir = scratch("damaged");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("journal-box.jsonl");
        std::fs::write(
            &file,
            "not json at all\n\
             {\"at\":\"2020-01-01T00:00:00Z\",\"host\":\"box\",\"book\":\"sha256:abc\",\
             \"type\":\"position_set\",\"href\":\"c.xhtml\",\"block\":5,\"offset\":0}\n\
             {\"at\":\"2020-01-01T00:00:01Z\",\"host\":\"box\",\"book\":\
             \n",
        )
        .unwrap();

        let state = Journal::replay(&dir).unwrap();
        let book = BookId::from("sha256:abc".to_string());
        assert_eq!(state.position(&book).map(|l| l.block), Some(5));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn reopening_without_moving_adds_nothing() {
        let dir = scratch("reopen");
        let book = BookId::from("sha256:abc".to_string());

        let mut first = Journal::open(&dir).unwrap();
        first.record_position(&book, &locator(9, 4)).unwrap();

        // A second session starts from the replayed state.
        let state = Journal::replay(&dir).unwrap();
        let mut second = Journal::open(&dir).unwrap();
        second.assume_written(state.position(&book).cloned());
        second.record_position(&book, &locator(9, 4)).unwrap();

        let file = dir.join(format!("journal-{}.jsonl", crate::paths::hostname()));
        assert_eq!(std::fs::read_to_string(&file).unwrap().lines().count(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_comment_survives_a_replay() {
        let dir = scratch("note");
        let book = BookId::from("sha256:abc".to_string());
        let mut journal = Journal::open(&dir).unwrap();

        let id = journal.next_id();
        journal
            .append(
                &book,
                Payload::HighlightAdded {
                    id: id.clone(),
                    href: "c.xhtml".into(),
                    slices: vec![Slice {
                        block: 1,
                        start: 0,
                        end: 4,
                    }],
                    color: Color::Yellow,
                    quote: "word".into(),
                },
            )
            .unwrap();
        journal
            .append(
                &book,
                Payload::NoteSet {
                    id: id.clone(),
                    text: "my thought".into(),
                },
            )
            .unwrap();

        let state = Journal::replay(&dir).unwrap();
        let marks = state.annotations(&book);
        assert_eq!(marks.len(), 1, "annotation missing after replay");
        assert_eq!(marks[0].note.as_deref(), Some("my thought"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn removing_an_annotation_takes_its_comment_along() {
        let dir = scratch("remove");
        let book = BookId::from("sha256:abc".to_string());
        let mut journal = Journal::open(&dir).unwrap();
        let id = journal.next_id();
        journal
            .append(
                &book,
                Payload::HighlightAdded {
                    id: id.clone(),
                    href: "c.xhtml".into(),
                    slices: vec![Slice {
                        block: 0,
                        start: 0,
                        end: 2,
                    }],
                    color: Color::Blue,
                    quote: "ab".into(),
                },
            )
            .unwrap();
        journal
            .append(
                &book,
                Payload::NoteSet {
                    id: id.clone(),
                    text: "x".into(),
                },
            )
            .unwrap();
        journal
            .append(&book, Payload::AnnotationRemoved { id })
            .unwrap();

        assert!(Journal::replay(&dir).unwrap().annotations(&book).is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_empty_directory_replays_to_nothing() {
        let dir = scratch("empty");
        let state = Journal::replay(&dir).unwrap();
        assert!(
            state
                .position(&BookId::from("sha256:abc".to_string()))
                .is_none()
        );
    }

    #[test]
    fn repeated_positions_are_written_once() {
        let dir = scratch("dedupe");
        let book = BookId::from("sha256:abc".to_string());
        let mut journal = Journal::open(&dir).unwrap();
        journal.record_position(&book, &locator(3, 0)).unwrap();
        journal.record_position(&book, &locator(3, 0)).unwrap();

        let text = std::fs::read_to_string(
            dir.join(format!("journal-{}.jsonl", crate::paths::hostname())),
        )
        .unwrap();
        assert_eq!(text.lines().count(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }
}

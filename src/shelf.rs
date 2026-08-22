//! Browsing the library.
//!
//! The state of the list view: which book the cursor is on, how the list is
//! ordered, what is filtered. Separate from `library`, which holds the data and
//! knows nothing about a screen.

use crate::identity::BookId;
use crate::journal::State;
use crate::library::{self, Entry, Order};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Browse,
    /// Typing a filter.
    Filter,
    Help,
}

/// What the shelf asks the session to do next.
#[derive(Debug, Clone)]
pub enum Action {
    None,
    /// Open this book for reading.
    Open {
        id: BookId,
        path: PathBuf,
    },
    Quit,
}

pub struct Shelf {
    /// Every book, unfiltered, in the current order.
    all: Vec<Entry>,
    /// What the list shows.
    shown: Vec<Entry>,
    cursor: usize,
    order: Order,
    filter: String,
    filter_input: String,
    pub mode: Mode,
    status: Option<String>,
    /// Pending first key of a sequence, such as `g` in `gg`.
    pending: Option<char>,
    /// Rows the list can show, set by the view before each draw.
    view_height: u16,
    scroll: usize,
}

impl Shelf {
    pub fn new(state: &State) -> Self {
        let mut all = library::entries(state);
        // Title first: it is what the eye looks for, and it is the one field
        // almost every book fills in.
        let order = Order::Title;
        library::sort(&mut all, order);
        let shown = all.clone();
        Self {
            all,
            shown,
            cursor: 0,
            order,
            filter: String::new(),
            filter_input: String::new(),
            mode: Mode::Browse,
            status: None,
            pending: None,
            view_height: 1,
            scroll: 0,
        }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.shown
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn order(&self) -> Order {
        self.order
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// The filter prompt, while one is being typed.
    pub fn filter_input(&self) -> Option<&str> {
        if self.mode == Mode::Filter {
            Some(&self.filter_input)
        } else {
            None
        }
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    pub fn total(&self) -> usize {
        self.all.len()
    }

    /// Tells the shelf how many rows it has, before drawing.
    pub fn prepare(&mut self, height: u16) {
        self.view_height = height.max(1);
        self.follow_cursor();
    }

    fn follow_cursor(&mut self) {
        let height = self.view_height as usize;
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + height {
            self.scroll = self.cursor + 1 - height;
        }
        let max = self.shown.len().saturating_sub(height);
        self.scroll = self.scroll.min(max);
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        if let Some(first) = self.pending.take() {
            if first == 'g' && key.code == KeyCode::Char('g') {
                self.cursor = 0;
                self.follow_cursor();
                return Action::None;
            }
        }
        match self.mode {
            Mode::Browse => self.handle_browse_key(key),
            Mode::Filter => {
                self.handle_filter_key(key);
                Action::None
            }
            Mode::Help => {
                self.mode = Mode::Browse;
                Action::None
            }
        }
    }

    fn handle_browse_key(&mut self, key: KeyEvent) -> Action {
        let last = self.shown.len().saturating_sub(1);
        let page = self.view_height.saturating_sub(2).max(1) as usize;

        match key.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('j') | KeyCode::Down => {
                self.cursor = (self.cursor + 1).min(last);
                self.status = None;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.cursor = self.cursor.saturating_sub(1);
                self.status = None;
            }
            KeyCode::Char(' ') | KeyCode::PageDown => self.cursor = (self.cursor + page).min(last),
            KeyCode::Backspace | KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(page),
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('G') => self.cursor = last,
            KeyCode::Char('/') => {
                self.filter_input = self.filter.clone();
                self.mode = Mode::Filter;
            }
            KeyCode::Esc => {
                if !self.filter.is_empty() {
                    self.filter.clear();
                    self.apply();
                    self.status = Some("filter cleared".into());
                } else {
                    self.status = None;
                }
            }
            // A shortcut for the one order that answers "where was I?".
            KeyCode::Char('r') => {
                self.order = Order::Recent;
                self.apply();
                self.status = Some("sorted by last read".into());
            }
            // Cycles through the orders rather than needing four keys.
            KeyCode::Char('s') => {
                self.order = self.order.next();
                self.apply();
                self.status = Some(format!("sorted by {}", self.order.label()));
            }
            KeyCode::Enter | KeyCode::Char('l') => return self.open_selected(),
            _ => {}
        }
        self.follow_cursor();
        Action::None
    }

    fn handle_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Browse;
                self.filter_input.clear();
            }
            KeyCode::Enter => {
                self.filter = std::mem::take(&mut self.filter_input);
                self.mode = Mode::Browse;
                self.apply();
                self.status = if self.shown.is_empty() {
                    Some(format!("nothing matches {:?}", self.filter))
                } else {
                    None
                };
            }
            KeyCode::Backspace => {
                self.filter_input.pop();
            }
            KeyCode::Char(c) => self.filter_input.push(c),
            _ => {}
        }
    }

    /// Re-sorts and re-filters, keeping the cursor on the same book where it
    /// still shows.
    fn apply(&mut self) {
        let selected = self.shown.get(self.cursor).map(|e| e.id.clone());
        library::sort(&mut self.all, self.order);
        self.shown = library::filter(&self.all, &self.filter);
        self.cursor = selected
            .and_then(|id| self.shown.iter().position(|e| e.id == id))
            .unwrap_or(0);
        self.follow_cursor();
    }

    fn open_selected(&mut self) -> Action {
        let Some(entry) = self.shown.get(self.cursor) else {
            return Action::None;
        };
        let Some(path) = entry.record.path().cloned() else {
            self.status = Some("no file recorded for this book".into());
            return Action::None;
        };
        if !path.exists() {
            self.status = Some(format!("file is gone: {}", path.display()));
            return Action::None;
        }
        Action::Open {
            id: entry.id.clone(),
            path,
        }
    }

    /// The key bindings, for the help.
    pub const BINDINGS: &'static [(&'static str, &'static str)] = &[
        ("j k", "down, up"),
        ("Space Backspace", "page down, up"),
        ("gg G", "first, last"),
        ("Enter l", "open the book"),
        ("/", "filter by title, author, series or tag"),
        ("Esc", "clear the filter"),
        ("r", "sort by last read"),
        ("s", "cycle the order: title, author, series, recent"),
        ("q", "quit"),
    ];
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::BookRecord;
    use ratatui::crossterm::event::{KeyEventKind, KeyEventState, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn shelf_of(titles: &[&str]) -> Shelf {
        let all: Vec<Entry> = titles
            .iter()
            .enumerate()
            .map(|(i, title)| {
                let mut record = BookRecord::new(PathBuf::from(format!("/tmp/{i}.epub")));
                record.title = Some((*title).into());
                record.authors = vec![format!("Author {i}")];
                Entry {
                    id: BookId::from(format!("sha256:{i}")),
                    record,
                    progress: None,
                    last_read: None,
                }
            })
            .collect();
        let mut shelf = Shelf {
            all: all.clone(),
            shown: all,
            cursor: 0,
            order: Order::Title,
            filter: String::new(),
            filter_input: String::new(),
            mode: Mode::Browse,
            status: None,
            pending: None,
            view_height: 10,
            scroll: 0,
        };
        shelf.apply();
        shelf
    }

    #[test]
    fn moves_the_cursor_without_running_off_the_ends() {
        let mut shelf = shelf_of(&["a", "b", "c"]);
        shelf.handle_key(key(KeyCode::Char('k')));
        assert_eq!(shelf.cursor(), 0, "must not go above the first");
        for _ in 0..5 {
            shelf.handle_key(key(KeyCode::Char('j')));
        }
        assert_eq!(shelf.cursor(), 2, "must not go past the last");
    }

    #[test]
    fn gg_and_G_jump_to_the_ends() {
        let mut shelf = shelf_of(&["a", "b", "c"]);
        shelf.handle_key(key(KeyCode::Char('G')));
        assert_eq!(shelf.cursor(), 2);
        shelf.handle_key(key(KeyCode::Char('g')));
        shelf.handle_key(key(KeyCode::Char('g')));
        assert_eq!(shelf.cursor(), 0);
    }

    #[test]
    fn filtering_narrows_the_list_and_esc_restores_it() {
        let mut shelf = shelf_of(&["Anathem", "Cryptonomicon", "Seveneves"]);
        shelf.handle_key(key(KeyCode::Char('/')));
        for c in "crypto".chars() {
            shelf.handle_key(key(KeyCode::Char(c)));
        }
        shelf.handle_key(key(KeyCode::Enter));
        assert_eq!(shelf.entries().len(), 1);

        shelf.handle_key(key(KeyCode::Esc));
        assert_eq!(shelf.entries().len(), 3);
    }

    #[test]
    fn the_cursor_stays_on_its_book_when_the_order_changes() {
        let mut shelf = shelf_of(&["Zebra", "Apple", "Mango"]);
        // Sorted by title: Apple, Mango, Zebra. Put the cursor on Mango.
        shelf.handle_key(key(KeyCode::Char('j')));
        let before = shelf.entries()[shelf.cursor()].id.clone();
        shelf.handle_key(key(KeyCode::Char('s')));
        assert_eq!(shelf.entries()[shelf.cursor()].id, before);
    }

    #[test]
    fn a_filter_that_matches_nothing_says_so() {
        let mut shelf = shelf_of(&["Anathem"]);
        shelf.handle_key(key(KeyCode::Char('/')));
        for c in "zzz".chars() {
            shelf.handle_key(key(KeyCode::Char(c)));
        }
        shelf.handle_key(key(KeyCode::Enter));
        assert!(shelf.entries().is_empty());
        assert!(
            shelf
                .status()
                .is_some_and(|s| s.contains("nothing matches"))
        );
    }

    #[test]
    fn opening_a_book_whose_file_is_gone_reports_it() {
        let mut shelf = shelf_of(&["Anathem"]);
        match shelf.handle_key(key(KeyCode::Enter)) {
            Action::None => {
                assert!(shelf.status().is_some_and(|s| s.contains("file is gone")))
            }
            other => panic!("expected no action, got {other:?}"),
        }
    }

    #[test]
    fn q_quits() {
        let mut shelf = shelf_of(&["a"]);
        assert!(matches!(
            shelf.handle_key(key(KeyCode::Char('q'))),
            Action::Quit
        ));
    }

    #[test]
    fn the_view_follows_the_cursor() {
        let mut shelf = shelf_of(&["a", "b", "c", "d", "e", "f"]);
        shelf.prepare(3);
        shelf.handle_key(key(KeyCode::Char('G')));
        assert!(
            shelf.scroll() + 3 > shelf.cursor(),
            "cursor {} must be visible with scroll {}",
            shelf.cursor(),
            shelf.scroll()
        );
    }
}

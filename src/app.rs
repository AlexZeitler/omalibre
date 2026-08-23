//! Reader state and key handling.

use crate::annotation::{Annotation, Color, Slice};
use crate::doc::{Chapter, Locator};
use crate::epub::Book;
use crate::identity::BookId;
use crate::journal::{Journal, Payload, State};
use crate::layout::{self, Index, LayoutOptions, Line};
use anyhow::Result;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Scrolling and reading. `j` and `k` move the page.
    Reading,
    /// A cursor sits in the text. `j` and `k` move the cursor.
    Normal,
    /// A selection grows from an anchor to the cursor.
    Visual,
    Contents,
    /// The list of annotations in this book.
    Annotations,
    /// The key bindings.
    Help,
    /// Typing a search term.
    Search,
}

/// A position in the laid-out text: a line and a character within it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cursor {
    line: usize,
    column: usize,
}

/// A place to come back to.
///
/// Both parts matter: the view has to look as it did, and the cursor has to sit
/// on the link that was followed, not merely somewhere on its line.
#[derive(Debug, Clone)]
struct Jump {
    /// Topmost visible line, so the page looks the same.
    view: Locator,
    /// Where the cursor stood, in text coordinates.
    cursor: Option<(usize, usize)>,
}

/// A comment the user is about to write in `$EDITOR`. The event loop performs
/// the edit, because only it may leave and re-enter the terminal.
#[derive(Debug, Clone)]
pub struct EditRequest {
    pub annotation_id: String,
    pub initial_text: String,
    pub quote: String,
}

/// One picture of the chapter: where it sits, where its bytes are, and how much
/// room it takes. Known before anything is decoded.
struct ImageSlot {
    block: usize,
    src: String,
    rows: u16,
}

pub struct App {
    book: Book,
    chapter_index: usize,
    chapter: Chapter,
    lines: Vec<Line>,
    /// Index of the topmost visible line.
    scroll: usize,
    /// Width the current layout was built for.
    laid_out_for: u16,
    view_height: u16,
    pub mode: Mode,
    contents_cursor: usize,
    annotations_cursor: usize,
    cursor: Option<Cursor>,
    /// Where the selection started, in text coordinates so a resize cannot
    /// invalidate it.
    anchor: Option<(usize, usize)>,
    /// True while `V` is in force: the selection covers whole lines.
    linewise: bool,
    pending: Option<char>,
    status: Option<String>,
    pub should_quit: bool,
    /// True when the reader was left for good rather than to pick another book.
    pub should_quit_program: bool,
    options: LayoutOptions,
    id: BookId,
    journal: Journal,
    pending_restore: Option<Locator>,
    /// All annotations of this book, replayed at startup and kept in step.
    annotations: Vec<Annotation>,
    /// Pictures on screen, by block. Only what the view shows is kept: a
    /// chapter of rendered formulas holds hundreds of pictures, and decoding
    /// them all took twenty seconds before the first line appeared.
    images: std::collections::HashMap<usize, crate::image::Rendered>,
    /// Every picture of the chapter with the size it will take, measured from
    /// the image headers. The layout needs all of them; decoding does not.
    image_slots: Vec<ImageSlot>,
    /// Width and maximum height the slots were measured for. Rendering reuses
    /// it, so a picture comes out exactly as tall as the layout reserved.
    image_box: (u16, u16),
    /// How this terminal draws pictures, decided once at startup.
    image_backend: crate::image::Backend,
    /// Pixel size of one cell, needed to scale pictures to whole cells.
    cell_size: crate::image::CellSize,
    /// Colours from the active Omarchy theme.
    theme: crate::theme::Watcher,
    search: crate::search::Search,
    /// Where the reader was before following links, most recent last. `Ctrl-o`
    /// walks back out, as Vim's jump list does.
    jumps: Vec<Jump>,
    /// Cursor position to restore once the layout exists. A cursor lives on a
    /// line, which only exists after wrapping.
    pending_cursor: Option<(usize, usize)>,
    /// What has been typed into the search prompt so far.
    search_input: String,
    pub pending_edit: Option<EditRequest>,
}

impl App {
    pub fn new(
        mut book: Book,
        id: BookId,
        journal: Journal,
        state: &State,
        options: LayoutOptions,
    ) -> Result<Self> {
        let restore = state.position(&id).cloned();
        let annotations = state.annotations(&id);

        // Open the chapter the position points into, not always the first.
        let chapter_index = restore
            .as_ref()
            .and_then(|locator| book.spine.iter().position(|item| item.href == locator.href))
            .unwrap_or(0);
        let chapter = load_chapter(&mut book, chapter_index);

        Ok(Self {
            book,
            chapter_index,
            chapter,
            lines: Vec::new(),
            scroll: 0,
            laid_out_for: 0,
            view_height: 1,
            mode: Mode::Reading,
            contents_cursor: chapter_index,
            annotations_cursor: 0,
            cursor: None,
            anchor: None,
            linewise: false,
            pending: None,
            status: None,
            should_quit: false,
            should_quit_program: false,
            options,
            id,
            journal,
            pending_restore: restore,
            annotations,
            images: std::collections::HashMap::new(),
            image_slots: Vec::new(),
            image_box: (0, 0),
            image_backend: crate::image::Backend::HalfBlocks,
            cell_size: crate::image::CellSize::default(),
            theme: crate::theme::Watcher::new(),
            search: crate::search::Search::default(),
            jumps: Vec::new(),
            pending_cursor: None,
            search_input: String::new(),
            pending_edit: None,
        })
    }

    /// Tells the reader how this terminal draws pictures. Set once at startup,
    /// after the terminal has been asked.
    pub fn set_image_backend(
        &mut self,
        backend: crate::image::Backend,
        cell_size: crate::image::CellSize,
    ) {
        self.image_backend = backend;
        self.cell_size = cell_size;
        self.images.clear();
        self.invalidate_layout();
    }

    pub fn search(&self) -> &crate::search::Search {
        &self.search
    }

    /// The search prompt, while one is being typed.
    pub fn search_input(&self) -> Option<&str> {
        if self.mode == Mode::Search {
            Some(&self.search_input)
        } else {
            None
        }
    }

    pub fn theme(&self) -> crate::theme::Theme {
        self.theme.theme()
    }

    /// Re-reads the theme file when it changed, which happens on every Omarchy
    /// theme switch. Returns true when the colours moved, so the caller can
    /// repaint.
    pub fn refresh_theme(&mut self) -> bool {
        if self.theme.refresh() {
            // Comment lines carry their annotation's colour, so the layout has
            // to be rebuilt for the new palette.
            self.invalidate_layout();
            true
        } else {
            false
        }
    }

    pub fn image_backend(&self) -> crate::image::Backend {
        self.image_backend
    }

    /// Identifies what is on screen: chapter, scroll position, layout width and
    /// mode.
    ///
    /// Pixel pictures live outside the text buffer, so a partial redraw leaves
    /// them behind. When this token changes, the screen has to be cleared before
    /// the next draw. The mode belongs in it because an overlay covers the text
    /// with cells, which leaves any picture underneath showing through.
    pub fn frame_token(&self) -> (usize, usize, u16, Mode) {
        (
            self.chapter_index,
            self.scroll,
            self.laid_out_for,
            self.mode,
        )
    }

    /// True when a picture needs a pixel protocol to appear.
    pub fn has_pixel_images(&self) -> bool {
        self.image_backend != crate::image::Backend::HalfBlocks && !self.images.is_empty()
    }

    // ----- reporting for the view -----

    pub fn title(&self) -> &str {
        self.book.title()
    }

    pub fn chapter_title(&self) -> String {
        self.book
            .spine
            .get(self.chapter_index)
            .and_then(|item| item.title.clone())
            .unwrap_or_else(|| format!("Chapter {}", self.chapter_index + 1))
    }

    pub fn chapter_number(&self) -> (usize, usize) {
        (self.chapter_index + 1, self.book.spine.len())
    }

    pub fn contents(&self) -> Vec<String> {
        self.book
            .spine
            .iter()
            .enumerate()
            .map(|(i, item)| {
                item.title
                    .clone()
                    .unwrap_or_else(|| format!("Chapter {}", i + 1))
            })
            .collect()
    }

    pub fn contents_cursor(&self) -> usize {
        self.contents_cursor
    }

    pub fn annotations(&self) -> &[Annotation] {
        &self.annotations
    }

    pub fn annotations_cursor(&self) -> usize {
        self.annotations_cursor
    }

    pub fn status(&self) -> Option<&str> {
        self.status.as_deref()
    }

    /// Annotations of the chapter on screen.
    pub fn annotations_here(&self) -> Vec<&Annotation> {
        self.annotations
            .iter()
            .filter(|a| a.href == self.chapter.href)
            .collect()
    }

    /// Works out how much room each of this chapter's pictures takes.
    ///
    /// Only the image headers are read, so this stays cheap however many
    /// pictures a chapter holds. A picture whose header cannot be read gets no
    /// slot, and the layout falls back to its alt text. A broken image must not
    /// cost the chapter.
    fn measure_images(&mut self, width: u16) {
        self.images.clear();
        self.image_slots.clear();
        self.image_box = (0, 0);
        if width < 8 {
            return;
        }
        // A picture belongs in the same column as the text, so it is measured
        // against the same width: `layout_full` narrows the window this way
        // too. Given the whole window instead, a picture broke out of the text
        // on both counts, too wide and, scaled in proportion, too tall.
        let width = width.min(self.options.max_width).max(8);
        // No picture may take more than this share of the view, so text stays
        // visible around it. The floor of four rows keeps a picture recognisable
        // in a short window, but never past the window itself: a picture taller
        // than the text area would be held back at every scroll position and
        // never appear at all.
        let max_rows = ((self.view_height as f32 * 0.8).round() as u16)
            .max(4)
            .min(self.view_height);
        self.image_box = (width, max_rows);

        let sources: Vec<(usize, String)> = self
            .chapter
            .blocks
            .iter()
            .enumerate()
            .filter_map(|(index, block)| match &block.kind {
                crate::doc::BlockKind::Image { src: Some(src) } => Some((index, src.clone())),
                _ => None,
            })
            .collect();

        for (index, src) in sources {
            let Ok(bytes) = self.book.read_binary(&src) else {
                continue;
            };
            let Ok(size) = crate::image::dimensions(&bytes) else {
                continue;
            };
            let (_, rows) =
                crate::image::measure(size, width, max_rows, self.image_backend, self.cell_size);
            if rows == 0 {
                continue;
            }
            self.image_slots.push(ImageSlot {
                block: index,
                src,
                rows,
            });
        }
    }

    /// How much room the layout must leave for each picture.
    fn image_placements(&self) -> Vec<layout::ImagePlacement> {
        self.image_slots
            .iter()
            .map(|slot| layout::ImagePlacement {
                block: slot.block,
                rows: slot.rows,
            })
            .collect()
    }

    /// Decodes the pictures the view shows and drops the rest.
    ///
    /// Called before every draw, because scrolling changes which ones are
    /// needed. Decoding one picture costs milliseconds; decoding a chapter of
    /// them costs seconds, which is why only what is on screen is done.
    fn render_visible(&mut self) {
        let (width, max_rows) = self.image_box;
        if width == 0 || self.image_slots.is_empty() {
            return;
        }
        let height = self.view_height as usize;
        let blocks_between = |from: usize, to: usize| -> std::collections::HashSet<usize> {
            let from = from.min(self.lines.len());
            let to = to.min(self.lines.len());
            self.lines[from..to]
                .iter()
                .filter(|line| matches!(line.kind, layout::LineKind::Image { .. }))
                .map(|line| line.block)
                .collect()
        };
        let visible = blocks_between(self.scroll, self.scroll + height);
        // Reading moves back as well as forward, and a picture just off the edge
        // is about to be wanted again. Keeping a screen either way spares the
        // decoding, and a handful of pictures is nothing to hold.
        let nearby = blocks_between(self.scroll.saturating_sub(height), self.scroll + height * 2);

        self.images.retain(|block, _| nearby.contains(block));

        // The slot's position is its Kitty id, so a picture keeps the same id
        // however often it leaves the screen and comes back.
        let pending: Vec<(u32, usize, String)> = self
            .image_slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| {
                visible.contains(&slot.block) && !self.images.contains_key(&slot.block)
            })
            // Ids start at 1; 0 is reserved by the Kitty protocol.
            .map(|(id, slot)| (id as u32 + 1, slot.block, slot.src.clone()))
            .collect();

        for (id, block, src) in pending {
            let Ok(bytes) = self.book.read_binary(&src) else {
                continue;
            };
            let rendered = crate::image::render(
                &bytes,
                width,
                max_rows,
                self.image_backend,
                id,
                self.cell_size,
            );
            if let Ok(rendered) = rendered {
                if rendered.height() > 0 {
                    self.images.insert(block, rendered);
                }
            }
        }
    }

    /// The picture of a block, once rendered.
    pub fn image_at(&self, block: usize) -> Option<&crate::image::Rendered> {
        self.images.get(&block)
    }

    /// Comments of this chapter, addressed to the block each one ends in so it
    /// appears right below the passage it belongs to.
    fn notes(&self) -> Vec<layout::NoteAnchor> {
        self.annotations
            .iter()
            .filter(|a| a.href == self.chapter.href)
            .filter_map(|a| {
                let text = a.note.as_ref()?;
                let last_block = a.slices.iter().map(|s| s.block).max()?;
                Some(layout::NoteAnchor {
                    block: last_block,
                    color: self.theme.theme().marks[a.color.index()],
                    text: text.clone(),
                })
            })
            .collect()
    }

    /// The link the cursor sits on, if any.
    pub fn link_at_cursor(&self) -> Option<&crate::doc::Link> {
        let (block, offset) = self.cursor_text_position()?;
        self.chapter.link_at(block, offset)
    }

    /// The annotation the cursor sits inside, if any. This is what `e`, `d` and
    /// `m` act on while reading, so an annotation can be changed where it is
    /// rather than only through the list.
    pub fn annotation_at_cursor(&self) -> Option<&Annotation> {
        let (block, offset) = self.cursor_text_position()?;
        self.annotations
            .iter()
            .find(|a| a.href == self.chapter.href && a.covers(block, offset))
    }

    /// The cursor's screen position, when a cursor exists.
    pub fn cursor_position(&self) -> Option<(usize, usize)> {
        self.cursor.map(|c| (c.line, c.column))
    }

    /// The selection as text coordinates, ordered from start to end.
    pub fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        if self.mode != Mode::Visual {
            return None;
        }
        let anchor = self.anchor?;
        let head = self.cursor_text_position()?;
        let (start, end) = if anchor <= head {
            (anchor, head)
        } else {
            (head, anchor)
        };
        if !self.linewise {
            return Some((start, end));
        }
        // Widen to whole lines. The anchor stays a text position, so a resize
        // re-derives the lines rather than invalidating the selection.
        let index = Index::new(&self.lines);
        let first = index.line_of(start.0, start.1)?;
        let last = index.line_of(end.0, end.1)?;
        let head_line = &self.lines[first];
        let tail_line = &self.lines[last];
        Some((
            (head_line.block, head_line.offset),
            (
                tail_line.block,
                tail_line.offset + tail_line.text_len().saturating_sub(1),
            ),
        ))
    }

    pub fn progress(&self) -> u16 {
        let total = self.lines.len();
        if total <= self.view_height as usize {
            return 100;
        }
        let last = total - self.view_height as usize;
        ((self.scroll.min(last) * 100) / last) as u16
    }

    /// First visible line, needed by the view to map screen rows to lines.
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    pub fn visible_lines(&self) -> &[Line] {
        let end = (self.scroll + self.view_height as usize).min(self.lines.len());
        &self.lines[self.scroll.min(self.lines.len())..end]
    }

    // ----- layout -----

    /// Rebuilds the layout when the view size changed, keeping the reading
    /// position. Called before each draw.
    pub fn prepare(&mut self, width: u16, height: u16) {
        self.view_height = height.max(1);
        if width == self.laid_out_for && !self.lines.is_empty() {
            self.clamp_scroll();
            // Scrolling moved other pictures into view, even though the layout
            // still holds.
            self.render_visible();
            return;
        }
        // Remember text positions, not line numbers: the wrap is about to change.
        let anchor_text = self.pending_restore.take().or_else(|| self.position());
        let cursor_text = self.cursor_text_position();

        self.measure_images(width);
        let placements = self.image_placements();
        self.lines = layout::layout_full(
            &self.chapter,
            width,
            self.options,
            &self.notes(),
            &placements,
        );
        self.laid_out_for = width;

        if let Some(anchor) = anchor_text {
            self.scroll = self.line_for(&anchor);
        }
        // A cursor being restored outranks the one that was there: it exists only
        // until the first layout has been built.
        if let Some((block, offset)) = self.pending_cursor.take().or(cursor_text) {
            self.cursor = self.cursor_at(block, offset);
        }
        self.clamp_scroll();
        // Last, because it needs the finished lines and the settled scroll.
        self.render_visible();
    }

    pub fn position(&self) -> Option<Locator> {
        let line = self.lines.get(self.scroll)?;
        Some(Locator {
            href: self.chapter.href.clone(),
            block: line.block,
            offset: line.offset,
        })
    }

    fn line_for(&self, locator: &Locator) -> usize {
        let mut best = 0;
        for (index, line) in self.lines.iter().enumerate() {
            // Comment lines carry no position of their own.
            if matches!(line.kind, crate::layout::LineKind::Note { .. }) {
                continue;
            }
            if line.block < locator.block
                || (line.block == locator.block && line.offset <= locator.offset)
            {
                best = index;
            } else {
                break;
            }
        }
        best
    }

    /// The cursor as a block and character offset.
    fn cursor_text_position(&self) -> Option<(usize, usize)> {
        let cursor = self.cursor?;
        let line = self.lines.get(cursor.line)?;
        Some((line.block, line.offset + cursor.column))
    }

    /// A cursor pointing at a block offset, clamped into the line that holds it.
    fn cursor_at(&self, block: usize, offset: usize) -> Option<Cursor> {
        let line_index = Index::new(&self.lines).line_of(block, offset)?;
        let line = self.lines.get(line_index)?;
        let column = offset
            .saturating_sub(line.offset)
            .min(line.text_len().saturating_sub(1));
        Some(Cursor {
            line: line_index,
            column,
        })
    }

    fn max_scroll(&self) -> usize {
        self.lines.len().saturating_sub(self.view_height as usize)
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.min(self.max_scroll());
    }

    fn scroll_by(&mut self, delta: isize) {
        let target = self.scroll as isize + delta;
        self.scroll = target.clamp(0, self.max_scroll() as isize) as usize;
        self.status = None;
    }

    /// Scrolls just enough to keep the cursor on screen.
    fn follow_cursor(&mut self) {
        let Some(cursor) = self.cursor else { return };
        let height = self.view_height as usize;
        if cursor.line < self.scroll {
            self.scroll = cursor.line;
        } else if cursor.line >= self.scroll + height {
            self.scroll = cursor.line + 1 - height;
        }
        self.clamp_scroll();
    }

    // ----- keys -----

    pub fn handle_key(&mut self, key: KeyEvent) {
        if let Some(first) = self.pending.take() {
            if self.handle_sequence(first, key) {
                return;
            }
        }
        match self.mode {
            Mode::Reading => self.handle_reading_key(key),
            Mode::Normal | Mode::Visual => self.handle_cursor_key(key),
            Mode::Contents => self.handle_contents_key(key),
            Mode::Annotations => self.handle_annotations_key(key),
            Mode::Search => self.handle_search_key(key),
            // Any key closes the help; there is nothing to do in it.
            Mode::Help => {
                self.mode = Mode::Reading;
                self.status = None;
            }
        }
    }

    fn handle_sequence(&mut self, first: char, key: KeyEvent) -> bool {
        match (first, key.code) {
            ('g', KeyCode::Char('g')) => {
                match self.mode {
                    Mode::Contents => self.contents_cursor = 0,
                    Mode::Annotations => self.annotations_cursor = 0,
                    Mode::Normal | Mode::Visual => {
                        self.cursor = self.first_cursor();
                        self.follow_cursor();
                    }
                    Mode::Reading => self.scroll = 0,
                    Mode::Help | Mode::Search => {}
                }
                true
            }
            ('d', KeyCode::Char('h')) => {
                self.delete_at_cursor();
                true
            }
            ('d', KeyCode::Char('a')) => {
                self.clear_note_at_cursor();
                true
            }
            // A different key cancels the deletion rather than guessing.
            ('d', _) => {
                self.status = Some("deletion cancelled".into());
                true
            }
            // `m` prefixes a colour. Without it, `b` and `g` would never reach
            // the colours: they mean "word back" and the start of `gg`.
            ('m', KeyCode::Char(c)) => {
                match Color::from_shortcut(c) {
                    Some(color) => match self.mode {
                        Mode::Visual => self.create_annotation(color, false),
                        Mode::Annotations => self.recolor_selected(color),
                        // Without a selection, recolour what the cursor is on.
                        Mode::Normal => self.recolor_at_cursor(color),
                        _ => self.status = Some("m picks a colour while selecting".into()),
                    },
                    None => {
                        self.status =
                            Some("colours: y yellow, g green, b blue, r red, p purple".into())
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn handle_reading_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code == KeyCode::Char('?') {
            self.mode = Mode::Help;
            return;
        }
        let half = (self.view_height / 2).max(1) as isize;
        let page = (self.view_height.saturating_sub(2)).max(1) as isize;

        match key.code {
            // Only `q` quits. Esc takes back what is in force, which is what a
            // Vim user expects, and it must never end the session by accident.
            // `q` leaves the book. Coming from the library that means going
            // back to it; started with a file it means leaving.
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('Q') => {
                self.should_quit = true;
                self.should_quit_program = true;
            }
            KeyCode::Esc => self.dismiss(),
            KeyCode::Char('j') | KeyCode::Down => self.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_by(-1),
            KeyCode::Char('d') if ctrl => self.scroll_by(half),
            KeyCode::Char('u') if ctrl => self.scroll_by(-half),
            KeyCode::Char('f') if ctrl => self.scroll_by(page),
            KeyCode::Char('b') if ctrl => self.scroll_by(-page),
            KeyCode::Char(' ') | KeyCode::PageDown => self.scroll_by(page),
            KeyCode::Backspace | KeyCode::PageUp => self.scroll_by(-page),
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('G') => self.scroll = self.max_scroll(),
            KeyCode::Char('L') | KeyCode::Char(']') => self.next_chapter(),
            KeyCode::Char('H') | KeyCode::Char('[') => self.previous_chapter(),
            KeyCode::Char('t') | KeyCode::Tab => self.open_contents(),
            KeyCode::Char('A') => self.open_annotations(),
            KeyCode::Char('o') if ctrl => self.jump_back(),
            // Entering normal mode puts a cursor into the text.
            KeyCode::Char('i') => self.enter_normal_mode(),
            KeyCode::Char('/') => self.open_search(),
            KeyCode::Char('n') => self.jump_to_match(true),
            KeyCode::Char('N') => self.jump_to_match(false),
            KeyCode::Char('v') => {
                self.enter_normal_mode();
                self.start_selection(false);
            }
            KeyCode::Char('V') => {
                self.enter_normal_mode();
                self.start_selection(true);
            }
            _ => {}
        }
    }

    fn handle_cursor_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if key.code == KeyCode::Char('?') {
            self.mode = Mode::Help;
            return;
        }
        let half = (self.view_height / 2).max(1) as isize;

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if self.mode == Mode::Visual {
                    self.leave_selection();
                } else {
                    self.leave_normal_mode();
                }
            }
            KeyCode::Char('Q') => {
                self.should_quit = true;
                self.should_quit_program = true;
            }
            KeyCode::Char('i') if self.mode == Mode::Normal => self.leave_normal_mode(),
            KeyCode::Char('/') if self.mode == Mode::Normal => self.open_search(),
            KeyCode::Char('n') if self.mode == Mode::Normal => self.jump_to_match(true),
            KeyCode::Char('N') if self.mode == Mode::Normal => self.jump_to_match(false),
            KeyCode::Char('v') => {
                if self.mode == Mode::Visual && !self.linewise {
                    self.leave_selection();
                } else {
                    self.start_selection(false);
                }
            }
            KeyCode::Char('V') => {
                if self.mode == Mode::Visual && self.linewise {
                    self.leave_selection();
                } else {
                    self.start_selection(true);
                }
            }
            KeyCode::Char('h') | KeyCode::Left => self.move_cursor_horizontally(-1),
            KeyCode::Char('l') | KeyCode::Right => self.move_cursor_horizontally(1),
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor_vertically(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor_vertically(-1),
            KeyCode::Char('d') if ctrl => self.move_cursor_vertically(half),
            KeyCode::Char('u') if ctrl => self.move_cursor_vertically(-half),
            KeyCode::Char('w') => self.move_cursor_word(true),
            KeyCode::Char('b') => self.move_cursor_word(false),
            KeyCode::Char('0') => self.move_cursor_to_line_edge(false),
            KeyCode::Char('$') => self.move_cursor_to_line_edge(true),
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('G') => {
                self.cursor = self.last_cursor();
                self.follow_cursor();
            }
            // `y` highlights straight away, in the default colour. Any other
            // colour goes through `m`, because `b` and `g` are movements.
            KeyCode::Char('y') if self.mode == Mode::Visual => {
                self.create_annotation(Color::Yellow, false)
            }
            KeyCode::Char('m') => self.pending = Some('m'),
            // A comment needs a highlight to hang off, so it creates one.
            KeyCode::Char('a') if self.mode == Mode::Visual => {
                self.create_annotation(Color::Yellow, true)
            }
            // With a cursor but no selection, these act on the annotation under
            // the cursor.
            KeyCode::Char('e') if self.mode == Mode::Normal => self.edit_note_at_cursor(),
            KeyCode::Char('d') if self.mode == Mode::Normal => self.start_delete(),
            KeyCode::Char('A') => self.open_annotations(),
            KeyCode::Char('t') | KeyCode::Tab => self.open_contents(),
            // Enter follows the link under the cursor; Ctrl-o walks back out,
            // as it does in Vim.
            KeyCode::Enter if self.mode == Mode::Normal => self.follow_link(),
            KeyCode::Char('o') if ctrl => self.jump_back(),
            _ => {}
        }
    }

    /// Collects the search term. Enter runs it, Esc drops it.
    fn handle_search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Reading;
                self.search_input.clear();
                self.status = None;
            }
            KeyCode::Enter => {
                let query = std::mem::take(&mut self.search_input);
                self.mode = Mode::Reading;
                if query.is_empty() {
                    self.search.clear();
                    self.status = None;
                    return;
                }
                self.search.set_query(query);
                self.search.scan(&self.chapter);
                // Start from where the reader stands, not from the chapter's top.
                let from = self
                    .position()
                    .map(|p| (p.block, p.offset))
                    .unwrap_or((0, 0));
                if self.search.go_to_first_after(from.0, from.1) {
                    self.show_current_match();
                } else {
                    // Nothing further on in this chapter, so look on ahead.
                    self.jump_to_match(true);
                }
            }
            KeyCode::Backspace => {
                self.search_input.pop();
            }
            KeyCode::Char(c) => self.search_input.push(c),
            _ => {}
        }
    }

    fn handle_contents_key(&mut self, key: KeyEvent) {
        let last = self.book.spine.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Tab | KeyCode::Char('t') => {
                self.mode = Mode::Reading
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.contents_cursor = (self.contents_cursor + 1).min(last)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.contents_cursor = self.contents_cursor.saturating_sub(1)
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('G') => self.contents_cursor = last,
            KeyCode::Enter => {
                let target = self.contents_cursor;
                self.go_to_chapter(target);
                self.mode = Mode::Reading;
            }
            _ => {}
        }
    }

    fn handle_annotations_key(&mut self, key: KeyEvent) {
        let last = self.annotations.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('A') => self.mode = Mode::Reading,
            KeyCode::Char('j') | KeyCode::Down => {
                self.annotations_cursor = (self.annotations_cursor + 1).min(last)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.annotations_cursor = self.annotations_cursor.saturating_sub(1)
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('g') => self.pending = Some('g'),
            KeyCode::Char('G') => self.annotations_cursor = last,
            KeyCode::Enter => self.jump_to_selected_annotation(),
            KeyCode::Char('e') => self.edit_selected_note(),
            KeyCode::Char('d') => self.delete_selected_annotation(),
            // Same prefix as while selecting, so one rule covers both places.
            KeyCode::Char('m') => self.pending = Some('m'),
            _ => {}
        }
    }

    // ----- cursor movement -----

    fn first_cursor(&self) -> Option<Cursor> {
        Index::new(&self.lines)
            .next_selectable(0)
            .map(|line| Cursor { line, column: 0 })
    }

    fn last_cursor(&self) -> Option<Cursor> {
        let index = Index::new(&self.lines);
        let line = index.previous_selectable(self.lines.len().saturating_sub(1))?;
        let column = self.lines[line].text_len().saturating_sub(1);
        Some(Cursor { line, column })
    }

    fn enter_normal_mode(&mut self) {
        self.mode = Mode::Normal;
        if self.cursor.is_none() || !self.cursor_is_visible() {
            // Start at the top of the view, where the eye already is.
            self.cursor = Index::new(&self.lines)
                .next_selectable(self.scroll)
                .map(|line| Cursor { line, column: 0 });
        }
        self.status = Some("cursor mode: Enter follows a link, v selects, i leaves".into());
    }

    fn cursor_is_visible(&self) -> bool {
        match self.cursor {
            Some(cursor) => {
                cursor.line >= self.scroll && cursor.line < self.scroll + self.view_height as usize
            }
            None => false,
        }
    }

    fn leave_normal_mode(&mut self) {
        self.mode = Mode::Reading;
        self.anchor = None;
        self.linewise = false;
        self.status = None;
    }

    fn start_selection(&mut self, linewise: bool) {
        if self.cursor.is_none() {
            self.enter_normal_mode();
        }
        self.anchor = self.cursor_text_position();
        if self.anchor.is_some() {
            self.mode = Mode::Visual;
            self.linewise = linewise;
            self.status = Some("y highlights, m+y/g/b/r/p picks a colour, a comments".into());
        }
    }

    fn leave_selection(&mut self) {
        self.mode = Mode::Normal;
        self.anchor = None;
        self.linewise = false;
        self.status = None;
    }

    fn move_cursor_horizontally(&mut self, delta: isize) {
        let Some(mut cursor) = self.cursor else {
            return;
        };
        let index = Index::new(&self.lines);
        if delta > 0 {
            let len = self.lines[cursor.line].text_len();
            if cursor.column + 1 < len {
                cursor.column += 1;
            } else if let Some(next) = index.next_selectable(cursor.line + 1) {
                cursor = Cursor {
                    line: next,
                    column: 0,
                };
            }
        } else if cursor.column > 0 {
            cursor.column -= 1;
        } else if cursor.line > 0 {
            if let Some(previous) = index.previous_selectable(cursor.line - 1) {
                cursor = Cursor {
                    line: previous,
                    column: self.lines[previous].text_len().saturating_sub(1),
                };
            }
        }
        self.cursor = Some(cursor);
        self.status = None;
        self.follow_cursor();
    }

    fn move_cursor_vertically(&mut self, delta: isize) {
        let Some(cursor) = self.cursor else { return };
        let index = Index::new(&self.lines);
        let mut line = cursor.line;
        let steps = delta.unsigned_abs();
        for _ in 0..steps {
            let next = if delta > 0 {
                index.next_selectable(line + 1)
            } else if line == 0 {
                None
            } else {
                index.previous_selectable(line - 1)
            };
            match next {
                Some(found) => line = found,
                None => break,
            }
        }
        let column = cursor
            .column
            .min(self.lines[line].text_len().saturating_sub(1));
        self.cursor = Some(Cursor { line, column });
        self.status = None;
        self.follow_cursor();
    }

    fn move_cursor_to_line_edge(&mut self, end: bool) {
        let Some(mut cursor) = self.cursor else {
            return;
        };
        cursor.column = if end {
            self.lines[cursor.line].text_len().saturating_sub(1)
        } else {
            0
        };
        self.cursor = Some(cursor);
        self.status = None;
    }

    /// Word-wise movement over the block's text, so it crosses line breaks the
    /// way the words run in the source.
    fn move_cursor_word(&mut self, forward: bool) {
        let Some((block, offset)) = self.cursor_text_position() else {
            return;
        };
        let Some(text) = self.chapter.blocks.get(block).map(|b| b.plain_text()) else {
            return;
        };
        let chars: Vec<char> = text.chars().collect();
        let target = if forward {
            next_word_start(&chars, offset)
        } else {
            previous_word_start(&chars, offset)
        };

        match target {
            Some(offset) => {
                self.cursor = self.cursor_at(block, offset);
                self.status = None;
                self.follow_cursor();
            }
            // Past the block's edge: continue in the neighbouring line.
            None => self.move_cursor_vertically(if forward { 1 } else { -1 }),
        }
    }

    // ----- annotations -----

    /// Turns the current selection into slices over the chapter's blocks.
    fn selection_slices(&self) -> Option<Vec<Slice>> {
        let ((start_block, start_offset), (end_block, end_offset)) = self.selection()?;
        let mut slices = Vec::new();
        for block in start_block..=end_block {
            let len = self.chapter.blocks.get(block)?.plain_text().chars().count();
            let from = if block == start_block {
                start_offset
            } else {
                0
            };
            // The selection includes the character under the cursor.
            let to = if block == end_block {
                (end_offset + 1).min(len)
            } else {
                len
            };
            if from < to {
                slices.push(Slice {
                    block,
                    start: from,
                    end: to,
                });
            }
        }
        if slices.is_empty() {
            None
        } else {
            Some(slices)
        }
    }

    fn quote_of(&self, slices: &[Slice]) -> String {
        let mut parts = Vec::new();
        for slice in slices {
            if let Some(block) = self.chapter.blocks.get(slice.block) {
                let text: String = block
                    .plain_text()
                    .chars()
                    .skip(slice.start)
                    .take(slice.end.saturating_sub(slice.start))
                    .collect();
                if !text.trim().is_empty() {
                    parts.push(text.trim().to_string());
                }
            }
        }
        parts.join(" ")
    }

    /// Records an annotation over the selection. `then_comment` opens the editor
    /// afterwards, so the annotation gains a comment right away.
    fn create_annotation(&mut self, color: Color, then_comment: bool) {
        let Some(slices) = self.selection_slices() else {
            self.status = Some("nothing selected".into());
            return;
        };
        let quote = self.quote_of(&slices);
        let id = self.journal.next_id();

        let payload = Payload::HighlightAdded {
            id: id.clone(),
            href: self.chapter.href.clone(),
            slices: slices.clone(),
            color,
            quote: quote.clone(),
        };
        if let Err(err) = self.journal.append(&self.id, payload) {
            self.status = Some(format!("cannot save annotation: {err}"));
            return;
        }

        self.annotations.push(Annotation {
            id: id.clone(),
            href: self.chapter.href.clone(),
            slices,
            color,
            quote: quote.clone(),
            note: None,
        });
        self.sort_annotations();

        self.mode = Mode::Normal;
        self.anchor = None;
        self.linewise = false;

        if then_comment {
            self.pending_edit = Some(EditRequest {
                annotation_id: id,
                initial_text: String::new(),
                quote,
            });
        } else {
            self.status = Some(format!("highlighted in {}", color.label()));
        }
        self.invalidate_layout();
    }

    /// Marks the layout as stale. Needed whenever annotations change, because
    /// comments are laid out as lines of their own.
    fn invalidate_layout(&mut self) {
        self.laid_out_for = 0;
    }

    /// Shows a message in the status line.
    pub fn report(&mut self, message: String) {
        self.status = Some(message);
    }

    /// Called by the event loop once `$EDITOR` returned.
    pub fn finish_edit(&mut self, id: &str, text: String) {
        let trimmed = text.trim().to_string();
        if let Err(err) = self.journal.append(
            &self.id,
            Payload::NoteSet {
                id: id.to_string(),
                text: trimmed.clone(),
            },
        ) {
            self.status = Some(format!("cannot save comment: {err}"));
            return;
        }
        if let Some(annotation) = self.annotations.iter_mut().find(|a| a.id == id) {
            annotation.note = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            };
        }
        self.invalidate_layout();
        self.status = Some("comment saved".into());
    }

    fn sort_annotations(&mut self) {
        self.annotations
            .sort_by(|a, b| a.href.cmp(&b.href).then_with(|| a.start().cmp(&b.start())));
    }

    fn open_annotations(&mut self) {
        if self.annotations.is_empty() {
            self.status = Some("no annotations yet".into());
            return;
        }
        self.annotations_cursor = self.annotations_cursor.min(self.annotations.len() - 1);
        self.mode = Mode::Annotations;
    }

    fn jump_to_selected_annotation(&mut self) {
        let Some(annotation) = self.annotations.get(self.annotations_cursor).cloned() else {
            return;
        };
        let chapter = self
            .book
            .spine
            .iter()
            .position(|item| item.href == annotation.href);
        if let Some(index) = chapter {
            if index != self.chapter_index {
                self.go_to_chapter(index);
            }
        }
        let (block, offset) = annotation.start();
        self.pending_restore = Some(Locator {
            href: annotation.href.clone(),
            block,
            offset,
        });
        // Force a rebuild so the pending position is applied.
        self.laid_out_for = 0;
        self.lines.clear();
        self.mode = Mode::Reading;
    }

    /// Writes or rewrites the comment of the annotation under the cursor.
    fn edit_note_at_cursor(&mut self) {
        let Some(annotation) = self.annotation_at_cursor() else {
            self.status = Some("no annotation here: select with v, then a".into());
            return;
        };
        self.pending_edit = Some(EditRequest {
            annotation_id: annotation.id.clone(),
            initial_text: annotation.note.clone().unwrap_or_default(),
            quote: annotation.quote.clone(),
        });
    }

    /// Begins a deletion at the cursor.
    ///
    /// An annotation can hold two things: the colour on the passage and the
    /// comment. While only one is present, `d` is unambiguous and acts at once.
    /// While both are, it waits for the object: `dh` drops the colour, `da` the
    /// comment.
    fn start_delete(&mut self) {
        let Some(annotation) = self.annotation_at_cursor() else {
            self.status = Some("no annotation here".into());
            return;
        };
        if annotation.has_note() {
            self.pending = Some('d');
            self.status = Some("dh deletes it all, da only the comment".into());
        } else {
            // Nothing to choose between: there is no comment to keep.
            let id = annotation.id.clone();
            self.remove(&id);
        }
    }

    /// Deletes the annotation the cursor is on, comment and all. A passage
    /// without a colour is not an annotation, so there is nothing left to keep.
    fn delete_at_cursor(&mut self) {
        let Some(id) = self.annotation_at_cursor().map(|a| a.id.clone()) else {
            self.status = Some("no annotation here".into());
            return;
        };
        self.remove(&id);
    }

    /// Removes the comment and leaves the annotation in place.
    fn clear_note_at_cursor(&mut self) {
        let Some(annotation) = self.annotation_at_cursor() else {
            self.status = Some("no annotation here".into());
            return;
        };
        if !annotation.has_note() {
            self.status = Some("no comment here".into());
            return;
        }
        let id = annotation.id.clone();
        if let Err(err) = self.journal.append(
            &self.id,
            Payload::NoteSet {
                id: id.clone(),
                text: String::new(),
            },
        ) {
            self.status = Some(format!("cannot remove comment: {err}"));
            return;
        }
        if let Some(existing) = self.annotations.iter_mut().find(|a| a.id == id) {
            existing.note = None;
        }
        self.invalidate_layout();
        self.status = Some("comment removed, annotation kept".into());
    }

    fn recolor_at_cursor(&mut self, color: Color) {
        let Some(id) = self.annotation_at_cursor().map(|a| a.id.clone()) else {
            self.status = Some("no annotation here".into());
            return;
        };
        self.recolor(&id, color);
    }

    fn edit_selected_note(&mut self) {
        let Some(annotation) = self.annotations.get(self.annotations_cursor) else {
            return;
        };
        self.pending_edit = Some(EditRequest {
            annotation_id: annotation.id.clone(),
            initial_text: annotation.note.clone().unwrap_or_default(),
            quote: annotation.quote.clone(),
        });
    }

    fn delete_selected_annotation(&mut self) {
        let Some(id) = self
            .annotations
            .get(self.annotations_cursor)
            .map(|a| a.id.clone())
        else {
            return;
        };
        self.remove(&id);
    }

    fn recolor_selected(&mut self, color: Color) {
        let Some(id) = self
            .annotations
            .get(self.annotations_cursor)
            .map(|a| a.id.clone())
        else {
            return;
        };
        self.recolor(&id, color);
    }

    /// Deletes an annotation, whether it was picked in the list or under the
    /// cursor.
    fn remove(&mut self, id: &str) {
        if let Err(err) = self
            .journal
            .append(&self.id, Payload::AnnotationRemoved { id: id.to_string() })
        {
            self.status = Some(format!("cannot delete: {err}"));
            return;
        }
        self.annotations.retain(|a| a.id != id);
        self.invalidate_layout();
        if self.annotations.is_empty() && self.mode == Mode::Annotations {
            self.mode = Mode::Reading;
        } else if !self.annotations.is_empty() {
            self.annotations_cursor = self.annotations_cursor.min(self.annotations.len() - 1);
        }
        self.status = Some("annotation deleted".into());
    }

    fn recolor(&mut self, id: &str, color: Color) {
        if let Err(err) = self.journal.append(
            &self.id,
            Payload::ColorSet {
                id: id.to_string(),
                color,
            },
        ) {
            self.status = Some(format!("cannot change colour: {err}"));
            return;
        }
        if let Some(existing) = self.annotations.iter_mut().find(|a| a.id == id) {
            existing.color = color;
        }
        self.invalidate_layout();
        self.status = Some(format!("recoloured to {}", color.label()));
    }

    // ----- navigation -----

    /// Opens a chapter by its href, for coming in from outside.
    pub fn go_to_href(&mut self, href: &str) {
        match self.find_in_spine(href) {
            Some(index) => self.go_to_chapter(index),
            None => self.status = Some(format!("chapter not found: {href}")),
        }
    }

    /// Runs a search and jumps to the first match, for coming in from a hit found
    /// elsewhere.
    pub fn search_for(&mut self, text: String) {
        self.search.set_query(text);
        self.search.scan(&self.chapter);
        if self.search.go_to_first() {
            self.show_current_match();
        } else {
            // Not in this chapter: look on through the book.
            self.jump_to_match(true);
        }
    }

    pub fn save_position(&mut self) {
        let Some(locator) = self.position() else {
            return;
        };
        if let Err(err) = self.journal.record_position(&self.id, &locator) {
            self.status = Some(format!("cannot save position: {err}"));
        }
    }

    /// Takes back whatever is in force: the search highlighting first, then any
    /// message. Never quits.
    fn dismiss(&mut self) {
        if self.search.is_active() {
            self.search.clear();
            self.status = Some("search cleared".into());
            return;
        }
        self.status = None;
    }

    /// Follows the link under the cursor.
    ///
    /// A target inside the book is resolved against the chapter it is written in,
    /// and the fragment decides the line: `notes.xhtml#fn20` lands on the
    /// footnote, not at the top of the notes. Where the reader came from is kept,
    /// so `Ctrl-o` walks back.
    fn follow_link(&mut self) {
        let Some(link) = self.link_at_cursor().cloned() else {
            self.status = Some("no link here".into());
            return;
        };
        // A link out of the book is shown rather than opened: this is a reader,
        // not a browser.
        if link.target.contains("://") || link.target.starts_with("mailto:") {
            self.status = Some(format!("external link: {}", link.target));
            return;
        }

        let (file, fragment) = split_target(&link.target);
        let here = Jump {
            view: self.position().unwrap_or_else(|| Locator {
                href: self.chapter.href.clone(),
                block: 0,
                offset: 0,
            }),
            // The cursor is on the link being followed, which is exactly where
            // coming back should put it.
            cursor: self.cursor_text_position(),
        };

        // An empty file part means the same chapter.
        let target_href = if file.is_empty() {
            self.chapter.href.clone()
        } else {
            resolve_href(&self.chapter.href, file)
        };

        let Some(target_index) = self.find_in_spine(&target_href) else {
            self.status = Some(format!("target is not in the reading order: {target_href}"));
            return;
        };

        if target_index != self.chapter_index {
            self.go_to_chapter(target_index);
        }
        // Now that the chapter is loaded, its anchors can place the fragment.
        let landing = fragment
            .and_then(|id| self.chapter.anchors.get(id).copied())
            .unwrap_or((0, 0));

        self.jumps.push(here);
        // Land at the target with a cursor, so the next link is one keypress
        // away and Ctrl-o has something to return to.
        self.pending_cursor = Some((landing.0, landing.1));
        self.mode = Mode::Normal;
        self.pending_restore = Some(Locator {
            href: self.chapter.href.clone(),
            block: landing.0,
            offset: landing.1,
        });
        self.laid_out_for = 0;
        self.lines.clear();
        self.status = match fragment {
            Some(id) if self.chapter.anchors.contains_key(id) => None,
            Some(id) => Some(format!("target {id:?} not found, showing the chapter")),
            None => None,
        };
    }

    /// Finds a chapter in the reading order.
    ///
    /// An exact match first. Failing that, the file name alone decides: books
    /// exported from publishing tools often keep links to a directory that no
    /// longer exists, such as `../Text/01.htm` when the file sits beside its
    /// neighbours. Refusing to follow those would make the contents of many books
    /// dead text, and a file name is unique inside a container in practice.
    fn find_in_spine(&self, href: &str) -> Option<usize> {
        if let Some(index) = self.book.spine.iter().position(|item| item.href == href) {
            return Some(index);
        }
        let name = std::path::Path::new(href).file_name()?;
        let matches: Vec<usize> = self
            .book
            .spine
            .iter()
            .enumerate()
            .filter(|(_, item)| std::path::Path::new(&item.href).file_name() == Some(name))
            .map(|(index, _)| index)
            .collect();
        // Only when it is unambiguous: two files of the same name in different
        // directories would be a guess.
        match matches.as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }

    /// Walks back out of followed links.
    fn jump_back(&mut self) {
        let Some(back) = self.jumps.pop() else {
            self.status = Some("nowhere to go back to".into());
            return;
        };
        if let Some(index) = self.find_in_spine(&back.view.href) {
            if index != self.chapter_index {
                // Going back must not push another entry onto the stack.
                self.chapter_index = index;
                self.chapter = load_chapter(&mut self.book, index);
                self.images.clear();
                self.contents_cursor = index;
                self.cursor = None;
                self.anchor = None;
                if self.search.is_active() {
                    self.search.scan(&self.chapter);
                }
            }
        }
        self.pending_restore = Some(back.view);
        self.pending_cursor = back.cursor;
        // Coming back into the text means the cursor is wanted, so the mode
        // follows it rather than dropping to plain reading.
        if back.cursor.is_some() && self.mode == Mode::Reading {
            self.mode = Mode::Normal;
        }
        self.laid_out_for = 0;
        self.lines.clear();
        self.status = None;
    }

    fn open_search(&mut self) {
        self.search_input = self.search.query().to_string();
        self.mode = Mode::Search;
        self.status = None;
    }

    /// Steps to the next or previous match, crossing chapters when this one holds
    /// no further match.
    ///
    /// Chapters are parsed as they are reached rather than up front, so opening a
    /// book stays instant even in a title with a hundred chapters.
    fn jump_to_match(&mut self, forward: bool) {
        if !self.search.is_active() {
            self.status = Some("nothing searched yet: / starts a search".into());
            return;
        }
        // Within the current chapter first.
        let stepped = if forward {
            self.search.next_in_chapter()
        } else {
            self.search.previous_in_chapter()
        };
        if stepped {
            self.show_current_match();
            return;
        }

        let count = self.book.spine.len();
        let mut index = self.chapter_index;
        for _ in 0..count {
            index = if forward {
                if index + 1 >= count {
                    break;
                }
                index + 1
            } else {
                if index == 0 {
                    break;
                }
                index - 1
            };

            let chapter = load_chapter(&mut self.book, index);
            let hits = crate::search::find_all(&chapter, self.search.query());
            if hits.is_empty() {
                continue;
            }
            // Found one: move there and pick the first or last match.
            self.save_position();
            self.chapter_index = index;
            self.chapter = chapter;
            self.contents_cursor = index;
            self.images.clear();
            self.cursor = None;
            self.anchor = None;
            self.search.scan(&self.chapter);
            if forward {
                self.search.go_to_first();
            } else {
                self.search.go_to_last();
            }
            self.show_current_match();
            return;
        }
        self.status = Some(format!("no more matches for {:?}", self.search.query()));
    }

    /// Scrolls to the match the reader is on and reports which it is.
    fn show_current_match(&mut self) {
        let Some((block, offset)) = self.search.current_position() else {
            return;
        };
        self.pending_restore = Some(Locator {
            href: self.chapter.href.clone(),
            block,
            offset,
        });
        // Force a rebuild so the pending position is applied.
        self.laid_out_for = 0;
        self.lines.clear();
        self.mode = Mode::Reading;
        self.status = match self.search.progress() {
            Some((at, total)) => Some(format!("{:?}  {at}/{total}", self.search.query())),
            None => None,
        };
    }

    fn open_contents(&mut self) {
        self.contents_cursor = self.chapter_index;
        self.mode = Mode::Contents;
    }

    fn next_chapter(&mut self) {
        if self.chapter_index + 1 < self.book.spine.len() {
            self.go_to_chapter(self.chapter_index + 1);
        } else {
            self.status = Some("end of book".into());
        }
    }

    fn previous_chapter(&mut self) {
        if self.chapter_index > 0 {
            self.go_to_chapter(self.chapter_index - 1);
        } else {
            self.status = Some("start of book".into());
        }
    }

    fn go_to_chapter(&mut self, index: usize) {
        if index >= self.book.spine.len() {
            return;
        }
        self.save_position();
        self.chapter_index = index;
        self.chapter = load_chapter(&mut self.book, index);
        self.images.clear();
        if self.search.is_active() {
            self.search.scan(&self.chapter);
        }
        self.contents_cursor = index;
        self.scroll = 0;
        self.cursor = None;
        self.anchor = None;
        if self.mode == Mode::Visual || self.mode == Mode::Normal {
            self.mode = Mode::Reading;
        }
        // Force a rebuild on the next draw.
        self.laid_out_for = 0;
        self.lines.clear();
        self.status = None;
    }
}

/// The key bindings, grouped for the help. Kept beside the handlers above so a
/// changed key does not leave a stale description behind.
pub const BINDINGS: &[(&str, &[(&str, &str)])] = &[
    (
        "Reading",
        &[
            ("j k", "line down, up"),
            ("Space Backspace", "page down, up"),
            ("Ctrl-d Ctrl-u", "half a page"),
            ("gg G", "start, end of chapter"),
            ("L ]", "next chapter"),
            ("H [", "previous chapter"),
            ("t Tab", "contents"),
            ("A", "annotations"),
            ("/", "search the book"),
            ("n N", "next, previous match"),
            ("i", "cursor mode, with a cursor in the text"),
            ("v V", "select by character, by line"),
            ("Esc", "clear the search"),
            ("q", "back to the library"),
            ("Q", "quit"),
        ],
    ),
    (
        "Cursor and selection",
        &[
            ("h l w b 0 $", "move by character, word, to line edge"),
            ("j k gg G", "move by line, to chapter edges"),
            ("v V", "start or end a selection"),
            ("y", "highlight in the default colour"),
            (
                "m then y g b r p",
                "highlight in yellow, green, blue, red, purple",
            ),
            ("a", "annotate with a comment in $EDITOR"),
            ("e", "write or change the comment under the cursor"),
            ("d", "delete the annotation under the cursor"),
            ("dh da", "delete it all, delete only the comment"),
            ("Enter", "follow the link under the cursor"),
            ("Ctrl-o", "back out of followed links"),
            ("/ n N", "search, next match, previous match"),
            ("Esc i", "leave the cursor"),
        ],
    ),
    (
        "Contents and annotations",
        &[
            ("j k gg G", "move the cursor"),
            ("Enter", "open the chapter, jump to the annotation"),
            ("e", "change a comment"),
            ("d", "delete an annotation"),
            ("m then colour", "recolour"),
            ("q Esc", "close"),
        ],
    ),
];

/// Splits a link target into its file part and its fragment.
fn split_target(target: &str) -> (&str, Option<&str>) {
    match target.split_once('#') {
        Some((file, fragment)) if fragment.is_empty() => (file, None),
        Some((file, fragment)) => (file, Some(fragment)),
        None => (target, None),
    }
}

/// Resolves a link's file part against the chapter it appears in.
fn resolve_href(from: &str, target: &str) -> String {
    let base = std::path::Path::new(from)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let combined = if base.is_empty() || target.starts_with('/') {
        target.trim_start_matches('/').to_string()
    } else {
        format!("{base}/{target}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in combined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts.join("/")
}

/// Start of the next word at or after `from`, if one exists in this block.
fn next_word_start(chars: &[char], from: usize) -> Option<usize> {
    let mut at = from;
    // Leave the current word.
    while at < chars.len() && !chars[at].is_whitespace() {
        at += 1;
    }
    // Skip the gap.
    while at < chars.len() && chars[at].is_whitespace() {
        at += 1;
    }
    if at < chars.len() { Some(at) } else { None }
}

/// Start of the word before `from`, if one exists in this block.
fn previous_word_start(chars: &[char], from: usize) -> Option<usize> {
    if from == 0 {
        return None;
    }
    let mut at = from - 1;
    while at > 0 && chars[at].is_whitespace() {
        at -= 1;
    }
    while at > 0 && !chars[at - 1].is_whitespace() {
        at -= 1;
    }
    Some(at)
}

/// Loads a chapter, turning a parse failure into a readable placeholder rather
/// than ending the session.
fn load_chapter(book: &mut Book, index: usize) -> Chapter {
    match book.chapter(index) {
        Ok(chapter) => chapter,
        Err(err) => {
            let href = book
                .spine
                .get(index)
                .map(|i| i.href.clone())
                .unwrap_or_default();
            Chapter {
                href,
                links: Vec::new(),
                anchors: std::collections::HashMap::new(),
                blocks: vec![crate::doc::Block {
                    kind: crate::doc::BlockKind::Paragraph,
                    runs: vec![crate::doc::Run {
                        text: format!("This chapter could not be read: {err}"),
                        style: crate::doc::RunStyle::default(),
                    }],
                }],
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_a_target_into_file_and_fragment() {
        assert_eq!(
            split_target("ch03.xhtml#fn20"),
            ("ch03.xhtml", Some("fn20"))
        );
        assert_eq!(split_target("#fn20"), ("", Some("fn20")));
        assert_eq!(split_target("ch03.xhtml"), ("ch03.xhtml", None));
        // A trailing hash names no target.
        assert_eq!(split_target("ch03.xhtml#"), ("ch03.xhtml", None));
    }

    #[test]
    fn resolves_a_target_against_its_chapter() {
        assert_eq!(
            resolve_href("OEBPS/ch01.xhtml", "ch02.xhtml"),
            "OEBPS/ch02.xhtml"
        );
        assert_eq!(
            resolve_href("OEBPS/text/ch01.xhtml", "../notes.xhtml"),
            "OEBPS/notes.xhtml"
        );
        assert_eq!(resolve_href("ch01.xhtml", "ch02.xhtml"), "ch02.xhtml");
    }

    #[test]
    fn word_movement_walks_forward_over_gaps() {
        let chars: Vec<char> = "one two  three".chars().collect();
        assert_eq!(next_word_start(&chars, 0), Some(4));
        assert_eq!(next_word_start(&chars, 4), Some(9));
        assert_eq!(next_word_start(&chars, 9), None);
    }

    #[test]
    fn word_movement_walks_backward() {
        let chars: Vec<char> = "one two  three".chars().collect();
        assert_eq!(previous_word_start(&chars, 9), Some(4));
        assert_eq!(previous_word_start(&chars, 4), Some(0));
        assert_eq!(previous_word_start(&chars, 0), None);
    }
}

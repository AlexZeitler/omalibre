//! Document model for a single chapter.
//!
//! A chapter parses into a flat list of blocks. A block's index in that list is
//! its address. Annotations and reading positions will anchor to it, so parsing
//! must stay deterministic: same file, same block list.

/// Inline styling of a text run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RunStyle {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub link: bool,
}

impl RunStyle {
    pub fn merged(self, other: RunStyle) -> Self {
        Self {
            bold: self.bold || other.bold,
            italic: self.italic || other.italic,
            code: self.code || other.code,
            link: self.link || other.link,
        }
    }
}

/// A stretch of text with uniform styling.
#[derive(Debug, Clone)]
pub struct Run {
    pub text: String,
    pub style: RunStyle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockKind {
    Heading(u8),
    Paragraph,
    Quote,
    /// Preformatted text. Line breaks are significant.
    Code,
    ListItem {
        depth: u8,
        ordinal: Option<usize>,
    },
    /// Horizontal rule from `<hr>`.
    Rule,
    /// An image. The runs hold its alt text, `src` the path inside the
    /// container, already resolved against the chapter.
    Image {
        src: Option<String>,
    },
}

#[derive(Debug, Clone)]
pub struct Block {
    pub kind: BlockKind,
    pub runs: Vec<Run>,
}

impl Block {
    /// The block's text without styling. Character offsets into this string are
    /// addresses within the block.
    pub fn plain_text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

/// A link in the text, with the range it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub block: usize,
    /// First character, inclusive.
    pub start: usize,
    /// Last character, exclusive.
    pub end: usize,
    /// The href as written in the book, still relative.
    pub target: String,
}

impl Link {
    pub fn covers(&self, block: usize, offset: usize) -> bool {
        self.block == block && offset >= self.start && offset < self.end
    }
}

/// A parsed chapter.
#[derive(Debug, Clone, Default)]
pub struct Chapter {
    /// Spine href addressing this chapter inside the book.
    pub href: String,
    pub blocks: Vec<Block>,
    /// Links found in the text, in reading order.
    pub links: Vec<Link>,
    /// Element ids and where they sit, so a link can land on the right line
    /// rather than at the top of a chapter.
    pub anchors: std::collections::HashMap<String, (usize, usize)>,
}

impl Chapter {
    /// The link covering a position, if any.
    pub fn link_at(&self, block: usize, offset: usize) -> Option<&Link> {
        self.links.iter().find(|link| link.covers(block, offset))
    }
}

/// Address of a text position in a book. Basis for reading positions and
/// annotations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Locator {
    pub href: String,
    pub block: usize,
    /// Character offset into the block's `plain_text()`.
    pub offset: usize,
}

/// Collects runs, merging adjacent ones that share a style.
#[derive(Default)]
pub struct RunBuilder {
    runs: Vec<Run>,
}

impl RunBuilder {
    /// Characters collected so far. Links are recorded as ranges into the block
    /// being built, so they need this while it is still open.
    pub fn char_count(&self) -> usize {
        self.runs.iter().map(|r| r.text.chars().count()).sum()
    }

    pub fn push(&mut self, text: &str, style: RunStyle) {
        if text.is_empty() {
            return;
        }
        match self.runs.last_mut() {
            Some(last) if last.style == style => last.text.push_str(text),
            _ => self.runs.push(Run {
                text: text.to_string(),
                style,
            }),
        }
    }

    /// True when nothing but whitespace was collected.
    pub fn is_blank(&self) -> bool {
        self.runs.iter().all(|r| r.text.trim().is_empty())
    }

    /// Returns the runs unchanged. For code, where leading spaces are the
    /// indentation and must survive.
    pub fn finish_verbatim(mut self) -> Vec<Run> {
        self.runs.retain(|r| !r.text.is_empty());
        self.runs
    }

    /// Returns the runs, trimming whitespace at the block's edges.
    pub fn finish(mut self) -> Vec<Run> {
        if let Some(first) = self.runs.first_mut() {
            first.text = first.text.trim_start().to_string();
        }
        if let Some(last) = self.runs.last_mut() {
            last.text = last.text.trim_end().to_string();
        }
        self.runs.retain(|r| !r.text.is_empty());
        self.runs
    }
}

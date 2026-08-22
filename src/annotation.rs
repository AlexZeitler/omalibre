//! Highlights and comments, and how they anchor to the text.
//!
//! An annotation must not hang off a screen line, because the wrap changes with
//! the window. It hangs off the document structure instead: a chapter href plus
//! character ranges inside blocks. A selection spanning several blocks
//! contributes one slice per block it touches.
//!
//! The quoted text is stored alongside. If the file changes and an anchor no
//! longer fits, the passage can still be found by its wording.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Color {
    Yellow,
    Green,
    Blue,
    Red,
    Purple,
}

impl Color {
    pub const ALL: [Color; 5] = [
        Color::Yellow,
        Color::Green,
        Color::Blue,
        Color::Red,
        Color::Purple,
    ];

    /// The key that picks this colour.
    pub fn shortcut(self) -> char {
        match self {
            Color::Yellow => 'y',
            Color::Green => 'g',
            Color::Blue => 'b',
            Color::Red => 'r',
            Color::Purple => 'p',
        }
    }

    pub fn from_shortcut(key: char) -> Option<Self> {
        Color::ALL.into_iter().find(|c| c.shortcut() == key)
    }

    /// Position in the theme's mark palette, in the order of `ALL`.
    pub fn index(self) -> usize {
        match self {
            Color::Yellow => 0,
            Color::Green => 1,
            Color::Blue => 2,
            Color::Red => 3,
            Color::Purple => 4,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Color::Yellow => "yellow",
            Color::Green => "green",
            Color::Blue => "blue",
            Color::Red => "red",
            Color::Purple => "purple",
        }
    }
}

/// A character range inside one block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slice {
    pub block: usize,
    /// First character, inclusive.
    pub start: usize,
    /// Last character, exclusive.
    pub end: usize,
}

impl Slice {
    pub fn contains(&self, block: usize, offset: usize) -> bool {
        self.block == block && offset >= self.start && offset < self.end
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub id: String,
    /// Chapter this annotation lives in.
    pub href: String,
    pub slices: Vec<Slice>,
    pub color: Color,
    /// The annotated text, for display in lists and as a fallback anchor.
    pub quote: String,
    /// A comment, when one was written.
    pub note: Option<String>,
}

impl Annotation {
    /// Where the annotation begins, for sorting and navigation.
    pub fn start(&self) -> (usize, usize) {
        self.slices
            .iter()
            .map(|s| (s.block, s.start))
            .min()
            .unwrap_or((0, 0))
    }

    pub fn covers(&self, block: usize, offset: usize) -> bool {
        self.slices.iter().any(|s| s.contains(block, offset))
    }

    pub fn has_note(&self) -> bool {
        self.note.as_ref().is_some_and(|n| !n.trim().is_empty())
    }
}

/// Relative luminance per WCAG 2.1.
fn luminance((r, g, b): (u8, u8, u8)) -> f32 {
    fn channel(value: u8) -> f32 {
        let v = value as f32 / 255.0;
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(r) + 0.7152 * channel(g) + 0.0722 * channel(b)
}

/// Contrast ratio between two colours, per WCAG 2.1.
pub fn contrast_ratio(a: (u8, u8, u8), b: (u8, u8, u8)) -> f32 {
    let (l1, l2) = (luminance(a), luminance(b));
    let (lighter, darker) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (lighter + 0.05) / (darker + 0.05)
}

/// Picks the text colour that reads best on a highlight.
///
/// A terminal has no transparency: a background colour replaces the text
/// instead of letting it through. As long as the terminal's own background is
/// unknown, both colours are set, which keeps the text legible in any theme.
/// Once the theme file is read, the highlight can be mixed against the real
/// background instead.
pub fn text_color_on(background: (u8, u8, u8)) -> (u8, u8, u8) {
    const BLACK: (u8, u8, u8) = (0x1c, 0x1c, 0x1c);
    const WHITE: (u8, u8, u8) = (0xf5, 0xf5, 0xf5);
    if contrast_ratio(background, BLACK) >= contrast_ratio(background, WHITE) {
        BLACK
    } else {
        WHITE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_colour_has_a_distinct_shortcut() {
        let mut keys: Vec<char> = Color::ALL.iter().map(|c| c.shortcut()).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), Color::ALL.len());
    }

    #[test]
    fn shortcuts_round_trip() {
        for color in Color::ALL {
            assert_eq!(Color::from_shortcut(color.shortcut()), Some(color));
        }
        assert_eq!(Color::from_shortcut('x'), None);
    }

    #[test]
    fn contrast_is_symmetric_and_bounded() {
        let white = (255, 255, 255);
        let black = (0, 0, 0);
        assert!((contrast_ratio(white, black) - 21.0).abs() < 0.01);
        assert_eq!(contrast_ratio(white, black), contrast_ratio(black, white));
        assert!((contrast_ratio(white, white) - 1.0).abs() < 0.01);
    }

    #[test]
    fn a_slice_covers_its_range_half_open() {
        let slice = Slice {
            block: 3,
            start: 5,
            end: 8,
        };
        assert!(!slice.contains(3, 4));
        assert!(slice.contains(3, 5));
        assert!(slice.contains(3, 7));
        assert!(!slice.contains(3, 8));
        assert!(!slice.contains(2, 6));
    }

    #[test]
    fn an_annotation_starts_at_its_earliest_slice() {
        let annotation = Annotation {
            id: "a".into(),
            href: "c.xhtml".into(),
            slices: vec![
                Slice {
                    block: 7,
                    start: 0,
                    end: 4,
                },
                Slice {
                    block: 5,
                    start: 9,
                    end: 20,
                },
            ],
            color: Color::Yellow,
            quote: "text".into(),
            note: None,
        };
        assert_eq!(annotation.start(), (5, 9));
        assert!(annotation.covers(7, 2));
        assert!(!annotation.covers(6, 0));
    }
}

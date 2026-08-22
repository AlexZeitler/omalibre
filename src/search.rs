//! Searching inside a book.
//!
//! The search runs over the parsed blocks, not the raw markup, so it finds what
//! the reader shows and never a tag name. Matching ignores case.
//!
//! Chapters are searched one at a time, starting from where the reader stands.
//! A book of a hundred chapters is not indexed up front: that would cost seconds
//! on opening, for something the reader may never ask for.

use crate::doc::Chapter;

/// Where a match sits: the block and the character offset inside it.
pub type Hit = (usize, usize);

#[derive(Debug, Clone, Default)]
pub struct Search {
    /// What was typed, as typed.
    query: String,
    /// Matches in the chapter currently laid out.
    hits: Vec<Hit>,
    /// Which of those the reader is on.
    current: Option<usize>,
}

impl Search {
    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn is_active(&self) -> bool {
        !self.query.is_empty()
    }

    pub fn hits(&self) -> &[Hit] {
        &self.hits
    }

    pub fn current(&self) -> Option<Hit> {
        self.current.and_then(|index| self.hits.get(index)).copied()
    }

    /// True when this position is the match the reader is on.
    pub fn is_current(&self, block: usize, offset: usize) -> bool {
        self.current() == Some((block, offset))
    }

    pub fn len(&self) -> usize {
        self.query.chars().count()
    }

    pub fn set_query(&mut self, query: String) {
        self.query = query;
        self.hits.clear();
        self.current = None;
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Re-runs the search over a chapter, keeping the query.
    pub fn scan(&mut self, chapter: &Chapter) {
        self.hits = find_all(chapter, &self.query);
        self.current = None;
    }

    /// Selects the first match at or after a position. Returns whether one was
    /// found in this chapter.
    pub fn go_to_first_after(&mut self, block: usize, offset: usize) -> bool {
        match self.hits.iter().position(|hit| *hit >= (block, offset)) {
            Some(index) => {
                self.current = Some(index);
                true
            }
            None => false,
        }
    }

    /// Selects the last match before a position.
    #[cfg(test)]
    pub fn go_to_last_before(&mut self, block: usize, offset: usize) -> bool {
        match self.hits.iter().rposition(|hit| *hit < (block, offset)) {
            Some(index) => {
                self.current = Some(index);
                true
            }
            None => false,
        }
    }

    pub fn go_to_first(&mut self) -> bool {
        self.current = if self.hits.is_empty() { None } else { Some(0) };
        self.current.is_some()
    }

    pub fn go_to_last(&mut self) -> bool {
        self.current = self.hits.len().checked_sub(1);
        self.current.is_some()
    }

    /// Steps to the next match within this chapter. False when there is none, so
    /// the caller can move on to the next chapter.
    pub fn next_in_chapter(&mut self) -> bool {
        match self.current {
            Some(index) if index + 1 < self.hits.len() => {
                self.current = Some(index + 1);
                true
            }
            None if !self.hits.is_empty() => {
                self.current = Some(0);
                true
            }
            _ => false,
        }
    }

    pub fn previous_in_chapter(&mut self) -> bool {
        match self.current {
            Some(index) if index > 0 => {
                self.current = Some(index - 1);
                true
            }
            None if !self.hits.is_empty() => {
                self.current = Some(self.hits.len() - 1);
                true
            }
            _ => false,
        }
    }

    /// Position of the match the reader is on, for scrolling to it.
    pub fn current_position(&self) -> Option<Hit> {
        self.current()
    }

    /// Which match of how many, for the status line.
    pub fn progress(&self) -> Option<(usize, usize)> {
        self.current.map(|index| (index + 1, self.hits.len()))
    }
}

/// Every match of `needle` in a chapter, in reading order.
///
/// Matching is case-insensitive. Comparing lowercased strings can shift offsets
/// for characters whose lowercase form has a different length, so the search runs
/// over lowercased characters rather than over a lowercased string.
pub fn find_all(chapter: &Chapter, needle: &str) -> Vec<Hit> {
    if needle.is_empty() {
        return Vec::new();
    }
    let needle: Vec<char> = needle.chars().flat_map(|c| c.to_lowercase()).collect();
    let mut hits = Vec::new();

    for (index, block) in chapter.blocks.iter().enumerate() {
        let text: Vec<char> = block
            .plain_text()
            .chars()
            .flat_map(|c| c.to_lowercase())
            .collect();
        if text.len() < needle.len() {
            continue;
        }
        // Overlapping matches are not reported twice: the search steps past a
        // match, as a reader would expect from `n`.
        let mut at = 0;
        while at + needle.len() <= text.len() {
            if text[at..at + needle.len()] == needle[..] {
                hits.push((index, at));
                at += needle.len();
            } else {
                at += 1;
            }
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Block, BlockKind, Run, RunStyle};

    fn chapter(texts: &[&str]) -> Chapter {
        Chapter {
            links: Vec::new(),
            anchors: std::collections::HashMap::new(),
            href: "c.xhtml".into(),
            blocks: texts
                .iter()
                .map(|text| Block {
                    kind: BlockKind::Paragraph,
                    runs: vec![Run {
                        text: (*text).into(),
                        style: RunStyle::default(),
                    }],
                })
                .collect(),
        }
    }

    #[test]
    fn finds_every_match_in_reading_order() {
        let chapter = chapter(&["the cat sat", "no match here", "cat again, cat"]);
        assert_eq!(find_all(&chapter, "cat"), vec![(0, 4), (2, 0), (2, 11)]);
    }

    #[test]
    fn ignores_case_on_both_sides() {
        let chapter = chapter(&["The Phoenix Framework"]);
        assert_eq!(find_all(&chapter, "phoenix"), vec![(0, 4)]);
        assert_eq!(find_all(&chapter, "PHOENIX"), vec![(0, 4)]);
    }

    #[test]
    fn an_empty_query_matches_nothing() {
        assert!(find_all(&chapter(&["text"]), "").is_empty());
    }

    #[test]
    fn a_query_longer_than_the_block_is_skipped() {
        assert!(find_all(&chapter(&["ab"]), "abc").is_empty());
    }

    #[test]
    fn overlapping_matches_are_reported_once() {
        // "aaaa" holds two non-overlapping "aa".
        assert_eq!(find_all(&chapter(&["aaaa"]), "aa"), vec![(0, 0), (0, 2)]);
    }

    #[test]
    fn steps_through_the_matches() {
        let mut search = Search::default();
        search.set_query("cat".into());
        search.scan(&chapter(&["cat", "cat"]));
        assert_eq!(search.hits().len(), 2);

        assert!(search.next_in_chapter());
        assert_eq!(search.current(), Some((0, 0)));
        assert!(search.next_in_chapter());
        assert_eq!(search.current(), Some((1, 0)));
        // Past the last one, so the caller moves to the next chapter.
        assert!(!search.next_in_chapter());
        assert_eq!(search.progress(), Some((2, 2)));
    }

    #[test]
    fn steps_backwards() {
        let mut search = Search::default();
        search.set_query("cat".into());
        search.scan(&chapter(&["cat", "cat"]));
        assert!(search.previous_in_chapter());
        // With no current match, going back starts at the last.
        assert_eq!(search.current(), Some((1, 0)));
        assert!(search.previous_in_chapter());
        assert_eq!(search.current(), Some((0, 0)));
        assert!(!search.previous_in_chapter());
    }

    #[test]
    fn jumps_to_the_match_after_a_position() {
        let mut search = Search::default();
        search.set_query("cat".into());
        search.scan(&chapter(&["cat", "dog", "cat"]));
        assert!(search.go_to_first_after(1, 0));
        assert_eq!(search.current(), Some((2, 0)));
        // Nothing after the last match, so the caller looks further on.
        assert!(!search.go_to_first_after(3, 0));
    }

    #[test]
    fn jumps_to_the_match_before_a_position() {
        let mut search = Search::default();
        search.set_query("cat".into());
        search.scan(&chapter(&["cat", "dog", "cat"]));
        assert!(search.go_to_last_before(2, 0));
        assert_eq!(search.current(), Some((0, 0)));
        assert!(!search.go_to_last_before(0, 0));
    }

    #[test]
    fn a_new_query_drops_the_old_matches() {
        let mut search = Search::default();
        search.set_query("cat".into());
        search.scan(&chapter(&["cat"]));
        assert_eq!(search.hits().len(), 1);
        search.set_query("dog".into());
        assert!(search.hits().is_empty());
        assert!(search.current().is_none());
    }
}

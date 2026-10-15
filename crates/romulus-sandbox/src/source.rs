//! Offsets, positions and spans in a SQL text.
//!
//! Every range this crate reports is a [`Span`], which carries the same range
//! in the three units its consumers count in: bytes (Rust strings and
//! `pg_query` locations), UTF-16 code units (JavaScript strings, CodeMirror and
//! Yjs `Y.Text` as the browser uses it) and 1-based line/column for people.

use std::ops::Range;

use serde::Serialize;

/// A 1-based line and column. Columns count Unicode scalar values, which is
/// what an editor's "Ln 3, Col 7" shows for text without combining marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Position {
    pub line: u32,
    pub column: u32,
}

/// A range of a SQL text, `start..end`, in bytes, UTF-16 code units and
/// line/column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[cfg_attr(feature = "utoipa", derive(utoipa::ToSchema))]
pub struct Span {
    /// Byte offset of the first character.
    pub start: usize,
    /// Byte offset just past the last character.
    pub end: usize,
    /// `start` in UTF-16 code units: a JavaScript string index.
    pub utf16_start: usize,
    /// `end` in UTF-16 code units.
    pub utf16_end: usize,
    /// Where `start` is.
    pub from: Position,
    /// Where `end` is (the position just past the range).
    pub to: Position,
}

impl Span {
    /// The byte range, to slice the text this span was made from.
    pub fn range(&self) -> Range<usize> {
        self.start..self.end
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// A line index over one text: turns byte offsets into [`Position`]s and
/// [`Span`]s in `O(log lines + line length)`. It does not keep the text, so it
/// can outlive the borrow; its methods take the same text again.
#[derive(Debug, Clone)]
pub struct LineIndex {
    len: usize,
    /// Byte offset of the start of each line.
    starts: Vec<usize>,
    /// UTF-16 offset of the start of each line.
    utf16_starts: Vec<usize>,
}

impl LineIndex {
    pub fn new(text: &str) -> Self {
        let mut starts = vec![0];
        let mut utf16_starts = vec![0];
        let mut utf16 = 0;
        for (offset, c) in text.char_indices() {
            utf16 += c.len_utf16();
            if c == '\n' {
                starts.push(offset + 1);
                utf16_starts.push(utf16);
            }
        }
        Self {
            len: text.len(),
            starts,
            utf16_starts,
        }
    }

    /// The position of a byte offset (clamped to the text, and to the start
    /// of the character it falls into).
    pub fn position(&self, text: &str, offset: usize) -> Position {
        let offset = floor_char_boundary(text, offset.min(self.len));
        let line = self.line_of(offset);
        let column = text[self.starts[line]..offset].chars().count() + 1;
        Position {
            line: saturating_u32(line + 1),
            column: saturating_u32(column),
        }
    }

    /// The UTF-16 offset of a byte offset.
    pub fn utf16(&self, text: &str, offset: usize) -> usize {
        let offset = floor_char_boundary(text, offset.min(self.len));
        let line = self.line_of(offset);
        self.utf16_starts[line]
            + text[self.starts[line]..offset]
                .chars()
                .map(char::len_utf16)
                .sum::<usize>()
    }

    /// The span of a byte range of `text` (the text this index was built on).
    pub fn span(&self, text: &str, range: Range<usize>) -> Span {
        let start = floor_char_boundary(text, range.start.min(self.len));
        let end = floor_char_boundary(text, range.end.clamp(start, self.len));
        Span {
            start,
            end,
            utf16_start: self.utf16(text, start),
            utf16_end: self.utf16(text, end),
            from: self.position(text, start),
            to: self.position(text, end),
        }
    }

    fn line_of(&self, offset: usize) -> usize {
        match self.starts.binary_search(&offset) {
            Ok(line) => line,
            Err(next) => next - 1,
        }
    }
}

/// The largest char boundary at or before `offset`.
pub(crate) fn floor_char_boundary(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// The byte offset of the `n`th character (0-based); the end of the text when
/// `n` is past it. Postgres reports error positions in characters.
pub(crate) fn char_to_byte(text: &str, n: usize) -> usize {
    text.char_indices()
        .nth(n)
        .map_or(text.len(), |(offset, _)| offset)
}

/// A `pg_query` location (an `i32` byte offset, `-1` when unknown).
pub(crate) fn location(value: i32) -> Option<usize> {
    usize::try_from(value).ok()
}

fn saturating_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::{LineIndex, Position, char_to_byte};

    #[test]
    fn positions_are_one_based_and_count_characters() {
        let text = "ab\ncafé x\n\nz";
        let index = LineIndex::new(text);
        assert_eq!(index.position(text, 0), Position { line: 1, column: 1 });
        assert_eq!(index.position(text, 3), Position { line: 2, column: 1 });
        // "café " is 6 bytes but 5 characters.
        let x = text.find('x').unwrap();
        assert_eq!(index.position(text, x), Position { line: 2, column: 6 });
        assert_eq!(
            index.position(text, text.len()),
            Position { line: 4, column: 2 }
        );
    }

    #[test]
    fn utf16_offsets_count_surrogate_pairs_twice() {
        let text = "-- ☕🐘\nCREATE";
        let index = LineIndex::new(text);
        let create = text.find("CREATE").unwrap();
        // "-- " (3) + ☕ (1 unit) + 🐘 (2 units) + "\n" (1) = 7.
        assert_eq!(index.utf16(text, create), 7);
        let span = index.span(text, create..text.len());
        assert_eq!((span.utf16_start, span.utf16_end), (7, 13));
        assert_eq!(span.from, Position { line: 2, column: 1 });
        assert_eq!(&text[span.range()], "CREATE");
    }

    #[test]
    fn offsets_inside_a_character_snap_back_to_its_start() {
        let text = "é";
        let index = LineIndex::new(text);
        let span = index.span(text, 1..2);
        assert_eq!((span.start, span.end), (0, 2));
    }

    #[test]
    fn character_indices_map_to_byte_offsets() {
        assert_eq!(char_to_byte("aé☕b", 3), "aé☕".len());
        assert_eq!(char_to_byte("ab", 9), 2);
    }
}

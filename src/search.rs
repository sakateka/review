use ratatui::text::Line;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub row: usize,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone)]
pub struct BufferSearch {
    pub word: String,
    pub matches: Vec<Match>,
    pub backward: bool,
}

// Use the same display-cell coordinates as rendering, including wide/combining
// Unicode. Search literal whole words, so punctuation never becomes a regex.
fn words(text: &str) -> impl Iterator<Item = (usize, usize, &str)> {
    let mut found = Vec::new();
    let mut start = None;
    let mut column = 0;
    for (byte, grapheme) in text.grapheme_indices(true) {
        let keyword = grapheme.chars().any(|c| c.is_alphanumeric() || c == '_');
        if keyword {
            start.get_or_insert((byte, column));
        } else if let Some((byte_start, cell_start)) = start.take() {
            found.push((cell_start, column, &text[byte_start..byte]));
        }
        column += grapheme.width();
    }
    if let Some((byte, cell)) = start {
        found.push((cell, column, &text[byte..]));
    }
    found.into_iter()
}

impl BufferSearch {
    pub fn at_cursor(
        lines: &[Line<'_>],
        cursor: (usize, usize),
        backward: bool,
    ) -> Option<(Self, (usize, usize))> {
        let text = lines.get(cursor.0)?.to_string();
        // Like Vim *, a cursor on whitespace/punctuation can use the next word
        // on this line. A cursor inside a word searches for that complete word.
        let (start, _, word) = words(&text).find(|(_, end, _)| *end > cursor.1)?;
        let mut matches = Vec::new();
        for (row, line) in lines.iter().enumerate() {
            let text = line.to_string();
            for (start, end, candidate) in words(&text) {
                if candidate == word {
                    matches.push(Match { row, start, end });
                }
            }
        }
        Some((
            Self {
                word: word.into(),
                matches,
                backward,
            },
            (cursor.0, start),
        ))
    }

    pub fn refresh(&mut self, lines: &[Line<'_>]) {
        self.matches.clear();
        for (row, line) in lines.iter().enumerate() {
            let text = line.to_string();
            for (start, end, word) in words(&text) {
                if word == self.word {
                    self.matches.push(Match { row, start, end });
                }
            }
        }
    }

    pub fn next(&self, cursor: (usize, usize), reverse: bool, count: usize) -> Option<Match> {
        let len = self.matches.len();
        if len == 0 {
            return None;
        }
        let index = if self.backward ^ reverse {
            let before = self.matches.partition_point(|m| (m.row, m.start) < cursor);
            (before + len - count % len) % len
        } else {
            let after = self.matches.partition_point(|m| (m.row, m.start) <= cursor);
            (after + count.saturating_sub(1) % len) % len
        };
        Some(self.matches[index])
    }
}

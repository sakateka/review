use ratatui::text::Line;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

// Motions follow rendered grapheme boundaries, never UTF-8 bytes or wide-cell halves.
pub fn word(
    lines: &[Line<'_>],
    cursor: (usize, usize),
    motion: char,
    count: usize,
) -> (usize, usize) {
    let big = motion.is_ascii_uppercase();
    let mut cells = Vec::new();
    for (row, line) in lines.iter().enumerate() {
        let text = line.to_string();
        let mut column = 0;
        for grapheme in text.graphemes(true) {
            let class = if grapheme.chars().all(char::is_whitespace) {
                0
            } else if big || grapheme.chars().any(|c| c.is_alphanumeric() || c == '_') {
                1
            } else {
                2
            };
            if grapheme.width() > 0 {
                cells.push(((row, column), class));
            }
            column += grapheme.width();
        }
        cells.push(((row, column), 0));
    }
    if cells.is_empty() {
        return cursor;
    }
    let mut i = cells
        .partition_point(|(position, _)| *position <= cursor)
        .saturating_sub(1);
    for _ in 0..count {
        let before = i;
        match motion.to_ascii_lowercase() {
            'w' => {
                let class = cells[i].1;
                while i + 1 < cells.len() && cells[i].1 == class {
                    i += 1;
                }
                while i + 1 < cells.len() && cells[i].1 == 0 {
                    i += 1;
                }
            }
            'b' => {
                i = i.saturating_sub(1);
                while i > 0 && cells[i].1 == 0 {
                    i -= 1;
                }
                let class = cells[i].1;
                while i > 0 && cells[i - 1].1 == class {
                    i -= 1;
                }
            }
            'e' => {
                i = (i + 1).min(cells.len() - 1);
                while i + 1 < cells.len() && cells[i].1 == 0 {
                    i += 1;
                }
                let class = cells[i].1;
                while i + 1 < cells.len() && cells[i + 1].1 == class {
                    i += 1;
                }
            }
            _ => {}
        }
        if before == i {
            break;
        }
    }
    cells[i].0
}

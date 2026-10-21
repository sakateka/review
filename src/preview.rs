use crate::{git::Item, source};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use similar::{ChangeTag, TextDiff};
use std::{ops::Range, path::Path};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug)]
pub struct Preview {
    pub lines: Vec<Line<'static>>,
    pub reason: Option<String>,
    pub width: usize,
}

type MappedColors<'a> = (
    &'a source::Sources,
    &'a [Option<Color>],
    &'a [Option<Color>],
);

fn syntax(side: &source::Side) -> anyhow::Result<Vec<Option<Color>>> {
    anyhow::ensure!(
        side.path.ends_with(b".rs"),
        "syntax highlighting currently supports Rust (.rs); other languages retain basic output"
    );
    let names = [
        "comment", "string", "keyword", "function", "type", "constant", "number", "operator",
        "variable",
    ];
    let colors = [
        Color::DarkGray,
        Color::Green,
        Color::Magenta,
        Color::Blue,
        Color::Cyan,
        Color::Yellow,
        Color::Yellow,
        Color::Cyan,
        Color::White,
    ];
    let mut configuration = HighlightConfiguration::new(
        tree_sitter_rust::LANGUAGE.into(),
        "rust",
        tree_sitter_rust::HIGHLIGHTS_QUERY,
        "",
        "",
    )?;
    configuration.configure(&names);
    let mut highlighter = Highlighter::new();
    let events = highlighter.highlight(&configuration, side.content.as_bytes(), None, |_| None)?;
    let mut stack = Vec::new();
    let mut bytes = vec![None; side.content.len()];
    for event in events {
        match event? {
            HighlightEvent::HighlightStart(highlight) => stack.push(colors[highlight.0]),
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => bytes[start..end].fill(stack.last().copied()),
        }
    }
    Ok(bytes)
}

fn changed_ranges(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let mut old_ranges = Vec::new();
    let mut new_ranges = Vec::new();
    let mut old_offset = 0;
    let mut new_offset = 0;
    for change in TextDiff::from_chars(old, new).iter_all_changes() {
        let length = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                old_offset += length;
                new_offset += length;
            }
            ChangeTag::Delete => {
                old_ranges.push(old_offset..old_offset + length);
                old_offset += length;
            }
            ChangeTag::Insert => {
                new_ranges.push(new_offset..new_offset + length);
                new_offset += length;
            }
        }
    }
    (old_ranges, new_ranges)
}

fn offsets(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(index, _)| index + 1))
        .collect()
}

impl Preview {
    pub fn original(bytes: &[u8]) -> Self {
        Self::compose(&String::from_utf8_lossy(bytes), None, None)
    }

    pub fn item(cwd: &Path, item: &Item) -> Self {
        let sources = source::load(cwd, item);
        let (mapped, reason) = match &sources {
            Ok(sources) => match (syntax(&sources.old), syntax(&sources.new)) {
                (Ok(old), Ok(new)) => (Some((sources, old, new)), None),
                (Err(error), _) | (_, Err(error)) => (None, Some(error.to_string())),
            },
            Err(error) => (
                None,
                Some(format!("Source enrichment unavailable: {error}")),
            ),
        };
        Self::compose(
            &String::from_utf8_lossy(&item.patch),
            mapped
                .as_ref()
                .map(|(sources, old, new)| (*sources, old.as_slice(), new.as_slice())),
            reason,
        )
    }

    fn compose(patch: &str, mapped: Option<MappedColors<'_>>, reason: Option<String>) -> Self {
        let raw: Vec<_> = patch.split_terminator('\n').collect();
        let mut remaining = (0, 0);
        let source_lines: Vec<_> = raw
            .iter()
            .map(|line| {
                if let Some((_, old_count, _, new_count)) = source::hunk(line) {
                    remaining = (old_count, new_count);
                    return false;
                }
                match line.as_bytes().first() {
                    Some(b' ') if remaining.0 > 0 && remaining.1 > 0 => {
                        remaining.0 -= 1;
                        remaining.1 -= 1;
                        true
                    }
                    Some(b'-') if remaining.0 > 0 => {
                        remaining.0 -= 1;
                        true
                    }
                    Some(b'+') if remaining.1 > 0 => {
                        remaining.1 -= 1;
                        true
                    }
                    _ => false,
                }
            })
            .collect();
        let mut emphasis: Vec<Vec<Range<usize>>> = vec![Vec::new(); raw.len()];
        let mut index = 0;
        while index < raw.len() {
            if source_lines[index] && raw[index].starts_with('-') {
                let start = index;
                while index < raw.len() && source_lines[index] && raw[index].starts_with('-') {
                    index += 1;
                }
                let middle = index;
                while index < raw.len() && source_lines[index] && raw[index].starts_with('+') {
                    index += 1;
                }
                for (old, new) in (start..middle).zip(middle..index) {
                    let (left, right) = changed_ranges(&raw[old][1..], &raw[new][1..]);
                    emphasis[old] = left;
                    emphasis[new] = right;
                }
            } else {
                index += 1;
            }
        }
        let old_offsets = mapped
            .map(|(sources, _, _)| offsets(&sources.old.content))
            .unwrap_or_default();
        let new_offsets = mapped
            .map(|(sources, _, _)| offsets(&sources.new.content))
            .unwrap_or_default();
        let mut old_line = 0;
        let mut new_line = 0;
        let mut lines = Vec::new();
        for (index, text) in raw.iter().enumerate() {
            if let Some((old, _, new, _)) = source::hunk(text) {
                old_line = old.saturating_sub(1);
                new_line = new.saturating_sub(1);
            }
            let prefix = text.as_bytes().first().copied();
            let source_line = source_lines[index];
            let base = match prefix {
                Some(b'+') if source_line => {
                    Style::default().fg(Color::Green).bg(Color::Rgb(10, 30, 10))
                }
                Some(b'-') if source_line => {
                    Style::default().fg(Color::Red).bg(Color::Rgb(35, 10, 10))
                }
                _ if text.starts_with("@@") => Style::default().fg(Color::Cyan),
                _ => Style::default(),
            };
            let map = if source_line {
                mapped.and_then(|(_, old, new)| {
                    if prefix == Some(b'-') {
                        old_offsets.get(old_line).map(|offset| (old, *offset))
                    } else {
                        new_offsets.get(new_line).map(|offset| (new, *offset))
                    }
                })
            } else {
                None
            };
            let mut spans = Vec::new();
            let mut column: usize = 0;
            for (byte, grapheme) in text.grapheme_indices(true) {
                let mut style = base;
                if source_line && byte > 0 {
                    if let Some((colors, offset)) = map
                        && let Some(Some(color)) = colors.get(offset + byte - 1)
                    {
                        style = style.fg(*color);
                    }
                    if emphasis[index].iter().any(|range| {
                        range.start < byte - 1 + grapheme.len() && range.end > byte - 1
                    }) {
                        style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
                    }
                }
                let rendered = if grapheme == "\t" {
                    " ".repeat(4 - column.saturating_sub(usize::from(source_line)) % 4)
                } else {
                    grapheme
                        .chars()
                        .map(|character| {
                            if character.is_control() {
                                character.escape_default().to_string()
                            } else {
                                character.to_string()
                            }
                        })
                        .collect::<String>()
                };
                column += rendered.width();
                spans.push(Span::styled(rendered, style));
            }
            lines.push(Line::from(spans));
            if source_line {
                if prefix != Some(b'+') {
                    old_line += 1;
                }
                if prefix != Some(b'-') {
                    new_line += 1;
                }
            }
        }
        let width = lines.iter().map(Line::width).max().unwrap_or(0);
        Self {
            lines,
            reason,
            width,
        }
    }

    pub fn visible(
        &self,
        top: usize,
        left: usize,
        height: usize,
        width: usize,
    ) -> Vec<Line<'static>> {
        self.lines
            .iter()
            .skip(top)
            .take(height)
            .map(|line| {
                let mut column = 0;
                let mut spans = Vec::new();
                for span in &line.spans {
                    for grapheme in span.content.graphemes(true) {
                        let size = grapheme.width();
                        let end = column + size;
                        if column >= left && end <= left + width {
                            spans.push(Span::styled(grapheme.to_string(), span.style));
                        } else if end > left && column < left + width {
                            let visible = end.min(left + width) - column.max(left);
                            spans.push(Span::styled(" ".repeat(visible), span.style));
                        }
                        column = end;
                    }
                }
                Line::from(spans)
            })
            .collect()
    }
}

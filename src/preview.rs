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

#[derive(Debug, Clone)]
pub struct Preview {
    pub lines: Vec<Line<'static>>,
    pub reason: Option<String>,
    pub title: Option<String>,
    pub search: Option<crate::search::BufferSearch>,
    pub width: usize,
    /// One-based old/new source rows, independent of syntax enrichment.
    pub locations: Vec<(Option<usize>, Option<usize>)>,
    /// Display cells to zero-based Unicode scalar columns; prefixes have no source.
    pub cells: Vec<Vec<Option<usize>>>,
}

type MappedColors<'a> = (
    &'a source::Sources,
    &'a [Option<Color>],
    &'a [Option<Color>],
);

fn syntax(side: &source::Side) -> anyhow::Result<Vec<Option<Color>>> {
    let path = std::path::Path::new(std::str::from_utf8(&side.path)?);
    let extension = path.extension().and_then(|ext| ext.to_str()).unwrap_or("");
    let (language, query) = match extension {
        "rs" => (
            tree_sitter_rust::LANGUAGE,
            tree_sitter_rust::HIGHLIGHTS_QUERY.to_owned(),
        ),
        "c" | "h" => (
            tree_sitter_c::LANGUAGE,
            tree_sitter_c::HIGHLIGHT_QUERY.to_owned(),
        ),
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => (
            tree_sitter_cpp::LANGUAGE,
            format!(
                "{}\n{}",
                tree_sitter_c::HIGHLIGHT_QUERY,
                tree_sitter_cpp::HIGHLIGHT_QUERY
            ),
        ),
        "go" => (
            tree_sitter_go::LANGUAGE,
            tree_sitter_go::HIGHLIGHTS_QUERY.to_owned(),
        ),
        "py" | "pyi" => (
            tree_sitter_python::LANGUAGE,
            tree_sitter_python::HIGHLIGHTS_QUERY.to_owned(),
        ),
        "zig" => (
            tree_sitter_zig::LANGUAGE,
            tree_sitter_zig::HIGHLIGHTS_QUERY.to_owned(),
        ),
        "sh" | "bash" => (
            tree_sitter_bash::LANGUAGE,
            tree_sitter_bash::HIGHLIGHT_QUERY.to_owned(),
        ),
        "yaml" | "yml" => (
            tree_sitter_yaml::LANGUAGE,
            tree_sitter_yaml::HIGHLIGHTS_QUERY.to_owned(),
        ),
        "json" => (
            tree_sitter_json::LANGUAGE,
            tree_sitter_json::HIGHLIGHTS_QUERY.to_owned(),
        ),
        _ => anyhow::bail!("syntax highlighting unavailable for this file type"),
    };
    anyhow::ensure!(
        side.content.len() <= 1024 * 1024,
        "source exceeds highlighting limit"
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
    let mut configuration =
        HighlightConfiguration::new(language.into(), extension, &query, "", "")?;
    configuration.configure(&names);
    let mut highlighter = Highlighter::new();
    let events =
        highlighter.highlight(&configuration, side.content.as_bytes(), None, None, |_| {
            None
        })?;
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
    if old.len().saturating_add(new.len()) > 8192 {
        return (
            std::iter::once(0..old.len()).collect(),
            std::iter::once(0..new.len()).collect(),
        );
    }
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
        {
            let mut preview =
                Self::compose(&crate::git::visible_bytes(bytes), None, None, false, false);
            preview.locations.fill((None, None));
            preview.cells.clear();
            preview
        }
    }

    pub fn item(cwd: &Path, item: &Item) -> Self {
        if item.patch.len() > 256 * 1024 {
            let mut preview = Self::original(&item.patch);
            preview.add_reason(
                "Large patch: bounded basic rendering; source navigation remains separate",
            );
            return preview;
        }
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
        let text = std::str::from_utf8(&item.patch);
        let escaped = text
            .is_err()
            .then(|| crate::git::visible_bytes(&item.patch));
        let mut preview = Self::compose(
            text.unwrap_or_else(|_| escaped.as_deref().expect("escaped invalid bytes")),
            mapped
                .as_ref()
                .map(|(sources, old, new)| (*sources, old.as_slice(), new.as_slice())),
            reason,
            text.is_ok(),
            false,
        );
        if text.is_err() {
            preview.locations.fill((None, None));
        }
        preview
    }

    fn compose(
        patch: &str,
        mapped: Option<MappedColors<'_>>,
        reason: Option<String>,
        escape_backslashes: bool,
        source_view: bool,
    ) -> Self {
        let mut rows = patch.split_terminator('\n');
        let raw: Vec<_> = rows.by_ref().take(20000).collect();
        let omitted_rows = rows.next().is_some();
        let mut omitted_columns = false;
        let mut remaining = (0, 0);
        let source_lines: Vec<_> = raw
            .iter()
            .map(|line| {
                if source_view {
                    return false;
                }
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
        let mut locations = Vec::new();
        let mut cells = Vec::new();
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
            locations.push(if source_line {
                (
                    if prefix != Some(b'+') {
                        Some(old_line + 1)
                    } else {
                        None
                    },
                    if prefix != Some(b'-') {
                        Some(new_line + 1)
                    } else {
                        None
                    },
                )
            } else {
                (None, None)
            });
            let mut spans = Vec::new();
            let graphemes: Vec<_> = if source_line {
                std::iter::once((0, &text[..1]))
                    .chain(
                        text[1..]
                            .grapheme_indices(true)
                            .map(|(byte, grapheme)| (byte + 1, grapheme)),
                    )
                    .take(4097)
                    .collect()
            } else {
                text.grapheme_indices(true).take(4097).collect()
            };
            omitted_columns |= graphemes.len() > 4096;
            let mut column: usize = 0;
            let mut scalar: usize = 0;
            let mut row_cells = Vec::new();
            let mut retained_graphemes = 0;
            for (byte, grapheme) in graphemes.into_iter().take(4096) {
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
                            if character == '\\' && escape_backslashes {
                                "\\\\".into()
                            } else if character.is_control() {
                                character.escape_default().to_string()
                            } else {
                                character.to_string()
                            }
                        })
                        .collect::<String>()
                };
                if source_view {
                    retained_graphemes += rendered.graphemes(true).count();
                    if retained_graphemes > 4096 {
                        omitted_columns = true;
                        break;
                    }
                }
                let source_column = if source_line && byte == 0 {
                    None
                } else {
                    Some(scalar)
                };
                row_cells.extend(std::iter::repeat_n(source_column, rendered.width()));
                if !(source_line && byte == 0) {
                    scalar += grapheme.chars().count();
                }
                column += rendered.width();
                spans.push(Span::styled(rendered, style));
            }
            cells.push(row_cells);
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
        let mut preview = Self {
            lines,
            reason,
            title: None,
            search: None,
            width,
            locations,
            cells,
        };
        if omitted_rows {
            preview.add_reason("Display omitted rows after 20000");
        }
        if omitted_columns {
            preview.add_reason("Display omitted columns after 4096 graphemes per row");
        }
        preview
    }

    fn add_reason(&mut self, note: &str) {
        if let Some(reason) = &mut self.reason {
            reason.push_str("; ");
            reason.push_str(note);
        } else {
            self.reason = Some(note.into());
        }
    }

    /// Refuse excluded coordinates before constructing a view. The retained
    /// prefix must never be presented as if it were an omitted target.
    pub fn target(
        source: &source::CapturedSource,
        line: usize,
        col: usize,
        len: usize,
    ) -> anyhow::Result<(Self, usize)> {
        anyhow::ensure!(
            line > 0 && line <= 20000,
            "Target unavailable: source row {line} exceeds the 20000-row display limit"
        );
        let text = std::str::from_utf8(&source.bytes)?;
        let row = text
            .split_terminator('\n')
            .nth(line - 1)
            .ok_or_else(|| anyhow::anyhow!("Target source row unavailable"))?;
        let target_end = col.saturating_add(len);
        let preview = Self::source(source);
        let cells = preview.cells.get(line - 1);
        let mut scalar = 0;
        let mut left = None;
        let mut retained = len > 0 && row.chars().count() >= target_end;
        // Validate the actual retained transform, including tab expansion and
        // graphemes whose combining scalars share their first scalar's cells.
        for grapheme in row.graphemes(true) {
            let end = scalar + grapheme.chars().count();
            if scalar < target_end && end > col {
                let cell =
                    cells.and_then(|cells| cells.iter().position(|cell| *cell == Some(scalar)));
                retained &= cell.is_some();
                if left.is_none() {
                    left = cell;
                }
            }
            scalar = end;
        }
        anyhow::ensure!(
            retained && left.is_some(),
            "Target unavailable: scalar column {} exceeds the retained 4096-grapheme row window",
            col + 1
        );
        Ok((preview, left.expect("validated retained target")))
    }

    pub fn source(source: &source::CapturedSource) -> Self {
        let mut preview = match std::str::from_utf8(&source.bytes) {
            Ok(text) => Self::compose(text, None, None, true, true),
            Err(_) => {
                let mut preview = Self::original(&source.bytes);
                preview.add_reason("Source positions unavailable: invalid UTF-8");
                return preview;
            }
        };
        if let Ok(text) = std::str::from_utf8(&source.bytes)
            && let Ok(colors) = syntax(&source::Side {
                path: source.path.clone(),
                content: text.into(),
            })
        {
            let starts = offsets(text);
            for (row, (line, raw)) in preview
                .lines
                .iter_mut()
                .zip(text.split_terminator('\n'))
                .enumerate()
            {
                let bytes: Vec<_> = raw.char_indices().map(|(byte, _)| byte).collect();
                let mut cell = 0;
                for span in &mut line.spans {
                    if let Some(Some(scalar)) =
                        preview.cells.get(row).and_then(|cells| cells.get(cell))
                        && let Some(byte) = bytes.get(*scalar)
                        && let Some(Some(color)) = colors.get(starts[row] + byte)
                    {
                        span.style = span.style.fg(*color);
                    }
                    cell += span.width();
                }
            }
        }
        preview.title = Some(format!(
            "Captured {} {}",
            source.side.label(),
            crate::git::visible_bytes(&source.path)
        ));
        preview.locations = (1..=preview.lines.len())
            .map(|line| match source.side {
                source::ComparedSide::Old => (Some(line), None),
                source::ComparedSide::New => (None, Some(line)),
            })
            .collect();
        preview.add_reason(&format!(
            "Captured {} {} · {}",
            source.side.label(),
            crate::git::visible_bytes(&source.path),
            &source.identity[..12]
        ));
        preview
    }

    /// Bounded hit testing uses the retained render transform, never a source guess.
    pub fn hit_test(&self, row: usize, column: usize) -> Option<usize> {
        self.cells.get(row)?.get(column).copied().flatten()
    }

    /// A clipped wide grapheme is rendered as padding and has no source target.
    pub fn visible_hit(
        &self,
        row: usize,
        column: usize,
        left: usize,
        width: usize,
    ) -> Option<usize> {
        if column < left || column >= left.saturating_add(width) {
            return None;
        }
        let mut cell = 0;
        for span in &self.lines.get(row)?.spans {
            for grapheme in span.content.graphemes(true) {
                let end = cell + grapheme.width();
                if column >= cell && column < end {
                    return (cell >= left && end <= left.saturating_add(width))
                        .then(|| self.hit_test(row, column))
                        .flatten();
                }
                cell = end;
            }
        }
        None
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
                        if column >= left.saturating_add(width) {
                            break;
                        }
                    }
                }
                Line::from(spans)
            })
            .enumerate()
            .map(|(row, mut line)| {
                if let Some(search) = &self.search {
                    let first = search.matches.partition_point(|m| m.row < top + row);
                    let matches = &search.matches
                        [first..search.matches.partition_point(|m| m.row <= top + row)];
                    let mut column = left;
                    for span in &mut line.spans {
                        let end = column + span.width();
                        if matches.iter().any(|m| column < m.end && end > m.start) {
                            span.style = span.style.bg(Color::Rgb(80, 65, 0));
                        }
                        column = end;
                    }
                }
                line
            })
            .collect()
    }
}

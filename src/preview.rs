use crate::{
    git::Item,
    source,
    theme::{Base, Role, Theme},
};
use ratatui::text::{Line, Span};
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
    removed: Option<Vec<bool>>,
    ordinary: Option<Box<Preview>>,
    /// Theme-independent meaning of every span, parallel to `lines`.
    roles: Vec<Vec<Role>>,
    theme: Theme,
}

type Captures = Vec<Option<u8>>;
type MappedCaptures<'a> = (&'a source::Sources, &'a [Option<u8>], &'a [Option<u8>]);

/// Per-byte capture indices into the shared theme capture table.
fn syntax(side: &source::Side) -> anyhow::Result<Captures> {
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
        "toml" => (
            tree_sitter_toml_ng::LANGUAGE,
            // Upstream captures an entire bare-key pair as a property and quoted
            // keys as strings. Restrict key coloring to the key nodes themselves.
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY
                .replace("(bare_key) @type", "(bare_key) @property")
                .replace("(quoted_key) @string", "(quoted_key) @property")
                .replace("(pair\n  (bare_key)) @property", ""),
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
    let mut names = vec![
        "comment", "string", "keyword", "function", "type", "constant", "number", "operator",
        "variable",
    ];
    if matches!(extension, "yaml" | "yml") {
        names.extend(["property", "boolean", "label", "attribute", "punctuation"]);
    }
    if extension == "toml" {
        names.extend(["property", "boolean", "string.special", "punctuation"]);
    }
    let captures: Vec<_> = names
        .iter()
        .map(|name| crate::theme::capture(name))
        .collect();
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
            HighlightEvent::HighlightStart(highlight) => stack.push(captures[highlight.0]),
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                bytes[start..end].fill(stack.last().copied().flatten())
            }
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
    pub fn original(bytes: &[u8], theme: &Theme) -> Self {
        let mut preview = Self::compose(
            &crate::git::visible_bytes(bytes),
            None,
            None,
            false,
            false,
            theme,
        );
        preview.locations.fill((None, None));
        preview.cells.clear();
        preview.removed = None;
        preview
    }

    pub fn item(cwd: &Path, item: &Item, theme: &Theme) -> Self {
        if item.patch.len() > 256 * 1024 {
            let mut preview = Self::compose(
                &crate::git::visible_bytes(&item.patch),
                None,
                None,
                false,
                false,
                theme,
            );
            preview.locations.fill((None, None));
            preview.cells.clear();
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
            theme,
        );
        if text.is_err() {
            preview.locations.fill((None, None));
        }
        preview
    }

    fn compose(
        patch: &str,
        mapped: Option<MappedCaptures<'_>>,
        reason: Option<String>,
        escape_backslashes: bool,
        source_view: bool,
        theme: &Theme,
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
        let mut roles = Vec::new();
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
                Some(b'+') if source_line => Base::Added,
                Some(b'-') if source_line => Base::Removed,
                _ if text.starts_with("@@") => Base::Hunk,
                _ => Base::Plain,
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
            let mut span_roles = Vec::new();
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
                let mut role = Role {
                    base,
                    ..Role::default()
                };
                if source_line && byte > 0 {
                    if let Some((captures, offset)) = map {
                        role.capture = captures.get(offset + byte - 1).copied().flatten();
                    }
                    role.emphasis = emphasis[index].iter().any(|range| {
                        range.start < byte - 1 + grapheme.len() && range.end > byte - 1
                    });
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
                spans.push(Span::raw(rendered));
                span_roles.push(role);
            }
            cells.push(row_cells);
            lines.push(Line::from(spans));
            roles.push(span_roles);
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
            removed: (!source_view).then(|| {
                raw.iter()
                    .zip(&source_lines)
                    .map(|(line, source)| *source && line.starts_with('-'))
                    .collect()
            }),
            ordinary: None,
            roles,
            theme: *theme,
        };
        preview.restyle(theme);
        if omitted_rows {
            preview.add_reason("Display omitted rows after 20000");
        }
        if omitted_columns {
            preview.add_reason("Display omitted columns after 4096 graphemes per row");
        }
        preview
    }

    /// Project only classified hunk removals from this captured rendering.
    /// Returns the corresponding cursor and viewport rows without loading Git/source.
    pub fn toggle_after_changes(&mut self, cursor: usize, top: usize) -> (usize, usize) {
        let search = self.search.clone();
        let mapped = if let Some(ordinary) = self.ordinary.take() {
            let retained: Vec<_> = (0..ordinary.lines.len())
                .filter(|row| {
                    !ordinary
                        .removed
                        .as_ref()
                        .is_some_and(|rows| rows.get(*row) == Some(&true))
                })
                .collect();
            let map = |row: usize| retained.get(row).copied().unwrap_or(0);
            let mapped = (map(cursor), map(top));
            *self = *ordinary;
            mapped
        } else if let Some(removed) = &self.removed {
            let retained: Vec<_> = (0..self.lines.len())
                .filter(|row| removed.get(*row) != Some(&true))
                .collect();
            let map = |row: usize| {
                retained
                    .partition_point(|index| *index < row)
                    .min(retained.len().saturating_sub(1))
            };
            let mapped = (map(cursor), map(top));
            let ordinary = self.clone();
            (self.lines, self.roles) = retained
                .iter()
                .map(|row| {
                    let mut line = ordinary.lines[*row].clone();
                    let mut roles = ordinary.roles.get(*row).cloned().unwrap_or_default();
                    if row
                        .checked_sub(1)
                        .is_some_and(|previous| removed.get(previous) == Some(&true))
                        && line.to_string().trim_start_matches('\\') == " No newline at end of file"
                    {
                        roles.resize(line.spans.len(), Role::default());
                        line.spans.push(Span::styled(
                            " (removed old-side line)",
                            self.theme.style(Role::default()),
                        ));
                        roles.push(Role::default());
                    }
                    (line, roles)
                })
                .unzip();
            self.locations = retained
                .iter()
                .map(|row| {
                    ordinary
                        .locations
                        .get(*row)
                        .copied()
                        .unwrap_or((None, None))
                })
                .collect();
            self.cells = retained
                .iter()
                .map(|row| ordinary.cells.get(*row).cloned().unwrap_or_default())
                .collect();
            for (line, cells) in self.lines.iter().zip(&mut self.cells) {
                cells.resize(line.width(), None);
            }
            self.width = self.lines.iter().map(Line::width).max().unwrap_or(0);
            self.title = Some("After changes · captured diff context".into());
            self.ordinary = Some(Box::new(ordinary));
            mapped
        } else {
            return (cursor, top);
        };
        self.search = search.map(|mut search| {
            search.refresh(&self.lines);
            search
        });
        mapped
    }

    /// Re-derive every span style, including the ordinary view kept behind
    /// the after-changes projection, without touching content or positions.
    pub fn restyle(&mut self, theme: &Theme) {
        self.theme = *theme;
        for (line, roles) in self.lines.iter_mut().zip(&self.roles) {
            for (span, role) in line.spans.iter_mut().zip(roles) {
                span.style = theme.style(*role);
            }
        }
        if let Some(ordinary) = &mut self.ordinary {
            ordinary.restyle(theme);
        }
    }

    /// Captured patch row index behind each displayed row, in either the
    /// ordinary or the after-changes view. Appended rows (Git stderr) have none.
    pub fn patch_rows(&self) -> Vec<Option<usize>> {
        let ordinary = self.ordinary.as_deref().unwrap_or(self);
        let total = ordinary.locations.len();
        let rows: Vec<usize> = if self.ordinary.is_some() {
            (0..ordinary.lines.len())
                .filter(|row| {
                    !ordinary
                        .removed
                        .as_ref()
                        .is_some_and(|rows| rows.get(*row) == Some(&true))
                })
                .collect()
        } else {
            (0..self.lines.len()).collect()
        };
        rows.into_iter()
            .map(|row| (row < total).then_some(row))
            .collect()
    }

    /// Append another rendering's rows below this one.
    pub fn append(&mut self, other: Preview) {
        self.width = self.width.max(other.width);
        self.roles.resize(self.lines.len(), Vec::new());
        self.lines.extend(other.lines);
        self.roles.extend(other.roles);
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
        theme: &Theme,
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
        let preview = Self::source(source, theme);
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

    pub fn source(source: &source::CapturedSource, theme: &Theme) -> Self {
        let mut preview = match std::str::from_utf8(&source.bytes) {
            Ok(text) => Self::compose(text, None, None, true, true, theme),
            Err(_) => {
                let mut preview = Self::original(&source.bytes, theme);
                preview.add_reason("Source positions unavailable: invalid UTF-8");
                return preview;
            }
        };
        if let Ok(text) = std::str::from_utf8(&source.bytes)
            && let Ok(captures) = syntax(&source::Side {
                path: source.path.clone(),
                content: text.into(),
            })
        {
            let starts = offsets(text);
            for (row, ((line, roles), raw)) in preview
                .lines
                .iter()
                .zip(&mut preview.roles)
                .zip(text.split_terminator('\n'))
                .enumerate()
            {
                let bytes: Vec<_> = raw.char_indices().map(|(byte, _)| byte).collect();
                let mut cell = 0;
                for (span, role) in line.spans.iter().zip(roles) {
                    if let Some(Some(scalar)) =
                        preview.cells.get(row).and_then(|cells| cells.get(cell))
                        && let Some(byte) = bytes.get(*scalar)
                        && let Some(Some(capture)) = captures.get(starts[row] + byte)
                    {
                        role.capture = Some(*capture);
                    }
                    cell += span.width();
                }
            }
            preview.restyle(theme);
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
                            span.style = span.style.bg(self.theme.search_bg);
                            if let Some(fg) = self.theme.search_fg {
                                span.style = span.style.fg(fg);
                            }
                        }
                        column = end;
                    }
                }
                line
            })
            .collect()
    }
}

use crate::{
    git::Item,
    source,
    theme::{Base, Role, Theme},
};
use ratatui::text::{Line, Span};
use similar::{ChangeTag, TextDiff};
use std::{cell::RefCell, collections::HashMap, ops::Range, path::Path, rc::Rc};
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
    /// Per-width cumulative row counts; the logical text remains authoritative.
    wrap_index: RefCell<Option<(usize, Rc<Vec<usize>>)>>,
}

type Captures = Vec<Option<u8>>;
type MappedCaptures<'a> = (&'a source::Sources, &'a [Option<u8>], &'a [Option<u8>]);

thread_local! {
    // Compiling the queries is expensive, especially when each rustdoc fence
    // is highlighted separately. Configurations are immutable after setup.
    static HIGHLIGHT_CONFIGS: RefCell<HashMap<String, Rc<HighlightConfiguration>>> = RefCell::new(HashMap::new());
}

/// Per-byte capture indices into the shared theme capture table.
fn syntax(side: &source::Side) -> anyhow::Result<Captures> {
    highlight(side, true)
}

fn highlight(side: &source::Side, embedded: bool) -> anyhow::Result<Captures> {
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
        // Tree-sitter stays authoritative; syntect covers the remaining languages.
        _ => return crate::syntect::highlight(path, &side.content),
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
    let cached = HIGHLIGHT_CONFIGS.with(|configs| configs.borrow().get(extension).cloned());
    let configuration = if let Some(configuration) = cached {
        configuration
    } else {
        let mut configuration =
            HighlightConfiguration::new(language.into(), extension, &query, "", "")?;
        configuration.configure(&names);
        let configuration = Rc::new(configuration);
        HIGHLIGHT_CONFIGS.with(|configs| {
            configs
                .borrow_mut()
                .insert(extension.to_owned(), configuration.clone());
        });
        configuration
    };
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
    if embedded && matches!(extension, "rs" | "go") {
        overlay_embedded(&side.content, extension, &mut bytes);
    }
    Ok(bytes)
}

// Extracted bytes retain a direct mapping to the captured source. Newlines inserted
// between comment fragments have no source mapping.
fn apply_embedded(text: &str, map: &[Option<usize>], extension: &str, bytes: &mut Captures) {
    if text.len() > 256 * 1024 {
        return;
    }
    let side = source::Side {
        path: format!("embedded.{extension}").into_bytes(),
        content: text.to_owned(),
    };
    if let Ok(captures) = highlight(&side, false) {
        for (capture, original) in captures.into_iter().zip(map) {
            if let (Some(capture), Some(original)) = (capture, original) {
                bytes[*original] = Some(capture);
            }
        }
    }
}

fn append_fragment(
    text: &str,
    range: Range<usize>,
    extracted: &mut String,
    map: &mut Vec<Option<usize>>,
) {
    extracted.push_str(&text[range.clone()]);
    map.extend(range.map(Some));
    extracted.push('\n');
    map.push(None);
}

fn rust_docs(text: &str, comments: &[Range<usize>], bytes: &mut Captures, budget: &mut usize) {
    let mut lines = Vec::new();
    for range in comments {
        let comment = &text[range.clone()];
        let block = comment.starts_with("/*");
        let start = range.start + 3;
        let end = if block && comment.ends_with("*/") {
            range.end - 2
        } else {
            range.end
        };
        if start > end {
            continue;
        }
        let mut offset = start;
        for line in text[start..end].split_inclusive('\n') {
            let end = offset + line.trim_end_matches(['\r', '\n']).len();
            let mut content = offset;
            if block {
                while content < end && matches!(text.as_bytes()[content], b' ' | b'\t') {
                    content += 1;
                }
                if content < end && text.as_bytes().get(content) == Some(&b'*') {
                    content += 1;
                }
            }
            lines.push(content..end);
            offset += line.len();
        }
    }
    let indentation = |line: &str| {
        let mut columns: usize = 0;
        for byte in line.bytes() {
            match byte {
                b' ' => columns += 1,
                b'\t' => columns += 4 - columns % 4,
                _ => break,
            }
        }
        columns
    };
    // Rustdoc removes the shared indentation of nonblank documentation lines
    // before handing their content to Markdown.
    let common_indent = lines
        .iter()
        .map(|range| &text[range.clone()])
        .filter(|line| !line.trim().is_empty())
        .map(indentation)
        .min()
        .unwrap_or(0);
    let mut open: Option<(u8, usize, bool, usize)> = None;
    for (index, range) in lines.iter().enumerate() {
        let raw = &text[range.clone()];
        if indentation(raw).saturating_sub(common_indent) > 3 {
            continue;
        }
        let line = raw.trim();
        let marker = line.as_bytes().first().copied();
        let count = marker.map_or(0, |m| line.bytes().take_while(|b| *b == m).count());
        if !matches!(marker, Some(b'`' | b'~')) || count < 3 {
            continue;
        }
        if let Some((m, n, supported, first)) = open {
            if marker == Some(m) && count >= n && line[count..].trim().is_empty() {
                if supported && *budget > 0 {
                    *budget -= 1;
                    let mut extracted = String::new();
                    let mut map = Vec::new();
                    for content in &lines[first..index] {
                        // Rustdoc strips a leading `# ` from hidden setup lines before
                        // parsing the example. Keep that marker in the comment style.
                        let line = &text[content.clone()];
                        let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
                        let hidden = &line[indent..];
                        if hidden == "#" || hidden.starts_with("# ") || hidden.starts_with("#\t") {
                            extracted.push_str(&line[..indent]);
                            map.extend((content.start..content.start + indent).map(Some));
                            let skip = if hidden.len() == 1 { 1 } else { 2 };
                            append_fragment(
                                text,
                                content.start + indent + skip..content.end,
                                &mut extracted,
                                &mut map,
                            );
                        } else {
                            append_fragment(text, content.clone(), &mut extracted, &mut map);
                        }
                    }
                    apply_embedded(&extracted, &map, "rs", bytes);
                }
                open = None;
            }
        } else {
            let info = line[count..].trim();
            let compile_fail = info
                .split([',', ' ', '\t'])
                .any(|attribute| attribute == "compile_fail");
            let supported = info.is_empty()
                || info
                    .split([',', ' ', '\t'])
                    .filter(|s| !s.is_empty())
                    .all(|s| {
                        matches!(
                            s,
                            "rust"
                                | "compile_fail"
                                | "no_run"
                                | "should_panic"
                                | "ignore"
                                | "allow_fail"
                                | "test_harness"
                                | "edition2015"
                                | "edition2018"
                                | "edition2021"
                                | "edition2024"
                        ) || (compile_fail
                            && s.len() == 5
                            && s.starts_with('E')
                            && s.as_bytes()[1..].iter().all(u8::is_ascii_digit))
                    });
            open = Some((marker.unwrap(), count, supported, index + 1));
        }
    }
}

fn overlay_embedded(text: &str, extension: &str, bytes: &mut Captures) {
    let language = if extension == "rs" {
        tree_sitter_rust::LANGUAGE
    } else {
        tree_sitter_go::LANGUAGE
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language.into()).is_err() {
        return;
    }
    let Some(tree) = parser.parse(text, None) else {
        return;
    };
    let mut nodes = vec![tree.root_node()];
    let mut comments = Vec::new();
    let mut imports = Vec::new();
    while let Some(node) = nodes.pop() {
        if extension == "rs" && matches!(node.kind(), "line_comment" | "block_comment") {
            if node.child_by_field_name("doc").is_some() {
                comments.push(node.byte_range());
            }
            continue;
        }
        if extension == "go" && node.kind() == "comment" {
            comments.push(node.byte_range());
            continue;
        }
        if extension == "go"
            && node.kind() == "import_spec"
            && node.child_by_field_name("name").is_none()
            && node
                .child_by_field_name("path")
                .is_some_and(|p| &text[p.byte_range()] == "\"C\"")
            && let Some(parent) = node.parent()
        {
            if parent.kind() == "import_declaration" {
                imports.push((parent.start_byte(), None));
            } else if parent.kind() == "import_spec_list" {
                let mut cursor = parent.walk();
                let count = parent
                    .named_children(&mut cursor)
                    .filter(|n| n.kind() == "import_spec")
                    .count();
                let fallback = (count == 1)
                    .then(|| parent.parent().map(|n| n.start_byte()))
                    .flatten();
                imports.push((node.start_byte(), fallback));
            }
        }
        let mut cursor = node.walk();
        nodes.extend(node.named_children(&mut cursor));
    }
    comments.sort_by_key(|r| r.start);
    if extension == "rs" {
        let mut group: Vec<Range<usize>> = Vec::new();
        let mut budget = 128;
        for range in comments {
            let adjacent = group.last().is_some_and(|previous| {
                text[previous.clone()].starts_with("//")
                    && text[range.clone()].starts_with("//")
                    && {
                        let gap = &text[previous.end..range.start];
                        gap.bytes().filter(|b| *b == b'\n').count()
                            + usize::from(text[previous.clone()].ends_with('\n'))
                            == 1
                            && gap.trim().is_empty()
                    }
            });
            if !adjacent && !group.is_empty() {
                rust_docs(text, &group, bytes, &mut budget);
                group.clear();
            }
            group.push(range);
        }
        rust_docs(text, &group, bytes, &mut budget);
    } else {
        let mut standalone = Vec::with_capacity(comments.len());
        for (index, range) in comments.iter().enumerate() {
            let line_start = text[..range.start].rfind('\n').map_or(0, |i| i + 1);
            let previous = index.checked_sub(1).map(|i| &comments[i]);
            let begins_line = text[line_start..range.start].trim().is_empty()
                || previous.is_some_and(|prior| {
                    prior.end >= line_start
                        && text[prior.end..range.start].trim().is_empty()
                        && standalone[index - 1]
                });
            standalone.push(begins_line);
        }
        for (import, fallback) in imports.into_iter().take(128) {
            let preceding = |import| {
                let mut end = import;
                let mut group = Vec::new();
                let count = comments.partition_point(|range| range.end <= import);
                for index in (0..count).rev() {
                    let range = &comments[index];
                    let gap = &text[range.end..end];
                    let newlines = gap.bytes().filter(|b| *b == b'\n').count();
                    if !gap.trim().is_empty()
                        || newlines > 1
                        || (end == import && newlines != 1)
                        || !standalone[index]
                    {
                        break;
                    }
                    group.push(range.clone());
                    end = range.start;
                }
                group.reverse();
                group
            };
            let mut group = preceding(import);
            if group.is_empty()
                && let Some(fallback) = fallback
            {
                group = preceding(fallback);
            }
            let mut extracted = String::new();
            let mut map = Vec::new();
            for range in group {
                let comment = &text[range.clone()];
                let start = range.start + 2;
                let end = if comment.starts_with("/*") && comment.ends_with("*/") {
                    range.end - 2
                } else {
                    range.end
                };
                if start <= end {
                    append_fragment(text, start..end, &mut extracted, &mut map);
                }
            }
            apply_embedded(&extracted, &map, "c", bytes);
        }
    }
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
            if item.old_mode == "000000" {
                preview.clear_new_file_background(theme);
            }
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
        if item.old_mode == "000000" {
            preview.clear_new_file_background(theme);
        }
        preview
    }

    fn clear_new_file_background(&mut self, theme: &Theme) {
        for role in self.roles.iter_mut().flatten() {
            role.base = match role.base {
                Base::Added => Base::Plain,
                Base::AddedMarker => Base::NewFileMarker,
                other => other,
            };
        }
        self.restyle(theme);
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
                    base: if base == Base::Added && byte == 0 {
                        Base::AddedMarker
                    } else {
                        base
                    },
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
            wrap_index: RefCell::new(None),
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
    pub fn after_changes(&self) -> bool {
        self.ordinary.is_some()
    }

    pub fn toggle_after_changes(&mut self, cursor: usize, top: usize) -> (usize, usize) {
        self.wrap_index.get_mut().take();
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
        self.wrap_index.get_mut().take();
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

    /// Cell ranges for each visual row, retaining grapheme boundaries.
    fn segments(&self, row: usize, width: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        let mut graphemes = self.lines[row]
            .spans
            .iter()
            .flat_map(|s| s.content.graphemes(true))
            .peekable();
        let mut cell = 0usize;
        let mut first = true;
        let width = width.max(1);
        std::iter::from_fn(move || {
            if !first && graphemes.peek().is_none() {
                return None;
            }
            first = false;
            let start = cell;
            while let Some(g) = graphemes.peek() {
                let size = g.width();
                if cell > start && cell.saturating_add(size) > start.saturating_add(width) {
                    break;
                }
                cell = cell.saturating_add(size);
                graphemes.next();
                if cell.saturating_sub(start) >= width {
                    break;
                }
            }
            Some((start, cell))
        })
    }

    fn wrap_index(&self, width: usize) -> Rc<Vec<usize>> {
        let width = width.max(1);
        if let Some((cached_width, rows)) = &*self.wrap_index.borrow()
            && *cached_width == width
        {
            return Rc::clone(rows);
        }
        let mut offsets = Vec::with_capacity(self.lines.len() + 1);
        offsets.push(0usize);
        for row in 0..self.lines.len() {
            offsets.push(offsets[row].saturating_add(self.segments(row, width).count()));
        }
        let offsets = Rc::new(offsets);
        *self.wrap_index.borrow_mut() = Some((width, Rc::clone(&offsets)));
        offsets
    }

    pub fn visual_rows(&self, width: usize) -> usize {
        *self.wrap_index(width).last().unwrap_or(&0)
    }

    pub fn visual_position(&self, row: usize, column: usize, width: usize) -> (usize, usize) {
        let row = row.min(self.lines.len().saturating_sub(1));
        if self.lines.is_empty() {
            return (0, 0);
        }
        let before = self.wrap_index(width)[row];
        for (index, (start, end)) in self.segments(row, width).enumerate() {
            if column < end || start == end {
                return (
                    before + index,
                    column.saturating_sub(start).min(width.saturating_sub(1)),
                );
            }
        }
        (before, 0)
    }

    pub fn visual_segment(&self, visual: usize, width: usize) -> Option<(usize, usize, usize)> {
        let offsets = self.wrap_index(width);
        if visual >= *offsets.last()? {
            return None;
        }
        let row = offsets.partition_point(|offset| *offset <= visual) - 1;
        self.segments(row, width)
            .nth(visual - offsets[row])
            .map(|(start, end)| (row, start, end))
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

#[cfg(test)]
mod embedded_tests {
    use super::*;

    #[test]
    fn many_doc_tests_keep_independent_highlighting() {
        let mut text = String::new();
        for i in 0..100 {
            text.push_str(&format!("/// example {i}\n/// ```compile_fail,E0080\n/// use std::fmt;\n/// struct Mine;\n/// impl Mine {{\n///     fn f() -> u64 {{ 42 }}\n/// }}\n/// ```\nfn f{i}() {{}}\n"));
        }
        let side = source::Side {
            path: b"x.rs".to_vec(),
            content: text,
        };
        let captures = syntax(&side).unwrap();
        let comment = captures[side.content.find("/// example").unwrap()];
        let mut checked = 0;
        for (offset, _) in side.content.match_indices("///     fn f() -> u64 { 42 }") {
            assert_ne!(captures[offset + "///     ".len()], comment);
            assert_ne!(captures[offset + "///     fn f() -> u64 { ".len()], comment);
            checked += 1;
        }
        assert_eq!(checked, 100);
    }

    fn captures(path: &str, text: &str) -> Captures {
        syntax(&source::Side {
            path: path.as_bytes().to_vec(),
            content: text.into(),
        })
        .unwrap()
    }

    fn at(captures: &Captures, text: &str, needle: &str) -> Option<u8> {
        captures[text.find(needle).unwrap()]
    }

    #[test]
    fn rust_doc_fences_are_projected_without_affecting_other_comments() {
        let text = "/// prose café\n/// ```compile_fail\n/// let x = 42;\n/// ```\n/// ```python\n/// let y = 3;\n/// ```\nfn f() {}\n/* ordinary let z = 9; */\n/**\n * ```rust\n * let n = 5;\n * ```\n */\nfn g() {}\n/// ```\n/// let unclosed = 2;\n";
        let c = captures("a.rs", text);
        let comment = at(&c, text, "prose");
        assert_ne!(at(&c, text, "let x"), comment);
        assert_ne!(at(&c, text, "let n"), comment);
        for needle in ["```compile_fail", "let y", "ordinary", "let unclosed"] {
            assert_eq!(at(&c, text, needle), comment, "{needle}");
        }
    }

    #[test]
    fn rustdoc_compile_fail_error_codes_are_highlighted() {
        let text = "/// prose\n/// ```compile_fail,E0080\n/// use yanet_sys::dp::Object;\n/// const TYPE: &str = \"mine\";\n/// ```\n/// ```rust,compile_fail,E0308,E0080\n/// let valid = 1;\n/// ```\n/// ```\n/// let implicit = 4;\n/// ```\n/// ```python,compile_fail,E0080\n/// let foreign = 2;\n/// ```\n/// ```compile_fail,E008x\n/// let invalid = 3;\n/// ```\nfn f() {}\n";
        let c = captures("a.rs", text);
        let comment = at(&c, text, "prose");
        for needle in ["use yanet", "const TYPE", "let valid", "let implicit"] {
            assert_ne!(at(&c, text, needle), comment, "{needle}");
        }
        for needle in ["```compile_fail,E0080", "let foreign", "let invalid"] {
            assert_eq!(at(&c, text, needle), comment, "{needle}");
        }
    }

    #[test]
    fn rustdoc_fences_respect_normalized_markdown_indentation() {
        let text = "/// prose\n///     ```rust\n///     let literal = 1;\n///     ```\n///    ```rust\n///    let valid = 2;\n///    ```\nfn f() {}\n";
        let c = captures("a.rs", text);
        let comment = at(&c, text, "prose");
        assert_eq!(at(&c, text, "let literal"), comment);
        assert_ne!(at(&c, text, "let valid"), comment);

        let indented = "///    ```rust\n///    let example = 1;\n///    ```\npub fn sample() {}\n";
        let c = captures("a.rs", indented);
        assert_ne!(at(&c, indented, "let example"), at(&c, indented, "///"));
    }

    #[test]
    fn cgo_adjacent_block_comments_are_a_single_preamble() {
        let text = "package p\n/* int first = 1; */ /* int second = 2; */\nimport \"C\"\nimport (\n \"fmt\"\n /* int grouped_first = 3; */ /* int grouped_second = 4; */\n \"C\"\n)\nimport \"io\" /* int trailing_first = 5; */ /* int trailing_second = 6; */\nimport \"C\"\n";
        let c = captures("a.go", text);
        let comment = at(&c, text, "/* int first");
        for token in [
            "int first",
            "int second",
            "int grouped_first",
            "int grouped_second",
        ] {
            assert_ne!(at(&c, text, token), comment, "{token}");
        }
        for token in ["int trailing_first", "int trailing_second"] {
            assert_eq!(at(&c, text, token), comment, "{token}");
        }
    }

    #[test]
    fn hidden_rustdoc_setup_preserves_following_highlighting() {
        let text =
            "/// ```compile_fail\n/// # use std::fmt;\n/// let value = 42;\n/// ```\nfn f() {}\n";
        let c = captures("a.rs", text);
        let comment = at(&c, text, "/// #");
        assert_ne!(at(&c, text, "use std"), comment);
        assert_ne!(at(&c, text, "let value"), comment);
        assert_ne!(at(&c, text, "42"), comment);
        assert_eq!(at(&c, text, "# use"), comment);
    }

    #[test]
    fn cgo_line_comments_and_resource_fallback() {
        let text = "package p\n// int line_value = 7;\nimport \"C\"\n";
        let c = captures("a.go", text);
        assert_ne!(at(&c, text, "int line_value"), at(&c, text, "//"));
        let huge = format!(
            "/// ```rust\n/// let value = 7;{}\n/// ```\nfn f() {{}}\n",
            " ".repeat(256 * 1024)
        );
        let c = captures("a.rs", &huge);
        assert_eq!(at(&c, &huge, "let value"), at(&c, &huge, "///"));
    }

    #[test]
    fn cgo_grouped_imports_use_only_their_own_standalone_comments() {
        let text = "package p\nimport (\n \"fmt\"\n // int grouped = 42;\n \"C\"\n \"os\"\n)\nimport (\n \"strings\" // int trailing_grouped = 5;\n \"C\"\n)\nimport \"io\" // int trailing_direct = 6;\nimport \"C\"\n";
        let c = captures("a.go", text);
        let comment = at(&c, text, "// int grouped");
        assert_ne!(at(&c, text, "int grouped"), comment);
        assert_ne!(at(&c, text, "42"), comment);
        assert_eq!(at(&c, text, "int trailing_grouped"), comment);
        assert_eq!(at(&c, text, "int trailing_direct"), comment);
    }

    #[test]
    fn cgo_grouped_single_import_accepts_declaration_doc_fallback() {
        let text = "package p\n// int fallback = 42;\nimport (\n \"C\"\n)\n";
        let c = captures("a.go", text);
        assert_ne!(at(&c, text, "int fallback"), at(&c, text, "//"));
    }

    #[test]
    fn cgo_requires_a_real_adjacent_import() {
        let text = "package p\n/*\nint x = 42;\n*/\nimport \"C\"\n// int unrelated = 8;\nimport \"fmt\"\nvar s = `/* int fake = 3; */ import \"C\"`\n";
        let c = captures("a.go", text);
        assert_ne!(at(&c, text, "int x"), at(&c, text, "int unrelated"));
        assert_eq!(at(&c, text, "int unrelated"), at(&c, text, "// int"));
        assert_ne!(at(&c, text, "int fake"), at(&c, text, "int x"));
    }
}

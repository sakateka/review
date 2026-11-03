use crate::app::{App, Focus};
use anyhow::Result;
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, KeyModifiers,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Row, Table, Wrap},
};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

fn clip_name(name: &str, width: usize) -> String {
    if name.width() <= width {
        return name.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let left_budget = width.saturating_sub(1).div_ceil(2);
    let right_budget = width.saturating_sub(1) / 2;
    fn take(parts: Vec<&str>, budget: usize) -> Vec<&str> {
        let mut used = 0;
        parts
            .into_iter()
            .take_while(|part| {
                used += part.width();
                used <= budget
            })
            .collect::<Vec<_>>()
    }
    let left = take(name.graphemes(true).collect(), left_budget).concat();
    let mut right = take(name.graphemes(true).rev().collect(), right_budget);
    right.reverse();
    format!("{left}…{}", right.concat())
}

fn file_row(
    item: &crate::git::Item,
    state: (bool, bool),
    width: usize,
    show_stats: bool,
    comment: Option<bool>,
    note: bool,
    theme: &crate::theme::Theme,
) -> Line<'static> {
    use crate::git::Statistics;
    let (reviewed, selected) = state;
    let name = item.name();
    let show_comment = comment.is_some() && width >= 4;
    let show_note = note && width >= 4 + usize::from(show_comment) * 2;
    let icon_cells = 2 * (usize::from(show_comment) + usize::from(show_note));
    let icon_width = icon_cells + usize::from(icon_cells > 0 && width > icon_cells + 2);
    let available = width.saturating_sub(2 + icon_width);
    let statistics = item.statistics();
    let label = match statistics {
        Statistics::Text {
            additions,
            deletions,
        } => (additions + deletions).to_string(),
        Statistics::Binary => "binary".into(),
        Statistics::Unavailable => "unavailable".into(),
    };
    // Preserve the label and markers first; long names and graphs yield space.
    let name_minimum = name.width().min(12);
    let show_stats = show_stats && available >= usize::from(!name.is_empty()) + 2 + label.width();
    let graph_width = if show_stats {
        available
            .saturating_sub(name_minimum + 3 + label.width())
            .min(12)
    } else {
        0
    };
    let (plus, minus) = match statistics {
        Statistics::Text {
            additions,
            deletions,
        } if additions + deletions > 0 => {
            let cells = graph_width.min(additions + deletions);
            let plus = if additions == 0 {
                0
            } else if deletions == 0 {
                cells
            } else if cells >= 2 {
                (cells as u128 * additions as u128 / (additions + deletions) as u128)
                    .clamp(1, (cells - 1) as u128) as usize
            } else {
                usize::from(additions >= deletions) * cells
            };
            (plus, cells - plus)
        }
        _ => (0, 0),
    };
    let suffix_width = if show_stats {
        2 + label.width()
            + if plus + minus > 0 {
                1 + plus + minus
            } else {
                0
            }
    } else {
        0
    };
    let name_width = available.saturating_sub(suffix_width);
    let displayed_name = if name.width() <= name_width {
        name
    } else if item.old_path != item.new_path && name_width >= 9 {
        let old = crate::git::visible_name(&item.old_path);
        let new = crate::git::visible_name(&item.new_path);
        let old_width = (name_width - 3) / 2;
        format!(
            "{} → {}",
            clip_name(&old, old_width),
            clip_name(&new, name_width - 3 - old_width)
        )
    } else {
        clip_name(&name, name_width)
    };
    let padding = available.saturating_sub(displayed_name.width() + suffix_width);
    let mut spans = vec![
        Span::styled(
            if reviewed { "+ " } else { "- " },
            Style::default().fg(if reviewed {
                theme.reviewed_mark
            } else {
                theme.pending_mark
            }),
        ),
        Span::styled(
            displayed_name,
            Style::default()
                .fg(if reviewed {
                    theme.reviewed_name
                } else {
                    theme.pending_name
                })
                .patch(if selected {
                    theme.selection
                } else {
                    Style::default()
                }),
        ),
    ];
    spans.push(Span::raw(" ".repeat(padding)));
    if icon_width > 0 {
        if icon_width > icon_cells {
            spans.push(Span::raw(" "));
        }
        if show_note {
            spans.push(Span::styled("📝", Style::default().fg(theme.comment)));
        }
        if show_comment {
            spans.push(Span::styled("💬", comment_style(comment.unwrap(), theme)));
        }
    }
    if show_stats {
        spans.push(Span::raw(format!("  {label}")));
        if plus + minus > 0 {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                "+".repeat(plus),
                Style::default().fg(theme.stat_plus),
            ));
            spans.push(Span::styled(
                "-".repeat(minus),
                Style::default().fg(theme.stat_minus),
            ));
        }
    }
    Line::from(spans)
}

fn comment_style(outdated: bool, theme: &crate::theme::Theme) -> Style {
    if outdated {
        Style::default()
            .fg(theme.comment_outdated)
            .add_modifier(Modifier::DIM)
    } else {
        Style::default().fg(theme.comment)
    }
}

/// One-line sum of every captured item's statistics, e.g. `Σ +3 −0 · 1 binary`.
fn totals_line(
    items: &[crate::git::Item],
    current: usize,
    theme: &crate::theme::Theme,
) -> Line<'static> {
    use crate::git::Statistics;
    let (mut additions, mut deletions, mut binary, mut unavailable) = (0usize, 0usize, 0, 0);
    for item in items {
        match item.statistics() {
            Statistics::Text {
                additions: added,
                deletions: removed,
            } => {
                additions = additions.saturating_add(added);
                deletions = deletions.saturating_add(removed);
            }
            Statistics::Binary => binary += 1,
            Statistics::Unavailable => unavailable += 1,
        }
    }
    let neutral = Style::default().fg(theme.metrics);
    let mut spans = vec![
        Span::styled("Σ ", neutral),
        Span::styled(
            format!("+{additions}"),
            Style::default().fg(theme.stat_plus),
        ),
        Span::raw(" "),
        Span::styled(
            format!("−{deletions}"),
            Style::default().fg(theme.stat_minus),
        ),
    ];
    if let Some(item) = items.get(current) {
        spans.push(Span::styled(" current ", neutral));
        match item.statistics() {
            Statistics::Text {
                additions,
                deletions,
            } => {
                spans.push(Span::styled(
                    format!("+{additions}"),
                    Style::default().fg(theme.stat_plus),
                ));
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    format!("−{deletions}"),
                    Style::default().fg(theme.stat_minus),
                ));
            }
            Statistics::Binary => spans.push(Span::styled("binary", neutral)),
            Statistics::Unavailable => spans.push(Span::styled("unavailable", neutral)),
        }
    }
    if binary > 0 {
        spans.push(Span::styled(format!(" · {binary} binary"), neutral));
    }
    if unavailable > 0 {
        spans.push(Span::styled(
            format!(" · {unavailable} unavailable"),
            neutral,
        ));
    }
    Line::from(spans)
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(frame.area());
    let original = app.capture.unavailable.is_some();
    app.file_area = ratatui::layout::Rect::default();
    app.preview_area = ratatui::layout::Rect::default();
    app.popup_area = Rect::default();
    let panels = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(if app.preview.is_some() && !original {
            vec![Constraint::Percentage(30), Constraint::Percentage(70)]
        } else {
            vec![Constraint::Percentage(100)]
        })
        .split(areas[0]);
    if !original {
        // Per-file statistics appear only while no preview is open.
        let show_stats = app.preview.is_none();
        let items: Vec<_> = app
            .capture
            .items
            .iter()
            .zip(&app.marks)
            .enumerate()
            .map(|(index, (item, reviewed))| {
                let comment = app
                    .comments
                    .for_item(item)
                    .map(|(_, comment)| comment.outdated)
                    .reduce(|a, b| a && b);
                let note = app.notes.for_item(item).next().is_some();
                ListItem::new(file_row(
                    item,
                    (*reviewed, index == app.selected),
                    panels[0]
                        .width
                        .saturating_sub(if comment.is_some() || note { 2 } else { 4 })
                        as usize,
                    show_stats,
                    comment,
                    note,
                    &app.theme,
                ))
            })
            .collect();
        let block = Block::default()
            .borders(Borders::ALL)
            .title(if app.capture.items.is_empty() {
                "Files — no changes"
            } else {
                "Files"
            })
            .border_style(Style::default().fg(if app.focus == Focus::Files {
                app.theme.border_focused
            } else {
                app.theme.border_unfocused
            }));
        let inner = block.inner(panels[0]);
        frame.render_widget(block, panels[0]);
        // The bottom inner row is reserved for the totals line whether or not
        // a diff is open, so the list height (and hit-testing) never changes
        // on open/close.
        let mut list_area = inner;
        if !app.capture.items.is_empty() && inner.height >= 2 {
            list_area.height -= 1;
            frame.render_widget(
                Paragraph::new(totals_line(&app.capture.items, app.selected, &app.theme)),
                Rect::new(inner.x, inner.y + list_area.height, inner.width, 1),
            );
        }
        let files = List::new(items)
            .highlight_style(app.theme.selection)
            .highlight_symbol("› ");
        let mut selection = ListState::default();
        app.file_area = list_area;
        *selection.offset_mut() = app.file_offset;
        frame.render_stateful_widget(files, list_area, &mut selection);
        app.file_offset = selection.offset();
    }
    if app.preview.is_some() {
        let area = if original { panels[0] } else { panels[1] };
        let inner = Block::default().borders(Borders::ALL).inner(area);
        let mut content = inner;
        content.height = content.height.saturating_sub(1);
        let status_area = ratatui::layout::Rect::new(
            inner.x,
            inner.y + content.height,
            inner.width,
            inner.height.min(1),
        );
        let markers = app.comment_markers();
        let notes = app.note_markers();
        let marker_width = 2 * (u16::from(markers.is_some()) + u16::from(notes.is_some()));
        let mut text_area = content;
        text_area.width = text_area.width.saturating_sub(marker_width);
        app.preview_area = text_area;
        app.layout_preview(text_area.height as usize, text_area.width as usize);
        let preview = app.preview.as_ref().expect("preview exists");
        let visual_top = if app.wrap { app.visual_top() } else { app.top };
        let rows: Vec<_> = (0..app.viewport.0)
            .filter_map(|offset| {
                if app.wrap {
                    preview
                        .visual_segment(visual_top + offset, app.viewport.1)
                        .map(|(row, start, _)| (row, start))
                } else {
                    (app.top + offset < preview.lines.len()).then_some((app.top + offset, app.left))
                }
            })
            .collect();
        let mut lines: Vec<_> = rows
            .iter()
            .filter_map(|&(row, left)| {
                preview
                    .visible(row, left, 1, app.viewport.1)
                    .into_iter()
                    .next()
            })
            .collect();
        if let Some((first, last)) = app.selected_rows() {
            for (offset, line) in lines.iter_mut().enumerate() {
                if (first..=last).contains(&rows[offset].0) {
                    if line.spans.is_empty() {
                        line.spans.push(Span::raw(" "));
                    }
                    for span in &mut line.spans {
                        span.style = span.style.patch(app.theme.selection);
                    }
                }
            }
        }
        let cursor_visual = if app.wrap {
            preview
                .visual_position(app.cursor.0, app.cursor.1, app.viewport.1)
                .0
        } else {
            app.cursor.0
        };
        let cursor_marker = |offset: usize| {
            app.focus == Focus::Preview
                && rows
                    .get(offset)
                    .is_some_and(|(row, _)| *row == app.cursor.0)
                && if app.wrap {
                    visual_top + offset == cursor_visual
                } else {
                    true
                }
        };
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(if original {
                    "Original Git output — file actions disabled"
                } else {
                    preview.title.as_deref().unwrap_or("Diff")
                })
                .border_style(Style::default().fg(if app.focus == Focus::Preview {
                    app.theme.border_focused
                } else {
                    app.theme.border_unfocused
                })),
            area,
        );
        frame.render_widget(Paragraph::new(lines), text_area);
        if let Some(markers) = &markers
            && content.width >= 2
        {
            for offset in 0..app.viewport.0 {
                if let Some(Some(outdated)) =
                    rows.get(offset).and_then(|(row, _)| markers.get(*row))
                {
                    frame.buffer_mut().set_string(
                        content.x + content.width - 2,
                        content.y + offset as u16,
                        "💬",
                        if cursor_marker(offset) {
                            comment_style(*outdated, &app.theme)
                                .remove_modifier(Modifier::DIM)
                                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                        } else {
                            comment_style(*outdated, &app.theme)
                        },
                    );
                }
            }
        }
        if let Some(notes) = &notes
            && content.width >= marker_width
        {
            for offset in 0..app.viewport.0 {
                if rows
                    .get(offset)
                    .and_then(|(row, _)| notes.get(*row))
                    .copied()
                    .unwrap_or(false)
                {
                    let has_comment = rows
                        .get(offset)
                        .and_then(|(row, _)| markers.as_ref().and_then(|markers| markers.get(*row)))
                        .is_some_and(Option::is_some);
                    let right = if has_comment { 4 } else { 2 };
                    frame.buffer_mut().set_string(
                        content.x + content.width - right,
                        content.y + offset as u16,
                        "📝",
                        if cursor_marker(offset) {
                            Style::default()
                                .fg(app.theme.comment)
                                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                        } else {
                            Style::default().fg(app.theme.comment)
                        },
                    );
                }
            }
        }
        let total = preview.lines.len();
        let (row, column) = if total == 0 {
            (0, 0)
        } else {
            (app.cursor.0 + 1, app.cursor.1 + 1)
        };
        let visual_total = if app.wrap {
            preview.visual_rows(app.viewport.1)
        } else {
            total
        };
        let scroll = if visual_total <= app.viewport.0 {
            "All".to_owned()
        } else if visual_top == 0 {
            "Top".to_owned()
        } else if visual_top >= visual_total.saturating_sub(app.viewport.0) {
            "Bot".to_owned()
        } else {
            format!("{}%", visual_top * 100 / (visual_total - app.viewport.0))
        };
        frame.render_widget(
            Paragraph::new(format!("{total} lines  {row}:{column}  {scroll}"))
                .style(Style::default().fg(app.theme.metrics)),
            status_area,
        );
        if app.help.is_none()
            && !app.popup_navigation()
            && app.focus == Focus::Preview
            && total > 0
            && app.viewport.0 > 0
            && app.viewport.1 > 0
        {
            let (row, column) = if app.wrap {
                let (row, column) =
                    preview.visual_position(app.cursor.0, app.cursor.1, app.viewport.1);
                (row.saturating_sub(visual_top), column)
            } else {
                (
                    app.cursor.0.saturating_sub(app.top),
                    app.cursor.1.saturating_sub(app.left),
                )
            };
            frame.set_cursor_position((
                app.preview_area.x + column as u16,
                app.preview_area.y + row as u16,
            ));
        }
    }
    let prompt = app.navigation.as_ref().filter(|n| n.editing).map(|n| {
        format!(
            "Search/outline: {}_ (Enter searches both sides; Escape returns)",
            n.query
        )
    });
    let omission = app
        .preview
        .as_ref()
        .and_then(|p| p.reason.as_deref())
        .and_then(|reason| {
            reason
                .split("; ")
                .find(|note| note.starts_with("Display omitted"))
        });
    if let Some(command) = &app.command {
        // The command line owns the status row and the terminal cursor.
        let text = format!(":{}", command.text);
        let width = text.width();
        let available = areas[1].width as usize;
        let skip = width.saturating_sub(available.saturating_sub(1));
        let mut shown = String::new();
        let mut cells = 0;
        for grapheme in text.graphemes(true) {
            if cells >= skip {
                shown.push_str(grapheme);
            }
            cells += grapheme.width();
        }
        let column = shown.width().min(available.saturating_sub(1));
        frame.render_widget(Paragraph::new(shown), areas[1]);
        if areas[1].width > 0 {
            frame.set_cursor_position((areas[1].x + column as u16, areas[1].y));
        }
    } else {
        let status = prompt.as_deref().or(omission).unwrap_or(app.status.trim());
        frame.render_widget(Paragraph::new(status), areas[1]);
    }
    frame.render_widget(Paragraph::new(app.bindings.legend()), areas[2]);
    if app.popup_navigation() {
        draw_navigation_popup(frame, app);
    }
    if let Some(palette) = &app.palette {
        draw_palette(frame, palette, &app.theme);
    }
    if let Some(editor) = &mut app.editor {
        draw_editor(frame, editor, &app.theme);
    }
    if let Some(popup) = &mut app.notes_popup {
        draw_notes(frame, popup, &app.theme);
    }
    if app.help.is_some() {
        draw_help(frame, app);
    }
}

/// Definition and reference candidates overlay the retained review view.
fn draw_navigation_popup(frame: &mut Frame, app: &mut App) {
    let screen = frame.area();
    let bordered = screen.width >= 5 && screen.height >= 5;
    let width = if bordered {
        screen.width.saturating_sub(2).min(100)
    } else {
        screen.width
    };
    let height = if bordered {
        let maximum = (u32::from(screen.height) * 70 / 100) as u16;
        let minimum = (u32::from(screen.height) * 50 / 100) as u16;
        let content = app
            .navigation
            .as_ref()
            .unwrap()
            .candidates
            .len()
            .saturating_add(4);
        let desired = u16::try_from(content).unwrap_or(u16::MAX);
        desired.max(minimum).min(maximum.max(3))
    } else {
        screen.height
    };
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    let footer = format!(
        " {} open · {} back · ↑/↓ select ",
        app.bindings.label(crate::keybindings::Action::OpenDiffView),
        app.bindings
            .label(crate::keybindings::Action::CloseDiffView)
    );
    let nav = app.navigation.as_mut().expect("popup navigation");
    let title = format!(
        " {:?} candidates for {:?} — {} results ",
        nav.operation,
        nav.query,
        nav.candidates.len()
    );
    let block = Block::default()
        .borders(if bordered {
            Borders::ALL
        } else {
            Borders::NONE
        })
        .border_style(Style::default().fg(app.theme.border_focused))
        .title(if bordered { title } else { String::new() })
        .title_bottom(if bordered { footer } else { String::new() });
    let inner = block.inner(area);
    // Reserve space for coverage even when there are no results. In a short
    // popup retain one selectable row whenever there is room for both.
    let coverage_rows = if inner.height <= 1 {
        inner.height
    } else if nav.candidates.is_empty() {
        inner.height.min(2)
    } else {
        inner.height.saturating_sub(1).min(2)
    };
    let list_area = Rect::new(
        inner.x,
        inner.y,
        inner.width,
        inner.height.saturating_sub(coverage_rows),
    );
    let coverage_area = Rect::new(inner.x, list_area.bottom(), inner.width, coverage_rows);
    app.popup_area = list_area;
    nav.viewport = list_area.height as usize;
    if nav.selected < nav.scroll {
        nav.scroll = nav.selected;
    }
    if nav.viewport > 0 && nav.selected >= nav.scroll + nav.viewport {
        nav.scroll = nav.selected + 1 - nav.viewport;
    }
    nav.scroll = nav
        .scroll
        .min(nav.candidates.len().saturating_sub(nav.viewport.max(1)));
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let coverage = if nav.coverage.is_empty() {
        "Searching…".to_owned()
    } else if nav.candidates.is_empty() {
        format!("{} · No results", nav.coverage)
    } else {
        nav.coverage.clone()
    };
    frame.render_widget(
        Paragraph::new(coverage)
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(app.theme.metrics)),
        coverage_area,
    );
    for (index, candidate) in nav
        .candidates
        .iter()
        .enumerate()
        .skip(nav.scroll)
        .take(nav.viewport)
    {
        let source = &app.capture.catalog.sources[candidate.source];
        let text = format!(
            "{} {} {}:{}:{} {} {}",
            if index == nav.selected { ">" } else { " " },
            source.side.label(),
            crate::app::candidate_field(&crate::git::visible_bytes(&source.path)),
            candidate.tag.line,
            candidate.tag.col + 1,
            crate::app::candidate_field(&candidate.tag.name),
            crate::app::candidate_field(candidate.tag.context.as_deref().unwrap_or(""))
        );
        let style = if index == nav.selected {
            app.theme.selection
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(text).style(style),
            Rect::new(
                inner.x,
                list_area.y + (index - nav.scroll) as u16,
                list_area.width,
                1,
            ),
        );
    }
}

/// Centered floating command palette; owns the terminal cursor while open.
fn draw_palette(
    frame: &mut Frame,
    palette: &crate::commands::Palette,
    theme: &crate::theme::Theme,
) {
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(90);
    let height = screen.height.saturating_sub(4).min(16);
    if width < 3 || height < 3 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_focused))
        .title(" Command palette ")
        .title_bottom(" enter run · esc cancel · ↑/↓ select ");
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let rows = inner.height as usize;
    let columns = inner.width as usize;
    if rows == 0 || columns == 0 {
        return;
    }
    // The query row keeps its end, and the cursor after it, in view.
    let text = format!("> {}", palette.query);
    let skip = text.width().saturating_sub(columns.saturating_sub(1));
    let mut shown = String::new();
    let mut cells = 0;
    for grapheme in text.graphemes(true) {
        if cells >= skip {
            shown.push_str(grapheme);
        }
        cells += grapheme.width();
    }
    let column = shown.width().min(columns - 1);
    frame.render_widget(
        Paragraph::new(shown),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    frame.set_cursor_position((inner.x + column as u16, inner.y));
    let list = rows - 1;
    if list == 0 {
        return;
    }
    let matches = palette.matches();
    if matches.is_empty() {
        frame.render_widget(
            Paragraph::new("No matching commands").style(Style::default().fg(theme.metrics)),
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
        );
        return;
    }
    let top = (palette.selected + 1).saturating_sub(list);
    for (offset, entry) in matches.iter().enumerate().skip(top).take(list) {
        let line = Line::from(vec![
            Span::raw(entry.text.clone()),
            Span::raw("  "),
            Span::styled(entry.summary, Style::default().fg(theme.metrics)),
        ]);
        let style = if offset == palette.selected {
            theme.selection
        } else {
            Style::default()
        };
        frame.render_widget(
            Paragraph::new(line).style(style),
            Rect::new(inner.x, inner.y + 1 + (offset - top) as u16, inner.width, 1),
        );
    }
}

fn draw_notes(frame: &mut Frame, popup: &mut crate::app::NotesPopup, theme: &crate::theme::Theme) {
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(90);
    let height = screen.height.saturating_sub(4).min(24);
    if width < 3 || height < 3 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_focused))
        .title(" Notes ")
        .title_bottom(" ↑/↓ scroll · esc close ");
    let inner = block.inner(area);
    let mut lines = Vec::new();
    for line in popup.text.split('\n') {
        let mut wrapped = String::new();
        let mut used = 0;
        for grapheme in line.graphemes(true) {
            let size = grapheme.width();
            if used > 0 && used + size > inner.width as usize {
                lines.push(Line::raw(std::mem::take(&mut wrapped)));
                used = 0;
            }
            wrapped.push_str(grapheme);
            used += size;
        }
        lines.push(Line::raw(wrapped));
    }
    popup.max_scroll = lines
        .len()
        .saturating_sub(inner.height as usize)
        .min(u16::MAX as usize) as u16;
    popup.scroll = popup.scroll.min(popup.max_scroll);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines).scroll((popup.scroll, 0)), inner);
}

/// Centered floating help; the active bindings are its only content source.
fn draw_help(frame: &mut Frame, app: &mut App) {
    let screen = frame.area();
    let entries = app.filtered_help();
    let lines = entries.len();
    let width = screen.width.saturating_sub(4).min(90);
    let height = (u32::from(screen.height) * 90 / 100) as u16;
    if width < 3 || height < 3 {
        app.layout_help(0, lines);
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(app.theme.border_focused))
        .title(" Help ")
        .title_bottom(" / search · ↑/↓ select · esc close ");
    let inner = block.inner(area);
    app.layout_help(inner.height.saturating_sub(1) as usize, lines);
    let help = app.help.expect("help is open");
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let key_width = entries
        .iter()
        .map(|(_, keys)| keys.width())
        .max()
        .unwrap_or(0)
        .max("Keybinding".width())
        .min(u16::MAX as usize) as u16;
    let rows = entries
        .into_iter()
        .enumerate()
        .skip(help.scroll)
        .map(|(index, (action, keys))| {
            let description = action.description();
            let positions =
                crate::app::fuzzy_positions(&format!("{keys}  {description}"), &app.help_query)
                    .unwrap_or_default();
            let keys_len = keys.chars().count();
            let style = if index == help.selected {
                app.theme.selection
            } else {
                Style::default()
            };
            Row::new(vec![
                help_matches(&keys, 0, &positions, &app.theme),
                help_matches(&description, keys_len + 2, &positions, &app.theme),
            ])
            .style(style)
        });
    let table_area = inner;
    frame.render_widget(
        Table::new(rows, [Constraint::Length(key_width), Constraint::Min(0)])
            .header(
                Row::new(vec!["Keybinding", "Description"])
                    .style(Style::default().add_modifier(Modifier::BOLD)),
            )
            .column_spacing(2),
        table_area,
    );
    if lines == 0 && table_area.height > 1 {
        frame.render_widget(
            Paragraph::new("No matches").style(Style::default().fg(app.theme.metrics)),
            Rect::new(table_area.x, table_area.y + 1, table_area.width, 1),
        );
    }
    if help.searching && screen.height > 0 && screen.width > 0 {
        let prompt = format!("/{}", app.help_query);
        let skip = prompt
            .width()
            .saturating_sub((screen.width as usize).saturating_sub(1));
        let shown: String = prompt
            .graphemes(true)
            .scan(0, |cells, grapheme| {
                let start = *cells;
                *cells += grapheme.width();
                Some(if start >= skip { grapheme } else { "" })
            })
            .collect();
        let prompt_area = Rect::new(screen.x, screen.y + screen.height - 1, screen.width, 1);
        frame.render_widget(Clear, prompt_area);
        frame.render_widget(Paragraph::new(shown.clone()), prompt_area);
        frame.set_cursor_position((
            screen.x + shown.width().min(screen.width.saturating_sub(1) as usize) as u16,
            prompt_area.y,
        ));
    }
}

fn help_matches(
    text: &str,
    offset: usize,
    positions: &[usize],
    theme: &crate::theme::Theme,
) -> Line<'static> {
    Line::from(
        text.chars()
            .enumerate()
            .map(|(index, c)| {
                let style = if positions.contains(&(index + offset)) {
                    Style::default()
                        .fg(theme.stat_plus)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };
                Span::styled(c.to_string(), style)
            })
            .collect::<Vec<_>>(),
    )
}

/// Centered floating comment editor; owns the terminal cursor while open.
fn draw_editor(
    frame: &mut Frame,
    editor: &mut crate::comments::Editor,
    theme: &crate::theme::Theme,
) {
    let screen = frame.area();
    let width = screen.width.saturating_sub(4).min(90);
    let height = screen.height.saturating_sub(4).min(14);
    if width < 3 || height < 3 {
        return;
    }
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.border_focused))
        .title(format!(" Comment · {} ", editor.title))
        .title_bottom(" enter save · ctrl-enter/alt-enter/ctrl-j newline · esc cancel ");
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    let rows = inner.height as usize;
    let columns = inner.width as usize;
    if rows == 0 || columns == 0 {
        return;
    }
    editor.wrap_width = Some(columns);
    let lines = editor.visual_lines(columns);
    let (cursor_row, cursor_column) = editor.visual_cursor(&lines, columns);
    let top = (cursor_row + 1).saturating_sub(rows);
    let text: Vec<Line> = lines
        .iter()
        .skip(top)
        .take(rows)
        .map(|line| Line::raw(line.text.to_owned()))
        .collect();
    frame.render_widget(Paragraph::new(text), inner);
    frame.set_cursor_position((
        inner.x + cursor_column as u16,
        inner.y + (cursor_row - top) as u16,
    ));
}

/// Whether keyboard enhancement flags were pushed and must be popped.
static ENHANCED_KEYBOARD: AtomicBool = AtomicBool::new(false);

fn restore_terminal() {
    if ENHANCED_KEYBOARD.swap(false, Ordering::SeqCst) {
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    }
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableMouseCapture,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
}

type PanicHook = Arc<dyn Fn(&std::panic::PanicHookInfo<'_>) + Send + Sync>;
struct Restore {
    hook: PanicHook,
    signals: Vec<signal_hook::SigId>,
}
impl Drop for Restore {
    fn drop(&mut self) {
        for signal in self.signals.drain(..) {
            signal_hook::low_level::unregister(signal);
        }
        if !std::thread::panicking() {
            let hook = self.hook.clone();
            std::panic::set_hook(Box::new(move |info| hook(info)));
        }
        restore_terminal();
    }
}

pub fn run(app: &mut App) -> Result<()> {
    let old_hook: PanicHook = std::panic::take_hook().into();
    let mut restore = Restore {
        hook: old_hook.clone(),
        signals: Vec::new(),
    };
    let terminal_started = Instant::now();
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
    // Disambiguated key codes distinguish Ctrl+Enter/Shift+Enter from Enter in
    // the comment editor. Alternate keys keep shifted Alt chords such as
    // alt-< reported as their shifted character. Only pushed when the terminal
    // answers the kitty keyboard query; every restore path pops it.
    if matches!(
        crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    ) {
        execute!(
            io::stdout(),
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            )
        )?;
        ENHANCED_KEYBOARD.store(true, Ordering::SeqCst);
    }
    let ui_thread = std::thread::current().id();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() == ui_thread {
            restore_terminal();
        }
        old_hook(info);
    }));
    let interrupted = Arc::new(AtomicBool::new(false));
    for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
        restore
            .signals
            .push(signal_hook::flag::register(signal, interrupted.clone())?);
    }
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<()> {
        let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
        let terminal_setup = terminal_started.elapsed();
        loop {
            app.poll_analysis();
            terminal.draw(|frame| draw(frame, app))?;
            if let Some(startup) = app.startup.take() {
                let first_frame = crate::telemetry::since_start();
                crate::telemetry::record(
                    "startup",
                    crate::telemetry::startup_fields(
                        &startup,
                        terminal_setup,
                        first_frame,
                        &app.capture,
                    ),
                );
            }
            #[cfg(test)]
            match std::env::var("REVIEW_TERMINAL_SCENARIO").as_deref() {
                Ok("panic") => panic!("injected UI panic"),
                Ok("error") => anyhow::bail!("injected terminal I/O failure"),
                _ => {}
            }
            if interrupted.load(Ordering::Relaxed) {
                break;
            }
            if !event::poll(Duration::from_millis(50))? {
                continue;
            }
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if key.code == event::KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        break;
                    }
                    if app.key_event(key) {
                        break;
                    }
                }
                Event::Mouse(mouse) => app.mouse(mouse, Instant::now()),
                _ => {}
            }
        }
        terminal.show_cursor()?;
        Ok(())
    }));
    drop(restore);
    match result {
        Ok(result) => result,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

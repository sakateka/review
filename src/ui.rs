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
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
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
    reviewed: bool,
    width: usize,
    theme: &crate::theme::Theme,
) -> Line<'static> {
    use crate::git::Statistics;
    let name = item.name();
    let available = width.saturating_sub(2);
    let statistics = item.statistics();
    let label = match statistics {
        Statistics::Text {
            additions,
            deletions,
        } => (additions + deletions).to_string(),
        Statistics::Binary => "binary".into(),
        Statistics::Unavailable => "unavailable".into(),
    };
    // Names retain at least twelve cells (or the entire tiny pane). Statistics
    // yield first; the graph is optional and bounded to twelve cells.
    let name_minimum = name.width().min(12);
    let show_stats = available >= name_minimum + 2 + label.width();
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
            Style::default().fg(if reviewed {
                theme.reviewed_name
            } else {
                theme.pending_name
            }),
        ),
    ];
    if show_stats {
        spans.push(Span::raw(format!("{}  {label}", " ".repeat(padding))));
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
    let panels = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(if app.preview.is_some() && !original {
            vec![Constraint::Percentage(30), Constraint::Percentage(70)]
        } else {
            vec![Constraint::Percentage(100)]
        })
        .split(areas[0]);
    if !original {
        let items: Vec<_> = app
            .capture
            .items
            .iter()
            .zip(&app.marks)
            .enumerate()
            .map(|(index, (item, reviewed))| {
                ListItem::new(file_row(
                    item,
                    *reviewed,
                    panels[0].width.saturating_sub(4) as usize,
                    &app.theme,
                ))
                .style(if index == app.selected {
                    app.theme.selection
                } else {
                    Style::default()
                })
            })
            .collect();
        let files = List::new(items)
            .block(
                Block::default()
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
                    })),
            )
            .highlight_style(app.theme.selection)
            .highlight_symbol("› ");
        let mut selection = ListState::default();
        app.file_area = Block::default().borders(Borders::ALL).inner(panels[0]);
        *selection.offset_mut() = app.file_offset;
        frame.render_stateful_widget(files, panels[0], &mut selection);
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
        // Commented diffs reserve a two-cell icon column at the right edge.
        let markers = app.comment_markers();
        let mut text_area = content;
        if markers.is_some() {
            text_area.width = text_area.width.saturating_sub(2);
        }
        app.preview_area = text_area;
        app.layout_preview(text_area.height as usize, text_area.width as usize);
        let preview = app.preview.as_ref().expect("preview exists");
        let mut lines = preview.visible(app.top, app.left, app.viewport.0, app.viewport.1);
        if let Some((first, last)) = app.selected_rows() {
            for (offset, line) in lines.iter_mut().enumerate() {
                if (first..=last).contains(&(app.top + offset)) {
                    if line.spans.is_empty() {
                        line.spans.push(Span::raw(" "));
                    }
                    for span in &mut line.spans {
                        span.style = span.style.patch(app.theme.selection);
                    }
                }
            }
        }
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
                if let Some(Some(outdated)) = markers.get(app.top + offset) {
                    let style = if *outdated {
                        Style::default()
                            .fg(app.theme.comment_outdated)
                            .add_modifier(Modifier::DIM)
                    } else {
                        Style::default().fg(app.theme.comment)
                    };
                    frame.buffer_mut().set_string(
                        content.x + content.width - 2,
                        content.y + offset as u16,
                        "💬",
                        style,
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
        let scroll = if total <= app.viewport.0 {
            "All".to_owned()
        } else if app.top == 0 {
            "Top".to_owned()
        } else if app.top >= total.saturating_sub(app.viewport.0) {
            "Bot".to_owned()
        } else {
            format!("{}%", app.top * 100 / (total - app.viewport.0))
        };
        frame.render_widget(
            Paragraph::new(format!("{total} lines  {row}:{column}  {scroll}"))
                .style(Style::default().fg(app.theme.metrics)),
            status_area,
        );
        if app.focus == Focus::Preview && total > 0 && app.viewport.0 > 0 && app.viewport.1 > 0 {
            frame.set_cursor_position((
                app.preview_area.x + (app.cursor.1 - app.left) as u16,
                app.preview_area.y + (app.cursor.0 - app.top) as u16,
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
    if let Some(editor) = &app.editor {
        draw_editor(frame, editor, &app.theme);
    }
}

/// Centered floating comment editor; owns the terminal cursor while open.
fn draw_editor(frame: &mut Frame, editor: &crate::comments::Editor, theme: &crate::theme::Theme) {
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
    let top = (editor.row + 1).saturating_sub(rows);
    let line = &editor.lines[editor.row];
    let before: String = line.chars().take(editor.column).collect();
    let cursor = before.width();
    let left = (cursor + 1).saturating_sub(columns);
    let text: Vec<Line> = editor
        .lines
        .iter()
        .skip(top)
        .take(rows)
        .map(|line| Line::raw(line.clone()))
        .collect();
    frame.render_widget(
        Paragraph::new(text).scroll((0, left.min(u16::MAX as usize) as u16)),
        inner,
    );
    frame.set_cursor_position((
        inner.x + (cursor - left) as u16,
        inner.y + (editor.row - top) as u16,
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
        loop {
            app.poll_analysis();
            terminal.draw(|frame| draw(frame, app))?;
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

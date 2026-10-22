use crate::app::{App, Focus};
use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
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

fn file_row(item: &crate::git::Item, reviewed: bool, width: usize) -> Line<'static> {
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
            Style::default().fg(if reviewed { Color::Gray } else { Color::Yellow }),
        ),
        Span::styled(
            displayed_name,
            Style::default().fg(if reviewed {
                Color::DarkGray
            } else {
                Color::White
            }),
        ),
    ];
    if show_stats {
        spans.push(Span::raw(format!("{}  {label}", " ".repeat(padding))));
        if plus + minus > 0 {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                "+".repeat(plus),
                Style::default().fg(Color::Green),
            ));
            spans.push(Span::styled(
                "-".repeat(minus),
                Style::default().fg(Color::Red),
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
                ))
                .style(if index == app.selected {
                    Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
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
                        Color::Cyan
                    } else {
                        Color::DarkGray
                    })),
            )
            .highlight_style(Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED))
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
        app.preview_area = content;
        app.layout_preview(content.height as usize, content.width as usize);
        let preview = app.preview.as_ref().expect("preview exists");
        let lines = preview.visible(app.top, app.left, app.viewport.0, app.viewport.1);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(if original {
                    "Original Git output — file actions disabled"
                } else {
                    preview.title.as_deref().unwrap_or("Diff")
                })
                .border_style(Style::default().fg(if app.focus == Focus::Preview {
                    Color::Cyan
                } else {
                    Color::DarkGray
                })),
            area,
        );
        frame.render_widget(Paragraph::new(lines), content);
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
                .style(Style::default().fg(Color::Cyan)),
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
    let status = prompt.as_deref().or(omission).unwrap_or(app.status.trim());
    frame.render_widget(Paragraph::new(status), areas[1]);
    frame.render_widget(Paragraph::new(app.bindings.legend()), areas[2]);
}

fn restore_terminal() {
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
                Event::Key(key) if key.kind != KeyEventKind::Release => {
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

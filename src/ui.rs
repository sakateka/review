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

pub fn draw(frame: &mut Frame, app: &mut App) {
    let areas = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(0),
            Constraint::Length(3),
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
                ListItem::new(Line::from(vec![
                    Span::styled(
                        if *reviewed { "+ " } else { "- " },
                        Style::default().fg(if *reviewed {
                            Color::Gray
                        } else {
                            Color::Yellow
                        }),
                    ),
                    Span::styled(
                        item.name(),
                        Style::default().fg(if *reviewed {
                            Color::DarkGray
                        } else {
                            Color::White
                        }),
                    ),
                ]))
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
        app.viewport = (
            area.height.saturating_sub(2) as usize,
            area.width.saturating_sub(2) as usize,
        );
        app.preview_area = Block::default().borders(Borders::ALL).inner(area);
        app.keep_cursor_visible();
        let preview = app.preview.as_ref().expect("preview exists");
        let lines = preview.visible(app.top, app.left, app.viewport.0, app.viewport.1);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(if original {
                        "Original Git output — file actions disabled"
                    } else {
                        "Diff"
                    })
                    .border_style(Style::default().fg(if app.focus == Focus::Preview {
                        Color::Cyan
                    } else {
                        Color::DarkGray
                    })),
            ),
            area,
        );
        if app.focus == Focus::Preview && app.viewport.0 > 0 && app.viewport.1 > 0 {
            frame.set_cursor_position((
                app.preview_area.x + (app.cursor.1 - app.left) as u16,
                app.preview_area.y + (app.cursor.0 - app.top) as u16,
            ));
        }
    }
    let explanation = [
        app.preview
            .as_ref()
            .and_then(|preview| preview.reason.as_deref()),
        app.capture.unavailable.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" | ");
    let prompt = app.navigation.as_ref().filter(|n| n.editing).map(|n| {
        format!(
            "Search/outline: {}_ (Enter searches both sides; Escape returns)",
            n.query
        )
    });
    let status = [
        explanation.as_str(),
        prompt.as_deref().unwrap_or(app.status.trim()),
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .collect::<Vec<_>>()
    .join(" | ");
    frame.render_widget(
        Paragraph::new(status).wrap(ratatui::widgets::Wrap { trim: false }),
        areas[1],
    );
    frame.render_widget(
        Paragraph::new(
            "arrows/hjkl move · Tab focus · Enter open · files: h close/l open · PgUp/Dn scroll · s search · d defs/r refs · b back · [old ]new · Space mark · q quit",
        ),
        areas[2],
    );
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
                    if app.key(key.code) {
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

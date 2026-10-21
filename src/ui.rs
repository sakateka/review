use crate::app::{App, Focus};
use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyEventKind, KeyModifiers},
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
use std::io;

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
            .map(|(item, reviewed)| {
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
        if !app.capture.items.is_empty() {
            selection.select(Some(app.selected));
        }
        frame.render_stateful_widget(files, panels[0], &mut selection);
    }
    if app.preview.is_some() {
        let area = if original { panels[0] } else { panels[1] };
        app.viewport = (
            area.height.saturating_sub(2) as usize,
            area.width.saturating_sub(2) as usize,
        );
        app.clamp_scroll();
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
    }
    let explanation = app.capture.unavailable.as_deref().or_else(|| {
        app.preview
            .as_ref()
            .and_then(|preview| preview.reason.as_deref())
    });
    let status = [explanation.unwrap_or(""), app.status.trim()]
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
            "l/Enter open · h back · jk/↑↓ move · ←→ scroll · Space mark · q quit · Tab focus",
        ),
        areas[2],
    );
}

struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}

pub fn run(app: &mut App) -> Result<()> {
    enable_raw_mode()?;
    let _restore = Restore;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let old_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
        old_hook(info);
    }));
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    loop {
        terminal.draw(|frame| draw(frame, app))?;
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
            _ => {}
        }
    }
    terminal.show_cursor()?;
    Ok(())
}

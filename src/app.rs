use crate::{git::Capture, preview::Preview, state::Store};
use crossterm::event::KeyCode;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Focus {
    Files,
    Preview,
}

pub struct App {
    pub capture: Capture,
    pub store: Store,
    pub marks: Vec<bool>,
    pub selected: usize,
    pub preview: Option<Preview>,
    pub focus: Focus,
    pub top: usize,
    pub left: usize,
    pub viewport: (usize, usize),
    pub status: String,
    state_error: bool,
}

impl App {
    pub fn new(capture: Capture, store: Store) -> Self {
        let (marks, status, state_error) = match store.load(&capture.items) {
            Ok(marks) => (marks, capture.diagnostic.clone(), false),
            Err(error) => (
                vec![false; capture.items.len()],
                format!("{error:#}; marking is disabled to preserve prior state"),
                true,
            ),
        };
        let original = capture.unavailable.is_some();
        let preview = original.then(|| {
            let mut output = capture.original.clone();
            if !capture.stderr.is_empty() {
                if !output.is_empty() && !output.ends_with(b"\n") {
                    output.push(b'\n');
                }
                output.extend_from_slice(b"Git stderr:\n");
                output.extend_from_slice(&capture.stderr);
            }
            Preview::original(&output)
        });
        Self {
            capture,
            store,
            marks,
            selected: 0,
            preview,
            focus: if original {
                Focus::Preview
            } else {
                Focus::Files
            },
            top: 0,
            left: 0,
            viewport: (0, 0),
            status,
            state_error,
        }
    }

    pub fn clamp_scroll(&mut self) {
        if let Some(preview) = &self.preview {
            self.top = self
                .top
                .min(preview.lines.len().saturating_sub(self.viewport.0));
            self.left = self.left.min(preview.width.saturating_sub(self.viewport.1));
        }
    }

    // Returns true only for an explicit quit. Drawing has no state/store effects.
    pub fn key(&mut self, key: KeyCode) -> bool {
        match key {
            KeyCode::Char('q') | KeyCode::Esc => return true,
            KeyCode::Enter => {
                if let Some(item) = self.capture.items.get(self.selected) {
                    let mut preview = Preview::item(&self.capture.cwd, item);
                    if !self.capture.stderr.is_empty() {
                        let mut warning = b"\nGit stderr:\n".to_vec();
                        warning.extend_from_slice(&self.capture.stderr);
                        let warning = Preview::original(&warning);
                        preview.width = preview.width.max(warning.width);
                        preview.lines.extend(warning.lines);
                    }
                    self.preview = Some(preview);
                    self.focus = Focus::Preview;
                    self.top = 0;
                    self.left = 0;
                } else if self.capture.unavailable.is_some() {
                    self.status = "File actions are unavailable in original-output mode".into();
                }
            }
            KeyCode::Tab if self.preview.is_some() && self.capture.unavailable.is_none() => {
                self.focus = if self.focus == Focus::Files {
                    Focus::Preview
                } else {
                    Focus::Files
                };
            }
            KeyCode::Char(' ') => self.toggle_mark(),
            KeyCode::Down | KeyCode::Char('j') => {
                if self.focus == Focus::Files {
                    self.select(1);
                } else {
                    self.top = self.top.saturating_add(1);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.focus == Focus::Files {
                    self.select(-1);
                } else {
                    self.top = self.top.saturating_sub(1);
                }
            }
            KeyCode::Left if self.focus == Focus::Preview => {
                self.left = self.left.saturating_sub(4)
            }
            KeyCode::Right if self.focus == Focus::Preview => {
                self.left = self.left.saturating_add(4)
            }
            _ => {}
        }
        self.clamp_scroll();
        false
    }

    fn select(&mut self, direction: isize) {
        self.selected = self
            .selected
            .saturating_add_signed(direction)
            .min(self.capture.items.len().saturating_sub(1));
        self.preview = None;
        self.top = 0;
        self.left = 0;
    }

    fn toggle_mark(&mut self) {
        if self.capture.unavailable.is_some() {
            self.status = "Marks are unavailable: file boundaries were not proven".into();
            return;
        }
        if self.state_error {
            return;
        }
        if let Some(mark) = self.marks.get(self.selected) {
            let mut next = self.marks.clone();
            next[self.selected] = !mark;
            match self.store.save(&self.capture.items, &next) {
                Ok(()) => {
                    self.marks = next;
                    self.status = "Review mark saved".into();
                }
                Err(error) => self.status = format!("Mark not saved: {error:#}"),
            }
        }
    }
}

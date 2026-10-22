use crate::keybindings::{Action, Bindings, Keymap, Resolution};
use crate::{git::Capture, preview::Preview, state::Store};
use crate::{
    navigation::{Candidate, Operation, Request, Worker},
    source::ComparedSide,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::time::{Duration, Instant};

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
    worker: Worker,
    generation: u64,
    pending: Option<Request>,
    pub navigation: Option<Navigation>,
    pub side: ComparedSide,
    source: Option<usize>,
    pub cursor: (usize, usize),
    history: Vec<View>,
    pub file_area: Rect,
    pub preview_area: Rect,
    pub file_offset: usize,
    last_click: Option<(usize, Instant)>,
    motion_count: usize,
    pub bindings: Bindings,
}

#[derive(Clone)]
pub struct Navigation {
    pub operation: Operation,
    pub query: String,
    pub editing: bool,
    pub candidates: Vec<Candidate>,
    pub selected: usize,
    pub coverage: String,
    position: Option<(usize, usize, usize)>,
}
#[derive(Clone)]
struct View {
    preview: Option<Preview>,
    top: usize,
    left: usize,
    side: ComparedSide,
    source: Option<usize>,
    cursor: (usize, usize),
    navigation: Option<Navigation>,
}
impl App {
    pub fn new(capture: Capture, store: Store) -> Self {
        let (marks, status, state_error) = match store.load_report(&capture.items) {
            Ok(loaded) => {
                let mut status = capture.diagnostic.clone();
                if loaded.pending_legacy {
                    if !status.is_empty() {
                        status.push_str("; ");
                    }
                    status.push_str("Some prior review marks remain pending because their complete comparison identity cannot be proven");
                }
                (loaded.marks, status, false)
            }
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
        let worker = Worker::start(capture.catalog.clone());
        Self {
            worker,
            generation: 0,
            pending: None,
            navigation: None,
            side: ComparedSide::New,
            source: None,
            cursor: (0, 0),
            file_area: Rect::default(),
            preview_area: Rect::default(),
            file_offset: 0,
            last_click: None,
            motion_count: 0,
            bindings: Bindings::default(),
            history: Vec::new(),
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

    fn clamp_cursor(&mut self) {
        if let Some(preview) = &self.preview {
            self.cursor.0 = self.cursor.0.min(preview.lines.len().saturating_sub(1));
            self.cursor.1 = self.cursor.1.min(
                preview
                    .lines
                    .get(self.cursor.0)
                    .map_or(0, |line| line.width().saturating_sub(1)),
            );
        } else {
            self.cursor = (0, 0);
        }
    }

    pub fn keep_cursor_visible(&mut self) {
        self.clamp_cursor();
        if self.cursor.0 < self.top {
            self.top = self.cursor.0;
        }
        if self.cursor.1 < self.left {
            self.left = self.cursor.1;
        }
        if self.cursor.0 >= self.top + self.viewport.0.max(1) {
            self.top = self.cursor.0 + 1 - self.viewport.0.max(1);
        }
        if self.cursor.1 >= self.left + self.viewport.1.max(1) {
            self.left = self.cursor.1 + 1 - self.viewport.1.max(1);
        }
        self.clamp_scroll();
    }

    fn move_cursor(&mut self, rows: isize, columns: isize) {
        self.cursor.0 = self.cursor.0.saturating_add_signed(rows);
        self.cursor.1 = self.cursor.1.saturating_add_signed(columns);
        self.keep_cursor_visible();
    }

    pub fn scroll_preview(&mut self, rows: isize, columns: isize) {
        self.top = self.top.saturating_add_signed(rows);
        self.left = self.left.saturating_add_signed(columns);
        self.clamp_scroll();
        self.cursor.0 = self
            .cursor
            .0
            .clamp(self.top, self.top + self.viewport.0.saturating_sub(1));
        self.cursor.1 = self
            .cursor
            .1
            .clamp(self.left, self.left + self.viewport.1.saturating_sub(1));
        self.keep_cursor_visible();
    }

    pub fn mouse(&mut self, mouse: MouseEvent, now: Instant) {
        let point = ratatui::layout::Position::new(mouse.column, mouse.row);
        let files = self.file_area.contains(point);
        let preview = self.preview.is_some() && self.preview_area.contains(point);
        match mouse.kind {
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => {
                self.last_click = None;
                let direction = if matches!(
                    mouse.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollLeft
                ) {
                    -3
                } else {
                    3
                };
                if files {
                    self.file_offset = self.file_offset.saturating_add_signed(direction).min(
                        self.capture
                            .items
                            .len()
                            .saturating_sub(self.file_area.height as usize),
                    );
                } else if preview {
                    if matches!(
                        mouse.kind,
                        MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight
                    ) {
                        self.scroll_preview(0, direction);
                    } else {
                        self.scroll_preview(direction, 0);
                    }
                }
            }
            MouseEventKind::Down(MouseButton::Left) if files => {
                let index = self.file_offset + (mouse.row - self.file_area.y) as usize;
                if index >= self.capture.items.len() {
                    self.last_click = None;
                    return;
                }
                let double = self.last_click.is_some_and(|(previous, time)| {
                    previous == index
                        && now.saturating_duration_since(time) <= Duration::from_millis(400)
                });
                self.selected = index;
                self.select(0);
                self.last_click = if double { None } else { Some((index, now)) };
                if double {
                    self.dispatch(Action::OpenDiffView, 1, false);
                }
            }
            MouseEventKind::Down(MouseButton::Left) if preview => {
                self.last_click = None;
                self.focus = Focus::Preview;
                let row = self.top + (mouse.row - self.preview_area.y) as usize;
                let column = self.left + (mouse.column - self.preview_area.x) as usize;
                if self
                    .preview
                    .as_ref()
                    .and_then(|preview| preview.lines.get(row))
                    .is_some_and(|line| column < line.width() || (column == 0 && line.width() == 0))
                {
                    self.cursor = (row, column);
                } else {
                    self.status = "No retained text at clicked position; cursor unchanged".into();
                }
            }
            MouseEventKind::Down(_) => {
                self.last_click = None;
            }
            _ => {}
        }
    }

    pub fn key_event(&mut self, key: KeyEvent) -> bool {
        self.last_click = None;
        if self.navigation.as_ref().is_some_and(|nav| nav.editing) {
            self.bindings.clear();
            self.motion_count = 0;
            match key.code {
                KeyCode::Esc => self.back(),
                KeyCode::Enter => {
                    self.navigation.as_mut().expect("navigation").editing = false;
                    self.submit();
                }
                KeyCode::Backspace => {
                    self.navigation.as_mut().expect("navigation").query.pop();
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                        && self
                            .navigation
                            .as_ref()
                            .is_some_and(|n| n.query.len() < 256) =>
                {
                    self.navigation.as_mut().expect("navigation").query.push(c);
                }
                _ => {}
            }
            return false;
        }
        // Escape always cancels an incomplete command without executing it.
        if key.code == KeyCode::Esc && (self.bindings.pending() || self.motion_count > 0) {
            self.bindings.clear();
            self.motion_count = 0;
            self.status.clear();
            return false;
        }
        if self.bindings.keymap == Keymap::Vim
            && !self.bindings.pending()
            && key.modifiers.is_empty()
            && let KeyCode::Char(digit @ '0'..='9') = key.code
            && (digit != '0' || self.motion_count > 0)
            && self.bindings.count_digit(digit)
        {
            self.motion_count =
                (self.motion_count * 10 + digit.to_digit(10).unwrap() as usize).min(9999);
            return false;
        }
        let buffer = self.focus == Focus::Preview && self.navigation.is_none();
        match self.bindings.resolve(key, buffer) {
            Resolution::Pending => false,
            Resolution::Unbound => {
                self.motion_count = 0;
                false
            }
            Resolution::Action(action) => {
                let count = self.motion_count.max(1);
                let explicit = self.motion_count > 0;
                self.motion_count = 0;
                self.dispatch(action, count, explicit)
            }
        }
    }

    #[cfg(test)]
    pub fn key(&mut self, key: KeyCode) -> bool {
        self.key_event(KeyEvent::new(key, KeyModifiers::NONE))
    }

    fn dispatch(&mut self, action: Action, count: usize, explicit: bool) -> bool {
        use Action::*;
        let buffer = self.focus == Focus::Preview && self.navigation.is_none();
        match action {
            Quit => return true,
            CloseDiffView if self.capture.unavailable.is_none() => self.close_preview(),
            GoBack => self.back(),
            SearchSymbols => self.begin_navigation(Operation::Search),
            GoToDefinition => self.begin_navigation(Operation::Definitions),
            GoToReferences => self.begin_navigation(Operation::References),
            AnalysisDetails if self.navigation.is_some() => {
                let coverage = self
                    .navigation
                    .as_ref()
                    .expect("navigation")
                    .coverage
                    .clone();
                self.save_view();
                self.navigation = None;
                self.source = None;
                self.preview = Some(Preview::original(coverage.replace("; ", "\n").as_bytes()));
                self.top = 0;
                self.left = 0;
                self.cursor = (0, 0);
                self.focus = Focus::Preview;
                self.status = format!("Analysis details; {} returns", self.bindings.label(GoBack));
            }
            OldSide => self.change_side(ComparedSide::Old),
            NewSide => self.change_side(ComparedSide::New),
            OpenDiffView if self.navigation.is_some() => self.open_candidate(),
            OpenDiffView => self.open_diff(),
            SwitchPane if self.preview.is_some() && self.capture.unavailable.is_none() => {
                self.focus = if self.focus == Focus::Files {
                    Focus::Preview
                } else {
                    Focus::Files
                };
            }
            ToggleMark if self.navigation.is_some() || self.source.is_some() => {
                self.status = format!(
                    "Close the source view with {} before marking",
                    self.bindings.label(CloseDiffView)
                );
            }
            ToggleMark => self.toggle_mark(),
            CursorDown | CursorUp => {
                let direction = if action == CursorDown {
                    count as isize
                } else {
                    -(count as isize)
                };
                if let Some(nav) = &mut self.navigation {
                    nav.selected = nav
                        .selected
                        .saturating_add_signed(direction)
                        .min(nav.candidates.len().saturating_sub(1));
                    self.render_candidates();
                } else if self.focus == Focus::Files {
                    self.select(direction);
                } else {
                    self.move_cursor(direction, 0);
                }
            }
            CursorLeft | CursorRight if buffer => self.move_cursor(
                0,
                if action == CursorLeft {
                    -(count as isize)
                } else {
                    count as isize
                },
            ),
            FirstLine => self.go_to_row(count.saturating_sub(1)),
            LastLine => {
                let last = if let Some(nav) = &self.navigation {
                    nav.candidates.len()
                } else if self.focus == Focus::Files {
                    self.capture.items.len()
                } else {
                    self.preview.as_ref().map_or(0, |p| p.lines.len())
                };
                self.go_to_row(if explicit {
                    count.saturating_sub(1)
                } else {
                    last.saturating_sub(1)
                });
            }
            PageUp | PageDown | HalfPageUp | HalfPageDown => {
                let half = matches!(action, HalfPageUp | HalfPageDown);
                let rows = if half {
                    self.viewport.0 / 2
                } else {
                    self.viewport.0
                };
                let direction = if matches!(action, PageUp | HalfPageUp) {
                    -1
                } else {
                    1
                };
                self.scroll_preview(direction * rows.max(1).saturating_mul(count) as isize, 0);
            }
            WordForward | WordBackward | WordEnd | BigWordForward | BigWordBackward
            | BigWordEnd
                if buffer =>
            {
                let motion = match action {
                    WordForward => 'w',
                    WordBackward => 'b',
                    WordEnd => 'e',
                    BigWordForward => 'W',
                    BigWordBackward => 'B',
                    _ => 'E',
                };
                if let Some(preview) = &self.preview {
                    self.cursor = crate::motion::word(&preview.lines, self.cursor, motion, count);
                }
                self.keep_cursor_visible();
            }
            LineStart | FirstNonblank | LineEnd if buffer => {
                if let Some(line) = self
                    .preview
                    .as_ref()
                    .and_then(|p| p.lines.get(self.cursor.0))
                {
                    self.cursor.1 = match action {
                        LineStart => 0,
                        FirstNonblank => {
                            use unicode_width::UnicodeWidthStr;
                            let text = line.to_string();
                            text[..text.len() - text.trim_start().len()].width()
                        }
                        _ => line.width().saturating_sub(1),
                    };
                }
                self.keep_cursor_visible();
            }
            SearchWord | SearchWordBackward | SearchNext | SearchPrevious if buffer => {
                self.search_buffer(
                    KeyCode::Char(match action {
                        SearchWord => '*',
                        SearchWordBackward => '#',
                        SearchNext => 'n',
                        _ => 'N',
                    }),
                    count,
                );
            }
            _ => {}
        }
        self.clamp_scroll();
        false
    }

    fn open_diff(&mut self) {
        if let Some(item) = self.capture.items.get(self.selected) {
            self.source = None;
            self.cursor = (0, 0);
            self.navigation = None;
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

    fn search_buffer(&mut self, key: KeyCode, count: usize) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        let mut origin = self.cursor;
        if matches!(key, KeyCode::Char('*' | '#')) {
            let Some((search, start)) = crate::search::BufferSearch::at_cursor(
                &preview.lines,
                self.cursor,
                key == KeyCode::Char('#'),
            ) else {
                self.status = "No word under cursor".into();
                return;
            };
            preview.search = Some(search);
            origin = start;
        }
        let Some(search) = &preview.search else {
            self.status = "No previous buffer search".into();
            return;
        };
        if let Some(found) = search.next(origin, key == KeyCode::Char('N'), count) {
            self.cursor = (found.row, found.start);
            let index = search.matches.iter().position(|m| *m == found).unwrap_or(0);
            self.status = format!("/{} · {}/{}", search.word, index + 1, search.matches.len());
            self.keep_cursor_visible();
        }
    }

    fn go_to_row(&mut self, row: usize) {
        if let Some(nav) = &mut self.navigation {
            nav.selected = row.min(nav.candidates.len().saturating_sub(1));
            self.render_candidates();
        } else if self.focus == Focus::Files {
            self.selected = row.min(self.capture.items.len().saturating_sub(1));
            self.select(0);
        } else {
            self.cursor.0 = row;
            self.keep_cursor_visible();
        }
    }

    fn close_preview(&mut self) {
        self.abandon_navigation();
        self.preview = None;
        self.focus = Focus::Files;
        self.cursor = (0, 0);
        self.top = 0;
        self.left = 0;
        self.status.clear();
    }

    fn select(&mut self, direction: isize) {
        self.abandon_navigation();
        self.selected = self
            .selected
            .saturating_add_signed(direction)
            .min(self.capture.items.len().saturating_sub(1));
        self.preview = None;
        self.focus = Focus::Files;
        self.cursor = (0, 0);
        self.top = 0;
        self.left = 0;
        let height = self.file_area.height as usize;
        if self.selected < self.file_offset {
            self.file_offset = self.selected;
        }
        if height > 0 && self.selected >= self.file_offset + height {
            self.file_offset = self.selected + 1 - height;
        }
    }

    fn save_view(&mut self) {
        if self.history.len() == 64 {
            self.history.remove(0);
        }
        self.history.push(View {
            preview: self.preview.clone(),
            top: self.top,
            left: self.left,
            side: self.side,
            source: self.source,
            cursor: self.cursor,
            navigation: self.navigation.clone(),
        });
    }
    fn invalidate_analysis(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.pending = None;
    }
    fn abandon_navigation(&mut self) {
        self.invalidate_analysis();
        self.navigation = None;
        self.source = None;
        self.history.clear();
    }
    fn back(&mut self) {
        self.invalidate_analysis();
        if let Some(view) = self.history.pop() {
            self.preview = view.preview;
            self.top = view.top;
            self.left = view.left;
            self.side = view.side;
            self.source = view.source;
            self.cursor = view.cursor;
            self.navigation = view.navigation;
            self.focus = if self.preview.is_some() {
                Focus::Preview
            } else {
                Focus::Files
            };
        } else {
            self.navigation = None;
            self.status = "No earlier navigation target".into();
        }
    }
    fn begin_navigation(&mut self, operation: Operation) {
        if self.capture.unavailable.is_some() {
            self.status = "Navigation unavailable: comparison provenance is opaque".into();
            return;
        }
        if let Some(candidate) = self
            .navigation
            .as_ref()
            .and_then(|nav| nav.candidates.get(nav.selected))
        {
            self.side = self.capture.catalog.sources[candidate.source].side;
        }
        let query = if operation == Operation::Search {
            String::new()
        } else {
            self.symbol_at_view().unwrap_or_default()
        };
        let position = if operation == Operation::Search {
            None
        } else {
            self.position_at_view()
        };
        if let Some((source, _, _)) = position {
            self.side = self.capture.catalog.sources[source].side;
        }
        if operation != Operation::Search && self.navigation.is_none() && position.is_none() {
            self.status =
                "Navigation unavailable: cursor has no mapped captured source position".into();
            return;
        }
        self.save_view();
        self.invalidate_analysis();
        self.navigation = Some(Navigation {
            position,
            operation,
            query,
            editing: operation == Operation::Search,
            candidates: Vec::new(),
            selected: 0,
            coverage: String::new(),
        });
        self.focus = Focus::Preview;
        if operation != Operation::Search {
            self.submit();
        }
    }
    fn submit(&mut self) {
        if let Some(nav) = &self.navigation {
            self.generation = self.generation.wrapping_add(1);
            self.pending = Some(Request {
                generation: self.generation,
                position: nav.position,
                operation: nav.operation,
                query: nav.query.clone(),
                side: if nav.operation == Operation::Search {
                    None
                } else {
                    Some(self.side)
                },
            });
            self.status = "Searching…".into();
            self.poll_analysis();
        }
    }
    pub fn poll_analysis(&mut self) {
        while let Some(results) = self.worker.poll() {
            if results.generation != self.generation {
                continue;
            }
            if let Some(nav) = &mut self.navigation {
                nav.query = results.query;
                nav.candidates = results.candidates;
                nav.coverage = results.coverage;
                nav.selected = 0;
                self.render_candidates();
            }
        }
        if let Some(request) = self.pending.take() {
            self.pending = self.worker.request(request).err();
        }
    }
    fn render_candidates(&mut self) {
        let Some(nav) = &self.navigation else {
            return;
        };
        let mut text = format!(
            "{:?} candidates for {:?} — {} results\n{} opens; {} returns; {} old / {} new\n",
            nav.operation,
            nav.query,
            nav.candidates.len(),
            self.bindings.label(Action::OpenDiffView),
            self.bindings.label(Action::GoBack),
            self.bindings.label(Action::OldSide),
            self.bindings.label(Action::NewSide),
        );
        for (index, candidate) in nav.candidates.iter().enumerate() {
            let source = &self.capture.catalog.sources[candidate.source];
            text.push_str(&format!(
                "{} {} {}:{}:{} {} {}\n",
                if index == nav.selected { ">" } else { " " },
                source.side.label(),
                candidate_field(&crate::git::visible_bytes(&source.path)),
                candidate.tag.line,
                candidate.tag.col + 1,
                candidate_field(&candidate.tag.name),
                candidate_field(candidate.tag.context.as_deref().unwrap_or(""))
            ));
        }
        self.preview = Some(Preview::original(text.as_bytes()));
        self.source = None;
        self.top = nav
            .selected
            .saturating_sub(self.viewport.0.saturating_sub(3));
        self.left = 0;
        self.cursor = (nav.selected + 2, 0);
        self.status = if nav.coverage.contains("Analysis failed") {
            format!(
                "Analysis failed; {} for details",
                self.bindings.label(Action::AnalysisDetails)
            )
        } else if nav.coverage.contains("no tagged symbol") {
            "No symbol at cursor".into()
        } else {
            format!("{} results", nav.candidates.len())
        };
    }
    fn open_candidate(&mut self) {
        let Some(candidate) = self
            .navigation
            .as_ref()
            .and_then(|nav| nav.candidates.get(nav.selected))
            .cloned()
        else {
            self.status =
                "No candidate selected; empty results never prove complete analysis".into();
            return;
        };
        let source = &self.capture.catalog.sources[candidate.source];
        let (preview, left) = match Preview::target(
            source,
            candidate.tag.line as usize,
            candidate.tag.col as usize,
            candidate.tag.len as usize,
        ) {
            Ok(target) => target,
            Err(error) => {
                self.status = error.to_string();
                return;
            }
        };
        let side = source.side;
        self.save_view();
        self.navigation = None;
        self.preview = Some(preview);
        self.side = side;
        self.source = Some(candidate.source);
        self.top = candidate.tag.line.saturating_sub(1) as usize;
        self.cursor = (candidate.tag.line.saturating_sub(1) as usize, left);
        // Source columns are Unicode scalars; rendered prefixes/backslash/tab
        // expansion have separate screen coordinates.
        self.left = left;
        self.focus = Focus::Preview;
        self.status.clear();
    }
    fn change_side(&mut self, side: ComparedSide) {
        if let Some(nav) = &mut self.navigation {
            self.side = side;
            if nav.operation == Operation::Search {
                if let Some(selected) = nav.candidates.iter().position(|candidate| {
                    self.capture.catalog.sources[candidate.source].side == side
                }) {
                    nav.selected = selected;
                    self.render_candidates();
                }
            } else {
                nav.position = None;
                self.submit();
            }
            return;
        }
        if let Some(id) = self.source {
            let path = self.capture.catalog.sources[id].path.clone();
            if let Some(other) = self.capture.catalog.find(side, &path) {
                self.side = side;
                self.source = Some(other);
                self.preview = Some(Preview::source(&self.capture.catalog.sources[other]));
                self.keep_cursor_visible();
            } else {
                self.status = format!("{} captured side unavailable for this path", side.label());
            }
        } else {
            self.side = side;
            self.status.clear();
        }
    }
    fn symbol_at_view(&self) -> Option<String> {
        self.navigation
            .as_ref()
            .and_then(|nav| nav.candidates.get(nav.selected))
            .map(|candidate| candidate.tag.name.to_string())
    }
    fn position_at_view(&self) -> Option<(usize, usize, usize)> {
        if self.navigation.is_some() {
            return None;
        }
        let preview = self.preview.as_ref()?;
        let (old, new) = *preview.locations.get(self.cursor.0)?;
        let (side, line) = match (old, new) {
            (Some(line), None) => (ComparedSide::Old, line),
            (None, Some(line)) => (ComparedSide::New, line),
            (Some(old), Some(new)) => (
                self.side,
                if self.side == ComparedSide::Old {
                    old
                } else {
                    new
                },
            ),
            (None, None) => return None,
        };
        let col = preview.visible_hit(
            self.cursor.0,
            self.cursor.1,
            self.left,
            self.viewport.1.max(1),
        )?;
        let id = if let Some(id) = self.source {
            id
        } else {
            let item = self.capture.items.get(self.selected)?;
            let path = if side == ComparedSide::Old {
                &item.old_path
            } else {
                &item.new_path
            };
            self.capture.catalog.find(side, path)?
        };
        // The catalog is the only authority, including bounds and scalar columns.
        let text = std::str::from_utf8(&self.capture.catalog.sources[id].bytes).ok()?;
        (text
            .split_terminator('\n')
            .nth(line.checked_sub(1)?)?
            .chars()
            .nth(col)
            .is_some())
        .then_some((id, line, col))
    }

    fn toggle_mark(&mut self) {
        if self.capture.unavailable.is_some() {
            self.status = "Marks are unavailable: file boundaries were not proven".into();
            return;
        }
        if self.state_error {
            return;
        }
        if self
            .capture
            .items
            .get(self.selected)
            .is_some_and(|item| item.canonical_identity.is_none())
            && !self.marks.get(self.selected).copied().unwrap_or(false)
        {
            self.status =
                "Mark unavailable: complete compared contents could not be proven for this item"
                    .into();
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

// Candidate selection/scrolling relies on exactly one display row per target.
fn candidate_field(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_control() {
                ch.escape_default().to_string()
            } else {
                ch.to_string()
            }
        })
        .collect()
}

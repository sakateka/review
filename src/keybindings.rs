use anyhow::{Result, bail};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Keymap {
    #[default]
    Vim,
    Emacs,
}
impl Keymap {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "vim" => Ok(Self::Vim),
            "emacs" => Ok(Self::Emacs),
            _ => bail!("unknown keymap {value:?}; expected vim or emacs"),
        }
    }
}

macro_rules! actions {
    ($($variant:ident => $name:literal),* $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        pub enum Action { $($variant),* }
        impl Action {
            pub const ALL: &'static [Self] = &[$(Self::$variant),*];
            pub fn name(self) -> &'static str { match self { $(Self::$variant => $name),* } }
        }
    }
}
actions! {
    CursorDown => "cursor_down", CursorUp => "cursor_up", CursorLeft => "cursor_left", CursorRight => "cursor_right",
    WordForward => "word_forward", WordBackward => "word_backward", WordEnd => "word_end",
    BigWordForward => "big_word_forward", BigWordBackward => "big_word_backward", BigWordEnd => "big_word_end",
    LineStart => "line_start", FirstNonblank => "first_nonblank", LineEnd => "line_end",
    FirstLine => "first_line", LastLine => "last_line", HalfPageUp => "half_page_up", HalfPageDown => "half_page_down",
    PageUp => "page_up", PageDown => "page_down", SearchWord => "search_word", SearchWordBackward => "search_word_backward",
    SearchNext => "search_next", SearchPrevious => "search_previous", OpenDiffView => "open_diff_view", CloseDiffView => "close_diff_view",
    SwitchPane => "switch_pane", CloseOrQuit => "close_or_quit", Quit => "quit", SearchSymbols => "search_symbols", GoToDefinition => "go_to_definition",
    GoToReferences => "go_to_references", GoBack => "go_back", OldSide => "old_side", NewSide => "new_side",
    AnalysisDetails => "analysis_details", ToggleMark => "toggle_mark",
}
#[derive(Clone)]
struct Binding {
    action: Action,
    keys: Vec<KeyEvent>,
    label: String,
    supplemental: bool,
}
#[derive(Clone)]
pub struct Bindings {
    pub keymap: Keymap,
    bindings: Vec<Binding>,
    pending: Vec<KeyEvent>,
    bound_digits: Vec<char>,
}
pub enum Resolution {
    Action(Action),
    Pending,
    Unbound,
}
impl Default for Bindings {
    fn default() -> Self {
        Self::new(Keymap::Vim, true, &BTreeMap::new()).expect("valid defaults")
    }
}
impl Bindings {
    pub fn new(
        keymap: Keymap,
        readline: bool,
        overrides: &BTreeMap<String, String>,
    ) -> Result<Self> {
        use Action::*;
        let mut bindings = Vec::new();
        let defaults: &[(Action, &[&str])] = &[
            (CursorDown, &["down"]),
            (CursorUp, &["up"]),
            (CursorLeft, &["left"]),
            (CursorRight, &["right"]),
            (PageUp, &["pageup"]),
            (PageDown, &["pagedown"]),
            (OpenDiffView, &["enter"]),
            (CloseDiffView, &["esc"]),
            (SwitchPane, &["tab"]),
            (CloseOrQuit, &["q"]),
            (Quit, &[",q"]),
            (SearchSymbols, &["/"]),
            (OldSide, &["["]),
            (NewSide, &["]"]),
            (AnalysisDetails, &["?"]),
            (ToggleMark, &["space"]),
            (SearchWord, &["*"]),
            (SearchWordBackward, &["#"]),
            (SearchNext, &["n"]),
            (SearchPrevious, &["N"]),
        ];
        let vim: &[(Action, &[&str])] = &[
            (GoToDefinition, &["gd"]),
            (GoToReferences, &["gr"]),
            (GoBack, &["ctrl-o"]),
            (CursorDown, &["j"]),
            (CursorUp, &["k"]),
            (CursorLeft, &["h"]),
            (CursorRight, &["l"]),
            (WordForward, &["w"]),
            (WordBackward, &["b"]),
            (WordEnd, &["e"]),
            (BigWordForward, &["W"]),
            (BigWordBackward, &["B"]),
            (BigWordEnd, &["E"]),
            (LineStart, &["0"]),
            (FirstNonblank, &["^"]),
            (LineEnd, &["$"]),
            (FirstLine, &["gg"]),
            (LastLine, &["G"]),
            (HalfPageUp, &["ctrl-u"]),
            (HalfPageDown, &["ctrl-d"]),
            (PageUp, &["ctrl-b"]),
            (PageDown, &["ctrl-f"]),
        ];
        let emacs: &[(Action, &[&str])] = &[
            (GoToDefinition, &["alt-."]),
            (GoToReferences, &["alt-?"]),
            (GoBack, &["alt-,"]),
            (CursorDown, &["ctrl-n"]),
            (CursorUp, &["ctrl-p"]),
            (CursorLeft, &["ctrl-b"]),
            (CursorRight, &["ctrl-f"]),
            (WordForward, &["alt-f"]),
            (WordBackward, &["alt-b"]),
            (LineStart, &["ctrl-a"]),
            (LineEnd, &["ctrl-e"]),
            (FirstLine, &["alt-<"]),
            (LastLine, &["alt->"]),
            (PageDown, &["ctrl-v"]),
            (PageUp, &["alt-v"]),
        ];
        for (action, labels) in
            defaults
                .iter()
                .chain(if keymap == Keymap::Vim { vim } else { emacs })
        {
            if !overrides.contains_key(action.name()) {
                for label in *labels {
                    bindings.push(Binding {
                        action: *action,
                        keys: parse_keys(label)?,
                        label: label.to_string(),
                        supplemental: false,
                    });
                }
            }
        }
        for (name, label) in overrides {
            let Some(action) = Action::ALL.iter().find(|a| a.name() == name) else {
                bail!("unknown keybindings action {name:?}");
            };
            if !label.is_empty() {
                bindings.push(Binding {
                    action: *action,
                    keys: parse_keys(label)?,
                    label: label.clone(),
                    supplemental: false,
                });
            }
        }
        for (index, binding) in bindings.iter().enumerate() {
            for other in &bindings[..index] {
                if collides(&binding.keys, &other.keys) {
                    bail!(
                        "ambiguous keybindings: {} ({}) conflicts with {} ({}); remap or disable one action",
                        binding.action.name(),
                        binding.label,
                        other.action.name(),
                        other.label
                    );
                }
            }
        }
        if readline {
            for (action, label) in [
                (LineStart, "ctrl-a"),
                (LineEnd, "ctrl-e"),
                (CursorDown, "ctrl-n"),
                (CursorUp, "ctrl-p"),
                (WordBackward, "alt-b"),
                (WordForward, "alt-f"),
            ] {
                // An override replaces supplemental defaults too.
                if overrides.contains_key(action.name()) {
                    continue;
                }
                let keys = parse_keys(label)?;
                if !bindings.iter().any(|b| collides(&keys, &b.keys)) {
                    bindings.push(Binding {
                        action,
                        keys,
                        label: label.into(),
                        supplemental: true,
                    });
                }
            }
        }
        let bound_digits = overrides
            .values()
            .filter(|s| !s.is_empty())
            .filter_map(|s| {
                parse_keys(s)
                    .ok()
                    .and_then(|keys| keys.first().copied())
                    .and_then(|key| {
                        if key.modifiers.is_empty()
                            && let KeyCode::Char(c @ '0'..='9') = key.code
                        {
                            Some(c)
                        } else {
                            None
                        }
                    })
            })
            .collect();
        Ok(Self {
            keymap,
            bindings,
            pending: Vec::new(),
            bound_digits,
        })
    }
    pub fn count_digit(&self, digit: char) -> bool {
        !self.bound_digits.contains(&digit)
    }
    pub fn clear(&mut self) {
        self.pending.clear();
    }
    pub fn pending(&self) -> bool {
        !self.pending.is_empty()
    }
    pub fn pending_keys(&self) -> &[KeyEvent] {
        &self.pending
    }
    pub fn resolve(&mut self, key: KeyEvent, buffer: bool) -> Resolution {
        self.resolve_filtered(key, buffer, false)
    }
    pub fn resolve_quit(&mut self, key: KeyEvent) -> Resolution {
        self.resolve_filtered(key, false, true)
    }
    fn resolve_filtered(&mut self, key: KeyEvent, buffer: bool, only_quit: bool) -> Resolution {
        self.pending.push(normalize(key));
        for binding in &self.bindings {
            if (binding.supplemental && !buffer) || (only_quit && binding.action != Action::Quit) {
                continue;
            }
            if binding.keys.starts_with(&self.pending) {
                if binding.keys.len() == self.pending.len() {
                    let action = binding.action;
                    self.pending.clear();
                    return Resolution::Action(action);
                }
                return Resolution::Pending;
            }
        }
        self.pending.clear();
        Resolution::Unbound
    }
    pub fn label(&self, action: Action) -> String {
        self.bindings
            .iter()
            .filter(|b| b.action == action && !b.supplemental)
            .map(|b| b.label.as_str())
            .collect::<Vec<_>>()
            .join("/")
    }
    pub fn help(&self) -> String {
        Action::ALL
            .iter()
            .map(|action| {
                let labels = self
                    .bindings
                    .iter()
                    .filter(|binding| binding.action == *action)
                    .map(|binding| {
                        if binding.supplemental {
                            format!("{} (buffers)", binding.label)
                        } else {
                            binding.label.clone()
                        }
                    })
                    .collect::<Vec<_>>();
                format!(
                    "  {}: {}\n",
                    action.name(),
                    if labels.is_empty() {
                        "(disabled)".into()
                    } else {
                        labels.join(", ")
                    }
                )
            })
            .collect()
    }
    pub fn legend(&self) -> String {
        use Action::*;
        [
            (CursorDown, "down"),
            (OpenDiffView, "open"),
            (SearchWord, "word"),
            (GoToDefinition, "definition"),
            (GoBack, "back"),
            (CloseDiffView, "close"),
            (CloseOrQuit, "close/quit"),
            (Quit, "quit"),
        ]
        .into_iter()
        .filter_map(|(action, text)| {
            let label = self.label(action);
            (!label.is_empty()).then(|| format!("{label} {text}"))
        })
        .collect::<Vec<_>>()
        .join(" · ")
    }
}
fn collides(a: &[KeyEvent], b: &[KeyEvent]) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
fn normalize(mut key: KeyEvent) -> KeyEvent {
    key.kind = crossterm::event::KeyEventKind::Press;
    key.state = crossterm::event::KeyEventState::NONE;
    if let KeyCode::Char(c) = key.code {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            key.code = KeyCode::Char(c.to_ascii_lowercase());
        }
        if !key.modifiers.contains(KeyModifiers::CONTROL)
            && (c.is_uppercase() || !c.is_alphabetic())
        {
            key.modifiers.remove(KeyModifiers::SHIFT);
        }
    }
    // Legacy terminal bytes cannot distinguish these chords from Tab/Enter.
    if key.modifiers == KeyModifiers::CONTROL {
        match key.code {
            KeyCode::Char('i') => {
                key.code = KeyCode::Tab;
                key.modifiers = KeyModifiers::NONE;
            }
            KeyCode::Char('m') => {
                key.code = KeyCode::Enter;
                key.modifiers = KeyModifiers::NONE;
            }
            _ => {}
        }
    }
    key
}
pub fn parse_keys(value: &str) -> Result<Vec<KeyEvent>> {
    let mut result = Vec::new();
    for token in value.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        let (modifiers, key) = if lower.starts_with("ctrl-") {
            (KeyModifiers::CONTROL, &token[5..])
        } else if lower.starts_with("alt-") {
            (KeyModifiers::ALT, &token[4..])
        } else {
            (KeyModifiers::NONE, token)
        };
        let code = match key.to_ascii_lowercase().as_str() {
            "esc" | "escape" => Some(KeyCode::Esc),
            "enter" => Some(KeyCode::Enter),
            "tab" => Some(KeyCode::Tab),
            "backspace" => Some(KeyCode::Backspace),
            "space" => Some(KeyCode::Char(' ')),
            "up" => Some(KeyCode::Up),
            "down" => Some(KeyCode::Down),
            "left" => Some(KeyCode::Left),
            "right" => Some(KeyCode::Right),
            "pageup" => Some(KeyCode::PageUp),
            "pagedown" => Some(KeyCode::PageDown),
            "home" => Some(KeyCode::Home),
            "end" => Some(KeyCode::End),
            _ => None,
        };
        if let Some(code) = code {
            result.push(KeyEvent::new(code, modifiers));
        } else if modifiers.is_empty()
            && key.chars().all(|c| !c.is_control() && !c.is_whitespace())
            && key.chars().count() <= 4
            && !key.contains('-')
        {
            result.extend(
                key.chars()
                    .map(|c| KeyEvent::new(KeyCode::Char(c), modifiers)),
            );
        } else if key.chars().count() == 1 {
            result.push(KeyEvent::new(
                KeyCode::Char(key.chars().next().unwrap()),
                modifiers,
            ));
        } else {
            bail!(
                "invalid key {token:?}; use a character, named key (enter, esc, space, arrows, pageup), ctrl-/alt- chord or sequence such as gd"
            );
        }
    }
    if result.is_empty() {
        bail!("invalid empty key sequence; use an empty string to disable an action");
    }
    for key in &result {
        if key.modifiers == KeyModifiers::CONTROL
            && let KeyCode::Char(character) = key.code
            && !character.is_ascii_alphabetic()
            && !matches!(character, ' ' | '4'..='7')
        {
            bail!(
                "unsupported Ctrl character {character:?}; use an ASCII letter, ctrl-space, digits ctrl-4 through ctrl-7, or a named terminal key such as esc or backspace"
            );
        }
    }
    let result: Vec<_> = result.into_iter().map(normalize).collect();
    if result.iter().skip(1).any(|key| key.code == KeyCode::Esc) {
        bail!(
            "Escape cancels pending key sequences; use a different key after the first key instead of Escape in {value:?}"
        );
    }
    if result
        .iter()
        .any(|key| key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
    {
        bail!("ctrl-c is reserved for terminal safety exit");
    }
    Ok(result)
}

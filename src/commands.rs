//! The `:` command line and the command palette: a small registry of commands
//! and settings, their completion, palette listing and execution. Commands
//! only change runtime state; they never write configuration, review state,
//! sources or Git.

use crate::{app::App, theme::Theme};

/// Maximum command text length in bytes.
const LIMIT: usize = 256;

pub struct Command {
    pub name: &'static str,
    pub usage: &'static str,
    pub summary: &'static str,
    /// Completion values of the optional argument, if it has a fixed set.
    values: Option<fn() -> Vec<&'static str>>,
}

/// The command whose arguments are the registered settings.
const SET: &str = "set";

pub const COMMANDS: &[Command] = &[
    Command {
        name: "colorscheme",
        usage: "colorscheme [<name>]",
        summary: "show or switch the color theme for this session",
        values: Some(Theme::names),
    },
    Command {
        name: SET,
        usage: "set <key>[=<value>]",
        summary: "show or change a runtime setting",
        values: None,
    },
];

pub struct Setting {
    pub key: &'static str,
    pub summary: &'static str,
    values: fn() -> Vec<&'static str>,
}

pub const COLORSCHEME: &str = "appearance.colorscheme";

pub const SETTINGS: &[Setting] = &[Setting {
    key: COLORSCHEME,
    summary: "color theme (runtime only; [appearance] colorscheme sets the startup theme)",
    values: Theme::names,
}];

#[derive(Debug, Default, Clone)]
pub struct CommandLine {
    pub text: String,
    completion: Option<Completion>,
}

#[derive(Debug, Clone)]
struct Completion {
    prefix: String,
    candidates: Vec<&'static str>,
    index: usize,
}

impl CommandLine {
    pub fn push(&mut self, character: char) {
        if self.text.len() + character.len_utf8() <= LIMIT {
            self.text.push(character);
        }
        self.completion = None;
    }

    pub fn pop(&mut self) {
        self.text.pop();
        self.completion = None;
    }

    /// Complete the word at the end of the text in place; repeated calls
    /// cycle through the matches. Without matches the text is unchanged.
    pub fn complete(&mut self, backward: bool) {
        if let Some(completion) = &mut self.completion {
            let count = completion.candidates.len();
            completion.index = if backward {
                (completion.index + count - 1) % count
            } else {
                (completion.index + 1) % count
            };
        } else {
            let (prefix, partial, all) = candidates(&self.text);
            let candidates: Vec<_> = all
                .into_iter()
                .filter(|candidate| {
                    candidate
                        .to_lowercase()
                        .starts_with(&partial.to_lowercase())
                        && prefix.len() + candidate.len() <= LIMIT
                })
                .collect();
            if candidates.is_empty() {
                return;
            }
            self.completion = Some(Completion {
                prefix: prefix.to_owned(),
                index: if backward { candidates.len() - 1 } else { 0 },
                candidates,
            });
        }
        let completion = self.completion.as_ref().expect("completion");
        self.text = format!(
            "{}{}",
            completion.prefix, completion.candidates[completion.index]
        );
    }
}

/// Split text into the fixed prefix, the partial word being completed and
/// all candidates for that position.
fn candidates(text: &str) -> (&str, &str, Vec<&'static str>) {
    let start = text.len() - text.trim_start().len();
    let body = &text[start..];
    let Some(space) = body.find(char::is_whitespace) else {
        return (
            &text[..start],
            body,
            COMMANDS.iter().map(|command| command.name).collect(),
        );
    };
    let name = &body[..space];
    let rest = &body[space..];
    let argument_start = start + space + (rest.len() - rest.trim_start().len());
    let argument = &text[argument_start..];
    match name {
        SET => match argument.find('=') {
            Some(equals) => {
                let key = argument[..equals].trim();
                let values = SETTINGS
                    .iter()
                    .find(|setting| setting.key == key)
                    .map(|setting| (setting.values)())
                    .unwrap_or_default();
                let after = &argument[equals + 1..];
                let value_start =
                    argument_start + equals + 1 + (after.len() - after.trim_start().len());
                (&text[..value_start], &text[value_start..], values)
            }
            None => (
                &text[..argument_start],
                argument,
                SETTINGS.iter().map(|setting| setting.key).collect(),
            ),
        },
        _ => match COMMANDS
            .iter()
            .find(|command| command.name == name)
            .and_then(|command| command.values)
        {
            Some(values) => (&text[..argument_start], argument, values()),
            None => (text, "", Vec::new()),
        },
    }
}

/// One runnable palette line: command text and its registry summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub text: String,
    pub summary: &'static str,
}

/// Every palette entry, in registry order: each command bare and with each
/// argument value; `set` is expanded per setting (`set <key>` and
/// `set <key>=<value>`) instead of being listed bare, since bare `set` only
/// reports an error.
pub fn entries() -> Vec<Entry> {
    let mut entries = Vec::new();
    for command in COMMANDS {
        if command.name == SET {
            for setting in SETTINGS {
                entries.push(Entry {
                    text: format!("{SET} {}", setting.key),
                    summary: setting.summary,
                });
                entries.extend((setting.values)().into_iter().map(|value| Entry {
                    text: format!("{SET} {}={value}", setting.key),
                    summary: setting.summary,
                }));
            }
            continue;
        }
        entries.push(Entry {
            text: command.name.to_owned(),
            summary: command.summary,
        });
        if let Some(values) = command.values {
            entries.extend(values().into_iter().map(|value| Entry {
                text: format!("{} {value}", command.name),
                summary: command.summary,
            }));
        }
    }
    entries
}

/// The command palette: a filter query over the registry entries and the
/// selected match.
#[derive(Debug, Clone)]
pub struct Palette {
    pub query: String,
    /// Index into the current matches.
    pub selected: usize,
    entries: Vec<Entry>,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            query: String::new(),
            selected: 0,
            entries: entries(),
        }
    }
}

impl Palette {
    pub fn push(&mut self, character: char) {
        if self.query.len() + character.len_utf8() <= LIMIT {
            self.query.push(character);
            self.selected = 0;
        }
    }

    pub fn pop(&mut self) {
        if self.query.pop().is_some() {
            self.selected = 0;
        }
    }

    /// Move the selection by `delta` matches, clamped to the match list.
    pub fn move_selection(&mut self, delta: isize) {
        let last = self.matches().len().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    /// Entries whose text or summary contains every query word, ignoring case.
    pub fn matches(&self) -> Vec<&Entry> {
        let words: Vec<String> = self
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.entries
            .iter()
            .filter(|entry| {
                let text = entry.text.to_lowercase();
                let summary = entry.summary.to_lowercase();
                words
                    .iter()
                    .all(|word| text.contains(word.as_str()) || summary.contains(word.as_str()))
            })
            .collect()
    }

    /// Command text of the selected match.
    pub fn selected_text(&self) -> Option<String> {
        self.matches()
            .get(self.selected)
            .map(|entry| entry.text.clone())
    }
}

/// Run submitted command text against the application; results and errors
/// are reported in the status row.
pub fn execute(app: &mut App, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let (name, argument) = text
        .split_once(char::is_whitespace)
        .map_or((text, ""), |(name, argument)| (name, argument.trim()));
    match name {
        "colorscheme" => setting(app, COLORSCHEME, (!argument.is_empty()).then_some(argument)),
        "set" if argument.is_empty() => {
            app.status = format!("set requires a setting such as {COLORSCHEME}=<name>");
        }
        "set" => match argument.split_once('=') {
            Some((key, value)) => setting(app, key.trim(), Some(value.trim())),
            None => setting(app, argument, None),
        },
        _ => app.status = format!("Unknown command {name:?}"),
    }
}

fn setting(app: &mut App, key: &str, value: Option<&str>) {
    if key != COLORSCHEME {
        app.status = format!("Unknown setting {key:?}");
        return;
    }
    match value {
        None => app.status = format!("{COLORSCHEME}={}", app.theme.name),
        Some(value) => match Theme::named(value) {
            Some(theme) => {
                app.set_theme(theme);
                app.status = format!("{COLORSCHEME}={} (this session only)", theme.name);
            }
            None => {
                app.status = format!("Unknown colorscheme {value:?}; press Tab to list themes");
            }
        },
    }
}

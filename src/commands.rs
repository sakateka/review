//! The `:` command line: a small registry of commands and settings, their
//! completion and their execution. Commands only change runtime state; they
//! never write configuration, review state, sources or Git.

use crate::{app::App, theme::Theme};

/// Maximum command text length in bytes.
const LIMIT: usize = 256;

pub struct Command {
    pub name: &'static str,
    pub usage: &'static str,
    pub summary: &'static str,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "colorscheme",
        usage: "colorscheme [<name>]",
        summary: "show or switch the color theme for this session",
    },
    Command {
        name: "set",
        usage: "set <key>[=<value>]",
        summary: "show or change a runtime setting",
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
        "colorscheme" => (&text[..argument_start], argument, Theme::names()),
        "set" => match argument.find('=') {
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
        _ => (text, "", Vec::new()),
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

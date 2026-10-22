//! Color themes: the bundled bat themes from two-face plus the original
//! `review-classic` palette and the `gruvbox-baby` Neovim palette. Themes are resolved eagerly into a small
//! copyable table of capture colors and interface roles.

use ratatui::style::{Color, Modifier, Style};
use std::sync::Mutex;
use two_face::re_exports::syntect::{
    highlighting::{Color as ThemeColor, Highlighter, Theme as SyntectTheme},
    parsing::Scope,
};
use two_face::theme::{EmbeddedLazyThemeSet, EmbeddedThemeName};

pub const CLASSIC: &str = "review-classic";
pub const GRUVBOX_BABY: &str = "gruvbox-baby";
pub const DEFAULT: &str = "gruvbox-dark";

/// Tree-sitter capture names that previews may request, with the TextMate
/// scopes consulted in order and the `review-classic` color.
const CAPTURES: &[(&str, &[&str], Color)] = &[
    ("comment", &["comment"], Color::DarkGray),
    ("string", &["string"], Color::Green),
    ("keyword", &["keyword", "storage"], Color::Magenta),
    (
        "function",
        &["entity.name.function", "support.function"],
        Color::Blue,
    ),
    (
        "type",
        &["entity.name.type", "storage.type", "support.type"],
        Color::Cyan,
    ),
    (
        "constant",
        &["constant.language", "constant", "support.constant"],
        Color::Yellow,
    ),
    ("number", &["constant.numeric"], Color::Yellow),
    ("operator", &["keyword.operator"], Color::Cyan),
    ("variable", &["variable.other", "variable"], Color::White),
    (
        "property",
        &["entity.name.tag", "variable.other.member"],
        Color::Cyan,
    ),
    (
        "boolean",
        &["constant.language.boolean", "constant.language"],
        Color::Magenta,
    ),
    (
        "label",
        &[
            "entity.name.other.anchor",
            "entity.name.label",
            "entity.name",
        ],
        Color::Blue,
    ),
    (
        "attribute",
        &["entity.other.attribute-name", "storage.modifier"],
        Color::Magenta,
    ),
    ("punctuation", &["punctuation"], Color::DarkGray),
    (
        "string.special",
        &["constant.other.date", "string"],
        Color::Blue,
    ),
];
const CAPTURE_COUNT: usize = CAPTURES.len();

/// Capture colors of luisiacc/gruvbox-baby (MIT) with its default medium
/// background, taken from its tree-sitter highlight groups.
const GRUVBOX_BABY_CAPTURES: &[(&str, Color)] = &[
    ("comment", Color::Rgb(0x66, 0x5c, 0x54)),
    ("string", Color::Rgb(0x98, 0x97, 0x1a)),
    ("keyword", Color::Rgb(0xfb, 0x49, 0x34)),
    ("function", Color::Rgb(0xee, 0xbd, 0x35)),
    ("type", Color::Rgb(0x8e, 0xc0, 0x7c)),
    ("constant", Color::Rgb(0xd4, 0x87, 0x9c)),
    ("number", Color::Rgb(0xb1, 0x62, 0x86)),
    ("operator", Color::Rgb(0xeb, 0xdb, 0xb2)),
    ("variable", Color::Rgb(0x7f, 0xa2, 0xac)),
    ("property", Color::Rgb(0x45, 0x85, 0x88)),
    ("boolean", Color::Rgb(0xd6, 0x5d, 0x0e)),
    ("label", Color::Rgb(0x45, 0x85, 0x88)),
    ("attribute", Color::Rgb(0xd6, 0x5d, 0x0e)),
    ("punctuation", Color::Rgb(0xeb, 0xdb, 0xb2)),
    ("string.special", Color::Rgb(0x83, 0xa5, 0x98)),
];

/// Index of a capture name in the shared capture table.
pub fn capture(name: &str) -> Option<u8> {
    CAPTURES
        .iter()
        .position(|(capture, _, _)| *capture == name)
        .map(|index| index as u8)
}

/// What a rendered span represents; styles are derived from it per theme.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Base {
    #[default]
    Plain,
    Added,
    Removed,
    Hunk,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Role {
    pub base: Base,
    pub capture: Option<u8>,
    pub emphasis: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub name: &'static str,
    captures: [Option<Color>; CAPTURE_COUNT],
    /// Text without a syntax capture; `None` keeps the terminal foreground.
    pub default_fg: Option<Color>,
    pub added_fg: Color,
    pub added_bg: Color,
    pub removed_fg: Color,
    pub removed_bg: Color,
    pub hunk: Color,
    pub search_bg: Color,
    pub reviewed_mark: Color,
    pub pending_mark: Color,
    pub reviewed_name: Color,
    pub pending_name: Color,
    pub stat_plus: Color,
    pub stat_minus: Color,
    pub border_focused: Color,
    pub border_unfocused: Color,
    pub metrics: Color,
    pub selection: Style,
    /// Comment icon; outdated comments use the gray `comment_outdated`.
    pub comment: Color,
    pub comment_outdated: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self::named(DEFAULT).expect("bundled default theme")
    }
}

static CACHE: Mutex<Vec<Theme>> = Mutex::new(Vec::new());

impl Theme {
    pub fn classic() -> Self {
        let mut captures = [None; CAPTURE_COUNT];
        for (slot, (_, _, color)) in captures.iter_mut().zip(CAPTURES) {
            *slot = Some(*color);
        }
        Self {
            name: CLASSIC,
            captures,
            default_fg: None,
            added_fg: Color::Green,
            added_bg: Color::Rgb(10, 30, 10),
            removed_fg: Color::Red,
            removed_bg: Color::Rgb(35, 10, 10),
            hunk: Color::Cyan,
            search_bg: Color::Rgb(80, 65, 0),
            reviewed_mark: Color::Gray,
            pending_mark: Color::Yellow,
            reviewed_name: Color::DarkGray,
            pending_name: Color::White,
            stat_plus: Color::Green,
            stat_minus: Color::Red,
            border_focused: Color::Cyan,
            border_unfocused: Color::DarkGray,
            metrics: Color::Cyan,
            selection: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            comment: Color::Yellow,
            comment_outdated: Color::DarkGray,
        }
    }

    pub fn gruvbox_baby() -> Self {
        let mut captures = [None; CAPTURE_COUNT];
        for (name, color) in GRUVBOX_BABY_CAPTURES {
            let index = capture(name).expect("known gruvbox-baby capture");
            captures[index as usize] = Some(*color);
        }
        let foreground = Color::Rgb(0xeb, 0xdb, 0xb2);
        let comment = Color::Rgb(0x66, 0x5c, 0x54);
        let soft_yellow = Color::Rgb(0xee, 0xbd, 0x35);
        let dark_gray = Color::Rgb(0x83, 0xa5, 0x98);
        let medium_gray = Color::Rgb(0x50, 0x49, 0x45);
        let added = Color::Rgb(0x68, 0x9d, 0x6a);
        let removed = Color::Rgb(0xfb, 0x49, 0x34);
        Self {
            name: GRUVBOX_BABY,
            captures,
            default_fg: Some(foreground),
            added_fg: added,
            added_bg: Color::Rgb(0x26, 0x33, 0x2c),
            removed_fg: removed,
            removed_bg: Color::Rgb(0x57, 0x2e, 0x33),
            hunk: dark_gray,
            search_bg: medium_gray,
            reviewed_mark: comment,
            pending_mark: soft_yellow,
            reviewed_name: comment,
            pending_name: foreground,
            stat_plus: added,
            stat_minus: removed,
            border_focused: dark_gray,
            border_unfocused: medium_gray,
            metrics: dark_gray,
            selection: Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            comment: soft_yellow,
            comment_outdated: comment,
        }
    }

    /// All selectable theme names, sorted case-insensitively.
    pub fn names() -> Vec<&'static str> {
        let mut names: Vec<_> = EmbeddedLazyThemeSet::theme_names()
            .iter()
            .map(|name| name.as_name())
            .chain([CLASSIC, GRUVBOX_BABY])
            .collect();
        names.sort_by_key(|name| name.to_lowercase());
        names
    }

    /// Case-insensitive lookup of a bundled theme, `review-classic` or
    /// `gruvbox-baby`.
    pub fn named(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case(CLASSIC) {
            return Some(Self::classic());
        }
        if name.eq_ignore_ascii_case(GRUVBOX_BABY) {
            return Some(Self::gruvbox_baby());
        }
        let embedded = *EmbeddedLazyThemeSet::theme_names()
            .iter()
            .find(|embedded| embedded.as_name().eq_ignore_ascii_case(name))?;
        let mut cache = CACHE.lock().unwrap_or_else(|poison| poison.into_inner());
        if let Some(theme) = cache.iter().find(|theme| theme.name == embedded.as_name()) {
            return Some(*theme);
        }
        let theme = Self::resolve(embedded);
        cache.push(theme);
        Some(theme)
    }

    fn resolve(embedded: EmbeddedThemeName) -> Self {
        let set = two_face::theme::extra();
        let source: &SyntectTheme = set.get(embedded);
        let highlighter = Highlighter::new(source);
        let styled = |scopes: &[&str]| -> Vec<ThemeColor> {
            scopes
                .iter()
                .filter_map(|scope| {
                    let scope = Scope::new(scope).ok()?;
                    highlighter.style_mod_for_stack(&[scope]).foreground
                })
                .collect()
        };
        let settings = &source.settings;
        let default_fg = settings.foreground.and_then(terminal_color);
        let background = settings.background.and_then(rgb);
        // Take the first explicitly styled scope, but prefer a later one when
        // the first repeats a color already given to an earlier capture, so
        // that themes reusing one color across scopes keep captures apart.
        let mut captures = [None; CAPTURE_COUNT];
        for index in 0..CAPTURE_COUNT {
            let colors: Vec<_> = styled(CAPTURES[index].1)
                .into_iter()
                .map(terminal_color)
                .collect();
            captures[index] = colors
                .iter()
                .find(|color| !captures[..index].contains(color))
                .or(colors.first())
                .copied()
                .unwrap_or(default_fg);
        }
        let classic = Self::classic();
        // Interface roles must stay readable on the theme background.
        let role = |scopes: &[&str], fallback: Color| {
            styled(scopes)
                .into_iter()
                .filter_map(terminal_color)
                .find(|color| readable(*color, background))
                .unwrap_or(fallback)
        };
        // True-color fallbacks can be blended into true-color backgrounds.
        let (green, red) = if background.is_some() {
            (Color::Rgb(64, 160, 64), Color::Rgb(208, 64, 64))
        } else {
            (Color::Green, Color::Red)
        };
        let mut added_fg = role(&["markup.inserted", "diff.inserted"], green);
        let mut removed_fg = role(&["markup.deleted", "diff.deleted"], red);
        if added_fg == removed_fg {
            (added_fg, removed_fg) = (green, red);
        }
        let tint = |color: Color, ratio: f32| {
            background.and_then(|background| blend(color, background, ratio))
        };
        let (mut added_bg, mut removed_bg) = (
            tint(added_fg, 0.2).unwrap_or(classic.added_bg),
            tint(removed_fg, 0.2).unwrap_or(classic.removed_bg),
        );
        if added_bg == removed_bg {
            (added_bg, removed_bg) = (classic.added_bg, classic.removed_bg);
        }
        let comment = role(&["comment"], Color::DarkGray);
        let foreground = default_fg.unwrap_or(Color::Reset);
        let warning = role(
            &["markup.changed", "diff.changed", "constant.numeric"],
            Color::Yellow,
        );
        let search_bg = settings
            .find_highlight
            .and_then(rgb)
            .or_else(|| rgb_of(warning))
            .and_then(|color| tint(color, 0.4))
            .filter(|color| *color != added_bg && *color != removed_bg)
            .unwrap_or(classic.search_bg);
        let mut accent = settings
            .accent
            .and_then(terminal_color)
            .unwrap_or_else(|| role(&["entity.name.function", "keyword"], Color::Cyan));
        let mut unfocused = settings
            .gutter_foreground
            .and_then(terminal_color)
            .unwrap_or(comment);
        if accent == unfocused {
            (accent, unfocused) = (Color::Cyan, Color::DarkGray);
        }
        let (mut reviewed, mut pending) = (comment, warning);
        if reviewed == pending {
            (reviewed, pending) = (classic.reviewed_mark, classic.pending_mark);
        }
        let mut pending_name = foreground;
        let mut reviewed_name = comment;
        if pending_name == reviewed_name {
            (reviewed_name, pending_name) = (classic.reviewed_name, classic.pending_name);
        }
        Self {
            name: embedded.as_name(),
            captures,
            default_fg,
            added_fg,
            added_bg,
            removed_fg,
            removed_bg,
            hunk: role(
                &["meta.diff.range", "meta.diff.header", "entity.name.section"],
                accent,
            ),
            search_bg,
            reviewed_mark: reviewed,
            pending_mark: pending,
            reviewed_name,
            pending_name,
            stat_plus: added_fg,
            stat_minus: removed_fg,
            border_focused: accent,
            border_unfocused: unfocused,
            metrics: accent,
            selection: classic.selection,
            comment: if pending == comment {
                classic.comment
            } else {
                pending
            },
            comment_outdated: comment,
        }
    }

    /// Color for a capture index, or `None` to keep the base foreground.
    pub fn capture(&self, index: u8) -> Option<Color> {
        self.captures.get(index as usize).copied().flatten()
    }

    pub fn style(&self, role: Role) -> Style {
        let mut style = match role.base {
            Base::Plain => self
                .default_fg
                .map_or_else(Style::default, |fg| Style::default().fg(fg)),
            Base::Added => Style::default().fg(self.added_fg).bg(self.added_bg),
            Base::Removed => Style::default().fg(self.removed_fg).bg(self.removed_bg),
            Base::Hunk => Style::default().fg(self.hunk),
        };
        if let Some(color) = role.capture.and_then(|index| self.capture(index)) {
            style = style.fg(color);
        }
        if role.emphasis {
            style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
        }
        style
    }
}

/// Convert like bat: alpha 0 encodes an ANSI palette index, alpha 1 the
/// terminal default, and anything else a true color.
fn terminal_color(color: ThemeColor) -> Option<Color> {
    match color.a {
        0 => Some(match color.r {
            0 => Color::Black,
            1 => Color::Red,
            2 => Color::Green,
            3 => Color::Yellow,
            4 => Color::Blue,
            5 => Color::Magenta,
            6 => Color::Cyan,
            7 => Color::Gray,
            index => Color::Indexed(index),
        }),
        1 => None,
        _ => Some(Color::Rgb(color.r, color.g, color.b)),
    }
}

fn rgb(color: ThemeColor) -> Option<Color> {
    terminal_color(color).filter(|color| matches!(color, Color::Rgb(..)))
}

/// Whether a true color differs enough in luminance from the background.
fn readable(color: Color, background: Option<Color>) -> bool {
    let luminance = |color| match color {
        Color::Rgb(r, g, b) => Some(0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32),
        _ => None,
    };
    match (luminance(color), background.and_then(luminance)) {
        (Some(color), Some(background)) => (color - background).abs() >= 48.0,
        _ => true,
    }
}

fn rgb_of(color: Color) -> Option<Color> {
    matches!(color, Color::Rgb(..)).then_some(color)
}

/// Mix `ratio` of `color` into `background`; only true colors can be mixed.
fn blend(color: Color, background: Color, ratio: f32) -> Option<Color> {
    let (Color::Rgb(r, g, b), Color::Rgb(br, bg, bb)) = (color, background) else {
        return None;
    };
    let mix = |c: u8, base: u8| (base as f32 + (c as f32 - base as f32) * ratio).round() as u8;
    Some(Color::Rgb(mix(r, br), mix(g, bg), mix(b, bb)))
}

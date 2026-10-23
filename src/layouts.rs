//! Keyboard layout tables that map non-Latin shortcut characters to the US
//! English character on the same physical key, like Vim's `langmap`.

use anyhow::{Result, bail};
use std::collections::HashMap;

/// Built-in layouts active by default, in conflict-resolution order.
pub const DEFAULT: &[&str] = &["ru", "uk", "be"];

type Rows = &'static [(&'static str, &'static str)];

/// Russian ЙЦУКЕН, exactly Vim's `russian-jcukenwin` positions.
const RUSSIAN: Rows = &[
    ("йцукенгшщзхъ", "qwertyuiop[]"),
    ("фывапролджэ", "asdfghjkl;'"),
    ("ячсмитьбю", "zxcvbnm,."),
    ("ё", "`"),
    ("ЙЦУКЕНГШЩЗХЪ", "QWERTYUIOP{}"),
    ("ФЫВАПРОЛДЖЭ", "ASDFGHJKL:\""),
    ("ЯЧСМИТЬБЮ", "ZXCVBNM<>"),
    ("Ё", "~"),
    ("№", "#"),
];

/// Ukrainian, the standard xkb `ua` layout.
const UKRAINIAN: Rows = &[
    ("йцукенгшщзхї", "qwertyuiop[]"),
    ("фівапролджє", "asdfghjkl;'"),
    ("ячсмитьбю", "zxcvbnm,."),
    ("ґ", "\\"),
    ("ЙЦУКЕНГШЩЗХЇ", "QWERTYUIOP{}"),
    ("ФІВАПРОЛДЖЄ", "ASDFGHJKL:\""),
    ("ЯЧСМИТЬБЮ", "ZXCVBNM<>"),
    ("Ґ", "|"),
    // Shifted apostrophe key of the default xkb `ua` (unicode) variant.
    ("\u{2bc}", "~"),
    ("№", "#"),
];

/// Belarusian, the standard xkb `by` layout (its `'` key is already ASCII).
const BELARUSIAN: Rows = &[
    ("йцукенгшўзх", "qwertyuiop["),
    ("фывапролджэ", "asdfghjkl;'"),
    ("ячсмітьбю", "zxcvbnm,."),
    ("ё", "`"),
    ("ЙЦУКЕНГШЎЗХ", "QWERTYUIOP{"),
    ("ФЫВАПРОЛДЖЭ", "ASDFGHJKL:\""),
    ("ЯЧСМІТЬБЮ", "ZXCVBNM<>"),
    ("Ё", "~"),
    ("№", "#"),
];

pub const NAMES: &[&str] = &["ru", "uk", "be"];

fn rows(name: &str) -> Option<Rows> {
    match name {
        "ru" => Some(RUSSIAN),
        "uk" => Some(UKRAINIAN),
        "be" => Some(BELARUSIAN),
        _ => None,
    }
}

/// The pairs of a built-in layout, or `None` for an unknown name.
pub fn pairs(name: &str) -> Option<Vec<(char, char)>> {
    rows(name).map(|rows| {
        rows.iter()
            .flat_map(|(from, to)| from.chars().zip(to.chars()))
            .collect()
    })
}

/// Build the translation map: the first listed built-in layout wins a
/// conflict, and custom `langmap` pairs override every built-in.
pub fn langmap<S: AsRef<str>>(layouts: &[S], custom: &str) -> Result<HashMap<char, char>> {
    let mut map = HashMap::new();
    for name in layouts {
        let name = name.as_ref();
        let Some(pairs) = pairs(name) else {
            bail!(
                "unknown [keyboard] layouts entry {name:?}; expected one of: {}",
                NAMES.join(", ")
            );
        };
        for (from, to) in pairs {
            map.entry(from).or_insert(to);
        }
    }
    map.extend(parse_langmap(custom)?);
    Ok(map)
}

/// Parse Vim `langmap` syntax: comma-separated `{from}{to}` pairs or
/// `{from-list};{to-list}` items; a backslash makes `,`, `;` or `\` literal.
pub fn parse_langmap(text: &str) -> Result<Vec<(char, char)>> {
    let mut result = Vec::new();
    if text.is_empty() {
        return Ok(result);
    }
    // Each item is split into its parts on unescaped `;`.
    let mut items: Vec<Vec<Vec<char>>> = vec![vec![Vec::new()]];
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let Some(escaped) = chars.next() else {
                    bail!("invalid [keyboard] langmap {text:?}: trailing backslash");
                };
                items.last_mut().unwrap().last_mut().unwrap().push(escaped);
            }
            ',' => items.push(vec![Vec::new()]),
            ';' => items.last_mut().unwrap().push(Vec::new()),
            _ => items.last_mut().unwrap().last_mut().unwrap().push(c),
        }
    }
    for item in items {
        match item.as_slice() {
            [pair] if pair.len() == 2 => result.push((pair[0], pair[1])),
            [from, to] if !from.is_empty() && from.len() == to.len() => {
                result.extend(from.iter().copied().zip(to.iter().copied()));
            }
            _ => bail!(
                "invalid [keyboard] langmap {text:?}: use comma-separated pairs such as \"оj,лk\" or equal-length lists such as \"ОЛ;jk\""
            ),
        }
    }
    if let Some((from, _)) = result.iter().find(|(from, _)| from.is_ascii()) {
        bail!(
            "invalid [keyboard] langmap {text:?}: {from:?} is ASCII; only non-ASCII characters are translated"
        );
    }
    Ok(result)
}

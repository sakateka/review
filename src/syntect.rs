//! Syntax highlighting fallback for languages without an embedded tree-sitter
//! grammar: bat's syntect syntax set bundled in two-face, with TextMate scopes
//! mapped onto the shared theme capture table.

use std::{path::Path, sync::LazyLock};
use two_face::re_exports::syntect::{
    parsing::{ParseState, Scope, ScopeStack, SyntaxReference, SyntaxSet},
    util::LinesWithEndings,
};

/// Scope prefixes and the theme capture each one selects.
const SCOPES: &[(&str, &str)] = &[
    ("comment", "comment"),
    ("string", "string"),
    ("markup.raw", "string"),
    ("constant.numeric", "number"),
    ("constant.language.boolean", "boolean"),
    ("constant", "constant"),
    ("support.constant", "constant"),
    ("keyword.operator", "operator"),
    ("keyword", "keyword"),
    ("storage", "keyword"),
    ("storage.modifier", "keyword"),
    ("markup.heading", "keyword"),
    ("storage.type", "type"),
    // Declaration keywords that TextMate grammars file under `storage.type`.
    ("storage.type.function", "keyword"),
    ("storage.type.class", "keyword"),
    ("entity.name.type", "type"),
    ("entity.name.class", "type"),
    ("entity.name.struct", "type"),
    ("entity.name.enum", "type"),
    ("entity.other.inherited-class", "type"),
    ("support.type", "type"),
    ("support.class", "type"),
    ("entity.name.function", "function"),
    ("support.function", "function"),
    ("variable.function", "function"),
    ("entity.name.tag", "property"),
    ("variable.other.member", "property"),
    ("variable.other.property", "property"),
    ("entity.other.attribute-name", "attribute"),
    ("entity.name.label", "label"),
    ("entity.name.section", "label"),
    ("variable", "variable"),
    ("punctuation", "punctuation"),
    // Delimiters take their token color, as in tree-sitter output.
    ("punctuation.definition.string", "string"),
    ("punctuation.definition.comment", "comment"),
    ("markup.underline.link", "string.special"),
];

/// Deserialized on first use only; tree-sitter languages never touch it.
static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(two_face::syntax::extra_newlines);

static TABLE: LazyLock<Vec<(Scope, u8)>> = LazyLock::new(|| {
    SCOPES
        .iter()
        .map(|(prefix, name)| {
            (
                Scope::new(prefix).expect("valid scope prefix"),
                crate::theme::capture(name).expect("known theme capture"),
            )
        })
        .collect()
});

/// Bytes the syntect fallback highlights per side. Lower than the 1 MiB
/// tree-sitter limit: syntect parses about 0.45 MB/s, and `Preview::item`
/// parses both diff sides in turn on the UI thread, so 128 KiB per side keeps
/// a whole diff under 1 s.
pub const LIMIT: usize = 128 * 1024;

/// Detection order: full file name, then extension, then first line.
fn detect<'a>(syntaxes: &'a SyntaxSet, path: &Path, content: &str) -> Option<&'a SyntaxReference> {
    let name = path.file_name().and_then(|name| name.to_str());
    let extension = path.extension().and_then(|extension| extension.to_str());
    name.and_then(|name| syntaxes.find_syntax_by_extension(name))
        .or_else(|| extension.and_then(|extension| syntaxes.find_syntax_by_extension(extension)))
        .or_else(|| {
            content
                .lines()
                .next()
                .and_then(|line| syntaxes.find_syntax_by_first_line(line))
        })
        .filter(|syntax| syntax.name != "Plain Text")
}

/// Capture index for a scope stack, given innermost-last as syntect stores it.
pub fn capture(stack: &[Scope]) -> Option<u8> {
    stack.iter().rev().find_map(|scope| {
        TABLE
            .iter()
            .filter(|(prefix, _)| prefix.is_prefix_of(*scope))
            .max_by_key(|(prefix, _)| prefix.len())
            .map(|(_, capture)| *capture)
    })
}

/// Per-byte capture indices for a file without a tree-sitter grammar.
pub fn highlight(path: &Path, content: &str) -> anyhow::Result<Vec<Option<u8>>> {
    let syntaxes = &*SYNTAXES;
    let Some(syntax) = detect(syntaxes, path, content) else {
        // The empty side of an added or deleted file has nothing to color and
        // no first line to detect from; the other side decides.
        anyhow::ensure!(
            content.is_empty(),
            "syntax highlighting unavailable for this file type"
        );
        return Ok(Vec::new());
    };
    anyhow::ensure!(content.len() <= LIMIT, "source exceeds highlighting limit");
    let mut state = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut bytes = vec![None; content.len()];
    let mut start = 0;
    for line in LinesWithEndings::from(content) {
        let mut current = capture(stack.as_slice());
        let mut offset = 0;
        for (position, op) in state.parse_line(line, syntaxes)? {
            bytes[start + offset..start + position].fill(current);
            stack.apply(&op)?;
            current = capture(stack.as_slice());
            offset = position;
        }
        bytes[start + offset..start + line.len()].fill(current);
        start += line.len();
    }
    Ok(bytes)
}

//! Bounded syntax supplementation over immutable captured bytes.
pub fn classify(path: &[u8], text: &str) -> Option<&'static str> {
    let basename = path.rsplit(|b| *b == b'/').next().unwrap_or(path);
    let extension = basename.rsplit(|b| *b == b'.').next().unwrap_or(basename);
    let shebang = text
        .get(..text.floor_char_boundary(text.len().min(4096)))
        .unwrap_or("")
        .lines()
        .next()
        .unwrap_or("");
    if let Some(shebang) = shebang.strip_prefix("#!") {
        let mut words = shebang.split_whitespace();
        let executable = words.next().unwrap_or("").rsplit('/').next().unwrap_or("");
        let interpreter = if executable == "env" {
            words
                .find(|word| !word.starts_with('-') && !word.contains('='))
                .unwrap_or("")
        } else {
            executable
        };
        let interpreter = interpreter.rsplit('/').next().unwrap_or("");
        if ["zsh", "fish", "ksh", "csh", "tcsh", "nu"].contains(&interpreter) {
            return None;
        }
        if ["sh", "bash"].contains(&interpreter) {
            return Some("buffer.sh");
        }
        if interpreter.starts_with("python") {
            return Some("buffer.py");
        }
        if matches!(extension, b"sh" | b"bash") {
            return None;
        }
    }
    match extension {
        b"rs" => Some("buffer.rs"),
        b"c" | b"h" => Some("buffer.c"),
        b"cc" | b"cpp" | b"cxx" | b"hpp" | b"hxx" | b"hh" | b"h++" | b"ipp" | b"tpp" | b"C"
        | b"H" => Some("buffer.cpp"),
        b"go" => Some("buffer.go"),
        b"zig" => Some("buffer.zig"),
        b"py" => Some("buffer.py"),
        b"sh" | b"bash" => Some("buffer.sh"),
        b"yaml" | b"yml" => Some("yaml"),
        b"json" => Some("json"),
        _ if matches!(basename, b".bashrc" | b".bash_profile" | b".profile") => Some("buffer.sh"),
        _ => None,
    }
}

/// Known extensions can be rejected before object acquisition. Unknown executable
/// or extensionless entries receive a separate, bounded shebang sniff allowance.
pub fn sniff_candidate(path: &[u8], executable: bool) -> bool {
    let basename = path.rsplit(|b| *b == b'/').next().unwrap_or(path);
    executable || !basename.contains(&b'.')
}
pub const SNIFF_BYTES: usize = 4096;
pub const SNIFF_FILES: usize = 1024;
pub const FILE_TAGS: usize = 100_000;
const WALK_NODES: usize = 1_000_000;
// The dependency eagerly reparses and materializes all query captures. Only
// ordinary trees enter it; larger/deeper inputs still use the bounded walker.
const RIPTAGS_BYTES: usize = 256 * 1024;
const RIPTAGS_NODES: usize = 50_000;
const RIPTAGS_DEPTH: usize = 128;

fn riptags_preflight(tree: &tree_sitter::Tree, source_bytes: usize) -> bool {
    if source_bytes > RIPTAGS_BYTES || tree.root_node().descendant_count() > RIPTAGS_NODES {
        return false;
    }
    let mut cursor = tree.walk();
    let mut depth = 0;
    loop {
        if depth > RIPTAGS_DEPTH {
            return false;
        }
        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return true;
            }
            depth -= 1;
        }
    }
}

fn language(selector: &str) -> tree_sitter::Language {
    match selector {
        "buffer.rs" => tree_sitter_rust::LANGUAGE.into(),
        "buffer.py" => tree_sitter_python::LANGUAGE.into(),
        "buffer.go" => tree_sitter_go::LANGUAGE.into(),
        "buffer.c" => tree_sitter_c::LANGUAGE.into(),
        "buffer.cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "buffer.zig" => tree_sitter_zig::LANGUAGE.into(),
        "buffer.sh" => tree_sitter_bash::LANGUAGE.into(),
        _ => unreachable!("classified language"),
    }
}

#[derive(Debug, Clone)]
pub struct Function {
    pub tag: riptags::Tag,
    pub documentation: Option<String>,
}

#[cfg(test)]
pub fn extract(selector: &str, source: &str) -> (Vec<riptags::Tag>, String) {
    let (tags, note, _) = extract_functions(selector, source);
    (tags, note)
}
pub fn extract_functions(
    selector: &str,
    source: &str,
) -> (Vec<riptags::Tag>, String, Vec<Function>) {
    use std::{
        collections::BTreeMap,
        ops::ControlFlow,
        time::{Duration, Instant},
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language(selector)).is_err() {
        return (Vec::new(), "parser unavailable".into(), Vec::new());
    }
    let started = Instant::now();
    let mut progress = |_: &tree_sitter::ParseState| {
        if started.elapsed() >= Duration::from_secs(2) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let Some(tree) = parser.parse_with_options(
        &mut |offset, _| &source.as_bytes()[offset..],
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
    ) else {
        return (
            Vec::new(),
            "parser failed or parsing time limit, skipped".into(),
            Vec::new(),
        );
    };
    let mut note = if tree.root_node().has_error() {
        "recovered syntax errors; partial syntax candidates".to_string()
    } else {
        "syntax declarations and identifier occurrences; partial".to_string()
    };
    let columns = scalar_columns(source);
    let mut tags = BTreeMap::new();
    let mut functions = Vec::new();
    let mut documentation_bytes = 0usize;
    if riptags_preflight(&tree, source.len()) {
        for tag in riptags::tag_source(selector, source) {
            let key = (tag.line, tag.col, tag.len);
            tags.entry(key).or_insert(tag);
            if tags.len() >= FILE_TAGS {
                note.push_str("; per-file tag limit (100000), truncated");
                break;
            }
        }
    } else {
        note.push_str("; rich riptags extraction skipped: source/tree complexity limit (256 KiB / 50000 nodes / depth 128); bounded syntax supplementation");
    }
    // A cursor walk retains constant traversal state even on pathological depth.
    let mut cursor = tree.walk();
    let mut visited = 0;
    let mut declaration_depth_limited = false;
    loop {
        visited += 1;
        if visited > WALK_NODES || tags.len() >= FILE_TAGS {
            note.push_str("; syntax traversal/tag limit, truncated");
            break;
        }
        let node = cursor.node();
        let kind = node.kind();
        let function = if matches!(selector, "buffer.c" | "buffer.cpp") {
            if cursor.field_name() == Some("declarator") {
                node.parent()
                    .and_then(|parent| function_node(parent, selector, Some(node)))
            } else {
                None
            }
        } else {
            function_node(node, selector, None)
        };
        if functions.len() < FILE_TAGS
            && let Some((name, wrapper)) = function
        {
            let mut documentation = function_documentation(node, wrapper, selector, source);
            if let Some(text) = &mut documentation {
                let available = (8 * 1024 * 1024usize).saturating_sub(documentation_bytes);
                let limit = available.min(64 * 1024);
                if text.len() > limit {
                    text.truncate(text.floor_char_boundary(limit));
                    text.push_str("\n[Documentation truncated by resource limit]");
                }
                documentation_bytes += text.len();
            }
            functions.push(Function {
                tag: riptags::Tag {
                    line: name.start_position().row as u32 + 1,
                    col: columns[name.start_byte()],
                    len: source[name.byte_range()].chars().count() as u32,
                    name: source[name.byte_range()].into(),
                    kind: riptags::OccKind::Definition,
                    sym_kind: riptags::SymKind::Variable,
                    owners: Vec::new(),
                    context: None,
                },
                documentation,
            });
        }
        if functions.len() < FILE_TAGS
            && let Some(name) = call_name(node, selector)
        {
            functions.push(Function {
                tag: riptags::Tag {
                    line: name.start_position().row as u32 + 1,
                    col: columns[name.start_byte()],
                    len: source[name.byte_range()].chars().count() as u32,
                    name: source[name.byte_range()].into(),
                    kind: riptags::OccKind::Path,
                    sym_kind: riptags::SymKind::Variable,
                    owners: Vec::new(),
                    context: None,
                },
                documentation: None,
            });
        }
        // Plain literal text never has identifier nodes; interpolation expressions
        // do, and remain useful references (including quoted shell expansions).
        let skip = kind.contains("comment");
        if !skip && identifier(kind) {
            let start = node.start_position();
            let col = columns[node.start_byte()];
            let name = &source[node.byte_range()];
            let key = (start.row as u32 + 1, col, name.chars().count() as u32);
            let definition = if cursor.depth() <= RIPTAGS_DEPTH as u32 {
                declaration(&cursor, selector)
            } else {
                if !declaration_depth_limited {
                    note.push_str("; declaration depth limit (128), partial definitions");
                    declaration_depth_limited = true;
                }
                false
            };
            let tag = riptags::Tag {
                line: key.0,
                col: key.1,
                len: key.2,
                name: name.into(),
                kind: if definition {
                    riptags::OccKind::Definition
                } else {
                    riptags::OccKind::Path
                },
                sym_kind: riptags::SymKind::Variable,
                owners: Vec::new(),
                context: None,
            };
            match tags.entry(key) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(tag);
                }
                std::collections::btree_map::Entry::Occupied(mut entry) => {
                    if definition && !entry.get().kind.is_definition() {
                        entry.insert(tag);
                    }
                }
            }
        }
        if !skip && cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return (tags.into_values().collect(), note, functions);
            }
        }
    }
    (tags.into_values().collect(), note, functions)
}
fn identifier(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "type_identifier"
            | "field_identifier"
            | "shorthand_field_identifier"
            | "shorthand_field_identifier_pattern"
            | "namespace_identifier"
            | "variable_name"
    )
}
/// Recognize declaration grammar positions, never infer a binding from spelling.
fn declaration(cursor: &tree_sitter::TreeCursor<'_>, selector: &str) -> bool {
    // The traversal cursor already knows each child's field. Reuse that state
    // rather than repeatedly scanning its parent's siblings for every identifier.
    let mut cursor = cursor.clone();
    for _ in 0..128 {
        let child = cursor.node();
        let field = cursor.field_name();
        if !cursor.goto_parent() {
            return false;
        }
        let parent = cursor.node();
        let kind = parent.kind();
        if matches!(
            kind,
            "function_item"
                | "function_signature_item"
                | "function_definition"
                | "function_declaration"
                | "method_declaration"
                | "struct_item"
                | "enum_item"
                | "trait_item"
                | "type_item"
                | "mod_item"
                | "const_item"
                | "static_item"
                | "field_declaration"
                | "type_parameter"
                | "type_parameter_declaration"
                | "const_parameter"
                | "class_definition"
                | "type_spec"
                | "var_spec"
                | "const_spec"
                | "parameter_declaration"
                | "variadic_parameter_declaration"
                | "alias_declaration"
                | "default_parameter"
                | "typed_default_parameter"
                | "variable_assignment"
                | "parameter"
                | "macro_definition"
        ) && field == Some("name")
        {
            return true;
        }
        match selector {
            "buffer.rs" => {
                if kind == "field_pattern"
                    && field == Some("name")
                    && parent.child_by_field_name("pattern").is_some()
                {
                    return false;
                }
                if matches!(
                    kind,
                    "let_declaration" | "parameter" | "let_condition" | "for_expression"
                ) {
                    return field == Some("pattern");
                }
                if kind == "closure_parameters" {
                    return true;
                }
                if matches!(
                    kind,
                    "tuple_pattern"
                        | "slice_pattern"
                        | "tuple_struct_pattern"
                        | "struct_pattern"
                        | "field_pattern"
                        | "mut_pattern"
                        | "ref_pattern"
                        | "reference_pattern"
                        | "or_pattern"
                        | "captured_pattern"
                ) {
                    if field == Some("type") {
                        return false;
                    }
                } else {
                    return false;
                }
            }
            "buffer.py" => {
                if kind == "assignment" {
                    return field == Some("left");
                }
                if matches!(kind, "parameters" | "lambda_parameters") {
                    return true;
                }
                if kind == "for_statement" {
                    return field == Some("left");
                }
                if !matches!(
                    kind,
                    "pattern_list"
                        | "tuple_pattern"
                        | "list_pattern"
                        | "list_splat_pattern"
                        | "dictionary_splat_pattern"
                        | "typed_parameter"
                ) {
                    return false;
                }
                if field == Some("type") {
                    return false;
                }
            }
            "buffer.go" => {
                if kind == "short_var_declaration" {
                    return field == Some("left");
                }
                if kind == "range_clause" {
                    let mut cursor = parent.walk();
                    return field == Some("left")
                        && parent
                            .children(&mut cursor)
                            .take(8)
                            .any(|node| node.kind() == ":=");
                }
                if kind != "expression_list" {
                    return false;
                }
            }
            "buffer.c" | "buffer.cpp" => {
                if matches!(
                    kind,
                    "declaration"
                        | "field_declaration"
                        | "parameter_declaration"
                        | "optional_parameter_declaration"
                        | "type_definition"
                        | "function_definition"
                ) {
                    return field == Some("declarator");
                }
                if kind.contains("declarator") {
                    if parent.child_by_field_name("declarator").is_some()
                        && field != Some("declarator")
                    {
                        return false;
                    }
                } else {
                    return false;
                }
            }
            "buffer.zig" => {
                return kind == "variable_declaration"
                    && child.kind() == "identifier"
                    && parent.named_child(0).is_some_and(|n| n == child);
            }
            _ => return false,
        }
    }
    false
}

/// Column lookup avoids repeatedly scanning long/tabbed/Unicode source rows.
pub fn scalar_columns(source: &str) -> Vec<u32> {
    let mut columns = vec![0; source.len() + 1];
    let mut column = 0;
    for (byte, scalar) in source.char_indices() {
        columns[byte] = column;
        if scalar == '\n' {
            column = 0;
        } else {
            column += 1;
        }
    }
    columns[source.len()] = column;
    columns
}

/// Only declaration nodes establish function identities; spelling is never evidence.
fn function_node<'a>(
    node: tree_sitter::Node<'a>,
    selector: &str,
    declarator: Option<tree_sitter::Node<'a>>,
) -> Option<(tree_sitter::Node<'a>, tree_sitter::Node<'a>)> {
    let supported = match selector {
        "buffer.rs" => matches!(node.kind(), "function_item" | "function_signature_item"),
        "buffer.py" | "buffer.sh" => node.kind() == "function_definition",
        "buffer.go" => matches!(node.kind(), "function_declaration" | "method_declaration"),
        "buffer.zig" => node.kind() == "function_declaration",
        "buffer.c" | "buffer.cpp" => matches!(
            node.kind(),
            "function_definition" | "declaration" | "field_declaration"
        ),
        _ => false,
    };
    if !supported {
        return None;
    }
    let name = if matches!(selector, "buffer.c" | "buffer.cpp") {
        let mut child = declarator?;
        let mut function = false;
        let mut found = None;
        for _ in 0..128 {
            if child.kind() == "function_declarator" {
                function = true;
            } else if matches!(child.kind(), "pointer_declarator" | "reference_declarator") {
                function = false;
            }
            if matches!(
                child.kind(),
                "identifier" | "field_identifier" | "operator_name" | "destructor_name"
            ) {
                found = Some(child);
                break;
            }
            child = child
                .child_by_field_name("declarator")
                .or_else(|| child.child_by_field_name("name"))
                .or_else(|| {
                    if child.kind() == "parenthesized_declarator" {
                        child.named_child(0)
                    } else {
                        None
                    }
                })?;
        }
        if !function {
            return None;
        }
        found?
    } else {
        node.child_by_field_name("name")?
    };
    let wrapper = node
        .parent()
        .filter(|parent| {
            matches!(
                parent.kind(),
                "decorated_definition" | "template_declaration"
            )
        })
        .unwrap_or(node);
    Some((name, wrapper))
}

fn function_documentation(
    node: tree_sitter::Node<'_>,
    mut wrapper: tree_sitter::Node<'_>,
    selector: &str,
    source: &str,
) -> Option<String> {
    if selector == "buffer.py" {
        let body = node.child_by_field_name("body")?;
        let mut cursor = body.walk();
        let first = body
            .named_children(&mut cursor)
            .find(|n| n.kind() != "comment")?;
        if first.kind() != "expression_statement" || first.named_child_count() != 1 {
            return None;
        }
        let string = first.named_child(0)?;
        if string.kind() != "string" {
            return None;
        }
        let raw = &source[string.byte_range()];
        let quote = raw.find(['\'', '"'])?;
        if !raw[..quote]
            .chars()
            .all(|c| matches!(c, 'r' | 'R' | 'u' | 'U'))
        {
            return None;
        }
        let mut walk = string.walk();
        if string
            .named_children(&mut walk)
            .any(|n| n.kind() == "interpolation")
        {
            return None;
        }
        let raw = &raw[quote..];
        let delimiter = if raw.starts_with("\"\"\"") || raw.starts_with("'''") {
            3
        } else {
            1
        };
        return raw
            .get(delimiter..raw.len().checked_sub(delimiter)?)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
    }
    let mut comments = Vec::new();
    let mut boundary = wrapper.start_byte();
    let mut scan_truncated = false;
    for scanned in 0..=1024 {
        let Some(previous) = wrapper.prev_named_sibling() else {
            break;
        };
        let gap = &source[previous.end_byte()..boundary];
        if !gap.chars().all(char::is_whitespace) || gap.bytes().filter(|b| *b == b'\n').count() > 1
        {
            break;
        }
        let raw = &source[previous.byte_range()];
        let line_start = source[..previous.start_byte()]
            .rfind('\n')
            .map_or(0, |byte| byte + 1);
        if !source[line_start..previous.start_byte()]
            .chars()
            .all(char::is_whitespace)
        {
            break;
        }

        if selector == "buffer.rs" && previous.kind() == "attribute_item" {
            wrapper = previous;
            boundary = previous.start_byte();
            continue;
        }
        if !previous.kind().contains("comment") {
            break;
        }
        let valid = match selector {
            "buffer.rs" => {
                (raw.starts_with("///") && !raw.starts_with("////"))
                    || (raw.starts_with("/**") && !raw.starts_with("/***"))
            }
            "buffer.zig" => raw.starts_with("///") && !raw.starts_with("////"),
            "buffer.sh" => raw.starts_with('#') && !raw.starts_with("#!"),
            _ => raw.starts_with("//") || raw.starts_with("/*"),
        };
        if !valid {
            break;
        }
        if scanned == 1024 {
            scan_truncated = true;
            break;
        }
        comments.push(raw);
        wrapper = previous;
        boundary = previous.start_byte();
    }
    comments.reverse();
    let mut text = comments
        .into_iter()
        .map(|raw| {
            if let Some(body) = raw.strip_prefix("/*").and_then(|s| s.strip_suffix("*/")) {
                body.trim_start_matches('*')
                    .lines()
                    .map(|line| line.trim().trim_start_matches('*').trim_start())
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                raw.lines()
                    .map(|line| {
                        line.trim_start()
                            .trim_start_matches(if selector == "buffer.sh" { '#' } else { '/' })
                            .trim_start()
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if scan_truncated {
        text.push_str("\n[Documentation truncated by comment scan limit]");
    }
    (!text.trim().is_empty()).then(|| text.trim().to_string())
}

fn call_name<'a>(node: tree_sitter::Node<'a>, selector: &str) -> Option<tree_sitter::Node<'a>> {
    let mut name = if selector == "buffer.sh" && node.kind() == "command" {
        node.child_by_field_name("name")?
    } else if matches!(node.kind(), "call_expression" | "call") {
        node.child_by_field_name("function")?
    } else {
        return None;
    };
    for _ in 0..8 {
        if matches!(name.kind(), "identifier" | "field_identifier" | "word") {
            return Some(name);
        }
        name = name
            .child_by_field_name("field")
            .or_else(|| name.child_by_field_name("attribute"))
            .or_else(|| name.child_by_field_name("function"))
            .or_else(|| name.child_by_field_name("name"))
            .or_else(|| {
                if name.kind() == "command_name" {
                    name.named_child(0)
                } else {
                    None
                }
            })?;
    }
    None
}

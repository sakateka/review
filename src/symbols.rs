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

pub fn extract(selector: &str, source: &str) -> (Vec<riptags::Tag>, String) {
    use std::{
        collections::BTreeMap,
        ops::ControlFlow,
        time::{Duration, Instant},
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language(selector)).is_err() {
        return (Vec::new(), "parser unavailable".into());
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
        );
    };
    let mut note = if tree.root_node().has_error() {
        "recovered syntax errors; partial syntax candidates".to_string()
    } else {
        "syntax declarations and identifier occurrences; partial".to_string()
    };
    let columns = scalar_columns(source);
    let mut tags = BTreeMap::new();
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
                return (tags.into_values().collect(), note);
            }
        }
    }
    (tags.into_values().collect(), note)
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

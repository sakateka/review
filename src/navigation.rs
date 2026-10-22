//! Buffer-only candidate analysis. No filesystem indexers or mutable ref lookup.
use crate::source::{Catalog, ComparedSide};
use riptags::{FileTags, OccKind, SymKind, SymbolIndex, Tag};
use std::{
    sync::mpsc::{self, Receiver, SyncSender},
    time::Duration,
};

pub const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 16 * MAX_FILE_BYTES;
const MAX_TAGS: usize = 100_000;
const MAX_RESULTS: usize = 1000;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Search,
    Definitions,
    References,
}
#[derive(Debug, Clone)]
pub struct Candidate {
    pub source: usize,
    pub tag: Tag,
}
#[derive(Debug)]
pub struct Results {
    pub generation: u64,
    pub query: String,
    pub candidates: Vec<Candidate>,
    pub coverage: String,
}
pub struct Request {
    pub generation: u64,
    pub position: Option<(usize, usize, usize)>,
    pub operation: Operation,
    pub query: String,
    pub side: Option<ComparedSide>,
}
pub struct Worker {
    sender: SyncSender<Request>,
    receiver: Receiver<Results>,
}
impl Worker {
    pub fn start(catalog: Catalog) -> Self {
        let (sender, requests) = mpsc::sync_channel::<Request>(1);
        let (answers, receiver) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let analyzed = std::panic::catch_unwind(|| {
                #[cfg(test)]
                if std::env::var("REVIEW_TERMINAL_SCENARIO").as_deref() == Ok("worker-panic") {
                    panic!("injected analysis failure");
                }
                Analysis::build(&catalog)
            });
            while let Ok(mut request) = requests.recv() {
                while let Ok(newer) = requests.try_recv() {
                    request = newer;
                }
                let answer = match &analyzed {
                    Ok(index) => index.query(&catalog, request),
                    Err(_) => Results {
                        generation: request.generation,
                        query: request.query,
                        candidates: Vec::new(),
                        coverage: "Analysis failed (worker panic); basic diff remains available"
                            .into(),
                    },
                };
                if answers.send(answer).is_err() {
                    break;
                }
            }
        });
        Self { sender, receiver }
    }
    pub fn request(&self, request: Request) -> Result<(), Request> {
        self.sender.try_send(request).map_err(|error| match error {
            mpsc::TrySendError::Full(request) | mpsc::TrySendError::Disconnected(request) => {
                request
            }
        })
    }
    pub fn poll(&self) -> Option<Results> {
        self.receiver.try_recv().ok()
    }
}
pub struct Analysis {
    index: SymbolIndex,
    coverage: String,
}
impl Analysis {
    pub fn build(catalog: &Catalog) -> Self {
        let mut files = Vec::new();
        let mut gaps = catalog.gaps.clone();
        let mut total = 0usize;
        let mut tags_total = 0;
        for (id, source) in catalog.sources.iter().enumerate() {
            let path = crate::git::visible_bytes(&source.path);
            if id >= 2048
                || source.bytes.len() > MAX_FILE_BYTES
                || total.saturating_add(source.bytes.len()) > MAX_TOTAL_BYTES
                || tags_total >= MAX_TAGS
            {
                gaps.push(format!("{path}: resource limit, skipped"));
                continue;
            }
            total += source.bytes.len();
            let Ok(text) = std::str::from_utf8(&source.bytes) else {
                gaps.push(format!("{path}: invalid UTF-8, skipped"));
                continue;
            };
            let Some(selector) = classify(&source.path, text) else {
                gaps.push(format!("{path}: unsupported language/shell dialect"));
                continue;
            };
            let (mut tags, note) = if selector == "json" || selector == "yaml" {
                structure(text, selector == "yaml")
            } else {
                (
                    riptags::tag_source(selector, text),
                    "syntax candidates; parser recovery status unknown".into(),
                )
            };
            if tags.len() > MAX_TAGS - tags_total {
                tags.truncate(MAX_TAGS - tags_total);
                gaps.push(format!("{path}: tag limit, truncated"));
            }
            tags_total += tags.len();
            gaps.push(format!("{} {path}: {note}", source.side.label()));
            files.push(FileTags {
                path: id.to_string(),
                mtime_ns: 0,
                size: source.bytes.len() as u64,
                tags,
            });
        }
        let coverage = format!(
            "Partial name/syntax analysis, never binding or complete references. {}",
            gaps.join("; ")
        );
        Self {
            index: SymbolIndex::from_tags(files, 0, Duration::ZERO),
            coverage,
        }
    }
    pub fn query(&self, catalog: &Catalog, mut request: Request) -> Results {
        if let Some((source, line, column)) = request.position
            && let Some(file) = self
                .index
                .fresh_files()
                .iter()
                .find(|file| file.path == source.to_string())
            && let Some(tag) = file
                .tags
                .iter()
                .filter(|tag| {
                    tag.line as usize == line
                        && tag.col as usize <= column
                        && (tag.col + tag.len) as usize > column
                })
                .min_by_key(|tag| (tag.line, (tag.col as usize).saturating_sub(column)))
        {
            request.query = tag.name.to_string();
        }
        let mut candidates = Vec::new();
        // Public pre-tagged index owns the symbol inventory; native source keys
        // are reversible integer IDs, independent of the closed language registry.
        let mut truncated = false;
        for file in self.index.fresh_files() {
            let id: usize = file.path.parse().expect("internal source ID");
            if request
                .side
                .is_some_and(|side| side != catalog.sources[id].side)
            {
                continue;
            }
            let visible_path = crate::git::visible_bytes(&catalog.sources[id].path);
            for tag in &file.tags {
                let matches = match request.operation {
                    Operation::Search => {
                        tag.kind.is_definition()
                            && (request.query.is_empty()
                                || tag.name.contains(&request.query)
                                || visible_path.contains(&request.query)
                                || tag
                                    .context
                                    .as_deref()
                                    .is_some_and(|c| c.contains(&request.query)))
                    }
                    Operation::Definitions => {
                        tag.kind.is_definition() && tag.name.as_ref() == request.query
                    }
                    Operation::References => {
                        !tag.kind.is_definition() && tag.name.as_ref() == request.query
                    }
                };
                if matches {
                    if candidates.len() >= MAX_RESULTS {
                        truncated = true;
                        break;
                    }
                    candidates.push(Candidate {
                        source: id,
                        tag: tag.clone(),
                    });
                }
            }
        }
        candidates.sort_by_key(|candidate| {
            let source = &catalog.sources[candidate.source];
            (
                source.side,
                source.path.clone(),
                candidate.tag.line,
                candidate.tag.col,
            )
        });
        let unavailable = request.position.is_some() && request.query.is_empty();
        Results {
            generation: request.generation,
            query: request.query,
            candidates,
            coverage: format!(
                "{}{}{}",
                if unavailable {
                    "Navigation unavailable: no tagged symbol at the exact cursor position. "
                } else {
                    ""
                },
                if truncated {
                    "Results truncated at 1000. "
                } else {
                    ""
                },
                self.coverage
            ),
        }
    }
}
fn classify(path: &[u8], text: &str) -> Option<&'static str> {
    let basename = path.rsplit(|b| *b == b'/').next().unwrap_or(path);
    let extension = basename.rsplit(|b| *b == b'.').next().unwrap_or(basename);
    let shebang = text.lines().next().unwrap_or("");
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
        if matches!(extension, b"sh" | b"bash") {
            return None;
        }
    }
    match extension {
        b"rs" => Some("buffer.rs"),
        b"c" | b"h" => Some("buffer.c"),
        b"cc" | b"cpp" | b"cxx" | b"hpp" | b"hxx" => Some("buffer.cpp"),
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
fn make_tag(
    node: tree_sitter::Node<'_>,
    source: &str,
    name: String,
    context: String,
    kind: OccKind,
) -> Tag {
    let start = node.start_position();
    let line_start = source[..node.start_byte()].rfind('\n').map_or(0, |n| n + 1);
    Tag {
        line: start.row as u32 + 1,
        col: source[line_start..node.start_byte()].chars().count() as u32,
        len: source[node.byte_range()].chars().count() as u32,
        kind,
        sym_kind: SymKind::Field,
        name: name.into(),
        owners: Vec::new(),
        context: Some(context.into()),
    }
}
fn structure(source: &str, yaml: bool) -> (Vec<Tag>, String) {
    let mut parser = tree_sitter::Parser::new();
    let language = if yaml {
        tree_sitter_yaml::LANGUAGE.into()
    } else {
        tree_sitter_json::LANGUAGE.into()
    };
    if parser.set_language(&language).is_err() {
        return (Vec::new(), "parser unavailable".into());
    }
    let Some(tree) = parser.parse(source, None) else {
        return (Vec::new(), "parser failed".into());
    };
    let mut tags = Vec::new();
    let limits = structural_walk(tree.root_node(), source, "$", yaml, &mut tags, 0);
    let recovery = if tree.root_node().has_error() {
        "recovered syntax errors; partial"
    } else {
        "structural keys/arrays; partial configuration semantics"
    };
    let mut coverage = recovery.to_string();
    if limits & DEPTH_LIMIT != 0 {
        coverage.push_str("; structural depth limit (128), truncated");
    }
    if limits & TAG_LIMIT != 0 {
        coverage.push_str("; structural tag limit (100000), truncated");
    }
    (tags, coverage)
}
const DEPTH_LIMIT: u8 = 1;
const TAG_LIMIT: u8 = 2;
fn structural_walk(
    node: tree_sitter::Node<'_>,
    source: &str,
    path: &str,
    yaml: bool,
    tags: &mut Vec<Tag>,
    depth: usize,
) -> u8 {
    if depth > 128 {
        return DEPTH_LIMIT;
    }
    if tags.len() >= MAX_TAGS {
        return TAG_LIMIT;
    }
    let mut limits = 0;
    let kind = node.kind();
    if matches!(kind, "pair" | "block_mapping_pair" | "flow_pair")
        && let Some(key) = node.child_by_field_name("key")
    {
        let raw = &source[key.byte_range()];
        let name = if !yaml {
            serde_json::from_str::<String>(raw).unwrap_or_else(|_| raw.into())
        } else {
            yaml_key(raw)
        };
        let next = format!("{path}[{}]", serde_json::to_string(&name).expect("string"));
        tags.push(make_tag(
            key,
            source,
            name,
            next.clone(),
            OccKind::Definition,
        ));
        if let Some(value) = node.child_by_field_name("value") {
            limits |= structural_walk(value, source, &next, yaml, tags, depth + 1);
        }
        return limits;
    }
    if matches!(kind, "anchor_name" | "alias_name") {
        tags.push(make_tag(
            node,
            source,
            source[node.byte_range()].into(),
            path.into(),
            if kind == "anchor_name" {
                OccKind::Definition
            } else {
                OccKind::Path
            },
        ));
        return 0;
    }
    let array = matches!(kind, "array" | "block_sequence" | "flow_sequence");
    let mut cursor = node.walk();
    for (index, child) in node
        .named_children(&mut cursor)
        .filter(|child| child.kind() != "comment")
        .enumerate()
    {
        let next = if array {
            format!("{path}[{index}]")
        } else {
            path.into()
        };
        limits |= structural_walk(child, source, &next, yaml, tags, depth + 1);
        if limits & TAG_LIMIT != 0 {
            break;
        }
    }
    limits
}

fn yaml_key(raw: &str) -> String {
    if let Some(quoted) = raw.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')) {
        return quoted.replace("''", "'");
    }
    if raw.starts_with('"')
        && let Ok(decoded) = serde_json::from_str::<String>(raw)
    {
        return decoded;
    }
    raw.to_string()
}

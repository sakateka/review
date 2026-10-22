//! Buffer-only candidate analysis. No filesystem indexers or mutable ref lookup.
use crate::source::{Catalog, ComparedSide};
use riptags::{FileTags, OccKind, SymKind, SymbolIndex, Tag};
use std::{
    sync::mpsc::{self, Receiver, SyncSender},
    time::Duration,
};

pub const MAX_FILE_BYTES: usize = 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 128 * MAX_FILE_BYTES;
const MAX_FILES: usize = 16384;
const MAX_TAGS: usize = 1_000_000;
const MAX_FILE_TAGS: usize = 100_000;
const MAX_RESULTS: usize = 1000;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Search,
    Definitions,
    References,
    Documentation,
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
    pub documentation: Option<String>,
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
                        documentation: None,
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
    functions: Vec<(usize, crate::symbols::Function)>,
}
impl Analysis {
    pub fn build(catalog: &Catalog) -> Self {
        Self::build_with_limits(catalog, MAX_FILES, MAX_TOTAL_BYTES, MAX_TAGS)
    }
    fn build_with_limits(
        catalog: &Catalog,
        max_files: usize,
        max_bytes: usize,
        max_tags: usize,
    ) -> Self {
        let mut files = Vec::new();
        let mut functions = Vec::new();
        let mut gaps = catalog.gaps.clone();
        let mut total = 0usize;
        let mut tags_total = 0;
        let mut eligible = 0;
        for (id, source) in catalog.sources.iter().enumerate() {
            let path = crate::git::visible_bytes(&source.path);
            let Ok(text) = std::str::from_utf8(&source.bytes) else {
                gaps.push(format!("{path}: invalid UTF-8, skipped"));
                continue;
            };
            let Some(selector) = crate::symbols::classify(&source.path, text) else {
                gaps.push(format!("{path}: unsupported language/shell dialect"));
                continue;
            };
            if eligible >= max_files
                || source.bytes.len() > MAX_FILE_BYTES
                || total.saturating_add(source.bytes.len()) > max_bytes
                || tags_total >= max_tags
            {
                gaps.push(format!("{path}: resource limit, skipped"));
                continue;
            }
            eligible += 1;
            total += source.bytes.len();
            let extracted = std::panic::catch_unwind(|| {
                if selector == "json" || selector == "yaml" {
                    let (tags, note) = structure(text, selector == "yaml");
                    (tags, note, Vec::new())
                } else {
                    crate::symbols::extract_functions(selector, text)
                }
            });
            let (mut tags, note, extracted_functions) = match extracted {
                Ok(extracted) => extracted,
                Err(_) => {
                    gaps.push(format!("{path}: extraction failed; skipped"));
                    continue;
                }
            };
            if tags.len() > MAX_FILE_TAGS {
                tags.truncate(MAX_FILE_TAGS);
                gaps.push(format!("{path}: per-file tag limit, truncated"));
            }
            if tags.len() > max_tags - tags_total {
                tags.truncate(max_tags - tags_total);
                gaps.push(format!("{path}: tag limit, truncated"));
            }
            functions.extend(
                extracted_functions
                    .into_iter()
                    .take(max_tags.saturating_sub(functions.len()))
                    .map(|function| (id, function)),
            );
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
            functions,
        }
    }
    pub fn query(&self, catalog: &Catalog, mut request: Request) -> Results {
        if request.position.is_some() {
            request.query.clear();
        }
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
            request.query = if request.operation == Operation::Documentation
                && !self.functions.iter().any(|(id, function)| {
                    *id == source
                        && function.tag.line == tag.line
                        && function.tag.col == tag.col
                        && function.tag.kind.is_definition() == tag.kind.is_definition()
                }) {
                String::new()
            } else {
                tag.name.to_string()
            };
        }
        if request.operation == Operation::Documentation {
            let exact = request.position.and_then(|(source, line, col)| {
                self.functions.iter().find(|(id, function)| {
                    *id == source
                        && function.tag.kind.is_definition()
                        && function.tag.line as usize == line
                        && function.tag.col as usize <= col
                        && col < (function.tag.col + function.tag.len) as usize
                })
            });
            let matches: Vec<_> = if let Some(function) = exact {
                vec![function]
            } else {
                self.functions
                    .iter()
                    .filter(|(id, function)| {
                        function.tag.kind.is_definition()
                            && request
                                .side
                                .is_none_or(|side| catalog.sources[*id].side == side)
                            && !request.query.is_empty()
                            && function.tag.name.as_ref() == request.query
                    })
                    .take(MAX_RESULTS + 1)
                    .collect()
            };
            let documentation = if matches.len() == 1 {
                matches[0].1.documentation.clone()
            } else {
                None
            };
            let coverage = if matches.is_empty() {
                "Documentation unavailable: no captured function at this position".into()
            } else if matches.len() == 1 && documentation.is_none() {
                "Documentation unavailable: function has no supported captured documentation".into()
            } else if matches.len() > MAX_RESULTS {
                "Documentation candidates truncated at 1000".into()
            } else {
                "Captured function documentation candidates".into()
            };
            return Results {
                generation: request.generation,
                query: request.query,
                candidates: matches
                    .into_iter()
                    .take(MAX_RESULTS)
                    .map(|(source, function)| Candidate {
                        source: *source,
                        tag: function.tag.clone(),
                    })
                    .collect(),
                coverage,
                documentation,
            };
        }
        let mut candidates = Vec::new();
        // Public pre-tagged index owns the symbol inventory; native source keys
        // are reversible integer IDs, independent of the closed language registry.
        let mut truncated = false;
        let mut ordered: Vec<_> = self.index.fresh_files().iter().collect();
        ordered.sort_by_key(|file| {
            let id: usize = file.path.parse().expect("internal source ID");
            (catalog.sources[id].side, &catalog.sources[id].path)
        });
        for file in ordered {
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
                    Operation::Documentation => unreachable!(),
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
            documentation: None,
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
fn make_tag(
    node: tree_sitter::Node<'_>,
    source: &str,
    name: String,
    context: String,
    kind: OccKind,
    columns: &[u32],
) -> Tag {
    let start = node.start_position();
    Tag {
        line: start.row as u32 + 1,
        col: columns[node.start_byte()],
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
    let columns = crate::symbols::scalar_columns(source);
    let limits = structural_walk(tree.root_node(), source, "$", yaml, &mut tags, 0, &columns);
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
    columns: &[u32],
) -> u8 {
    if depth > 128 {
        return DEPTH_LIMIT;
    }
    if tags.len() >= MAX_FILE_TAGS {
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
            columns,
        ));
        if let Some(value) = node.child_by_field_name("value") {
            limits |= structural_walk(value, source, &next, yaml, tags, depth + 1, columns);
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
            columns,
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
        limits |= structural_walk(child, source, &next, yaml, tags, depth + 1, columns);
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

#[cfg(test)]
mod limit_tests {
    use super::*;
    #[test]
    fn eligible_count_bytes_and_tags_are_independent_limits_and_failures_do_not_erase_valid_files()
    {
        let mut catalog = Catalog::default();
        for id in 0..3000 {
            catalog.insert(
                ComparedSide::New,
                format!("{id}.md").into_bytes(),
                vec![b'x'; 32],
            );
        }
        catalog.insert(ComparedSide::New, b"invalid.rs".to_vec(), vec![0xff]);
        catalog.insert(
            ComparedSide::New,
            b"first.rs".to_vec(),
            b"fn first() {}".to_vec(),
        );
        catalog.insert(
            ComparedSide::New,
            b"second.rs".to_vec(),
            b"fn second() {}".to_vec(),
        );
        let request = || Request {
            generation: 1,
            position: None,
            operation: Operation::Definitions,
            query: "first".into(),
            side: None,
        };
        for index in [
            Analysis::build_with_limits(&catalog, 1, 100, 100),
            Analysis::build_with_limits(&catalog, 10, 15, 100),
            Analysis::build_with_limits(&catalog, 10, 100, 1),
        ] {
            let result = index.query(&catalog, request());
            assert_eq!(result.candidates.len(), 1, "{result:?}");
            assert_eq!(
                catalog.sources[result.candidates[0].source].path,
                b"first.rs"
            );
            assert!(result.coverage.contains("resource limit"));
            assert!(result.coverage.contains("invalid UTF-8"));
        }
    }
    #[test]
    fn result_cap_selects_sorted_paths_before_cloning_candidates() {
        let mut catalog = Catalog::default();
        for id in (0..1100).rev() {
            catalog.insert(
                ComparedSide::New,
                format!("{id:04}.rs").into_bytes(),
                b"fn target() {}".to_vec(),
            );
        }
        let result = Analysis::build(&catalog).query(
            &catalog,
            Request {
                generation: 1,
                position: None,
                operation: Operation::Definitions,
                query: "target".into(),
                side: None,
            },
        );
        assert_eq!(result.candidates.len(), 1000);
        assert_eq!(
            catalog.sources[result.candidates[0].source].path,
            b"0000.rs"
        );
        assert_eq!(
            catalog.sources[result.candidates[999].source].path,
            b"0999.rs"
        );
        assert!(result.coverage.contains("Results truncated"));
    }
}

//! Review comments: the anchor model, the owned `review-<summary>.comments`
//! Markdown file in the launch directory, and the floating editor buffer.
//!
//! The comments file is the only file review ever writes outside its state
//! directory. It is created only when a comment exists, replaced atomically,
//! and removed with the last comment. Foreign and Git-tracked files are never
//! touched.

use crate::git::{self, Capture, Item};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
};

pub const HEADER: &str = "review-comments: v1";
pub const INSTRUCTION: &str = "Address each comment below; excerpts show the code as reviewed.";
const MARKER: &str = "<!-- review-comment ";
const CONTEXT: usize = 3;
const CANDIDATES: usize = 1000;

/// Classification of one captured patch row, mirroring the preview renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// `diff --git` through the line before the first `@@`.
    FileHeader,
    /// An `@@` line, numbered from zero within the item.
    Hunk(usize),
    /// A source row with its one-based old/new line numbers.
    Source {
        hunk: usize,
        old: Option<usize>,
        new: Option<usize>,
    },
    /// Any other row after the first hunk, such as a missing-newline note.
    Other(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PatchRow<'a> {
    pub kind: RowKind,
    pub text: &'a str,
}

/// Classify patch rows exactly like `Preview::compose` numbers them.
pub fn rows(patch: &str) -> Vec<PatchRow<'_>> {
    let mut result = Vec::new();
    let mut hunk: Option<usize> = None;
    let mut remaining = (0, 0);
    let (mut old, mut new) = (0, 0);
    for text in patch.split_terminator('\n') {
        if let Some((old_start, old_count, new_start, new_count)) = crate::source::hunk(text) {
            let index = hunk.map_or(0, |index| index + 1);
            hunk = Some(index);
            remaining = (old_count, new_count);
            old = old_start.saturating_sub(1);
            new = new_start.saturating_sub(1);
            result.push(PatchRow {
                kind: RowKind::Hunk(index),
                text,
            });
            continue;
        }
        let Some(index) = hunk else {
            result.push(PatchRow {
                kind: RowKind::FileHeader,
                text,
            });
            continue;
        };
        let location = match text.as_bytes().first() {
            Some(b' ') if remaining.0 > 0 && remaining.1 > 0 => {
                remaining.0 -= 1;
                remaining.1 -= 1;
                old += 1;
                new += 1;
                Some((Some(old), Some(new)))
            }
            Some(b'-') if remaining.0 > 0 => {
                remaining.0 -= 1;
                old += 1;
                Some((Some(old), None))
            }
            Some(b'+') if remaining.1 > 0 => {
                remaining.1 -= 1;
                new += 1;
                Some((None, Some(new)))
            }
            _ => None,
        };
        result.push(PatchRow {
            kind: match location {
                Some((old, new)) => RowKind::Source {
                    hunk: index,
                    old,
                    new,
                },
                None => RowKind::Other(index),
            },
            text,
        });
    }
    result
}

/// One reviewed patch row: its source numbers and exact captured text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub old: Option<usize>,
    pub new: Option<usize>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub old_path: String,
    pub new_path: String,
    /// Removed-line range on the old side.
    pub old: Option<(usize, usize)>,
    /// Added/context-line range on the new side.
    pub new: Option<(usize, usize)>,
    /// Empty for a file comment.
    pub anchors: Vec<Anchor>,
    /// Numbered patch excerpt as reviewed.
    pub excerpt: Vec<String>,
    pub text: String,
    pub outdated: bool,
}

#[derive(Serialize, Deserialize)]
struct Metadata {
    old_path: String,
    new_path: String,
    old: Option<(usize, usize)>,
    new: Option<(usize, usize)>,
    anchors: Vec<Anchor>,
    outdated: bool,
}

fn range(range: (usize, usize)) -> String {
    if range.0 == range.1 {
        range.0.to_string()
    } else {
        format!("{}-{}", range.0, range.1)
    }
}

fn paths(item: &Item) -> (String, String) {
    (
        git::visible_name(&item.old_path),
        git::visible_name(&item.new_path),
    )
}

impl Comment {
    pub fn is_file(&self) -> bool {
        self.anchors.is_empty()
    }

    pub fn belongs_to(&self, item: &Item) -> bool {
        (self.old_path.clone(), self.new_path.clone()) == paths(item)
    }

    pub fn path(&self) -> String {
        if self.old_path == self.new_path {
            self.new_path.clone()
        } else {
            format!("{} → {}", self.old_path, self.new_path)
        }
    }

    /// `path:range (side)` without the outdated marker.
    pub fn anchor_label(&self) -> String {
        let path = self.path();
        match (self.old, self.new) {
            (Some(old), Some(new)) => {
                format!("{path}:old {}, new {} (mixed)", range(old), range(new))
            }
            (Some(old), None) => format!("{path}:{} (old)", range(old)),
            (None, Some(new)) => format!("{path}:{} (new)", range(new)),
            (None, None) => format!("{path} (file)"),
        }
    }

    pub fn heading(&self) -> String {
        format!(
            "## {}{}",
            self.anchor_label(),
            if self.outdated { " [outdated]" } else { "" }
        )
    }

    /// Whether a classified row of this comment's item belongs to it.
    pub fn owns(&self, kind: RowKind) -> bool {
        match kind {
            RowKind::FileHeader => self.is_file(),
            RowKind::Source { old, new, .. } => self
                .anchors
                .iter()
                .any(|anchor| anchor.old == old && anchor.new == new),
            _ => false,
        }
    }

    pub fn overlaps(&self, other: &Comment) -> bool {
        self.old_path == other.old_path
            && self.new_path == other.new_path
            && if self.is_file() || other.is_file() {
                self.is_file() && other.is_file()
            } else {
                self.anchors.iter().any(|anchor| {
                    other
                        .anchors
                        .iter()
                        .any(|theirs| theirs.old == anchor.old && theirs.new == anchor.new)
                })
            }
    }

    /// Outdated when the file is gone or any anchor row no longer matches
    /// the captured row with the same numbers. Never relocated.
    pub fn check(&mut self, items: &[Item]) {
        let Some(item) = items.iter().find(|item| self.belongs_to(item)) else {
            self.outdated = true;
            return;
        };
        if self.is_file() {
            self.outdated = false;
            return;
        }
        let Ok(patch) = std::str::from_utf8(&item.patch) else {
            self.outdated = true;
            return;
        };
        let current = rows(patch);
        self.outdated = !self.anchors.iter().all(|anchor| {
            current.iter().any(|row| {
                matches!(row.kind, RowKind::Source { old, new, .. }
                    if old == anchor.old && new == anchor.new)
                    && row.text == anchor.text
            })
        });
    }
}

fn number(value: Option<usize>, width: usize) -> String {
    value.map_or_else(|| " ".repeat(width), |value| format!("{value:>width$}"))
}

fn excerpt(rows: &[PatchRow<'_>], range: std::ops::RangeInclusive<usize>) -> Vec<String> {
    let width = rows[range.clone()]
        .iter()
        .filter_map(|row| match row.kind {
            RowKind::Source { old, new, .. } => old.max(new),
            _ => None,
        })
        .max()
        .unwrap_or(0)
        .to_string()
        .len();
    rows[range]
        .iter()
        .map(|row| {
            let (old, new) = match row.kind {
                RowKind::Source { old, new, .. } => (old, new),
                _ => (None, None),
            };
            format!("{} {} {}", number(old, width), number(new, width), row.text)
        })
        .collect()
}

/// Build a comment draft from selected patch rows (display order, possibly
/// non-contiguous in the after-changes view). Errors explain refusals.
pub fn build(item: &Item, selected: &[usize]) -> Result<Comment, String> {
    let patch = std::str::from_utf8(&item.patch)
        .map_err(|_| "Comments need a UTF-8 captured patch".to_owned())?;
    let rows = rows(patch);
    let chosen: Vec<_> = selected
        .iter()
        .map(|index| rows.get(*index).copied().map(|row| (*index, row)))
        .collect::<Option<_>>()
        .ok_or("Selection is outside the captured patch")?;
    let Some(&(_, last)) = chosen.last() else {
        return Err("Nothing selected".into());
    };
    if chosen.len() > 1 && matches!(last.kind, RowKind::Hunk(_)) {
        return Err("A comment range cannot end on a hunk header (@@)".into());
    }
    let (old_path, new_path) = paths(item);
    let mut comment = Comment {
        old_path,
        new_path,
        old: None,
        new: None,
        anchors: Vec::new(),
        excerpt: Vec::new(),
        text: String::new(),
        outdated: false,
    };
    let header = |row: &PatchRow<'_>| matches!(row.kind, RowKind::FileHeader | RowKind::Hunk(_));
    if chosen.iter().all(|(_, row)| header(row)) {
        let end = rows
            .iter()
            .position(|row| row.kind != RowKind::FileHeader)
            .unwrap_or(rows.len());
        if end > 0 {
            comment.excerpt = excerpt(&rows, 0..=end.min(12) - 1);
        }
        return Ok(comment);
    }
    if chosen
        .iter()
        .any(|(_, row)| row.kind == RowKind::FileHeader)
    {
        return Err(
            "A comment range must stay within one hunk; it includes file header lines".into(),
        );
    }
    let hunk_of = |kind: RowKind| match kind {
        RowKind::Hunk(hunk) | RowKind::Source { hunk, .. } | RowKind::Other(hunk) => hunk,
        RowKind::FileHeader => usize::MAX,
    };
    let hunk = hunk_of(chosen[0].1.kind);
    if chosen.iter().any(|(_, row)| hunk_of(row.kind) != hunk) {
        return Err("A comment range must stay within one hunk".into());
    }
    let sources: Vec<_> = chosen
        .iter()
        .filter_map(|(index, row)| match row.kind {
            RowKind::Source { old, new, .. } => Some((*index, old, new, row.text)),
            _ => None,
        })
        .collect();
    if sources.is_empty() {
        return Err("No source line selected for a comment".into());
    }
    let extend = |range: &mut Option<(usize, usize)>, line: usize| {
        *range = Some(range.map_or((line, line), |(start, end)| {
            (start.min(line), end.max(line))
        }));
    };
    for (_, old, new, text) in &sources {
        match (old, new) {
            (Some(old), None) => extend(&mut comment.old, *old),
            (_, Some(new)) => extend(&mut comment.new, *new),
            (None, None) => {}
        }
        comment.anchors.push(Anchor {
            old: *old,
            new: *new,
            text: (*text).to_owned(),
        });
    }
    let start = rows
        .iter()
        .position(|row| row.kind == RowKind::Hunk(hunk))
        .unwrap_or(0);
    let end = rows
        .iter()
        .rposition(|row| row.kind != RowKind::FileHeader && hunk_of(row.kind) == hunk)
        .unwrap_or(rows.len() - 1);
    let first = sources.iter().map(|source| source.0).min().unwrap_or(start);
    let last = sources.iter().map(|source| source.0).max().unwrap_or(end);
    comment.excerpt = excerpt(
        &rows,
        first.saturating_sub(CONTEXT).max(start)..=(last + CONTEXT).min(end),
    );
    Ok(comment)
}

/// Readable filename summary of the original Git arguments.
pub fn summary(args: &[OsString]) -> String {
    let joined = args
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join("-");
    let mut result = String::new();
    for character in joined.chars() {
        let character = if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        {
            character
        } else {
            '-'
        };
        if !(character == '-' && result.ends_with('-')) {
            result.push(character);
        }
    }
    let mut summary = result.trim_matches('-').to_owned();
    summary.truncate(100);
    let summary = summary.trim_end_matches('-');
    if summary.is_empty() {
        "worktree".into()
    } else {
        summary.into()
    }
}

pub fn filename(summary: &str, candidate: usize) -> String {
    if candidate <= 1 {
        format!("review-{summary}.comments")
    } else {
        format!("review-{summary}-{candidate}.comments")
    }
}

fn visible_args(args: &[OsString]) -> Vec<String> {
    args.iter()
        .map(|arg| git::visible_bytes(arg.as_encoded_bytes()))
        .collect()
}

/// Quote one argument for a POSIX shell.
pub fn shell_quote(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c))
    {
        arg.into()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

fn front_matter(text: &str) -> Option<Vec<&str>> {
    let mut lines = text.split('\n');
    if lines.next()? != "---" || lines.next()? != HEADER {
        return None;
    }
    let mut result = Vec::new();
    for line in lines {
        if line == "---" {
            return Some(result);
        }
        result.push(line);
    }
    None
}

/// The file is ours: our header key line and identical original arguments.
pub fn owned(text: &str, args: &[OsString]) -> bool {
    front_matter(text).is_some_and(|lines| {
        lines.iter().any(|line| {
            line.strip_prefix("args: ")
                .and_then(|value| serde_json::from_str::<Vec<String>>(value).ok())
                .is_some_and(|stored| stored == visible_args(args))
        })
    })
}

fn fence(lines: &[String], info: &str) -> (String, String) {
    let mut longest = 0;
    for line in lines {
        let mut run = 0;
        for character in line.chars() {
            run = if character == '`' { run + 1 } else { 0 };
            longest = longest.max(run);
        }
    }
    let fence = "`".repeat((longest + 1).max(3));
    (format!("{fence}{info}"), fence)
}

fn json(value: &impl Serialize) -> String {
    // '>' only occurs inside JSON strings; escaping it keeps the HTML comment
    // closed only by our own terminator.
    serde_json::to_string(value)
        .expect("serializable")
        .replace('>', "\\u003e")
}

fn yaml_string(value: &str) -> String {
    serde_json::to_string(value).expect("serializable string")
}

fn yaml_optional(value: Option<&str>) -> String {
    value.map_or_else(|| "unavailable".into(), yaml_string)
}

fn utc(time: std::time::SystemTime) -> String {
    let seconds = time
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs() as i64);
    let (days, rest) = (seconds.div_euclid(86400), seconds.rem_euclid(86400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// Order comments by file-list order, then file comments before line comments
/// by first line; comments for absent files follow in stored order.
pub fn order(comments: &mut [Comment], items: &[Item]) {
    let key = |comment: &Comment| {
        let file = items
            .iter()
            .position(|item| comment.belongs_to(item))
            .unwrap_or(usize::MAX);
        let line = if file == usize::MAX || comment.is_file() {
            (0, 0)
        } else {
            (
                1,
                comment
                    .new
                    .map(|range| range.0)
                    .or(comment.old.map(|range| range.0))
                    .unwrap_or(0),
            )
        };
        (file, line)
    };
    comments.sort_by_key(key);
}

pub fn serialize(
    comments: &[Comment],
    args: &[OsString],
    provenance: &git::Provenance,
    written: std::time::SystemTime,
) -> String {
    let mut text = format!("---\n{HEADER}\n");
    let unavailable = |value: &Option<String>| {
        if provenance.no_index {
            "unavailable".to_owned()
        } else {
            yaml_optional(value.as_deref())
        }
    };
    text.push_str(&format!(
        "repository: {}\n",
        unavailable(&provenance.repository)
    ));
    text.push_str(&format!("remote: {}\n", unavailable(&provenance.remote)));
    text.push_str(&format!("args: {}\n", json(&visible_args(args))));
    for (name, side) in [("old", &provenance.old), ("new", &provenance.new)] {
        text.push_str(&format!("{name}:\n  label: {}\n", yaml_string(&side.label)));
        if let Some(reference) = &side.reference {
            text.push_str(&format!("  ref: {}\n", yaml_string(reference)));
        }
        text.push_str(&format!(
            "  commit: {}\n",
            yaml_optional(side.commit.as_deref())
        ));
        if let Some(object) = &side.object {
            text.push_str(&format!("  object: {}\n", yaml_string(object)));
        }
    }
    if let Some(base) = &provenance.merge_base {
        text.push_str(&format!("merge-base: {}\n", yaml_string(base)));
    }
    let command = std::iter::once("git".to_owned())
        .chain(std::iter::once("diff".to_owned()))
        .chain(args.iter().map(|arg| {
            shell_quote(
                &arg.to_str()
                    .map_or_else(|| git::visible_bytes(arg.as_encoded_bytes()), str::to_owned),
            )
        }))
        .collect::<Vec<_>>()
        .join(" ");
    text.push_str(&format!("command: {}\n", yaml_string(&command)));
    text.push_str(&format!("written: {}\n", yaml_string(&utc(written))));
    text.push_str(&format!("comments: {}\n", comments.len()));
    text.push_str(&format!(
        "outdated: {}\n---\n{INSTRUCTION}\n",
        comments.iter().filter(|comment| comment.outdated).count()
    ));
    for comment in comments {
        text.push('\n');
        text.push_str(&comment.heading());
        text.push('\n');
        text.push_str(MARKER);
        text.push_str(&json(&Metadata {
            old_path: comment.old_path.clone(),
            new_path: comment.new_path.clone(),
            old: comment.old,
            new: comment.new,
            anchors: comment.anchors.clone(),
            outdated: comment.outdated,
        }));
        text.push_str(" -->\n");
        let (open, close) = fence(&comment.excerpt, "");
        text.push_str(&format!("{open}\n"));
        for line in &comment.excerpt {
            text.push_str(line);
            text.push('\n');
        }
        text.push_str(&format!("{close}\n"));
        let body: Vec<String> = comment.text.split('\n').map(str::to_owned).collect();
        let (open, close) = fence(&body, "text");
        text.push_str(&format!("{open}\n{}\n{close}\n", comment.text));
    }
    text
}

pub fn parse(text: &str) -> Result<Vec<Comment>> {
    let header = front_matter(text).context("missing comments front matter")?;
    let mut lines = text.split('\n').skip(header.len() + 3);
    let mut comments = Vec::new();
    let fenced = |lines: &mut dyn Iterator<Item = &str>| -> Result<Vec<String>> {
        let open = lines.next().context("missing fenced block")?;
        let fence: String = open.chars().take_while(|c| *c == '`').collect();
        if fence.len() < 3 {
            bail!("expected a fenced block, found {open:?}");
        }
        let mut content = Vec::new();
        loop {
            let line = lines.next().context("unterminated fenced block")?;
            if line == fence {
                return Ok(content);
            }
            content.push(line.to_owned());
        }
    };
    while let Some(line) = lines.next() {
        if !line.starts_with("## ") {
            continue;
        }
        let metadata = lines
            .next()
            .and_then(|line| line.strip_prefix(MARKER)?.strip_suffix(" -->"))
            .context("comment heading without metadata")?;
        let metadata: Metadata =
            serde_json::from_str(metadata).context("invalid comment metadata")?;
        let excerpt = fenced(&mut lines)?;
        let body = fenced(&mut lines)?;
        comments.push(Comment {
            old_path: metadata.old_path,
            new_path: metadata.new_path,
            old: metadata.old,
            new: metadata.new,
            anchors: metadata.anchors,
            excerpt,
            text: body.join("\n"),
            outdated: metadata.outdated,
        });
    }
    Ok(comments)
}

fn write_atomically(cwd: &Path, path: &Path, text: &str, replace: bool) -> Result<()> {
    let mut temporary =
        tempfile::NamedTempFile::new_in(cwd).context("cannot create temporary comments file")?;
    temporary
        .write_all(text.as_bytes())
        .context("cannot write comments file")?;
    temporary
        .as_file()
        .sync_all()
        .context("cannot flush comments file")?;
    if replace {
        temporary
            .persist(path)
            .context("cannot atomically replace comments file")?;
    } else {
        temporary
            .persist_noclobber(path)
            .context("cannot create comments file")?;
    }
    Ok(())
}

/// In-memory comments plus the owned file that persists them.
#[derive(Debug)]
pub struct Store {
    pub comments: Vec<Comment>,
    cwd: PathBuf,
    args: Vec<OsString>,
    summary: String,
    /// The owned file currently holding these comments.
    pub path: Option<PathBuf>,
    /// Set when an owned file cannot be parsed; it is preserved untouched.
    pub disabled: Option<String>,
    provenance: Option<git::Provenance>,
}

impl Store {
    pub fn empty(cwd: &Path, args: &[OsString]) -> Self {
        Self {
            comments: Vec::new(),
            cwd: cwd.into(),
            args: args.to_vec(),
            summary: summary(args),
            path: None,
            disabled: None,
            provenance: None,
        }
    }

    /// Load the owned comments file, if any. Never writes unless stored
    /// outdated flags changed, which rewrites the existing owned file.
    pub fn load(capture: &Capture) -> (Self, Option<String>) {
        let mut store = Self::empty(&capture.cwd, &capture.args);
        if capture.unavailable.is_some() {
            return (store, None);
        }
        for candidate in 1..=CANDIDATES {
            let name = filename(&store.summary, candidate);
            let path = store.cwd.join(&name);
            // Keep scanning past missing names: an owned file may follow a gap.
            let Ok(bytes) = std::fs::read(&path) else {
                continue;
            };
            let text = String::from_utf8_lossy(&bytes).into_owned();
            if !owned(&text, &store.args) || git::tracked(&store.cwd, Path::new(&name)) {
                continue;
            }
            store.path = Some(path);
            match parse(&text) {
                Ok(comments) => store.comments = comments,
                Err(error) => {
                    let message = format!(
                        "Comments file {name} cannot be parsed ({error:#}); comments are not saved to preserve it"
                    );
                    store.disabled = Some(message.clone());
                    return (store, Some(message));
                }
            }
            break;
        }
        let before: Vec<_> = store.comments.iter().map(|c| c.outdated).collect();
        for comment in &mut store.comments {
            comment.check(&capture.items);
        }
        let changed = before
            != store
                .comments
                .iter()
                .map(|c| c.outdated)
                .collect::<Vec<_>>();
        let mut message = None;
        let outdated = store.comments.iter().filter(|c| c.outdated).count();
        if !store.comments.is_empty() {
            message = Some(format!(
                "{} comment(s) loaded{}",
                store.comments.len(),
                if outdated > 0 {
                    format!(", {outdated} outdated")
                } else {
                    String::new()
                }
            ));
        }
        if changed && let Err(error) = store.persist(&capture.items) {
            message = Some(format!("Comments file not updated: {error:#}"));
        }
        (store, message)
    }

    fn owned_at(&self, path: &Path) -> bool {
        std::fs::read(path).is_ok_and(|bytes| owned(&String::from_utf8_lossy(&bytes), &self.args))
    }

    /// Resolve the file to write: the remembered owned file, else the first
    /// owned matching candidate, else the first nonexistent untracked name.
    fn target(&self) -> Result<(PathBuf, bool)> {
        let untracked = |path: &Path| {
            path.file_name()
                .is_some_and(|name| !git::tracked(&self.cwd, Path::new(name)))
        };
        if let Some(path) = &self.path {
            if path.exists() {
                if self.owned_at(path) && untracked(path) {
                    return Ok((path.clone(), true));
                }
            } else if untracked(path) {
                return Ok((path.clone(), false));
            }
        }
        // Prefer an owned matching file anywhere among the candidates, even
        // after a gap; only then create at the first free untracked name.
        let mut free = Vec::new();
        for candidate in 1..=CANDIDATES {
            let path = self.cwd.join(filename(&self.summary, candidate));
            if path.symlink_metadata().is_ok() {
                if self.owned_at(&path) && untracked(&path) {
                    return Ok((path, true));
                }
            } else {
                free.push(path);
            }
        }
        if let Some(path) = free.into_iter().find(|path| untracked(path)) {
            return Ok((path, false));
        }
        bail!("no free comments filename in {}", self.cwd.display())
    }

    /// Write every comment to the owned file, or remove the file when there
    /// are none. Returns the path written, or `None` after removal.
    pub fn persist(&mut self, items: &[Item]) -> Result<Option<PathBuf>> {
        if let Some(message) = &self.disabled {
            bail!("{message}");
        }
        order(&mut self.comments, items);
        if self.comments.is_empty() {
            if let Some(path) = self.path.take()
                && path.exists()
                && self.owned_at(&path)
            {
                if path
                    .file_name()
                    .is_some_and(|name| git::tracked(&self.cwd, Path::new(name)))
                {
                    bail!(
                        "{} is tracked by Git and was left untouched",
                        path.display()
                    );
                }
                std::fs::remove_file(&path).context("cannot remove comments file")?;
            }
            return Ok(None);
        }
        let (path, exists) = self.target()?;
        let provenance = self
            .provenance
            .get_or_insert_with(|| git::provenance(&self.cwd, &self.args))
            .clone();
        let text = serialize(
            &self.comments,
            &self.args,
            &provenance,
            std::time::SystemTime::now(),
        );
        write_atomically(&self.cwd, &path, &text, exists)?;
        self.path = Some(path.clone());
        Ok(Some(path))
    }

    pub fn for_item<'a>(&'a self, item: &'a Item) -> impl Iterator<Item = (usize, &'a Comment)> {
        self.comments
            .iter()
            .enumerate()
            .filter(move |(_, comment)| comment.belongs_to(item))
    }

    /// Index of the comment owning a classified row of `item`.
    pub fn at(&self, item: &Item, kind: RowKind) -> Option<usize> {
        self.for_item(item)
            .find(|(_, comment)| comment.owns(kind))
            .map(|(index, _)| index)
    }
}

/// The floating multi-line comment editor buffer.
#[derive(Debug, Clone)]
pub struct Editor {
    pub title: String,
    pub lines: Vec<String>,
    pub row: usize,
    /// Cursor position in Unicode scalars within `lines[row]`.
    pub column: usize,
    pub existing: Option<usize>,
    pub draft: Comment,
}

impl Editor {
    pub fn new(draft: Comment, existing: Option<usize>) -> Self {
        let lines: Vec<String> = draft.text.split('\n').map(str::to_owned).collect();
        let row = lines.len() - 1;
        let column = lines[row].chars().count();
        Self {
            title: draft.anchor_label(),
            lines,
            row,
            column,
            existing,
            draft,
        }
    }

    fn byte(&self) -> usize {
        self.lines[self.row]
            .char_indices()
            .nth(self.column)
            .map_or(self.lines[self.row].len(), |(index, _)| index)
    }

    pub fn insert(&mut self, character: char) {
        let byte = self.byte();
        self.lines[self.row].insert(byte, character);
        self.column += 1;
    }

    pub fn newline(&mut self) {
        let byte = self.byte();
        let rest = self.lines[self.row].split_off(byte);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.column = 0;
    }

    pub fn backspace(&mut self) {
        if self.column > 0 {
            self.column -= 1;
            let byte = self.byte();
            self.lines[self.row].remove(byte);
        } else if self.row > 0 {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.column = self.lines[self.row].chars().count();
            self.lines[self.row].push_str(&line);
        }
    }

    pub fn left(&mut self) {
        if self.column > 0 {
            self.column -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.column = self.lines[self.row].chars().count();
        }
    }

    pub fn right(&mut self) {
        if self.column < self.lines[self.row].chars().count() {
            self.column += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.column = 0;
        }
    }

    pub fn vertical(&mut self, down: bool) {
        if down && self.row + 1 < self.lines.len() {
            self.row += 1;
        } else if !down && self.row > 0 {
            self.row -= 1;
        }
        self.column = self.column.min(self.lines[self.row].chars().count());
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }
}

#[cfg(test)]
pub(crate) fn utc_for_test(seconds: u64) -> String {
    utc(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
}

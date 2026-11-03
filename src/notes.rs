//! Read-only notes supplied in `review.notes` at the worktree root.
use crate::git::{self, Capture, Item};
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct Note {
    pub path: String,
    pub line: usize,
    pub column: usize,
    pub text: String,
}

#[derive(Default)]
pub struct Notes {
    pub entries: Vec<Note>,
    prefix: PathBuf,
    relative: bool,
}

impl Notes {
    pub fn load(capture: &Capture) -> (Self, Option<String>) {
        let root = git::command(&capture.cwd)
            .args(["rev-parse", "--show-toplevel"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| {
                PathBuf::from(OsString::from_vec(
                    output
                        .stdout
                        .strip_suffix(b"\n")
                        .unwrap_or(&output.stdout)
                        .to_vec(),
                ))
            });
        let Some(root) = root else {
            return (Self::default(), None);
        };
        let path = root.join("review.notes");
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), None);
            }
            Err(error) => {
                return (
                    Self::default(),
                    Some(format!("Cannot read review.notes: {error}")),
                );
            }
        };
        let prefix = capture
            .cwd
            .strip_prefix(&root)
            .unwrap_or(Path::new(""))
            .to_owned();
        let relative = capture.args.iter().any(|arg| arg == "--relative");
        let mut result = Self {
            entries: Vec::new(),
            prefix,
            relative,
        };
        let mut current: Option<Note> = None;
        let mut invalid = 0usize;
        let mut body_started = false;
        for line in text.lines() {
            if let Some(header) = line.strip_prefix('#') {
                if let Some(note) = current.take() {
                    result.entries.push(note);
                }
                current = parse_header(header);
                body_started = false;
                if current.is_none() {
                    invalid += 1;
                }
            } else if let Some(note) = &mut current {
                if body_started {
                    note.text.push('\n');
                }
                note.text.push_str(line);
                body_started = true;
            } else if !line.is_empty() {
                invalid += 1;
            }
        }
        if let Some(note) = current {
            result.entries.push(note);
        }
        let status =
            (invalid > 0).then(|| format!("review.notes: ignored {invalid} invalid line(s)"));
        (result, status)
    }

    pub fn for_item<'a>(&'a self, item: &'a Item) -> impl Iterator<Item = &'a Note> {
        let item_path = Path::new(OsStr::from_bytes(&item.new_path));
        let path = if self.relative {
            self.prefix.join(item_path)
        } else {
            item_path.to_owned()
        };
        self.entries
            .iter()
            .filter(move |note| path.as_os_str().as_encoded_bytes() == note.path.as_bytes())
    }
}

fn parse_header(header: &str) -> Option<Note> {
    let (path_line, column) = header.rsplit_once(':')?;
    let (path, line) = path_line.rsplit_once(':')?;
    let line: usize = line.parse().ok()?;
    let column: usize = column.parse().ok()?;
    if line == 0
        || column == 0
        || path.is_empty()
        || !Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
    {
        return None;
    }
    Some(Note {
        path: path.into(),
        line,
        column,
        text: String::new(),
    })
}

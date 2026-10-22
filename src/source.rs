use crate::git::{self, Item, SourceIdentity};
use anyhow::{Context, Result, bail};
use std::{io::Write, path::Path, process::Stdio};

#[derive(Debug)]
pub struct Side {
    pub path: Vec<u8>,
    pub content: String,
}

#[derive(Debug)]
pub struct Sources {
    pub old: Side,
    pub new: Side,
}

fn blob(cwd: &Path, oid: &str) -> Result<Vec<u8>> {
    if oid.bytes().all(|byte| byte == b'0') {
        return Ok(Vec::new());
    }
    let result = git::command(cwd).args(["cat-file", "blob", oid]).output()?;
    if !result.status.success() {
        bail!("compared blob {oid} is unavailable");
    }
    Ok(result.stdout)
}

pub(crate) fn hunk(line: &str) -> Option<(usize, usize, usize, usize)> {
    fn range(value: &str) -> Option<(usize, usize)> {
        let (start, count) = value[1..].split_once(',').unwrap_or((&value[1..], "1"));
        Some((start.parse().ok()?, count.parse().ok()?))
    }
    let mut fields = line.strip_prefix("@@ ")?.split_whitespace();
    let old = fields.next()?;
    let new = fields.next()?;
    if !old.starts_with('-') || !new.starts_with('+') || fields.next()? != "@@" {
        return None;
    }
    let (old_start, old_count) = range(old)?;
    let (new_start, new_count) = range(new)?;
    Some((old_start, old_count, new_start, new_count))
}

/// Parse the ASCII coordinate prefix without requiring source context in a
/// hunk heading to be UTF-8.
pub(crate) fn hunk_bytes(line: &[u8]) -> Option<(usize, usize, usize, usize)> {
    let end = line.windows(3).position(|part| part == b" @@")? + 3;
    hunk(std::str::from_utf8(&line[..end]).ok()?)
}

pub(crate) struct Reconstructed {
    pub old: Vec<u8>,
    pub new: Vec<u8>,
    pub new_oid: String,
}

// Reconstruct bytes for identity/state independently from text enrichment.
// Missing-newline markers describe the preceding source row; they are not
// source bytes. All context still has to match the pinned old blob exactly.
pub(crate) fn reconstruct(cwd: &Path, item: &Item) -> Result<Reconstructed> {
    let old = match &item.old_source {
        SourceIdentity::Blob(oid) => blob(cwd, oid)?,
        SourceIdentity::Empty => Vec::new(),
        _ => bail!("this representation has no proven ordinary old source"),
    };
    reconstruct_from_old(cwd, item, old, true)
}

pub(crate) fn reconstruct_from_old(
    cwd: &Path,
    item: &Item,
    old: Vec<u8>,
    verify_oid: bool,
) -> Result<Reconstructed> {
    if !matches!(
        item.new_source,
        SourceIdentity::CapturedPatch | SourceIdentity::Empty
    ) {
        bail!("this representation has no proven ordinary new source");
    }
    let old_lines: Vec<_> = old.split_inclusive(|byte| *byte == b'\n').collect();
    let lines: Vec<_> = item.patch.split_inclusive(|byte| *byte == b'\n').collect();
    let mut result = Vec::new();
    let mut cursor = 0;
    let mut expected: Option<(usize, usize)> = None;
    let mut seen = (0, 0);
    for (index, line) in lines.iter().enumerate() {
        if let Some((start, count, new_start, new_count)) = hunk_bytes(line) {
            if expected.is_some_and(|value| value != seen) {
                bail!("hunk counts do not map to compared source");
            }
            let target = if count == 0 {
                start
            } else {
                start.saturating_sub(1)
            };
            if target < cursor || target > old_lines.len() {
                bail!("old hunk coordinate is unavailable");
            }
            for unchanged in &old_lines[cursor..target] {
                result.extend_from_slice(unchanged);
            }
            cursor = target;
            let new_target = if new_count == 0 {
                new_start
            } else {
                new_start.saturating_sub(1)
            };
            if result.split_inclusive(|byte| *byte == b'\n').count() != new_target {
                bail!("new hunk coordinate is unavailable");
            }
            expected = Some((count, new_count));
            seen = (0, 0);
        } else if expected.is_some() {
            if line.starts_with(b"\\ No newline at end of file") {
                continue;
            }
            let mut payload = &line[1..];
            if lines
                .get(index + 1)
                .is_some_and(|next| next.starts_with(b"\\ No newline at end of file"))
            {
                payload = payload
                    .strip_suffix(b"\n")
                    .context("missing-newline marker has no source row")?;
            }
            match line.first() {
                Some(b' ' | b'-') => {
                    if old_lines.get(cursor).copied() != Some(payload) {
                        bail!("patch differs from pinned old source");
                    }
                    cursor += 1;
                    seen.0 += 1;
                    if line[0] == b' ' {
                        result.extend_from_slice(payload);
                        seen.1 += 1;
                    }
                }
                Some(b'+') => {
                    result.extend_from_slice(payload);
                    seen.1 += 1;
                }
                _ => bail!("patch contains an unmapped source line"),
            }
        }
    }
    if expected.is_none() || expected != Some(seen) {
        bail!("incomplete captured hunk");
    }
    for unchanged in &old_lines[cursor..] {
        result.extend_from_slice(unchanged);
    }
    let mut child = git::command(cwd)
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .context("cannot verify captured source")?
        .write_all(&result)?;
    let hash = child.wait_with_output()?;
    if !hash.status.success() {
        bail!("cannot hash reconstructed source");
    }
    let new_oid = String::from_utf8(hash.stdout)?.trim().to_owned();
    if item.new_source == SourceIdentity::Empty {
        if !result.is_empty() {
            bail!("deleted source unexpectedly contains bytes");
        }
    } else if verify_oid && !new_oid.starts_with(&item.new_oid) {
        bail!("patch differs from the captured new-source hash");
    }
    Ok(Reconstructed {
        old,
        new: result,
        new_oid,
    })
}

// Enrichment retains its explicit text/newline limitations. State identity uses
// the verified byte reconstruction above and does not depend on highlighting.
pub fn load(cwd: &Path, item: &Item) -> Result<Sources> {
    if ![&item.old_mode, &item.new_mode]
        .iter()
        .all(|mode| matches!(mode.as_str(), "100644" | "100755" | "000000"))
    {
        bail!("source enrichment requires ordinary file modes");
    }
    let patch = std::str::from_utf8(&item.patch).context("patch is not UTF-8")?;
    if patch.lines().any(|line| line.starts_with("\\ No newline")) {
        bail!("source mapping for missing final newlines is unavailable");
    }
    if !matches!(
        item.old_source,
        SourceIdentity::Blob(_) | SourceIdentity::Empty
    ) || !matches!(
        item.new_source,
        SourceIdentity::CapturedPatch | SourceIdentity::Empty
    ) {
        bail!("source enrichment requires mapped ordinary patch sources");
    }
    let (old_bytes, new_bytes) = if let Some((old, new)) = &item.captured_sources {
        (old.clone(), new.clone())
    } else {
        let reconstructed = reconstruct(cwd, item)?;
        (reconstructed.old, reconstructed.new)
    };
    Ok(Sources {
        old: Side {
            path: item.old_path.clone(),
            content: String::from_utf8(old_bytes).context("compared source is not UTF-8")?,
        },
        new: Side {
            path: item.new_path.clone(),
            content: String::from_utf8(new_bytes).context("compared source is not UTF-8")?,
        },
    })
}

/// Native identity and immutable content stay outside the UTF-8 tag registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ComparedSide {
    Old,
    New,
}
impl ComparedSide {
    pub fn label(self) -> &'static str {
        match self {
            Self::Old => "old",
            Self::New => "new",
        }
    }
}
#[derive(Debug, Clone)]
pub struct CapturedSource {
    pub side: ComparedSide,
    pub path: Vec<u8>,
    pub bytes: std::sync::Arc<[u8]>,
    pub identity: String,
}
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    pub sources: Vec<CapturedSource>,
    pub gaps: Vec<String>,
    lookup: std::collections::BTreeMap<(ComparedSide, Vec<u8>), usize>,
    usage: [(usize, usize); 2],
}
impl Catalog {
    pub fn insert(&mut self, side: ComparedSide, path: Vec<u8>, bytes: Vec<u8>) {
        let identity = crate::state::digest([bytes.as_slice()]);
        let source = CapturedSource {
            side,
            path,
            bytes: bytes.into(),
            identity,
        };
        let key = (side, source.path.clone());
        let usage = &mut self.usage[side as usize];
        if let Some(&id) = self.lookup.get(&key) {
            usage.1 = usage.1 - self.sources[id].bytes.len() + source.bytes.len();
            self.sources[id] = source;
        } else {
            usage.0 += 1;
            usage.1 += source.bytes.len();
            self.lookup.insert(key, self.sources.len());
            self.sources.push(source);
        }
    }
    pub fn insert_captured(&mut self, side: ComparedSide, path: Vec<u8>, bytes: &[u8]) {
        self.insert_with_limits(
            side,
            path,
            bytes,
            crate::git::CATALOG_FILES_PER_SIDE,
            crate::git::CATALOG_BYTES_PER_SIDE,
        );
    }
    fn insert_with_limits(
        &mut self,
        side: ComparedSide,
        path: Vec<u8>,
        bytes: &[u8],
        max_files: usize,
        max_bytes: usize,
    ) {
        let (count, total) = self.usage[side as usize];
        let previous = self
            .find(side, &path)
            .map(|id| self.sources[id].bytes.len());
        if bytes.len() > crate::navigation::MAX_FILE_BYTES
            || (previous.is_none() && count >= max_files)
            || total - previous.unwrap_or(0) + bytes.len() > max_bytes
        {
            self.gaps.push(format!(
                "{} {}: catalog count/byte limit, skipped",
                side.label(),
                crate::git::visible_bytes(&path)
            ));
            return;
        }
        self.insert(side, path, bytes.to_vec());
    }
    pub fn find(&self, side: ComparedSide, path: &[u8]) -> Option<usize> {
        self.lookup.get(&(side, path.to_vec())).copied()
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;
    #[test]
    fn independent_side_budgets_preserve_stable_ids_and_changed_content() {
        let mut catalog = Catalog::default();
        for side in [ComparedSide::Old, ComparedSide::New] {
            catalog.insert_with_limits(side, b"changed.rs".to_vec(), b"first", 2, 8);
            catalog.insert_with_limits(side, b"tiny.rs".to_vec(), b"x", 2, 8);
            catalog.insert_with_limits(side, b"extra.rs".to_vec(), b"x", 2, 8);
            let id = catalog.find(side, b"changed.rs").unwrap();
            catalog.insert_with_limits(side, b"changed.rs".to_vec(), b"second", 2, 8);
            assert_eq!(catalog.find(side, b"changed.rs"), Some(id));
            assert_eq!(catalog.sources[id].bytes.as_ref(), b"second");
            catalog.insert_with_limits(side, b"changed.rs".to_vec(), b"too many bytes", 2, 8);
            assert_eq!(catalog.sources[id].bytes.as_ref(), b"second");
            assert_eq!(catalog.find(side, b"extra.rs"), None);
        }
        assert_eq!(catalog.sources.len(), 4);
        assert_eq!(catalog.gaps.len(), 4);
    }
}

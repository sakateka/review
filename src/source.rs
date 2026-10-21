use crate::git::{self, Item};
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

// Reconstruct the new side from a pinned old blob and exactly the captured
// patch. We never read the checkout to fill gaps in an old or mutable side.
pub fn load(cwd: &Path, item: &Item) -> Result<Sources> {
    let old =
        String::from_utf8(blob(cwd, &item.old_oid)?).context("compared source is not UTF-8")?;
    let patch = std::str::from_utf8(&item.patch).context("patch is not UTF-8")?;
    if patch.lines().any(|line| line.starts_with("\\ No newline")) {
        bail!("source mapping for missing final newlines is unavailable");
    }
    let old_lines: Vec<_> = old.split_inclusive('\n').collect();
    let mut result = String::new();
    let mut cursor = 0;
    let mut expected: Option<(usize, usize)> = None;
    let mut seen = (0, 0);
    for line in patch.split_inclusive('\n') {
        if let Some((start, count, new_start, new_count)) = hunk(line) {
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
                result.push_str(unchanged);
            }
            cursor = target;
            let new_target = if new_count == 0 {
                new_start
            } else {
                new_start.saturating_sub(1)
            };
            if result.lines().count() != new_target {
                bail!("new hunk coordinate is unavailable");
            }
            expected = Some((count, new_count));
            seen = (0, 0);
        } else if expected.is_some() {
            match line.as_bytes().first() {
                Some(b' ' | b'-') => {
                    if old_lines.get(cursor).copied() != Some(&line[1..]) {
                        bail!("patch differs from pinned old source");
                    }
                    cursor += 1;
                    seen.0 += 1;
                    if let Some(context) = line.strip_prefix(' ') {
                        result.push_str(context);
                        seen.1 += 1;
                    }
                }
                Some(b'+') => {
                    result.push_str(&line[1..]);
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
        result.push_str(unchanged);
    }
    if !item.new_oid.bytes().all(|byte| byte == b'0') {
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
            .write_all(result.as_bytes())?;
        let hash = child.wait_with_output()?;
        if !hash.status.success() || String::from_utf8_lossy(&hash.stdout).trim() != item.new_oid {
            bail!("patch differs from the captured new-source hash");
        }
    } else if !result.is_empty() {
        bail!("deleted source unexpectedly contains bytes");
    }
    Ok(Sources {
        old: Side {
            path: item.old_path.clone(),
            content: old,
        },
        new: Side {
            path: item.new_path.clone(),
            content: result,
        },
    })
}

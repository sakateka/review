use crate::git::Item;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize, Deserialize)]
struct Saved {
    version: u32,
    marks: BTreeMap<String, String>,
    #[serde(default)]
    tombstones: BTreeSet<String>,
    #[serde(default)]
    pending: BTreeMap<String, String>,
}

pub fn digest(parts: impl IntoIterator<Item = impl AsRef<[u8]>>) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        let part = part.as_ref();
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    format!("{:x}", hash.finalize())
}

#[derive(Debug)]
pub struct Store {
    pub path: PathBuf,
    pub(crate) legacy_path: PathBuf,
}

pub struct LoadedMarks {
    pub marks: Vec<bool>,
    pub pending_legacy: bool,
}

impl Store {
    pub fn for_session(cwd: &Path, args: &[OsString]) -> Result<Self> {
        let base = if let Some(path) =
            std::env::var_os("XDG_STATE_HOME").filter(|path| !path.is_empty())
        {
            PathBuf::from(path)
        } else {
            PathBuf::from(
                std::env::var_os("HOME").context("HOME and XDG_STATE_HOME are unavailable")?,
            )
            .join(".local/state")
        };
        if !base.is_absolute() {
            bail!("state home must be an absolute path");
        }
        Ok(Self::at(&base, cwd, args))
    }

    pub fn at(base: &Path, cwd: &Path, args: &[OsString]) -> Self {
        let context: BTreeMap<_, _> = std::env::vars_os()
            .filter(|(key, _)| key.as_encoded_bytes().starts_with(b"GIT_"))
            .collect();
        let mut parts = vec![
            b"review-session-v2".as_slice(),
            cwd.as_os_str().as_encoded_bytes(),
            b"git-environment",
        ];
        // Repository identity separates linked worktrees and repositories
        // reached through the same cwd/environment without depending on refs.
        let mut repository_context = Vec::new();
        for option in ["--show-toplevel", "--git-common-dir", "--git-path"] {
            let output = if option == "--git-path" {
                crate::git::command(cwd)
                    .args(["rev-parse", option, "index"])
                    .output()
            } else {
                crate::git::command(cwd)
                    .args(["rev-parse", option])
                    .output()
            };
            if let Ok(output) = output
                && output.status.success()
            {
                repository_context
                    .extend_from_slice(output.stdout.strip_suffix(b"\n").unwrap_or(&output.stdout));
                repository_context.push(0);
            }
        }
        parts.push(&repository_context);
        for (key, value) in &context {
            parts.push(key.as_encoded_bytes());
            parts.push(value.as_encoded_bytes());
        }
        parts.push(b"original-arguments");
        parts.extend(args.iter().map(|arg| arg.as_encoded_bytes()));
        let mut legacy_parts = vec![
            b"review-session-v1".as_slice(),
            cwd.as_os_str().as_encoded_bytes(),
            b"git-environment",
        ];
        for (key, value) in &context {
            legacy_parts.push(key.as_encoded_bytes());
            legacy_parts.push(value.as_encoded_bytes());
        }
        legacy_parts.push(b"original-arguments");
        legacy_parts.extend(args.iter().map(|arg| arg.as_encoded_bytes()));
        Self {
            path: base.join("review").join(format!("{}.json", digest(parts))),
            legacy_path: base
                .join("review")
                .join(format!("{}.json", digest(legacy_parts))),
        }
    }

    #[cfg(test)]
    pub fn load(&self, items: &[Item]) -> Result<Vec<bool>> {
        Ok(self.load_report(items)?.marks)
    }

    fn read_saved(path: &Path) -> Result<Option<Saved>> {
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error).context("cannot read review state"),
        };
        let saved: Saved = serde_json::from_slice(&bytes)
            .context("corrupt review state; marks were not restored")?;
        if !matches!(saved.version, 1 | 2) {
            bail!("unsupported review state version {}", saved.version);
        }
        Ok(Some(saved))
    }

    // Both paths remain inputs after migration. Legacy bytes are never edited;
    // explicit v2 decisions take durable precedence over them, including unmark.
    fn records(&self) -> Result<Saved> {
        let primary = Self::read_saved(&self.path)?;
        let legacy = Self::read_saved(&self.legacy_path)?;
        let mut combined = Saved {
            version: 2,
            marks: BTreeMap::new(),
            tombstones: BTreeSet::new(),
            pending: BTreeMap::new(),
        };
        if let Some(legacy) = legacy {
            combined.pending.extend(legacy.pending);
            combined.pending.extend(legacy.marks);
        }
        if let Some(primary) = primary {
            combined.pending.extend(primary.pending);
            if primary.version == 1 {
                combined.pending.extend(primary.marks);
            } else {
                combined.marks = primary.marks;
                combined.tombstones = primary.tombstones;
            }
        }
        Ok(combined)
    }

    pub fn load_report(&self, items: &[Item]) -> Result<LoadedMarks> {
        let saved = self.records()?;
        let proven_legacy = |item: &Item, value: &String| {
            item.canonical_identity.is_some() && item.legacy_fingerprints.contains(value)
        };
        let marks = items
            .iter()
            .map(|item| {
                let key = digest([&item.new_path]);
                if saved.tombstones.contains(&key) {
                    return false;
                }
                if let Some(value) = saved.marks.get(&key) {
                    return item.canonical_identity.as_ref() == Some(value);
                }
                saved
                    .pending
                    .get(&key)
                    .is_some_and(|value| proven_legacy(item, value))
            })
            .collect();
        let pending_legacy = saved.pending.iter().any(|(key, value)| {
            !saved.tombstones.contains(key)
                && !saved.marks.contains_key(key)
                && !items
                    .iter()
                    .any(|item| digest([&item.new_path]) == *key && proven_legacy(item, value))
        });
        Ok(LoadedMarks {
            marks,
            pending_legacy,
        })
    }

    pub fn save(&self, items: &[Item], marks: &[bool]) -> Result<()> {
        // Recheck both files before replacing anything, preserving corrupt or
        // unsupported data even if it appeared since the application loaded.
        let mut saved = self.records()?;
        for (item, mark) in items.iter().zip(marks) {
            let key = digest([&item.new_path]);
            if *mark {
                let identity = item
                    .canonical_identity
                    .clone()
                    .context("complete compared contents could not be proven")?;
                saved.marks.insert(key.clone(), identity);
                saved.tombstones.remove(&key);
            } else if saved.marks.remove(&key).is_some()
                || saved.pending.get(&key).is_some_and(|value| {
                    item.canonical_identity.is_some() && item.legacy_fingerprints.contains(value)
                })
            {
                saved.tombstones.insert(key);
            }
        }
        let bytes = serde_json::to_vec(&saved)?;
        let directory = self.path.parent().context("state path has no parent")?;
        std::fs::create_dir_all(directory).context("cannot create review state directory")?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory)
            .context("cannot create temporary review state")?;
        temporary
            .write_all(&bytes)
            .context("cannot write review state")?;
        temporary
            .as_file()
            .sync_all()
            .context("cannot flush review state")?;
        temporary
            .persist(&self.path)
            .context("cannot atomically replace review state")?;
        Ok(())
    }
}

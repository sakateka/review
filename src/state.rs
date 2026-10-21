use crate::git::Item;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, Serialize, Deserialize)]
struct Saved {
    version: u32,
    marks: BTreeMap<String, String>,
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

fn fingerprint(item: &Item) -> String {
    digest([&item.old_path, &item.new_path, &item.metadata, &item.patch])
}

#[derive(Debug)]
pub struct Store {
    pub path: PathBuf,
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
            b"review-session-v1".as_slice(),
            cwd.as_os_str().as_encoded_bytes(),
            b"git-environment",
        ];
        for (key, value) in &context {
            parts.push(key.as_encoded_bytes());
            parts.push(value.as_encoded_bytes());
        }
        parts.push(b"original-arguments");
        parts.extend(args.iter().map(|arg| arg.as_encoded_bytes()));
        Self {
            path: base.join("review").join(format!("{}.json", digest(parts))),
        }
    }

    pub fn load(&self, items: &[Item]) -> Result<Vec<bool>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(vec![false; items.len()]);
            }
            Err(error) => return Err(error).context("cannot read review state"),
        };
        let saved: Saved = serde_json::from_slice(&bytes)
            .context("corrupt review state; marks were not restored")?;
        if saved.version != 1 {
            bail!("unsupported review state version {}", saved.version);
        }
        Ok(items
            .iter()
            .map(|item| {
                saved
                    .marks
                    .get(&digest([&item.new_path]))
                    .is_some_and(|saved| {
                        saved == &fingerprint(item) || item.legacy_fingerprints.contains(saved)
                    })
            })
            .collect())
    }

    pub fn save(&self, items: &[Item], marks: &[bool]) -> Result<()> {
        let marks = items
            .iter()
            .zip(marks)
            .filter(|(_, mark)| **mark)
            .map(|(item, _)| (digest([&item.new_path]), fingerprint(item)))
            .collect();
        let bytes = serde_json::to_vec(&Saved { version: 1, marks })?;
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

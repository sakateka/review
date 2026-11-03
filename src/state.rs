use crate::git::Item;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::Write,
    os::unix::ffi::OsStringExt,
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
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug)]
pub struct Store {
    pub path: PathBuf,
    positions_path: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Position {
    pub row: usize,
    pub column: usize,
    pub top: usize,
    pub left: usize,
    pub after_changes: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Positions {
    pub last: Option<String>,
    pub files: BTreeMap<String, Position>,
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
        // The per-worktree Git directory distinguishes linked worktrees and
        // ignores invocation arguments, current refs and the caller's subdir.
        let git_path = |option: &str| {
            crate::git::command(cwd)
                .args(["rev-parse", option])
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
                })
                .map(|path| path.canonicalize().unwrap_or(path))
        };
        let git_dir = git_path("--absolute-git-dir");
        let root = git_path("--show-toplevel")
            .unwrap_or_else(|| cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()));
        let key = digest([
            b"review-displayed-patch-v3".as_slice(),
            git_dir
                .as_deref()
                .unwrap_or(&root)
                .as_os_str()
                .as_encoded_bytes(),
            root.as_os_str().as_encoded_bytes(),
        ]);
        let comparison = digest(
            std::iter::once(key.as_bytes())
                .chain(args.iter().map(|arg| arg.as_os_str().as_encoded_bytes())),
        );
        Self {
            path: base.join("review").join(format!("{key}.json")),
            positions_path: base
                .join("review")
                .join("positions")
                .join(format!("{comparison}.json")),
        }
    }

    pub fn load_positions(&self) -> Result<Positions> {
        match std::fs::read(&self.positions_path) {
            Ok(bytes) => {
                serde_json::from_slice(&bytes).context("cannot read saved cursor positions")
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Positions::default()),
            Err(error) => Err(error).context("cannot read saved cursor positions"),
        }
    }

    pub fn save_positions(&self, positions: &Positions) -> Result<()> {
        let directory = self
            .positions_path
            .parent()
            .context("position path has no parent")?;
        std::fs::create_dir_all(directory).context("cannot create position directory")?;
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(&mut temporary, positions)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(&self.positions_path)
            .context("cannot save cursor positions")?;
        Ok(())
    }

    fn records(&self) -> Result<Saved> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Saved {
                    version: 3,
                    marks: BTreeMap::new(),
                });
            }
            Err(error) => return Err(error).context("cannot read review state"),
        };
        let saved: Saved = serde_json::from_slice(&bytes)
            .context("corrupt review state; marks were not restored")?;
        if saved.version != 3 {
            bail!("unsupported review state version {}", saved.version);
        }
        Ok(saved)
    }

    pub fn load(&self, items: &[Item]) -> Result<Vec<bool>> {
        let saved = self.records()?;
        Ok(items
            .iter()
            .map(|item| saved.marks.get(&digest([&item.new_path])) == Some(&item.patch_identity()))
            .collect())
    }

    /// Only explicit decisions are updated. A stale instance must not write
    /// its other visible marks over more recent decisions from another process.
    pub fn save_mark(&self, item: &Item, mark: bool) -> Result<()> {
        self.save_updates(std::iter::once((item, mark)))
    }

    #[cfg(test)]
    pub fn save(&self, items: &[Item], marks: &[bool]) -> Result<()> {
        self.save_updates(items.iter().zip(marks.iter().copied()))
    }

    fn save_updates<'a>(&self, updates: impl IntoIterator<Item = (&'a Item, bool)>) -> Result<()> {
        let directory = self.path.parent().context("state path has no parent")?;
        std::fs::create_dir_all(directory).context("cannot create review state directory")?;
        // Serialize read/modify/replace across active processes. The lock file
        // stays in the external state directory, never the reviewed repository.
        let lock = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.path.with_extension("lock"))
            .context("cannot open review state lock")?;
        lock.lock().context("cannot lock review state")?;
        // Re-read before replacing so corrupt/unsupported state is preserved.
        let mut saved = self.records()?;
        for (item, mark) in updates {
            let key = digest([&item.new_path]);
            if mark {
                saved.marks.insert(key, item.patch_identity());
            } else {
                saved.marks.remove(&key);
            }
        }
        let bytes = serde_json::to_vec(&saved)?;
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

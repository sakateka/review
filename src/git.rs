use anyhow::{Context, Result, bail};
use std::{
    ffi::OsString,
    path::Path,
    process::{Command, Output},
};

#[derive(Debug, Clone)]
pub struct Item {
    pub old_path: Vec<u8>,
    pub new_path: Vec<u8>,
    pub old_oid: String,
    pub new_oid: String,
    pub patch: Vec<u8>,
    pub metadata: Vec<u8>,
}

impl Item {
    pub fn name(&self) -> String {
        String::from_utf8_lossy(&self.new_path)
            .escape_debug()
            .to_string()
    }
}

#[derive(Debug)]
pub struct Capture {
    pub items: Vec<Item>,
    pub original: Vec<u8>,
    pub unavailable: Option<String>,
    pub diagnostic: String,
    pub cwd: std::path::PathBuf,
}

pub(crate) fn command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(cwd)
        .args(["-c", "core.fsmonitor=false"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_PAGER", "cat");
    for (key, _) in std::env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"GIT_TRACE") {
            command.env_remove(key);
        }
    }
    command
}

fn output(cwd: &Path, args: &[OsString], structured: bool) -> Result<Output> {
    let mut command = command(cwd);
    command.args(["--no-pager", "diff", "--no-ext-diff", "--no-textconv"]);
    if structured {
        command.args([
            "--raw",
            "-z",
            "--patch",
            "--full-index",
            "--no-abbrev",
            "--no-color",
        ]);
    }
    let result = command
        .args(args)
        .output()
        .context("cannot execute system Git")?;
    let differences_are_success = args
        .iter()
        .any(|arg| arg == "--exit-code" || arg == "--quiet" || arg == "--no-index");
    if !result.status.success() && !(differences_are_success && result.status.code() == Some(1)) {
        bail!(
            "Git comparison failed ({}): {}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        );
    }
    Ok(result)
}

// Unknown options are passed to Git unchanged in original-output mode. This
// conservative nominal parser never guesses the boundaries of another format.
fn nominal(args: &[OsString]) -> bool {
    let mut after_paths = false;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if after_paths {
            continue;
        }
        if arg == "--" {
            after_paths = true;
            continue;
        }
        if arg == "-S" || arg == "-G" {
            args.next();
            continue;
        }
        let bytes = arg.as_encoded_bytes();
        if bytes.starts_with(b"-")
            && !matches!(
                bytes,
                b"--cached"
                    | b"--staged"
                    | b"--pickaxe-all"
                    | b"--exit-code"
                    | b"--reverse"
                    | b"-R"
            )
            && !bytes.starts_with(b"-S")
            && !bytes.starts_with(b"-G")
        {
            return false;
        }
    }
    true
}

fn reject_writes(args: &[OsString]) -> Result<()> {
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if arg == "-S" || arg == "-G" {
            args.next();
            continue;
        }
        let bytes = arg.as_encoded_bytes();
        if (bytes.starts_with(b"--out") && !bytes.starts_with(b"--output-indicator-"))
            || (bytes.len() > 2 && b"--ext-diff".starts_with(bytes))
            || bytes.starts_with(b"--ext-diff")
            || (bytes.len() > 2 && bytes != b"--text" && b"--textconv".starts_with(bytes))
            || bytes.starts_with(b"--textconv")
        {
            bail!(
                "Git option {:?} conflicts with read-only capture (output destinations and external/textconv helpers are unavailable)",
                arg
            );
        }
    }
    Ok(())
}

pub fn capture(cwd: &Path, args: &[OsString]) -> Result<Capture> {
    reject_writes(args)?;
    // Clean/process filters can execute arbitrary helpers while Git hashes the
    // working tree, even with --no-textconv. Until a comparison-specific policy
    // is available, diagnose configured filters rather than invoking them.
    let filters = command(cwd)
        .args([
            "config",
            "--null",
            "--get-regexp",
            r"^filter\..*\.(clean|process)$",
        ])
        .output()
        .context("cannot inspect Git's source-filter policy")?;
    if filters.status.success() && !filters.stdout.is_empty() {
        bail!(
            "configured Git clean/process filters are unavailable during read-only review; their external helpers may write files"
        );
    }
    if !filters.status.success() && filters.status.code() != Some(1) {
        bail!(
            "cannot inspect Git configuration: {}",
            String::from_utf8_lossy(&filters.stderr)
        );
    }
    if nominal(args) {
        let result = output(cwd, args, true)?;
        if let Some(items) = parse(&result.stdout) {
            return Ok(Capture {
                items,
                original: Vec::new(),
                unavailable: None,
                diagnostic: String::from_utf8_lossy(&result.stderr).into_owned(),
                cwd: cwd.into(),
            });
        }
    }
    let result = output(cwd, args, false)?;
    Ok(Capture { items: Vec::new(), original: result.stdout,
        unavailable: Some("File structure is unavailable for this output. Marks and source enrichment are disabled.".into()),
        diagnostic: String::from_utf8_lossy(&result.stderr).into_owned(), cwd: cwd.into() })
}

fn take_nul<'a>(bytes: &mut &'a [u8]) -> Option<&'a [u8]> {
    let end = bytes.iter().position(|byte| *byte == 0)?;
    let value = &bytes[..end];
    *bytes = &bytes[end + 1..];
    Some(value)
}

fn parse(mut bytes: &[u8]) -> Option<Vec<Item>> {
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    let mut items = Vec::new();
    while bytes.first() == Some(&b':') {
        let metadata = take_nul(&mut bytes)?;
        let fields: Vec<_> = metadata.split(|byte| *byte == b' ').collect();
        if fields.len() != 5 || !matches!(fields[4], b"M" | b"A" | b"D" | b"T") {
            return None;
        }
        let old_oid = std::str::from_utf8(fields[2]).ok()?.to_owned();
        let new_oid = std::str::from_utf8(fields[3]).ok()?.to_owned();
        if ![&old_oid, &new_oid].iter().all(|oid| {
            (oid.len() == 40 || oid.len() == 64) && oid.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            return None;
        }
        let path = take_nul(&mut bytes)?.to_vec();
        items.push(Item {
            old_path: path.clone(),
            new_path: path,
            old_oid,
            new_oid,
            patch: Vec::new(),
            metadata: metadata.to_vec(),
        });
    }
    if bytes.first() != Some(&0) {
        return None;
    }
    bytes = &bytes[1..];
    let mut starts: Vec<usize> = bytes
        .windows(11)
        .enumerate()
        .filter_map(|(index, line)| {
            (line == b"diff --git " && (index == 0 || bytes[index - 1] == b'\n')).then_some(index)
        })
        .collect();
    if starts.len() != items.len() || starts.first() != Some(&0) {
        return None;
    }
    starts.push(bytes.len());
    for (index, item) in items.iter_mut().enumerate() {
        let patch = &bytes[starts[index]..starts[index + 1]];
        // A textual patch's index record must agree with the raw item. Binary,
        // mode-only and transformed patches safely retain original-output mode.
        let lines: Vec<_> = patch.split(|byte| *byte == b'\n').collect();
        let identity = lines.iter().find_map(|line| line.strip_prefix(b"index "))?;
        let hashes = identity.split(|byte| *byte == b' ').next()?;
        let hashes = std::str::from_utf8(hashes).ok()?;
        let (old, new) = hashes.split_once("..")?;
        if old != item.old_oid
            || (item.new_oid.bytes().any(|byte| byte != b'0') && new != item.new_oid)
            || new.len() != old.len()
            || !new.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !lines.iter().any(|line| line.starts_with(b"@@ "))
        {
            return None;
        }
        let old_path = lines.iter().find_map(|line| line.strip_prefix(b"--- "))?;
        let new_path = lines.iter().find_map(|line| line.strip_prefix(b"+++ "))?;
        if !matches_path(old_path, b"a/", &item.old_path, old)
            || !matches_path(new_path, b"b/", &item.new_path, new)
        {
            return None;
        }
        // The patch computes the actual mutable-side hash; the raw record may
        // contain zeroes for that same side. Keep its proven content identity.
        item.new_oid = new.to_owned();
        item.patch = patch.to_vec();
    }
    Some(items)
}

fn matches_path(encoded: &[u8], prefix: &[u8], path: &[u8], oid: &str) -> bool {
    if oid.bytes().all(|byte| byte == b'0') {
        return encoded == b"/dev/null";
    }
    let encoded = encoded.strip_suffix(b"\t").unwrap_or(encoded);
    let Some(decoded) = unquote(encoded) else {
        return false;
    };
    decoded.strip_prefix(prefix) == Some(path)
}

fn unquote(encoded: &[u8]) -> Option<Vec<u8>> {
    if !encoded.starts_with(b"\"") {
        return Some(encoded.to_vec());
    }
    let encoded = encoded.strip_prefix(b"\"")?.strip_suffix(b"\"")?;
    let mut result = Vec::new();
    let mut bytes = encoded.iter().copied().peekable();
    while let Some(byte) = bytes.next() {
        if byte != b'\\' {
            result.push(byte);
            continue;
        }
        let byte = bytes.next()?;
        result.push(match byte {
            b'a' => 7,
            b'b' => 8,
            b'f' => 12,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 11,
            b'\\' | b'"' => byte,
            b'0'..=b'7' => {
                let mut value = u16::from(byte - b'0');
                for _ in 0..2 {
                    if bytes.peek().is_some_and(|next| matches!(next, b'0'..=b'7')) {
                        value = value * 8 + u16::from(bytes.next()? - b'0');
                    }
                }
                u8::try_from(value).ok()?
            }
            _ => return None,
        });
    }
    Some(result)
}

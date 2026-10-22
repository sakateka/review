use anyhow::{Context, Result, bail};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, ExitStatus},
};

/// Only explicit identities authorize source access. A hash printed by Git is
/// not by itself proof that a mutable, binary, or multi-parent side is a blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceIdentity {
    Blob(String),
    Empty,
    CapturedPatch,
    Unavailable,
}

#[derive(Debug, Clone)]
pub struct Item {
    pub old_path: Vec<u8>,
    pub new_path: Vec<u8>,
    pub old_oid: String,
    pub new_oid: String,
    pub old_mode: String,
    pub new_mode: String,
    pub status: String,
    pub old_source: SourceIdentity,
    pub new_source: SourceIdentity,
    pub patch: Vec<u8>,
    pub legacy_fingerprints: Vec<String>,
    /// Digest of both complete captured sides and semantic item metadata.
    /// Missing when the representation cannot prove complete side bytes.
    pub canonical_identity: Option<String>,
    pub captured_sources: Option<(Vec<u8>, Vec<u8>)>,
    statistics: Statistics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Statistics {
    Text { additions: usize, deletions: usize },
    Binary,
    Unavailable,
}

impl Item {
    pub fn statistics(&self) -> Statistics {
        self.statistics
    }

    /// Count only the validated native hunks, never a reconstructed/live diff.
    fn derive_statistics(&self) -> Statistics {
        let mut in_hunk = false;
        let (mut additions, mut deletions) = (0, 0);
        for line in self.patch.split(|byte| *byte == b'\n') {
            if line.starts_with(b"@@ ") {
                in_hunk = true;
            } else if in_hunk {
                match line.first() {
                    Some(b'+') => additions += 1,
                    Some(b'-') => deletions += 1,
                    _ => {}
                }
            } else if line.starts_with(b"Binary files ") {
                return Statistics::Binary;
            }
        }
        if in_hunk
            || self
                .captured_sources
                .as_ref()
                .is_some_and(|(old, new)| old == new)
        {
            Statistics::Text {
                additions,
                deletions,
            }
        } else {
            // A metadata header alone can hide content suppressed by Git's
            // whitespace/filter options. Only complete equal sides prove zero.
            Statistics::Unavailable
        }
    }

    pub fn name(&self) -> String {
        if self.old_path != self.new_path {
            format!(
                "{} → {}",
                visible_name(&self.old_path),
                visible_name(&self.new_path)
            )
        } else {
            visible_name(&self.new_path)
        }
    }
}

pub(crate) fn visible_name(bytes: &[u8]) -> String {
    visible_bytes(bytes)
        .chars()
        .map(|character| {
            if character.is_control() {
                character.escape_default().to_string()
            } else {
                character.to_string()
            }
        })
        .collect()
}

/// Escape invalid UTF-8 individually, and escape literal backslashes so distinct
/// native byte sequences cannot collapse to the same visible name/output.
pub fn visible_bytes(mut bytes: &[u8]) -> String {
    let mut result = String::new();
    while !bytes.is_empty() {
        let (valid, invalid) = match std::str::from_utf8(bytes) {
            Ok(text) => (text.len(), 0),
            Err(error) => (
                error.valid_up_to(),
                error
                    .error_len()
                    .unwrap_or(bytes.len() - error.valid_up_to()),
            ),
        };
        for character in std::str::from_utf8(&bytes[..valid])
            .expect("validated prefix")
            .chars()
        {
            if character == '\\' {
                result.push_str("\\\\");
            } else if character.is_control()
                && character != '\n'
                && character != '\t'
                && character != '\r'
            {
                result.extend(character.escape_default());
            } else {
                result.push(character);
            }
        }
        for byte in &bytes[valid..valid + invalid] {
            result.push_str(&format!("\\x{byte:02x}"));
        }
        bytes = &bytes[valid + invalid..];
    }
    result
}

#[derive(Debug)]
pub struct Capture {
    pub items: Vec<Item>,
    pub original: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: ExitStatus,
    pub unavailable: Option<String>,
    pub diagnostic: String,
    pub cwd: PathBuf,
    pub args: Vec<OsString>,
    pub catalog: crate::source::Catalog,
}

pub(crate) fn command(cwd: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .current_dir(cwd)
        .args(["-c", "core.fsmonitor=false"])
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_PAGER", "cat")
        .env("GIT_TERMINAL_PROMPT", "0");
    for (key, _) in std::env::vars_os() {
        if key.as_encoded_bytes().starts_with(b"GIT_TRACE") {
            command.env_remove(key);
        }
    }
    command
}

#[derive(Default)]
struct Analysis {
    operands: Vec<OsString>,
    paths: Vec<OsString>,
    ext: Option<bool>,
    textconv: Option<bool>,
    index_only: bool,
    no_index: bool,
    ignore_submodules: Option<Vec<u8>>,
    merge_base: bool,
    immutable: bool,
    opaque: bool,
    custom_prefixes: bool,
    content_omissions: bool,
    reverse: bool,
}

// Git's value-taking options own the following token even if it resembles a
// flag or the inner separator. We never rewrite the tokens passed to Git.
fn takes_value(option: &[u8], no_index: bool) -> bool {
    const REQUIRED: &[&[u8]] = &[
        b"--anchored",
        b"--src-prefix",
        b"--dst-prefix",
        b"--line-prefix",
        b"--output-indicator-new",
        b"--output-indicator-old",
        b"--output-indicator-context",
        b"--diff-filter",
        b"--find-object",
        b"--diff-algorithm",
        b"--word-diff-regex",
        b"--ignore-matching-lines",
        b"--color-moved-ws",
        b"--ws-error-highlight",
        b"--stat-width",
        b"--stat-name-width",
        b"--stat-count",
        b"--stat-graph-width",
        b"--inter-hunk-context",
        b"--rotate-to",
        b"--skip-to",
    ];
    if REQUIRED.contains(&option) {
        return true;
    }
    if no_index && option.starts_with(b"--") && option.len() > 2 {
        // These optional-value options accept their value only after '='. In
        // particular --unified MUST NOT consume a following --output option.
        const OPTIONAL_OR_FLAG: &[&[u8]] = &[
            b"--stat",
            b"--color",
            b"--color-moved",
            b"--color-words",
            b"--word-diff",
            b"--find-renames",
            b"--find-copies",
            b"--find-copies-harder",
            b"--ignore-submodules",
            b"--relative",
            b"--submodule",
            b"--shortstat",
            b"--summary",
            b"--diff-merges",
            b"--output",
            b"--abbrev",
            b"--unified",
            b"--indent-heuristic",
            b"--minimal",
        ];
        return REQUIRED
            .iter()
            .filter(|full| full.starts_with(option))
            .count()
            == 1
            && !OPTIONAL_OR_FLAG.iter().any(|full| full.starts_with(option));
    }
    if option.starts_with(b"-") && !option.starts_with(b"--") {
        let flags = option[1..].iter().enumerate();
        for (index, flag) in flags {
            if b"SGOIl".contains(flag) {
                return index + 2 == option.len();
            }
            if !b"psuWwbaRDcmrtzv".contains(flag) {
                return false;
            }
        }
    }
    false
}

fn analyze(cwd: &Path, args: &[OsString]) -> Result<Analysis> {
    // Repository diff has an initial parser that does not accept the same
    // abbreviations as the no-index parser. Probe only repository context;
    // never execute a trial comparison to infer argument ownership.
    let mut no_index = !command(cwd)
        .args(["rev-parse", "--git-dir"])
        .output()?
        .status
        .success();
    let mut preflight = 0;
    while let Some(arg) = args.get(preflight) {
        preflight += 1;
        let bytes = arg.as_encoded_bytes();
        if bytes == b"--" {
            break;
        }
        if bytes == b"--no-index" {
            no_index = true;
        }
        if takes_value(bytes, false) {
            preflight += 1;
        }
    }

    let mut analysis = Analysis {
        no_index,
        ..Analysis::default()
    };
    let mut src_default = true;
    let mut dst_default = true;
    let prefixes = command(cwd)
        .args([
            "config",
            "--null",
            "--get-regexp",
            r"^diff\.(srcprefix|dstprefix|noprefix|mnemonicprefix)$",
        ])
        .output()?;
    if !prefixes.status.success() && prefixes.status.code() != Some(1) {
        bail!("cannot inspect Git prefix configuration");
    }
    for record in prefixes.stdout.split(|byte| *byte == 0) {
        let Some(split) = record.iter().position(|byte| *byte == b'\n') else {
            continue;
        };
        let value = &record[split + 1..];
        match &record[..split] {
            b"diff.srcprefix" => src_default = value == b"a/",
            b"diff.dstprefix" => dst_default = value == b"b/",
            b"diff.noprefix" | b"diff.mnemonicprefix"
                if !matches!(value, b"false" | b"no" | b"off" | b"0") =>
            {
                src_default = false;
                dst_default = false;
            }
            _ => {}
        }
    }
    let mut after_paths = false;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        index += 1;
        let bytes = arg.as_encoded_bytes();
        if after_paths {
            analysis.paths.push(arg.clone());
            continue;
        }
        if bytes == b"--" {
            after_paths = true;
            continue;
        }
        let option = bytes.split(|byte| *byte == b'=').next().unwrap_or(bytes);
        if option.len() > 2 && b"--output".starts_with(option) || option == b"--output" {
            bail!(
                "Git option {arg:?} conflicts with read-only capture: output destinations are unavailable"
            );
        }
        let value = bytes
            .strip_prefix(option)
            .and_then(|suffix| suffix.strip_prefix(b"="))
            .or_else(|| args.get(index).map(|arg| arg.as_encoded_bytes()));
        if option == b"--default-prefix" {
            src_default = true;
            dst_default = true;
        }
        if option == b"--no-prefix" {
            src_default = false;
            dst_default = false;
        }
        if option == b"--src-prefix"
            || (no_index && takes_value(option, true) && b"--src-prefix".starts_with(option))
        {
            src_default = value == Some(b"a/".as_slice());
        }
        if option == b"--dst-prefix"
            || (no_index && takes_value(option, true) && b"--dst-prefix".starts_with(option))
        {
            dst_default = value == Some(b"b/".as_slice());
        }
        // Git accepts unambiguous long-option abbreviations for helper flags.
        if option.len() > 2 && b"--ext-diff".starts_with(option) {
            analysis.ext = Some(true);
        }
        if option.len() > 2 && option != b"--text" && b"--textconv".starts_with(option) {
            analysis.textconv = Some(true);
        }
        if option == b"--no-ext-diff" {
            analysis.ext = Some(false);
        }
        if option == b"--no-textconv" {
            analysis.textconv = Some(false);
        }
        if matches!(option, b"--cached" | b"--staged") {
            analysis.index_only = true;
        }
        if option == b"--no-index" {
            analysis.no_index = true;
        }
        if option == b"--ignore-submodules" {
            analysis.ignore_submodules = Some(
                bytes
                    .iter()
                    .position(|byte| *byte == b'=')
                    .map_or(b"all".as_slice(), |position| &bytes[position + 1..])
                    .to_vec(),
            );
        }
        if option == b"--no-ignore-submodules" {
            analysis.ignore_submodules = Some(b"none".to_vec());
        }
        if option == b"-R"
            || option == b"--reverse"
            || (option.starts_with(b"-")
                && !option.starts_with(b"--")
                && option[1..]
                    .iter()
                    .all(|byte| b"psuWwbaRDcmrtzv".contains(byte))
                && option.contains(&b'R'))
        {
            analysis.reverse = true;
        }
        if option == b"--merge-base" {
            analysis.merge_base = true;
        }
        if matches!(option, b"--no-patch" | b"-s" | b"--quiet") {
            analysis.opaque = true;
        }
        if matches!(option, b"--patch" | b"-p" | b"--no-quiet") {
            analysis.opaque = false;
        }
        if matches!(
            option,
            b"-w"
                | b"-b"
                | b"--ignore-space-at-eol"
                | b"--ignore-cr-at-eol"
                | b"--ignore-blank-lines"
                | b"--word-diff"
                | b"--color-words"
        ) || b"--ignore-all-space".starts_with(option) && option.len() > 2
            || b"--ignore-space-change".starts_with(option) && option.len() > 2
            || b"--ignore-matching-lines".starts_with(option) && option.len() > 2
            || b"--word-diff-regex".starts_with(option) && option.len() > 2
        {
            analysis.content_omissions = true;
        }
        if takes_value(option, no_index) && option == bytes {
            index += 1;
        } else if !bytes.starts_with(b"-") {
            analysis.operands.push(arg.clone());
        }
    }
    if analysis.no_index {
        analysis.operands.append(&mut analysis.paths);
    }
    analysis.custom_prefixes = !src_default || !dst_default;
    if analysis.ext == Some(true) || analysis.textconv == Some(true) {
        bail!(
            "Git helper option conflicts with read-only capture: external/textconv helpers are unavailable"
        );
    }
    let mut revisions = 0;
    for operand in &analysis.operands {
        let result = command(cwd)
            .args([
                OsString::from("rev-parse"),
                OsString::from("--revs-only"),
                OsString::from("--end-of-options"),
                operand.clone(),
            ])
            .output()?;
        revisions += result
            .stdout
            .split(|byte| *byte == b'\n')
            .filter(|line| {
                let line = line.strip_prefix(b"^").unwrap_or(line);
                matches!(line.len(), 40 | 64) && line.iter().all(u8::is_ascii_hexdigit)
            })
            .count();
    }
    analysis.immutable = analysis.index_only || analysis.no_index || revisions >= 2;
    Ok(analysis)
}

fn policy(cwd: &Path, analysis: &Analysis) -> Result<()> {
    policy_depth(cwd, analysis, 0)
}

fn policy_depth(cwd: &Path, analysis: &Analysis, depth: usize) -> Result<()> {
    if depth > 16 {
        bail!("nested submodule helper policy exceeds safe inspection depth");
    }
    if std::env::var_os("GIT_EXTERNAL_DIFF").is_some() && analysis.ext != Some(false) {
        bail!(
            "GIT_EXTERNAL_DIFF conflicts with read-only capture: external helpers are unavailable"
        );
    }
    let config = command(cwd)
        .args([
            "config",
            "--null",
            "--get-regexp",
            r"^(filter\..*\.(clean|process)|diff\..*\.(command|textconv)|diff\.external)$",
        ])
        .output()?;
    if !config.status.success() && config.status.code() != Some(1) {
        bail!(
            "cannot inspect Git configuration: {}",
            visible_bytes(&config.stderr)
        );
    }
    let config_records: Vec<_> = config.stdout.split(|byte| *byte == 0).collect();
    // This applies outside repositories too, before any enumeration can fail.
    if analysis.ext != Some(false)
        && config_records
            .iter()
            .any(|record| record.starts_with(b"diff.external\n"))
    {
        bail!("configured external diff helper conflicts with read-only capture");
    }
    let risky_driver = config_records.iter().any(|record| {
        record.starts_with(b"diff.")
            && ((analysis.ext != Some(false)
                && record.windows(9).any(|part| part == b".command\n"))
                || (analysis.textconv != Some(false)
                    && record.windows(10).any(|part| part == b".textconv\n")))
    });
    let root = command(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !root.status.success() || analysis.no_index {
        if risky_driver {
            bail!(
                "cannot prove diff-driver applicability for this non-tree comparison under read-only capture"
            );
        }
        return Ok(());
    }
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let root = PathBuf::from(OsString::from_vec(
        root.stdout
            .strip_suffix(b"\n")
            .unwrap_or(&root.stdout)
            .to_vec(),
    ));
    let mut revisions = Vec::new();
    let mut paths = analysis.paths.clone();
    for operand in &analysis.operands {
        let result = command(cwd)
            .args([
                OsString::from("rev-parse"),
                OsString::from("--revs-only"),
                OsString::from("--end-of-options"),
                operand.clone(),
            ])
            .output()?;
        let mut found = false;
        for line in result.stdout.split(|byte| *byte == b'\n') {
            let line = line.strip_prefix(b"^").unwrap_or(line);
            if matches!(line.len(), 40 | 64) && line.iter().all(u8::is_ascii_hexdigit) {
                revisions.push(OsString::from_vec(line.to_vec()));
                found = true;
            }
        }
        if !found {
            paths.push(operand.clone());
        }
    }
    if analysis.index_only && revisions.is_empty() {
        // --cached/--staged implicitly compare HEAD to the index. A staged
        // deletion is absent from the index but its HEAD-side driver applies.
        // An unborn HEAD has no historical tree; Git then compares an empty
        // old side, so the index candidates below remain sufficient.
        let head = command(cwd)
            .args(["rev-parse", "--verify", "HEAD^{tree}"])
            .output()?;
        if head.status.success() {
            revisions.push(OsString::from_vec(
                head.stdout
                    .strip_suffix(b"\n")
                    .unwrap_or(&head.stdout)
                    .to_vec(),
            ));
        }
    }
    if analysis.merge_base && !revisions.is_empty() {
        let mut args = revisions.clone();
        if args.len() == 1 {
            args.push("HEAD".into());
        }
        let bases = command(cwd)
            .args(["merge-base", "--all"])
            .args(args)
            .output()?;
        if !bases.status.success() {
            bail!("cannot prove merge-base helper applicability under read-only capture");
        }
        revisions.extend(
            bases
                .stdout
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
                .map(|line| OsString::from_vec(line.to_vec())),
        );
    }
    // No pathspec means git diff covers the whole repository even from sub/.
    // With explicit pathspecs, execute ls-files from the original cwd so Git
    // owns prefix, magic, exclusion and native-byte interpretation.
    let exclusions_only = !paths.is_empty()
        && paths.iter().all(|path| {
            let bytes = path.as_encoded_bytes();
            bytes.starts_with(b":!")
                || bytes.starts_with(b":^")
                || (bytes.starts_with(b":(")
                    && bytes
                        .iter()
                        .position(|byte| *byte == b')')
                        .is_some_and(|end| {
                            bytes[2..end]
                                .split(|byte| *byte == b',')
                                .any(|magic| magic == b"exclude")
                        }))
        });
    if paths.is_empty() || exclusions_only {
        paths.insert(0, ":/".into());
    }
    let mut candidates = std::collections::BTreeSet::new();
    for tree in std::iter::once(None).chain(revisions.iter().map(Some)) {
        let mut list = command(cwd);
        list.args(["ls-files", "--full-name", "--cached", "-z"]);
        if let Some(tree) = tree {
            let mut option = OsString::from("--with-tree=");
            option.push(tree);
            list.arg(option);
        }
        let files = list.arg("--").args(&paths).output()?;
        if !files.status.success() {
            if risky_driver || (!analysis.immutable && !config.stdout.is_empty()) {
                bail!(
                    "cannot prove complete helper candidates under read-only capture: {}",
                    visible_bytes(&files.stderr)
                );
            }
            continue;
        }
        candidates.extend(
            files
                .stdout
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .map(Vec::from),
        );
    }
    if !analysis.immutable {
        let stages = command(cwd)
            .args(["ls-files", "--full-name", "--stage", "-z", "--"])
            .args(&paths)
            .output()?;
        if !stages.status.success() {
            bail!("cannot inspect submodule helper applicability");
        }
        for entry in stages.stdout.split(|byte| *byte == 0) {
            if !entry.starts_with(b"160000 ") {
                continue;
            }
            let Some(tab) = entry.iter().position(|byte| *byte == b'\t') else {
                bail!("cannot inspect submodule identity");
            };
            let path = &entry[tab + 1..];
            if submodule_worktree_ignored(&root, path, analysis)? {
                continue;
            }
            let nested = root.join(std::ffi::OsStr::from_bytes(path));
            if nested.is_dir() && nested.join(".git").exists() {
                policy_depth(&nested, &Analysis::default(), depth + 1)?;
            }
        }
    }
    if config.stdout.is_empty() {
        return Ok(());
    }
    // File input avoids the two-pipe deadlock when check-attr emits output
    // while consuming a large candidate set.
    use std::io::{Seek, Write};
    let mut input = tempfile::tempfile()?;
    for path in candidates {
        input.write_all(&path)?;
        input.write_all(&[0])?;
    }
    input.rewind()?;
    let attrs = command(&root)
        .args(["check-attr", "-z", "--stdin", "filter", "diff"])
        .stdin(input)
        .output()?;
    if !attrs.status.success() {
        bail!("cannot inspect Git attributes safely");
    }
    for triple in attrs
        .stdout
        .split(|byte| *byte == 0)
        .collect::<Vec<_>>()
        .chunks_exact(3)
    {
        let attr = triple[1];
        let driver = triple[2];
        let prefix = if attr == b"filter" {
            b"filter.".as_slice()
        } else {
            b"diff.".as_slice()
        };
        let mut key = prefix.to_vec();
        key.extend(driver);
        key.push(b'.');
        let applicable = config_records.iter().any(|record| {
            if !record.starts_with(&key) {
                return false;
            }
            if attr == b"filter" {
                !analysis.immutable
            } else if record[key.len()..].starts_with(b"textconv\n") {
                analysis.textconv != Some(false)
            } else {
                analysis.ext != Some(false)
            }
        });
        if applicable {
            bail!(
                "applicable Git {} helper for {} conflicts with read-only capture; clean/process filters and external/textconv helpers cannot be executed safely",
                visible_bytes(attr),
                visible_bytes(triple[0])
            );
        }
    }

    Ok(())
}

fn submodule_worktree_ignored(root: &Path, path: &[u8], analysis: &Analysis) -> Result<bool> {
    if let Some(ignore) = &analysis.ignore_submodules {
        return Ok(matches!(ignore.as_slice(), b"all" | b"dirty"));
    }
    use std::os::unix::ffi::OsStringExt;
    let modules = command(root)
        .args([
            "config",
            "--file",
            ".gitmodules",
            "--null",
            "--get-regexp",
            r"^submodule\..*\.path$",
        ])
        .output()?;
    for record in modules.stdout.split(|byte| *byte == 0) {
        let Some(split) = record.iter().position(|byte| *byte == b'\n') else {
            continue;
        };
        if &record[split + 1..] != path {
            continue;
        }
        let mut key = record[..split]
            .strip_suffix(b"path")
            .context("invalid submodule path key")?
            .to_vec();
        key.extend_from_slice(b"ignore");
        let key = OsString::from_vec(key);
        for module_file in [false, true] {
            let mut config = command(root);
            config.arg("config");
            if module_file {
                config.args(["--file", ".gitmodules"]);
            }
            let setting = config.arg("--get").arg(&key).output()?;
            if setting.status.success() {
                return Ok(matches!(
                    setting
                        .stdout
                        .strip_suffix(b"\n")
                        .unwrap_or(&setting.stdout),
                    b"all" | b"dirty"
                ));
            }
            if setting.status.code() != Some(1) {
                bail!("cannot inspect effective submodule ignore policy");
            }
        }
    }
    let setting = command(root)
        .args(["config", "--get", "diff.ignoreSubmodules"])
        .output()?;
    if !setting.status.success() && setting.status.code() != Some(1) {
        bail!("cannot inspect effective submodule ignore policy");
    }
    Ok(matches!(
        setting
            .stdout
            .strip_suffix(b"\n")
            .unwrap_or(&setting.stdout),
        b"all" | b"dirty"
    ))
}

pub fn capture(cwd: &Path, args: &[OsString]) -> Result<Capture> {
    capture_with_hook(cwd, args, || {})
}

pub(crate) fn capture_with_hook(
    cwd: &Path,
    args: &[OsString],
    after_comparison: impl FnOnce(),
) -> Result<Capture> {
    let analysis = analyze(cwd, args)?;
    policy(cwd, &analysis)?;
    let before = if analysis.no_index || !analysis.immutable || analysis.index_only {
        input_snapshot(cwd, &analysis).ok()
    } else {
        None
    };
    let refs_before = revision_snapshot(cwd, &analysis);
    // Exactly one comparison. Its bytes, diagnostics and actual status survive
    // even when there is no provable mapping, or Git reports findings/errors.
    let result = command(cwd)
        .args(["--no-pager", "diff"])
        .args(args)
        .output()
        .context("cannot execute system Git")?;
    after_comparison();
    let refs_after = revision_snapshot(cwd, &analysis);
    let refs_stable = refs_before.is_some() && refs_before == refs_after;
    let mut items = if !analysis.opaque
        && !analysis.custom_prefixes
        && result.status.code().is_some_and(|code| code <= 1)
    {
        parse(cwd, &result.stdout)
    } else {
        None
    };
    let after = before
        .as_ref()
        .and_then(|_| input_snapshot(cwd, &analysis).ok());
    if let Some(items) = &mut items {
        for item in items {
            let stable = before.as_ref().zip(after.as_ref()).is_some_and(|(a, b)| {
                if analysis.index_only {
                    return a == b;
                }
                let keys = [&item.new_path, &item.old_path];
                let paths_stable = keys
                    .into_iter()
                    .take(if analysis.no_index { 2 } else { 1 })
                    .all(|path| {
                        let mode = [b"\0MODE".as_slice(), path.as_slice()].concat();
                        a.get(path) == b.get(path) && a.get(&mode) == b.get(&mode)
                    });
                paths_stable && a.get(b"\0GIT_INDEX".as_slice()) == b.get(b"\0GIT_INDEX".as_slice())
            });
            let proven = canonical_sources(
                cwd,
                item,
                &analysis,
                stable,
                after.as_ref(),
                refs_before.as_deref().filter(|_| refs_stable),
            );
            if let Some((identity, old, new)) = proven {
                item.canonical_identity = Some(identity);
                item.captured_sources = Some((old, new));
            }
            item.statistics = item.derive_statistics();
            if item.statistics == Statistics::Unavailable {
                let content_oid = match item.status.as_str() {
                    "A" if item.old_mode == "000000" => Some(&item.new_oid),
                    "D" if item.new_mode == "000000" => Some(&item.old_oid),
                    _ => None,
                };
                // Empty creation/deletion has no hunk. A verified empty Git
                // object plus the explicit absent side proves zero even when
                // mutable-source capture cannot restore a review identity.
                if content_oid.is_some_and(|oid| {
                    !oid.is_empty()
                        && !oid.bytes().all(|byte| byte == b'0')
                        && object_side(cwd, oid).is_some_and(|bytes| bytes.is_empty())
                }) {
                    item.statistics = Statistics::Text {
                        additions: 0,
                        deletions: 0,
                    };
                }
            }
            item.legacy_fingerprints =
                legacy_fingerprints(cwd, item, &analysis).unwrap_or_default();
        }
    }
    let unavailable = items.is_none().then(|| "File structure is unavailable for this output. Marks and source enrichment are disabled.".into());
    let mut diagnostic = if result.status.success() {
        visible_bytes(&result.stderr)
    } else {
        format!(
            "Git status {}{}{}",
            result
                .status
                .code()
                .map_or_else(|| "signal".into(), |code| code.to_string()),
            if result.stderr.is_empty() { "" } else { ": " },
            visible_bytes(&result.stderr)
        )
    };
    if let Some(items) = &items {
        let unproven = items
            .iter()
            .filter(|item| item.canonical_identity.is_none())
            .count();
        if unproven > 0 {
            if !diagnostic.is_empty() {
                diagnostic.push_str("; ");
            }
            diagnostic.push_str(&format!("{unproven} item(s) lack complete captured-content proof; their marks cannot be restored"));
        }
    }
    let mut catalog = crate::source::Catalog::default();
    if let Some(items) = &items {
        for item in items {
            if let Some((old, new)) = &item.captured_sources {
                if matches!(item.old_mode.as_str(), "100644" | "100755")
                    && catalog_eligible(&item.old_path, old)
                {
                    catalog.insert_captured(
                        crate::source::ComparedSide::Old,
                        item.old_path.clone(),
                        old,
                    );
                }
                if matches!(item.new_mode.as_str(), "100644" | "100755")
                    && catalog_eligible(&item.new_path, new)
                {
                    catalog.insert_captured(
                        crate::source::ComparedSide::New,
                        item.new_path.clone(),
                        new,
                    );
                }
            } else {
                catalog
                    .gaps
                    .push(format!("{}: compared source unavailable", item.name()));
            }
        }
    }
    if refs_stable || analysis.no_index || (refs_before.is_none() && analysis.operands.is_empty()) {
        let stable = before
            .as_ref()
            .zip(after.as_ref())
            .filter(|(a, b)| a == b)
            .map(|(a, _)| a);
        let trees = refs_before.as_deref().unwrap_or_default();
        let mut acquisition_gaps = Vec::new();
        let sides = if analysis.no_index {
            stable.and_then(|snapshot| no_index_catalog(snapshot, &analysis, &mut acquisition_gaps))
        } else if analysis.index_only {
            trees
                .first()
                .and_then(|tree| tree_catalog(cwd, tree, &mut acquisition_gaps))
                .zip(stable.and_then(|s| index_catalog(cwd, s, &mut acquisition_gaps)))
        } else if trees.len() == 2 {
            tree_catalog(cwd, &trees[0], &mut acquisition_gaps).zip(tree_catalog(
                cwd,
                &trees[1],
                &mut acquisition_gaps,
            ))
        } else {
            let old = if let Some(tree) = trees.first() {
                tree_catalog(cwd, tree, &mut acquisition_gaps)
            } else {
                stable.and_then(|s| index_catalog(cwd, s, &mut acquisition_gaps))
            };
            old.zip(stable.map(|s| worktree_catalog(s, &mut acquisition_gaps)))
        };
        catalog.gaps.extend(acquisition_gaps);
        if let Some((mut old, mut new)) = sides {
            if analysis.reverse && trees.len() != 2 {
                std::mem::swap(&mut old, &mut new);
            }
            for (side, files) in [
                (crate::source::ComparedSide::Old, old),
                (crate::source::ComparedSide::New, new),
            ] {
                for (path, bytes) in files {
                    if catalog.find(side, &path).is_none() {
                        catalog.insert_captured(side, path, &bytes);
                    }
                }
            }
            catalog.gaps.push("Search includes captured tracked ordinary files; untracked files, submodules and symlinks are excluded".into());
        } else {
            catalog.gaps.push("Full comparison catalog unavailable for these operands/provenance; searching proven changed sources only".into());
        }
    } else {
        catalog
            .gaps
            .push("Comparison inputs moved; searching proven changed sources only".into());
    }
    Ok(Capture {
        catalog,
        items: items.unwrap_or_default(),
        original: result.stdout,
        stderr: result.stderr,
        status: result.status,
        unavailable,
        diagnostic,
        cwd: cwd.into(),
        args: args.to_vec(),
    })
}

fn canonical_sources(
    cwd: &Path,
    item: &Item,
    analysis: &Analysis,
    stable: bool,
    snapshot: Option<&std::collections::BTreeMap<Vec<u8>, Vec<u8>>>,
    revisions: Option<&[Vec<u8>]>,
) -> Option<(String, Vec<u8>, Vec<u8>)> {
    let sources = crate::source::reconstruct(cwd, item).ok();
    let (old, new) = if let Some(sources) = sources {
        (sources.old, sources.new)
    } else if stable && analysis.index_only {
        index_sides(cwd, snapshot?, revisions?, item, analysis.reverse)?
    } else if stable && !analysis.no_index && !analysis.immutable {
        let files = snapshot?;
        let new = files.get(&item.new_path)?.clone();
        let metadata_only = !analysis.no_index
            && !analysis.content_omissions
            && item.old_oid.is_empty()
            && item.new_oid.is_empty()
            && (item.patch.windows(9).any(|part| part == b"old mode ")
                || matches!(item.status.as_str(), "R100" | "C100"));
        let old = if metadata_only {
            new.clone()
        } else if analysis.no_index {
            files.get(&item.old_path)?.clone()
        } else if !item.old_oid.is_empty() && item.old_oid.bytes().all(|byte| byte == b'0') {
            Vec::new()
        } else if item.old_oid.is_empty() {
            return None;
        } else {
            object_side(cwd, &item.old_oid)?
        };
        if !metadata_only
            && (!hash_matches(cwd, &old, &item.old_oid) || !hash_matches(cwd, &new, &item.new_oid))
        {
            return None;
        }
        (old, new)
    } else if stable && analysis.no_index {
        let files = snapshot?;
        let old = if item.old_mode == "000000" {
            Vec::new()
        } else {
            files.get(&item.old_path)?.clone()
        };
        let new = if item.new_mode == "000000" {
            Vec::new()
        } else {
            files.get(&item.new_path)?.clone()
        };
        if let Ok(reconstructed) =
            crate::source::reconstruct_from_old(cwd, item, old.clone(), false)
        {
            if reconstructed.new != new {
                return None;
            }
        } else if !(item.old_oid.is_empty()
            && item.new_oid.is_empty()
            && !analysis.content_omissions
            && old == new)
            && (!hash_matches(cwd, &old, &item.old_oid) || !hash_matches(cwd, &new, &item.new_oid))
        {
            return None;
        }
        (old, new)
    } else if analysis.immutable && !analysis.no_index {
        if !item.old_oid.is_empty() && !item.new_oid.is_empty() {
            (
                object_side(cwd, &item.old_oid)?,
                object_side(cwd, &item.new_oid)?,
            )
        } else {
            reference_sides(cwd, revisions?, item)?
        }
    } else {
        return None;
    };
    if !stable && (analysis.no_index || !analysis.immutable || analysis.index_only) {
        return None;
    }
    if analysis.no_index || !analysis.immutable {
        let files = snapshot?;
        let actual_new = if item.new_mode == "000000" {
            &[]
        } else {
            files
                .get(&item.new_path)
                .map(Vec::as_slice)
                .unwrap_or_default()
        };
        let mode_key = [b"\0MODE".as_slice(), item.new_path.as_slice()].concat();
        let old_mode_key = [b"\0MODE".as_slice(), item.old_path.as_slice()].concat();
        if actual_new != new.as_slice()
            || (analysis.no_index
                && item.old_mode != "000000"
                && files
                    .get(&old_mode_key)
                    .is_some_and(|mode| mode != item.old_mode.as_bytes()))
            || (item.new_mode != "000000"
                && files
                    .get(&mode_key)
                    .is_some_and(|mode| mode != item.new_mode.as_bytes()))
        {
            return None;
        }
    }
    let kind = [item.status.as_bytes().first().copied()?];
    let identity = crate::state::digest([
        b"review-item-v2".as_slice(),
        item.old_path.as_slice(),
        item.new_path.as_slice(),
        item.old_mode.as_bytes(),
        item.new_mode.as_bytes(),
        kind.as_slice(),
        old.as_slice(),
        new.as_slice(),
    ]);
    Some((identity, old, new))
}

// Pin the actual tree operands before/after the native comparison. Tree IDs
// are provenance handles only; canonical identities never include them.
fn revision_snapshot(cwd: &Path, analysis: &Analysis) -> Option<Vec<Vec<u8>>> {
    use std::os::unix::ffi::OsStringExt;
    if analysis.no_index {
        return None;
    }
    let mut operands = if analysis.index_only && analysis.operands.is_empty() {
        vec![OsString::from("HEAD")]
    } else {
        analysis.operands.clone()
    };
    let mut symmetric = analysis.merge_base;
    if operands.len() == 1 && !analysis.index_only {
        let bytes = operands[0].as_encoded_bytes();
        let split = bytes
            .windows(3)
            .position(|part| part == b"...")
            .map(|i| (i, 3))
            .or_else(|| {
                bytes
                    .windows(2)
                    .position(|part| part == b"..")
                    .map(|i| (i, 2))
            });
        if let Some((index, length)) = split {
            symmetric |= length == 3;
            let left = if index == 0 {
                b"HEAD".to_vec()
            } else {
                bytes[..index].to_vec()
            };
            let right = if index + length == bytes.len() {
                b"HEAD".to_vec()
            } else {
                bytes[index + length..].to_vec()
            };
            operands = vec![OsString::from_vec(left), OsString::from_vec(right)];
        }
    }
    if symmetric {
        if operands.len() != 2 {
            return None;
        }
        let output = command(cwd)
            .arg("merge-base")
            .args(&operands)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        operands[0] = OsString::from_vec(output.stdout.strip_suffix(b"\n")?.to_vec());
    }
    if operands.is_empty() && !analysis.index_only {
        return Some(Vec::new());
    }
    if operands.len() > 2 {
        return None;
    }
    let mut trees: Vec<Vec<u8>> = operands
        .iter()
        .map(|operand| {
            let mut expression = operand.as_encoded_bytes().to_vec();
            expression.extend_from_slice(b"^{tree}");
            let output = command(cwd)
                .args(["rev-parse", "--verify", "--end-of-options"])
                .arg(OsString::from_vec(expression))
                .output()
                .ok()?;
            output.status.success().then(|| {
                output
                    .stdout
                    .strip_suffix(b"\n")
                    .unwrap_or(&output.stdout)
                    .to_vec()
            })
        })
        .collect::<Option<_>>()?;
    if analysis.reverse && !analysis.index_only {
        trees.reverse();
    }
    Some(trees)
}

fn entry_side(cwd: &Path, entries: &[u8], path: &[u8], mode: &str, index: bool) -> Option<Vec<u8>> {
    if mode == "000000" {
        return Some(Vec::new());
    }
    let entry = entries.split(|byte| *byte == 0).find(|entry| {
        entry
            .iter()
            .position(|byte| *byte == b'\t')
            .is_some_and(|tab| &entry[tab + 1..] == path)
    })?;
    let tab = entry.iter().position(|byte| *byte == b'\t')?;
    let fields: Vec<_> = entry[..tab].split(|byte| *byte == b' ').collect();
    if fields.len() != 3
        || fields[0] != mode.as_bytes()
        || (index && fields[2] != b"0")
        || (!index && fields[1] != b"blob")
    {
        return None;
    }
    object_side(
        cwd,
        std::str::from_utf8(fields[if index { 1 } else { 2 }]).ok()?,
    )
}

fn tree_side(cwd: &Path, tree: &[u8], path: &[u8], mode: &str) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    let output = command(cwd)
        .args(["ls-tree", "-rz", "--full-tree"])
        .arg(std::ffi::OsStr::from_bytes(tree))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    entry_side(cwd, &output.stdout, path, mode, false)
}

fn index_sides(
    cwd: &Path,
    snapshot: &std::collections::BTreeMap<Vec<u8>, Vec<u8>>,
    trees: &[Vec<u8>],
    item: &Item,
    reverse: bool,
) -> Option<(Vec<u8>, Vec<u8>)> {
    let tree = trees.first()?;
    let entries = snapshot.get(b"\0INDEX_ENTRIES".as_slice())?;
    if reverse {
        Some((
            entry_side(cwd, entries, &item.old_path, &item.old_mode, true)?,
            tree_side(cwd, tree, &item.new_path, &item.new_mode)?,
        ))
    } else {
        Some((
            tree_side(cwd, tree, &item.old_path, &item.old_mode)?,
            entry_side(cwd, entries, &item.new_path, &item.new_mode, true)?,
        ))
    }
}

fn reference_sides(cwd: &Path, revisions: &[Vec<u8>], item: &Item) -> Option<(Vec<u8>, Vec<u8>)> {
    if revisions.len() != 2 {
        return None;
    }
    Some((
        tree_side(cwd, &revisions[0], &item.old_path, &item.old_mode)?,
        tree_side(cwd, &revisions[1], &item.new_path, &item.new_mode)?,
    ))
}

fn hash_matches(cwd: &Path, bytes: &[u8], expected: &str) -> bool {
    if expected.is_empty() || expected.bytes().all(|byte| byte == b'0') {
        return bytes.is_empty();
    }
    use std::io::Write;
    use std::process::Stdio;
    let Ok(mut child) = command(cwd)
        .args(["hash-object", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
    else {
        return false;
    };
    if child
        .stdin
        .take()
        .is_none_or(|mut stdin| stdin.write_all(bytes).is_err())
    {
        return false;
    }
    child.wait_with_output().ok().is_some_and(|output| {
        output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .trim()
                .starts_with(expected)
    })
}

// Snapshot tracked working-tree bytes without following symlinks, along with
// the index and explicit no-index operands. Comparing both snapshots around
// the one native diff ties reconstructed sides to stable input bytes.
fn input_snapshot(
    cwd: &Path,
    analysis: &Analysis,
) -> Result<std::collections::BTreeMap<Vec<u8>, Vec<u8>>> {
    use std::os::unix::ffi::OsStringExt;
    let mut paths = Vec::<Vec<u8>>::new();
    let mut root = cwd.to_path_buf();
    if analysis.no_index {
        paths.extend(
            analysis
                .operands
                .iter()
                .rev()
                .take(2)
                .map(|path| path.as_encoded_bytes().to_vec()),
        );
    } else {
        let output = command(cwd)
            .args(["rev-parse", "--show-toplevel"])
            .output()?;
        if !output.status.success() {
            bail!("cannot locate repository root");
        }
        root = PathBuf::from(OsString::from_vec(
            output
                .stdout
                .strip_suffix(b"\n")
                .unwrap_or(&output.stdout)
                .to_vec(),
        ));
        let listed = command(cwd)
            .args(["ls-files", "--full-name", "-z", "--", ":/"])
            .output()?;
        if !listed.status.success() {
            bail!("cannot enumerate tracked files for capture consistency");
        }
        paths.extend(
            listed
                .stdout
                .split(|byte| *byte == 0)
                .filter(|path| !path.is_empty())
                .map(<[u8]>::to_vec),
        );
    }
    let mut snapshot = std::collections::BTreeMap::new();
    for raw in paths
        .into_iter()
        .filter(|path| !analysis.index_only && !path.starts_with(b"\0"))
    {
        let path = root.join(std::ffi::OsString::from_vec(raw.clone()));
        if analysis.no_index
            && std::fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.is_dir())
        {
            snapshot_tree(&path, &raw, &mut snapshot)?;
        } else {
            snapshot_path(&path, &raw, &mut snapshot)?;
        }
    }
    if !analysis.no_index {
        // Git owns relative environment-path interpretation (which can change
        // when GIT_DIR/GIT_WORK_TREE are explicit), so ask for its absolute path.
        let index = command(cwd)
            .args(["rev-parse", "--path-format=absolute", "--git-path", "index"])
            .output()?;
        if !index.status.success() {
            bail!("cannot locate the active Git index for capture consistency");
        }
        let index_path = PathBuf::from(OsString::from_vec(
            index
                .stdout
                .strip_suffix(b"\n")
                .unwrap_or(&index.stdout)
                .to_vec(),
        ));
        snapshot.insert(b"\0GIT_INDEX".to_vec(), std::fs::read(index_path)?);
        let entries = command(cwd)
            .args(["ls-files", "--full-name", "--stage", "-z", "--", ":/"])
            .output()?;
        if !entries.status.success() {
            bail!("cannot capture index entries");
        }
        snapshot.insert(b"\0INDEX_ENTRIES".to_vec(), entries.stdout);
    }
    Ok(snapshot)
}

fn snapshot_tree(
    directory: &Path,
    key: &[u8],
    snapshot: &mut std::collections::BTreeMap<Vec<u8>, Vec<u8>>,
) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let mut child_key = key.to_vec();
        if !child_key.is_empty() {
            child_key.push(b'/');
        }
        child_key.extend_from_slice(entry.file_name().as_bytes());
        let path = entry.path();
        if path.is_dir() && !std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
            snapshot_tree(&path, &child_key, snapshot)?;
        } else {
            snapshot_path(&path, &child_key, snapshot)?;
        }
    }
    Ok(())
}

fn snapshot_path(
    path: &Path,
    key: &[u8],
    snapshot: &mut std::collections::BTreeMap<Vec<u8>, Vec<u8>>,
) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let (value, mode) = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => (
            std::fs::read_link(path)?
                .as_os_str()
                .as_encoded_bytes()
                .to_vec(),
            b"120000".to_vec(),
        ),
        Ok(metadata) if metadata.is_file() => (
            std::fs::read(path)?,
            if metadata.mode() & 0o100 != 0 {
                b"100755".to_vec()
            } else {
                b"100644".to_vec()
            },
        ),
        _ => (Vec::new(), b"000000".to_vec()),
    };
    let mut keys = vec![key.to_vec()];
    if let Some(relative) = key.strip_prefix(b"/") {
        keys.push(relative.to_vec());
    }
    for key in keys {
        snapshot.insert(key.clone(), value.clone());
        snapshot.insert(
            [b"\0MODE".as_slice(), key.as_slice()].concat(),
            mode.clone(),
        );
    }
    Ok(())
}

fn object_side(cwd: &Path, oid: &str) -> Option<Vec<u8>> {
    if oid.is_empty() || oid.bytes().all(|byte| byte == b'0') {
        return Some(Vec::new());
    }
    let output = command(cwd).args(["cat-file", "blob", oid]).output().ok()?;
    output.status.success().then_some(output.stdout)
}

fn legacy_fingerprints(cwd: &Path, item: &Item, analysis: &Analysis) -> Option<Vec<String>> {
    if item.old_path != item.new_path || !matches!(item.status.as_str(), "M" | "A" | "D" | "T") {
        return None;
    }
    let sources = crate::source::reconstruct(cwd, item).ok()?;
    let full_new = sources.new_oid;
    let zero = "0".repeat(full_new.len());
    let old = if item.old_source == SourceIdentity::Empty {
        &zero
    } else {
        &item.old_oid
    };
    let new = if item.new_source == SourceIdentity::Empty {
        &zero
    } else {
        &full_new
    };
    let mut canonical = Vec::new();
    for line in item.patch.split_inclusive(|byte| *byte == b'\n') {
        if let Some(identity) = line.strip_prefix(b"index ") {
            let suffix = identity
                .iter()
                .position(|byte| *byte == b' ')
                .map(|position| &identity[position..])
                .unwrap_or(b"\n");
            canonical.extend_from_slice(format!("index {old}..{new}").as_bytes());
            canonical.extend_from_slice(suffix);
        } else {
            canonical.extend_from_slice(line);
        }
    }
    // For a worktree comparison Git can print either a zero raw ID or the
    // index's full ID when the stat cache proves it already knows that side.
    // Both baseline records describe these exact verified captured bytes.
    let raw_ids = if analysis.immutable {
        vec![new]
    } else {
        vec![new, &zero]
    };
    let mut fingerprints = Vec::new();
    for raw_new in raw_ids {
        let metadata = format!(
            ":{} {} {old} {raw_new} {}",
            item.old_mode, item.new_mode, item.status
        );
        fingerprints.push(crate::state::digest([
            item.old_path.as_slice(),
            &item.new_path,
            metadata.as_bytes(),
            canonical.as_slice(),
        ]));
    }
    fingerprints.sort();
    fingerprints.dedup();
    Some(fingerprints)
}

fn oid(value: &[u8]) -> Option<String> {
    (value.len() >= 4 && value.len() <= 64 && value.iter().all(u8::is_ascii_hexdigit))
        .then(|| String::from_utf8_lossy(value).into_owned())
}

fn resolve_blob(cwd: &Path, value: &str) -> Option<String> {
    let output = command(cwd)
        .args(["rev-parse", "--verify", &format!("{value}^{{blob}}")])
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

pub(crate) fn parse(cwd: &Path, bytes: &[u8]) -> Option<Vec<Item>> {
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    if !bytes.starts_with(b"diff --git ") {
        return None;
    }
    // A source line in a unified hunk has a mandatory indicator, so these
    // complete line starts cannot be confused with header-like source bytes.
    let mut starts: Vec<_> = bytes
        .windows(11)
        .enumerate()
        .filter_map(|(index, part)| {
            (part == b"diff --git " && (index == 0 || bytes[index - 1] == b'\n')).then_some(index)
        })
        .collect();
    starts.push(bytes.len());
    let mut items = Vec::new();
    for bounds in starts.windows(2) {
        let patch = &bytes[bounds[0]..bounds[1]];
        if !valid_section(patch) {
            return None;
        }
        let lines: Vec<_> = patch.split(|byte| *byte == b'\n').collect();
        let mut header = Vec::new();
        for line in lines.iter().skip(1) {
            if line.starts_with(b"@@ ") {
                break;
            }
            header.push(*line);
        }
        let field = |prefix: &[u8]| header.iter().find_map(|line| line.strip_prefix(prefix));
        let (mut old_path, mut new_path) = header_paths(lines[0].strip_prefix(b"diff --git ")?)?;
        let mut old_mode = "100644".to_owned();
        let mut new_mode = old_mode.clone();
        for (prefix, target) in [
            (b"old mode ".as_slice(), &mut old_mode),
            (b"new mode ".as_slice(), &mut new_mode),
        ] {
            if let Some(mode) = field(prefix) {
                *target = std::str::from_utf8(mode).ok()?.to_owned();
            }
        }
        let mut status = "M".to_owned();
        if let Some(mode) = field(b"new file mode ") {
            status = "A".into();
            old_mode = "000000".into();
            new_mode = std::str::from_utf8(mode).ok()?.into();
        }
        if let Some(mode) = field(b"deleted file mode ") {
            status = "D".into();
            new_mode = "000000".into();
            old_mode = std::str::from_utf8(mode).ok()?.into();
        }
        for (kind, from, to) in [
            ("R", b"rename from ".as_slice(), b"rename to ".as_slice()),
            ("C", b"copy from ".as_slice(), b"copy to ".as_slice()),
        ] {
            if let Some(path) = field(from) {
                if unquote(path)? != old_path || unquote(field(to)?)? != new_path {
                    return None;
                }
                status = format!(
                    "{kind}{}",
                    std::str::from_utf8(field(b"similarity index ")?.strip_suffix(b"%")?).ok()?
                );
            }
        }
        if let Some(score) = field(b"dissimilarity index ") {
            status = format!("M{}", std::str::from_utf8(score.strip_suffix(b"%")?).ok()?);
        }
        let (mut old_oid, mut new_oid) = (String::new(), String::new());
        if let Some(identity) = field(b"index ") {
            let mut fields = identity.split(|byte| *byte == b' ');
            let hashes = fields.next()?;
            let split = hashes.windows(2).position(|part| part == b"..")?;
            old_oid = oid(&hashes[..split])?;
            new_oid = oid(&hashes[split + 2..])?;
            if let Some(mode) = fields.next() {
                old_mode = std::str::from_utf8(mode).ok()?.into();
                new_mode = old_mode.clone();
            }
        }
        if let Some(path) = field(b"--- ") {
            let decoded = unquote(path.strip_suffix(b"\t").unwrap_or(path))?;
            if decoded != b"/dev/null" && decoded.strip_prefix(b"a/") != Some(old_path.as_slice()) {
                return None;
            }
        }
        if let Some(path) = field(b"+++ ") {
            let decoded = unquote(path.strip_suffix(b"\t").unwrap_or(path))?;
            if decoded != b"/dev/null" && decoded.strip_prefix(b"b/") != Some(new_path.as_slice()) {
                return None;
            }
        }
        if ![&old_mode, &new_mode]
            .iter()
            .all(|mode| mode.len() == 6 && mode.bytes().all(|byte| matches!(byte, b'0'..=b'7')))
        {
            return None;
        }
        if !lines.iter().any(|line| line.starts_with(b"@@ "))
            && !(field(b"new file mode ").is_some()
                || field(b"deleted file mode ").is_some()
                || (field(b"old mode ").is_some() && field(b"new mode ").is_some())
                || status.starts_with('R')
                || status.starts_with('C')
                || header.iter().any(|line| line.starts_with(b"Binary files ")))
        {
            return None;
        }
        let reconstructable =
            matches!(old_mode.as_str(), "100644" | "100755" | "120000" | "000000")
                && matches!(new_mode.as_str(), "100644" | "100755" | "120000" | "000000");
        let textual = lines.iter().any(|line| line.starts_with(b"@@ "));
        let old_source = if !reconstructable || !textual {
            SourceIdentity::Unavailable
        } else if old_oid.bytes().all(|byte| byte == b'0') {
            SourceIdentity::Empty
        } else if let Some(full) = resolve_blob(cwd, &old_oid) {
            old_oid = full.clone();
            SourceIdentity::Blob(full)
        } else {
            SourceIdentity::Unavailable
        };
        let new_source = if !reconstructable || !textual {
            SourceIdentity::Unavailable
        } else if new_oid.bytes().all(|byte| byte == b'0') {
            SourceIdentity::Empty
        } else {
            SourceIdentity::CapturedPatch
        };
        // Keep the legacy record representation for ordinary items. Extended
        // identities include both paths/modes/status in the persisted digest.
        items.push(Item {
            old_path: std::mem::take(&mut old_path),
            new_path: std::mem::take(&mut new_path),
            old_oid,
            new_oid,
            old_mode,
            new_mode,
            status,
            old_source,
            new_source,
            patch: patch.to_vec(),
            legacy_fingerprints: Vec::new(),
            canonical_identity: None,
            captured_sources: None,
            statistics: Statistics::Unavailable,
        });
        let item = items.last_mut().expect("inserted item");
        item.statistics = item.derive_statistics();
    }
    let mut identities = std::collections::BTreeSet::new();
    if items
        .iter()
        .any(|item| !identities.insert((&item.old_path, &item.new_path)))
    {
        return None;
    }
    Some(items)
}

fn valid_section(patch: &[u8]) -> bool {
    let mut remaining = (0usize, 0usize);
    let mut in_hunk = false;
    let mut evidence = false;
    for line in patch.split_inclusive(|byte| *byte == b'\n').skip(1) {
        if line.starts_with(b"@@ ") {
            if remaining != (0, 0) {
                return false;
            }
            let Some((_, old, _, new)) = crate::source::hunk_bytes(line) else {
                return false;
            };
            remaining = (old, new);
            in_hunk = true;
            evidence = true;
        } else if in_hunk {
            match line.first() {
                Some(b' ') if remaining.0 > 0 && remaining.1 > 0 => {
                    remaining.0 -= 1;
                    remaining.1 -= 1;
                }
                Some(b'-') if remaining.0 > 0 => remaining.0 -= 1,
                Some(b'+') if remaining.1 > 0 => remaining.1 -= 1,
                Some(b'\\') if line.starts_with(b"\\ No newline at end of file") => {}
                _ => return false,
            }
        } else if [
            b"old mode ".as_slice(),
            b"new mode ",
            b"new file mode ",
            b"deleted file mode ",
            b"similarity index ",
            b"dissimilarity index ",
            b"rename from ",
            b"rename to ",
            b"copy from ",
            b"copy to ",
            b"index ",
            b"--- ",
            b"+++ ",
        ]
        .iter()
        .any(|prefix| line.starts_with(prefix))
            || (line.starts_with(b"Binary files ") && line.ends_with(b" differ\n"))
        {
            evidence = true;
        } else {
            // Includes binary-patch payloads, submodule summaries, unknown
            // indicators and formats. Keep those complete native bytes opaque.
            return false;
        }
    }
    evidence && remaining == (0, 0)
}

fn header_paths(bytes: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut candidates = Vec::new();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte != b' ' {
            continue;
        }
        if let (Some(old), Some(new)) = (unquote(&bytes[..index]), unquote(&bytes[index + 1..]))
            && let (Some(old), Some(new)) = (old.strip_prefix(b"a/"), new.strip_prefix(b"b/"))
        {
            candidates.push((old.to_vec(), new.to_vec()));
        }
    }
    (candidates.len() == 1).then(|| candidates.remove(0))
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

// Catalog construction reads only captured index entries or pinned tree objects.
type CatalogFiles = std::collections::BTreeMap<Vec<u8>, Vec<u8>>;
// These limits apply before reading any broad-catalog object content. Existing
// comparison output and canonical item proofs deliberately retain their policy.
pub(crate) const CATALOG_FILES_PER_SIDE: usize = 8192;
pub(crate) const CATALOG_BYTES_PER_SIDE: usize = 64 * 1024 * 1024;
const CATALOG_LIST_BYTES: usize = 16 * 1024 * 1024;
fn listed_catalog(
    cwd: &Path,
    entries: &[u8],
    index: bool,
    gaps: &mut Vec<String>,
) -> Option<CatalogFiles> {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::process::Stdio;
    let mut child = command(cwd)
        .args(["cat-file", "--batch-command"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let result = (|| {
        let input = child.stdin.as_mut()?;
        let mut output = BufReader::new(child.stdout.take()?);
        let mut files = CatalogFiles::new();
        let mut total = 0usize;
        let mut omitted = 0usize;
        let mut sniffed = 0usize;
        let mut sniff_bytes = 0usize;
        for entry in entries.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let tab = entry.iter().position(|b| *b == b'\t')?;
            let fields: Vec<_> = entry[..tab].split(|b| *b == b' ').collect();
            if fields.len() != 3 {
                return None;
            }
            if !matches!(fields[0], b"100644" | b"100755") || (index && fields[2] != b"0") {
                continue;
            }
            let path = &entry[tab + 1..];
            let known = crate::symbols::classify(path, "").is_some();
            if !known {
                if !crate::symbols::sniff_candidate(path, fields[0] == b"100755") {
                    continue;
                }
                if sniffed >= crate::symbols::SNIFF_FILES {
                    omitted += 1;
                    continue;
                }
            }
            if files.len() >= CATALOG_FILES_PER_SIDE {
                omitted += 1;
                continue;
            }
            let oid = std::str::from_utf8(fields[if index { 1 } else { 2 }]).ok()?;
            if !matches!(oid.len(), 40 | 64) || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            writeln!(input, "info {oid}").ok()?;
            input.flush().ok()?;
            let mut header = String::new();
            output.by_ref().take(256).read_line(&mut header).ok()?;
            let parts: Vec<_> = header.split_whitespace().collect();
            if parts.len() != 3 || parts[1] != "blob" {
                return None;
            }
            let size: usize = parts[2].parse().ok()?;
            if size > crate::navigation::MAX_FILE_BYTES
                || total.saturating_add(size) > CATALOG_BYTES_PER_SIDE
            {
                omitted += 1;
                continue;
            }
            if !known {
                sniffed += 1;
                if sniff_bytes.saturating_add(size)
                    > crate::symbols::SNIFF_BYTES * crate::symbols::SNIFF_FILES
                {
                    omitted += 1;
                    continue;
                }
                sniff_bytes += size;
            }
            writeln!(input, "contents {oid}").ok()?;
            input.flush().ok()?;
            let mut content_header = String::new();
            output
                .by_ref()
                .take(256)
                .read_line(&mut content_header)
                .ok()?;
            if content_header != header {
                return None;
            }
            let mut bytes = vec![0; size];
            output.read_exact(&mut bytes).ok()?;
            let mut newline = [0];
            output.read_exact(&mut newline).ok()?;
            if newline != *b"\n" {
                return None;
            }
            if !catalog_eligible(path, &bytes) {
                continue;
            }
            total += size;
            files.insert(path.to_vec(), bytes);
        }
        if omitted > 0 {
            gaps.push(format!("Catalog acquisition omitted {omitted} source(s): 8192 eligible files / 1 MiB per file / 64 MiB per side byte limit or script sniff allowance"));
        }
        Some(files)
    })();
    // Also reap on malformed headers or early limits; no per-object processes.
    let _ = child.kill();
    let _ = child.wait();
    result
}
fn tree_catalog(cwd: &Path, tree: &[u8], gaps: &mut Vec<String>) -> Option<CatalogFiles> {
    use std::{io::Read, os::unix::ffi::OsStrExt, process::Stdio};
    let mut child = command(cwd)
        .args(["ls-tree", "-rz", "--full-tree"])
        .arg(std::ffi::OsStr::from_bytes(tree))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut entries = Vec::new();
    let read = child
        .stdout
        .take()?
        .take(CATALOG_LIST_BYTES as u64 + 1)
        .read_to_end(&mut entries);
    if read.is_err() || entries.len() > CATALOG_LIST_BYTES {
        let _ = child.kill();
        let _ = child.wait();
        gaps.push("Catalog acquisition tree listing exceeds 16 MiB; this side omitted".into());
        return Some(CatalogFiles::new());
    }
    if !child.wait().ok()?.success() {
        return None;
    }
    listed_catalog(cwd, &entries, false, gaps)
}
fn index_catalog(
    cwd: &Path,
    snapshot: &CatalogFiles,
    gaps: &mut Vec<String>,
) -> Option<CatalogFiles> {
    listed_catalog(
        cwd,
        snapshot.get(b"\0INDEX_ENTRIES".as_slice())?,
        true,
        gaps,
    )
}
fn catalog_eligible(path: &[u8], bytes: &[u8]) -> bool {
    // Eligibility uses the same bounded shebang prefix as analysis. Invalid UTF-8
    // in a supported file stays retained so analysis can report its omission.
    let prefix = &bytes[..bytes.len().min(crate::symbols::SNIFF_BYTES)];
    crate::symbols::classify(
        path,
        std::str::from_utf8(prefix).unwrap_or_else(|error| {
            std::str::from_utf8(&prefix[..error.valid_up_to()]).unwrap_or("")
        }),
    )
    .is_some()
}
fn worktree_catalog(snapshot: &CatalogFiles, gaps: &mut Vec<String>) -> CatalogFiles {
    worktree_catalog_selected(snapshot, gaps, None)
}
fn worktree_catalog_selected(
    snapshot: &CatalogFiles,
    gaps: &mut Vec<String>,
    prefix: Option<&[u8]>,
) -> CatalogFiles {
    let mut total = 0usize;
    let mut omitted = 0usize;
    let mut sniffed = 0usize;
    let mut files = CatalogFiles::new();
    for (path, bytes) in snapshot {
        if prefix.is_some_and(|prefix| {
            path.as_slice() != prefix
                && !path
                    .strip_prefix(prefix)
                    .is_some_and(|tail| tail.starts_with(b"/"))
        }) {
            continue;
        }
        if path.starts_with(b"\0") {
            continue;
        }
        let Some(mode) = snapshot.get(&[b"\0MODE".as_slice(), path].concat()) else {
            continue;
        };
        if !matches!(mode.as_slice(), b"100644" | b"100755") {
            continue;
        }
        if crate::symbols::classify(path, "").is_none() {
            if !crate::symbols::sniff_candidate(path, mode == b"100755") {
                continue;
            }
            if sniffed >= crate::symbols::SNIFF_FILES {
                omitted += 1;
                continue;
            }
            sniffed += 1;
        }
        if !catalog_eligible(path, bytes) {
            continue;
        }
        if files.len() >= CATALOG_FILES_PER_SIDE
            || bytes.len() > crate::navigation::MAX_FILE_BYTES
            || total.saturating_add(bytes.len()) > CATALOG_BYTES_PER_SIDE
        {
            omitted += 1;
            continue;
        }
        total += bytes.len();
        files.insert(path.clone(), bytes.clone());
    }
    if omitted > 0 {
        gaps.push(format!("Catalog acquisition omitted {omitted} snapshot sources: 8192 eligible files / 1 MiB per file / 64 MiB per side limit or script sniff allowance"));
    }
    files
}

fn no_index_catalog(
    snapshot: &CatalogFiles,
    analysis: &Analysis,
    gaps: &mut Vec<String>,
) -> Option<(CatalogFiles, CatalogFiles)> {
    let operands: Vec<_> = analysis.operands.iter().rev().take(2).collect();
    if operands.len() != 2 {
        return None;
    }
    let mut select = |operand: &OsString| {
        let prefix = operand.as_encoded_bytes();
        worktree_catalog_selected(snapshot, gaps, Some(prefix))
            .into_iter()
            .map(|(path, bytes)| (path.strip_prefix(b"/").unwrap_or(&path).to_vec(), bytes))
            .collect()
    };
    Some((select(operands[1]), select(operands[0])))
}

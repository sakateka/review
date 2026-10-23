//! Exercise a review binary using a real POSIX PTY; no optional tools.
//!
//! `REVIEW_BINARY` selects an installed binary (default: the cargo-built one).
//! `REVIEW_TEST_BINARY` names the crate's unit-test binary; when set, the
//! injected panic/I/O-failure lifecycle scenarios also run.

#[path = "terminal_smoke/session.rs"]
mod session;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use session::{Color, Session, TestEnv, char_index, contains, idx, rgb, snapshot, tail};

type Snapshot = BTreeMap<String, Vec<u8>>;

const CLASSIC_CONFIG: &str = "[appearance]\ncolorscheme = \"review-classic\"\n";
const PREVIEW_CALLER: &str = "fn preview_caller() { preview_unique(); let _: PreviewType; }";
const PREVIEW_AMBIGUOUS_CALLER: &str = "fn preview_ambiguous_caller() { preview_ambiguous(); }";
const CENTERED_CALLER: &str =
    "fn centered_caller() { centered_start(); centered_middle(); centered_end(); }";
const FIXTURE_NAMES: [&str; 9] = [
    "sample.rs",
    "sample.c",
    "sample.h",
    "sample.cpp",
    "sample.go",
    "sample.zig",
    "sample.py",
    "sample.sh",
    "sample.bash",
];

fn which(program: &str) -> PathBuf {
    let path = std::env::var_os("PATH").expect("PATH is unset");
    std::env::split_paths(&path)
        .map(|directory| directory.join(program))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| panic!("{program} not found in PATH"))
}

/// Python's `subprocess.check_output(..., stderr=STDOUT)`.
fn check_output(mut command: Command) -> Vec<u8> {
    let output = command
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("{command:?} failed to start: {error}"));
    assert!(
        output.status.success(),
        "{command:?} failed: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut combined = output.stdout;
    combined.extend_from_slice(&output.stderr);
    combined
}

fn write(path: &Path, text: impl AsRef<[u8]>) {
    std::fs::write(path, text).unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("non-UTF-8 temporary path")
}

/// The first screen line containing `text`, with its row.
fn find_line(session: &Session, text: &str) -> (usize, String) {
    session
        .lines()
        .into_iter()
        .enumerate()
        .find(|(_, line)| line.contains(text))
        .unwrap_or_else(|| panic!("{text:?} not on screen\n{}", session.text()))
}

/// Python's `yaml_color`/`toml_color`: the color at the first `token` cell.
fn assert_token(session: &Session, token: &str, expected: Color) {
    let (row, line) = find_line(session, token);
    let color = session.color(row, char_index(&line, token).unwrap());
    assert_eq!(color, expected, "{token}\n{}", session.text());
}

fn token_color(session: &Session, row_text: &str, token: &str) -> Color {
    let (row, line) = find_line(session, row_text);
    session.color(
        row,
        char_index(&line, row_text).unwrap() + char_index(row_text, token).unwrap(),
    )
}

fn status_row(session: &Session) -> String {
    session.lines()[28].clone()
}

/// Column and row of the first screen occurrence of `text`.
fn locate(session: &Session, text: &str) -> (usize, usize) {
    let (row, line) = find_line(session, text);
    (char_index(&line, text).unwrap(), row)
}

fn pointer(code: u32, column: usize, row: usize, release: bool) -> Vec<u8> {
    let end = if release { 'm' } else { 'M' };
    format!("\x1b[<{code};{};{}{end}", column + 1, row + 1).into_bytes()
}

fn click(x: usize, y: usize) -> Vec<u8> {
    [pointer(0, x, y, false), pointer(0, x, y, true)].concat()
}

fn cursor(session: &Session) -> (i64, i64) {
    (session.row, session.col)
}

fn view(session: &Session) -> (String, i64, i64) {
    (session.text(), session.row, session.col)
}

struct Ctx {
    binary: PathBuf,
    test_binary: Option<PathBuf>,
    git: PathBuf,
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    config_file: PathBuf,
    env: TestEnv,
    before: Snapshot,
    yaml_text: String,
    toml_text: String,
}

impl Ctx {
    /// Git in the main repository with the test's own environment.
    fn git_run(&self, words: &[&str]) -> Vec<u8> {
        let mut command = Command::new(&self.git);
        command.args(words).current_dir(&self.repo);
        check_output(command)
    }

    /// Git in `cwd` with the child environment.
    fn git_in(&self, cwd: &Path, words: &[&str]) -> Vec<u8> {
        let mut command = Command::new(&self.git);
        command.args(words).current_dir(cwd);
        self.env.apply(&mut command);
        check_output(command)
    }

    /// A non-interactive run with captured output (`subprocess.run`).
    fn run(&self, args: &[&str], cwd: Option<&Path>) -> Output {
        let mut command = Command::new(&self.binary);
        command.args(args).stdin(Stdio::null());
        if let Some(cwd) = cwd {
            command.current_dir(cwd);
        }
        self.env.apply(&mut command);
        command.output().unwrap()
    }

    /// `subprocess.run(..., check=True, text=True).stdout`.
    fn help(&self, args: &[&str]) -> String {
        let output = self.run(args, None);
        assert!(output.status.success(), "{args:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    fn session_in(&self, cwd: &Path, args: &[&str]) -> Session {
        Session::new(&self.binary, args, cwd, &self.env, None, false)
    }

    fn session(&self, args: &[&str]) -> Session {
        self.session_in(&self.repo, args)
    }

    fn default_session(&self) -> Session {
        self.session(&["--", "HEAD"])
    }

    fn repo_snapshot(&self) -> Snapshot {
        snapshot(&self.repo)
    }

    fn state_snapshot(&self) -> Snapshot {
        snapshot(&self.state)
    }
}

/// Compare two snapshots, naming only the added, removed and changed paths.
fn assert_same(before: &Snapshot, after: &Snapshot, what: &str) {
    let removed: Vec<&String> = before
        .keys()
        .filter(|path| !after.contains_key(*path))
        .collect();
    let added: Vec<&String> = after
        .keys()
        .filter(|path| !before.contains_key(*path))
        .collect();
    let changed: Vec<&String> = before
        .iter()
        .filter(|(path, bytes)| after.get(*path).is_some_and(|other| other != *bytes))
        .map(|(path, _)| path)
        .collect();
    assert!(
        removed.is_empty() && added.is_empty() && changed.is_empty(),
        "{what}: added {added:?}, removed {removed:?}, changed {changed:?}"
    );
}

fn pty_passed(message: &str) {
    println!("PTY passed: {message}");
}

#[test]
fn terminal_smoke() {
    let binary = match std::env::var_os("REVIEW_BINARY") {
        Some(path) => {
            let path = PathBuf::from(path);
            assert!(
                path.is_file(),
                "REVIEW_BINARY={} does not name a file",
                path.display()
            );
            std::fs::canonicalize(path).unwrap()
        }
        None => PathBuf::from(env!("CARGO_BIN_EXE_review")),
    };
    let test_binary = std::env::var_os("REVIEW_TEST_BINARY").map(|path| {
        let path = PathBuf::from(path);
        assert!(
            path.is_file(),
            "REVIEW_TEST_BINARY={} does not name a file",
            path.display()
        );
        std::fs::canonicalize(path).unwrap()
    });
    if test_binary.is_none() {
        eprintln!(
            "terminal_smoke: REVIEW_TEST_BINARY is unset; skipping the injected panic/I/O-failure scenarios"
        );
    }
    let git = which("git");
    for (option, expected) in [("--help", "review ["), ("--version", "review ")] {
        let output = Command::new(&binary)
            .arg(option)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "{option}: {output:?}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.starts_with(expected), "{stdout}");
    }

    let directory = tempfile::Builder::new()
        .prefix("review-pty-")
        .tempdir()
        .unwrap();
    let root = directory.path().to_path_buf();
    let mut ctx = setup(binary, test_binary, git, root);

    colors_themes_and_command_line(&mut ctx);
    navigation_and_lifecycle(&mut ctx);
    config_and_keymaps(&ctx);
    documentation(&ctx);
    previews(&ctx);
    quitting(&ctx);
    let (acceptance, left, right) = acceptance_setup(&ctx);
    indexing(&ctx);
    cursor_and_mouse(&ctx);
    diff_status(&ctx);
    file_statistics(&ctx);
    marks_invalidation_and_compatibility(&ctx, &acceptance, left, right);
    comments(&ctx);

    let suffix = if ctx.test_binary.is_some() {
        ", injected panic/I/O failure"
    } else {
        ""
    };
    pty_passed(&format!(
        "installed language navigation, terminal lifecycle, R1-R13 workflow, diff cursor/scroll status, branch/ref/staged/worktree/no-index invalidation, Git compatibility and original-output fallback{suffix}"
    ));
}

fn setup(binary: PathBuf, test_binary: Option<PathBuf>, git: PathBuf, root: PathBuf) -> Ctx {
    let repo = root.join("repo");
    std::fs::create_dir(&repo).unwrap();
    let tools = root.join("tools");
    let env = TestEnv {
        vars: vec![
            ("TERM".into(), "xterm-256color".into()),
            ("PATH".into(), tools.clone().into_os_string()),
            ("HOME".into(), root.clone().into_os_string()),
            ("XDG_STATE_HOME".into(), root.join("state").into_os_string()),
            (
                "XDG_CONFIG_HOME".into(),
                root.join("config").into_os_string(),
            ),
            ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
            ("GIT_CONFIG_NOSYSTEM".into(), OsString::from("1")),
        ],
    };
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut ctx = Ctx {
        binary,
        test_binary,
        git: git.clone(),
        state: root.join("state"),
        config_file: root.join("config/review/config.toml"),
        root: root.clone(),
        repo: repo.clone(),
        env,
        before: Snapshot::new(),
        yaml_text: read(&fixtures.join("yaml-colors.yaml")),
        toml_text: read(&fixtures.join("toml-colors.toml")),
    };
    ctx.git_run(&["init", "-q"]);
    ctx.git_run(&["config", "user.name", "PTY Test"]);
    ctx.git_run(&["config", "user.email", "pty@example.invalid"]);
    write(
        &repo.join("docs.rs"),
        "/// Captured ambiguous documentation.\nfn doc_choice() {}\nmod other {\nfn doc_choice() {}\n}\nfn docs_caller() { doc_choice(); }\n",
    );
    write(
        &repo.join("target.rs"),
        "/// Captured terminal documentation.\nfn main_target() {}\n",
    );
    write(
        &repo.join("preview.rs"),
        format!(
            "struct PreviewType {{ value: usize }}\nfn preview_unique() {{}}\n{PREVIEW_CALLER}\nmod one {{ fn preview_ambiguous() {{}} }}\nmod two {{ fn preview_ambiguous() {{}} }}\n{PREVIEW_AMBIGUOUS_CALLER}\nfn variable_owner() {{ let preview_variable = 1; }}\n"
        ),
    );
    let mut centered_source = vec!["// captured context".to_owned(); 101];
    for (row, name) in [(1, "start"), (50, "middle"), (99, "end")] {
        centered_source[row] = format!("fn centered_{name}() {{}}");
    }
    centered_source[10] = CENTERED_CALLER.to_owned();
    write(&repo.join("centered.rs"), centered_source.join("\n") + "\n");
    let long_line = format!("\t// {}\n", "界e\u{301}🙂".repeat(150));
    write(
        &repo.join("caller.rs"),
        format!("fn caller() {{ main_target(); }}\n{long_line}"),
    );
    let fixture_root = fixtures.join("navigation");
    for name in FIXTURE_NAMES.iter().chain(&["sample.yaml", "sample.json"]) {
        let text = read(&fixture_root.join(name));
        let extension = name.rsplit('.').next().unwrap();
        let text = text
            .replace("target", &format!("navtarget_{extension}_end"))
            .replace("port", &format!("navport_{extension}"));
        write(&repo.join(name), text);
    }
    ctx.git_run(&["add", "."]);
    ctx.git_run(&["commit", "-qm", "initial"]);
    write(
        &repo.join("caller.rs"),
        format!("fn caller() {{ main_target(); main_target(); }}\n{long_line}"),
    );
    std::fs::create_dir(&tools).unwrap();
    std::os::unix::fs::symlink(&git, tools.join("git")).unwrap();
    ctx.before = ctx.repo_snapshot();
    ctx
}

fn colors_themes_and_command_line(ctx: &mut Ctx) {
    let root = ctx.root.clone();
    let repo = ctx.repo.clone();
    // The palette assertions below pin the discovered config to review-classic;
    // the theme block checks the gruvbox-baby default without any config.
    std::fs::create_dir_all(ctx.config_file.parent().unwrap()).unwrap();
    write(&ctx.config_file, CLASSIC_CONFIG);

    // Installed YAML colors in the actual diff and captured source panes.
    let yaml_text = ctx.yaml_text.clone();
    write(&repo.join("colors.yaml"), &yaml_text);
    ctx.git_run(&["add", "colors.yaml"]);
    ctx.git_run(&["commit", "-qm", "yaml colors"]);
    write(&repo.join("colors.yaml"), yaml_text.replace("42", "43"));
    let yaml_before = ctx.repo_snapshot();
    let mut session = ctx.session(&["--", "HEAD", "--", "colors.yaml"]);
    session.wait_text("Files");
    session.send(b"\r");
    session.wait_text("+  count: 43");
    assert_token(&session, "count:", idx(6));
    assert_token(&session, "43", idx(3));
    assert_token(&session, "true", idx(5));
    session.send(b"\x1b");
    session.send(b"/service\r");
    session.wait_text("Search candidates");
    session.send(b"\r");
    session.wait_text("Captured old colors.yaml");
    for (token, color) in [
        ("service:", 6),
        ("café", 2),
        ("42", 3),
        ("true", 5),
        ("shared", 4),
        ("# review", 8),
    ] {
        assert_token(&session, token, idx(color));
    }
    session.send(b",q");
    session.finish(0);
    assert!(!root.join("state").exists(), "YAML viewing changed marks");
    assert_same(
        &yaml_before,
        &ctx.repo_snapshot(),
        "YAML viewing changed repository bytes",
    );
    std::fs::remove_file(repo.join("colors.yaml")).unwrap();
    ctx.git_run(&["rm", "--cached", "colors.yaml"]);
    ctx.git_run(&["commit", "-qm", "remove yaml fixture"]);
    ctx.before = ctx.repo_snapshot();

    // TOML has renderer support without navigation indexing. Open its actual diff.
    let toml_text = ctx.toml_text.clone();
    write(&repo.join("colors.toml"), &toml_text);
    ctx.git_run(&["add", "colors.toml"]);
    ctx.git_run(&["commit", "-qm", "toml colors"]);
    write(&repo.join("colors.toml"), toml_text.replace("42", "43"));
    let toml_before = ctx.repo_snapshot();
    let mut session = ctx.session(&["--", "HEAD", "--", "colors.toml"]);
    session.wait_text("Files");
    session.send(b"\r");
    session.wait_text("+count = 43");
    for (token, color) in [
        ("count", 6),
        ("42", 3),
        ("43", 3),
        ("café", 2),
        ("true", 5),
        ("1979-05-27", 4),
        ("# review", 8),
    ] {
        assert_token(&session, token, idx(color));
    }
    session.send(b"a");
    session.wait_text("After changes");
    assert!(!session.text().contains("-count = 42"));
    for (token, color) in [("count", 6), ("43", 3), ("true", 5), ("1979-05-27", 4)] {
        assert_token(&session, token, idx(color));
    }
    session.send(b"a");
    session.wait_text("-count = 42");
    assert!(!session.text().contains("After changes"));
    session.send(b"q");
    session.wait_text("Files");
    session.send(b",q");
    session.finish(0);
    assert!(!root.join("state").exists(), "TOML viewing changed marks");
    assert_same(
        &toml_before,
        &ctx.repo_snapshot(),
        "TOML viewing changed repository bytes",
    );
    std::fs::remove_file(repo.join("colors.toml")).unwrap();
    ctx.git_run(&["rm", "--cached", "colors.toml"]);
    ctx.git_run(&["commit", "-qm", "remove toml fixture"]);
    ctx.before = ctx.repo_snapshot();

    // Installed themes and command line: gruvbox-baby without config, runtime-only
    // switching, completion, cancel, errors, Emacs alt-x and invalid startup names.
    std::fs::remove_file(&ctx.config_file).unwrap();
    let mut session = ctx.default_session();
    session.wait_text("Files");
    session.send(b"\r");
    session.wait_text("+fn caller()");
    assert_eq!(
        token_color(&session, "+fn caller()", "fn"),
        rgb(251, 73, 52),
        "{:?}",
        session.colors
    );
    assert_eq!(
        token_color(&session, "+fn caller()", "caller"),
        rgb(238, 189, 53)
    );
    session.send(b":");
    assert!(status_row(&session).starts_with(':'), "{}", session.text());
    assert!(
        session.cursor_visible && cursor(&session) == (28, 1),
        "{:?}",
        cursor(&session)
    );
    session.send(b"colo\t");
    session.wait_text(":colorscheme ");
    session.send(b" gru\t");
    session.wait_text(":colorscheme gruvbox-baby");
    session.send(b"\t");
    session.wait_text(":colorscheme gruvbox-dark");
    session.send(b"\t");
    session.wait_text(":colorscheme gruvbox-light");
    assert_eq!(
        cursor(&session),
        (28, ":colorscheme gruvbox-light".len() as i64)
    );
    session.send(b"\x1b");
    assert!(
        !session.text().contains(":colorscheme"),
        "{}",
        session.text()
    );
    assert!(
        session.text().contains("┌Diff"),
        "Escape in the command line closed the diff"
    );
    assert_eq!(
        token_color(&session, "+fn caller()", "fn"),
        rgb(251, 73, 52)
    );
    session.send(b":colorscheme nope\r");
    session.wait_text("Unknown colorscheme \"nope\"");
    assert_eq!(
        token_color(&session, "+fn caller()", "fn"),
        rgb(251, 73, 52)
    );
    session.send(b":colorscheme review-classic\r");
    session.wait_text("appearance.colorscheme=review-classic");
    assert_eq!(
        token_color(&session, "+fn caller()", "fn"),
        idx(5),
        "{}",
        session.text()
    );
    assert_eq!(token_color(&session, "+fn caller()", "caller"), idx(4));
    session.send(b":set appearance.colorscheme=gruvbox-dark\r");
    session.wait_text("appearance.colorscheme=gruvbox-dark");
    assert_eq!(
        token_color(&session, "+fn caller()", "fn"),
        rgb(251, 73, 52)
    );
    session.send(b",q");
    session.finish(0);
    let mut session = ctx.session(&["--keymap", "emacs", "--", "HEAD"]);
    session.wait_text("Files");
    session.send(b"\x1bx");
    assert!(status_row(&session).starts_with(':'), "{}", session.text());
    session.send(b"colorscheme\r");
    session.wait_text("appearance.colorscheme=gruvbox-baby");
    session.send(b",q");
    session.finish(0);
    // Command palette: alt-p filters registry entries and runs them through the
    // command line's execution; cancelling leaves the view and theme untouched.
    let palette_state = ctx.state_snapshot();
    let palette_repo = ctx.repo_snapshot();
    let mut session = ctx.default_session();
    session.wait_text("Files");
    session.send(b"\r");
    session.wait_text("+fn caller()");
    assert_eq!(
        token_color(&session, "+fn caller()", "fn"),
        rgb(251, 73, 52)
    );
    session.send(b"\x1bp");
    session.wait_text(" Command palette ");
    session.wait_text(" enter run · esc cancel · ↑/↓ select ");
    session.send(b"classic");
    session.wait_text("> classic");
    session.wait_text("set appearance.colorscheme=review-classic");
    let (query_column, query_row) = locate(&session, "> classic");
    assert_eq!(
        cursor(&session),
        (query_row as i64, (query_column + "> classic".len()) as i64)
    );
    session.send(b"\r");
    session.wait_text("appearance.colorscheme=review-classic (this session only)");
    assert!(
        !session.text().contains(" Command palette "),
        "{}",
        session.text()
    );
    assert!(
        status_row(&session)
            .starts_with("appearance.colorscheme=review-classic (this session only)"),
        "{}",
        session.text()
    );
    assert_eq!(
        token_color(&session, "+fn caller()", "fn"),
        idx(5),
        "{}",
        session.text()
    );
    let before_cancel = session.text();
    session.send(b"\x1bp");
    session.wait_text(" Command palette ");
    session.send(b"dark");
    session.wait_text("> dark");
    session.send(b"\x1b");
    assert!(
        !session.text().contains(" Command palette "),
        "{}",
        session.text()
    );
    assert_eq!(session.text(), before_cancel);
    assert!(
        session.text().contains("┌Diff"),
        "Escape in the palette closed the diff"
    );
    assert_eq!(token_color(&session, "+fn caller()", "fn"), idx(5));
    session.send(b",q");
    session.finish(0);
    let mut session = ctx.session(&["--keymap", "emacs", "--", "HEAD"]);
    session.wait_text("Files");
    session.send(b"\r");
    session.wait_text("+fn caller()");
    session.send(b"\x1bp");
    session.wait_text(" Command palette ");
    session.send(b"appearance");
    session.wait_text("set appearance.colorscheme=1337");
    assert!(
        !session.text().contains("│colorscheme "),
        "{}",
        session.text()
    );
    // Esc cancels once without running anything.
    session.send(b"\x1b");
    assert!(
        !session.text().contains(" Command palette "),
        "{}",
        session.text()
    );
    session.send(b"\x1bp");
    session.wait_text(" Command palette ");
    session.send(b"appearance classic");
    session.wait_text("set appearance.colorscheme=review-classic");
    // Ctrl-N is clamped on the single match.
    session.send(b"\x0e\r");
    session.wait_text("appearance.colorscheme=review-classic (this session only)");
    assert_eq!(token_color(&session, "+fn caller()", "fn"), idx(5));
    session.send(b"\x1bp");
    session.wait_text(" Command palette ");
    session.send(b"\x03");
    session.finish(0);
    assert_same(
        &palette_state,
        &ctx.state_snapshot(),
        "command palette changed review state",
    );
    assert_same(
        &palette_repo,
        &ctx.repo_snapshot(),
        "command palette changed repository bytes",
    );
    pty_passed("command palette");
    // Default-theme Rust captured source view and YAML/TOML diffs and source.
    assert_same(
        &ctx.before,
        &ctx.repo_snapshot(),
        "theme sessions changed repository bytes",
    );
    let mut session = ctx.default_session();
    session.wait_text("Files");
    session.send(b"/caller\r");
    session.wait_text("Search candidates");
    session.send(b"\r");
    session.wait_text("Captured ");
    assert!(session.text().contains("caller.rs"), "{}", session.text());
    // The view opens scrolled to the symbol column, so "fn" is off-screen.
    assert_eq!(
        token_color(&session, "caller() {", "caller"),
        rgb(238, 189, 53),
        "{}",
        session.text()
    );
    assert_eq!(
        token_color(&session, "// ", "//"),
        rgb(102, 92, 84),
        "{}",
        session.text()
    );
    session.send(b",q");
    session.finish(0);
    write(&repo.join("colors.yaml"), &yaml_text);
    write(&repo.join("colors.toml"), &toml_text);
    ctx.git_run(&["add", "colors.yaml", "colors.toml"]);
    ctx.git_run(&["commit", "-qm", "theme fixtures"]);
    write(&repo.join("colors.yaml"), yaml_text.replace("42", "43"));
    write(&repo.join("colors.toml"), toml_text.replace("42", "43"));
    let theme_before = ctx.repo_snapshot();
    let mut session = ctx.session(&["--", "HEAD", "--", "colors.yaml"]);
    session.wait_text("Files");
    session.send(b"\r");
    session.wait_text("+  count: 43");
    for (row_text, token, color) in [
        ("+  count: 43", "count", rgb(69, 133, 136)),
        ("+  count: 43", "43", rgb(177, 98, 134)),
    ] {
        assert_eq!(
            token_color(&session, row_text, token),
            color,
            "{token}\n{}",
            session.text()
        );
    }
    session.send(b"\x1b");
    session.send(b"/service\r");
    session.wait_text("Search candidates");
    session.send(b"\r");
    session.wait_text("Captured old colors.yaml");
    for (token, color) in [
        ("service:", rgb(69, 133, 136)),
        ("café", rgb(152, 151, 26)),
        ("42", rgb(177, 98, 134)),
        ("# review", rgb(102, 92, 84)),
    ] {
        assert_eq!(
            token_color(&session, token, token),
            color,
            "{token}\n{}",
            session.text()
        );
    }
    session.send(b",q");
    session.finish(0);
    let mut session = ctx.session(&["--", "HEAD", "--", "colors.toml"]);
    session.wait_text("Files");
    session.send(b"\r");
    session.wait_text("+count = 43");
    for (row_text, token, color) in [
        ("+count = 43", "count", rgb(69, 133, 136)),
        ("+count = 43", "43", rgb(177, 98, 134)),
    ] {
        assert_eq!(
            token_color(&session, row_text, token),
            color,
            "{token}\n{}",
            session.text()
        );
    }
    for (token, color) in [("café", rgb(152, 151, 26)), ("# review", rgb(102, 92, 84))] {
        assert_eq!(
            token_color(&session, token, token),
            color,
            "{token}\n{}",
            session.text()
        );
    }
    session.send(b",q");
    session.finish(0);
    assert_same(
        &theme_before,
        &ctx.repo_snapshot(),
        "theme sessions changed repository bytes",
    );
    for name in ["colors.yaml", "colors.toml"] {
        std::fs::remove_file(repo.join(name)).unwrap();
        ctx.git_run(&["rm", "-q", "--cached", name]);
    }
    ctx.git_run(&["commit", "-qm", "remove theme fixtures"]);
    ctx.before = ctx.repo_snapshot();
    assert!(!root.join("state").exists(), "theme commands changed marks");
    assert!(
        !ctx.config_file.exists(),
        "theme commands wrote configuration"
    );
    let bad_theme = root.join("bad-theme.toml");
    write(&bad_theme, "[appearance]\ncolorscheme = \"nope\"\n");
    let result = ctx.run(
        &["--config", path_str(&bad_theme), "--", "HEAD"],
        Some(&repo),
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        result.status.code() == Some(1)
            && stderr.starts_with("review: ")
            && stderr.contains("\"nope\""),
        "{stderr}"
    );
    assert!(
        !stderr.contains("interactive terminal"),
        "theme was not validated before terminal setup"
    );
    write(&ctx.config_file, CLASSIC_CONFIG);
    pty_passed(
        "default theme, command line, completion, runtime theme switching and invalid themes",
    );
}

fn navigation_and_lifecycle(ctx: &mut Ctx) {
    let root = &ctx.root;
    // Installed captured-context toggle: reversible, explicit marks unchanged,
    // source bytes immutable, and ordinary mode restored on reopen/restart.
    let mut session = ctx.default_session();
    session.wait_text("Files");
    session.send(b"a");
    assert!(!session.text().contains("After changes"));
    session.send(b"\r");
    session.wait_text("-fn caller() { main_target(); }");
    session.send(b"a");
    session.wait_text("After changes");
    assert!(!session.text().contains("-fn caller() { main_target(); }"));
    assert!(
        session
            .text()
            .contains("+fn caller() { main_target(); main_target(); }")
    );
    assert!(session.text().contains("@@"));
    assert!(
        !root.join("state").exists(),
        "toggle must not save review state"
    );
    session.send(b"a");
    session.wait_text("-fn caller() { main_target(); }");
    session.send(b"a");
    session.send(b"\x1b");
    session.send(b"\r");
    session.wait_text("-fn caller() { main_target(); }");
    assert!(!session.text().contains("After changes"));
    session.send(b",q");
    session.finish(0);
    assert_same(
        &ctx.before,
        &ctx.repo_snapshot(),
        "after-changes toggle changed repository bytes",
    );

    let mut session = ctx.default_session();
    session.wait_text("Files");
    for name in FIXTURE_NAMES {
        let symbol = format!("navtarget_{}_end", name.rsplit('.').next().unwrap());
        session.send(format!("/{symbol}\r").as_bytes());
        session.wait_text("2 results");
        session.send(b"\r");
        session.wait_text(&format!("Captured old {name}"));
        session.send(b"gd");
        session.wait_text("Definitions candidates");
        session.wait_text("1 results");
        session.send(b"\x0fgr");
        session.wait_text("References candidates");
        session.wait_text("1 results");
        session.send(b"\x1b");
    }
    for extension in ["yaml", "json"] {
        session.send(format!("/navport_{extension}\r").as_bytes());
        session.wait_text("Search candidates");
        session.send(b"\r");
        session.wait_text(&format!("Captured old sample.{extension}"));
        session.send(b"gd");
        session.wait_text("Definitions candidates");
        session.send(b"\x0fgr");
        session.wait_text("References candidates");
        session.wait_text("0 results");
        session.send(b"\x1b");
    }
    session.send(b"\r");
    session.wait_text("界");
    session.send(&[b"\x1b[C".repeat(20), b"\x1b[D".repeat(5)].concat());
    session.send(b"\x1b");
    session.send(b"/");
    session.send(b"main_target\r");
    session.wait_text("results");
    session.send(b"\r");
    session.wait_text("Captured old");
    session.send(b"]gr");
    session.wait_text("References");
    session.send(b"\r\x0f\x0f");
    session.send(b"\x1b");
    for (rows, cols) in [(1, 1), (3, 8), (30, 140)] {
        session.resize(rows, cols);
    }
    session.send(b"m");
    session.wait_text("Review mark saved");
    session.send(b",q");
    session.finish(0);
    for exit_method in ["q", "ctrl-c", "SIGINT", "SIGTERM"] {
        let mut session = ctx.default_session();
        session.wait_text("Files");
        session.wait_text("+ ");
        match exit_method {
            "SIGINT" => session.signal(libc::SIGINT),
            "SIGTERM" => session.signal(libc::SIGTERM),
            "q" => session.send(b"q"),
            _ => session.send(b"\x03"),
        }
        session.finish(0);
    }
    let mut session = ctx.session(&["--", "missing-reference"]);
    session.wait_text("Git status");
    session.send(b",q");
    session.finish(128);
    if let Some(test_binary) = &ctx.test_binary {
        for scenario in ["panic", "error", "worker-panic"] {
            let mut session = Session::new(
                test_binary,
                &["--exact", "tests::terminal_lifecycle_worker", "--nocapture"],
                &ctx.repo,
                &ctx.env,
                Some(scenario),
                false,
            );
            if scenario == "worker-panic" {
                session.wait_text("Files");
                session.send(b"/\r");
                session.wait_text("Analysis failed");
                assert!(
                    session.termios() != session.original,
                    "caught worker panic disabled raw mode"
                );
                assert!(
                    !contains(&session.output, b"\x1b[?1049l"),
                    "caught worker panic left alternate screen"
                );
                session.send(b",q");
            }
            session.finish(if scenario == "panic" { 101 } else { 0 });
        }
    }
    assert_eq!(
        ctx.before,
        ctx.repo_snapshot(),
        "review changed repository bytes"
    );
    pty_passed("language navigation and terminal lifecycle");
}

fn config_and_keymaps(ctx: &Ctx) {
    let root = &ctx.root;
    let repo = &ctx.repo;
    // Config and keymap acceptance uses the installed executable and actual
    // terminal modifier events, independent of the in-process dispatch tests.
    let config_before = ctx.repo_snapshot();
    write(
        &ctx.config_file,
        "[features]\nkeymap = \"emacs\"\nreadline = false\n",
    );
    let mut session = ctx.default_session();
    session.wait_text("Files");
    session.send(b"hl");
    assert!(
        !session.text().contains("┌Diff"),
        "legacy file-pane h/l opened a diff"
    );
    session.send(b"\r");
    session.wait_text("Diff");
    let origin = cursor(&session);
    session.send(b"jdrs");
    assert_eq!(
        cursor(&session),
        origin,
        "legacy/Vim keys leaked into Emacs map"
    );
    assert!(!session.text().contains("candidates for"));
    session.send(b"\x0e"); // C-n
    assert_eq!(cursor(&session), (origin.0 + 1, origin.1));
    session.send(b"\x10"); // C-p
    assert_eq!(cursor(&session), origin);
    session.send(b"\x06"); // C-f
    assert_eq!(cursor(&session), (origin.0, origin.1 + 1));
    session.send(b"\x02"); // C-b
    assert_eq!(cursor(&session), origin);
    session.send(b"\x05"); // C-e
    assert!(session.col > origin.1);
    session.send(b"\x01"); // C-a
    assert_eq!(cursor(&session), origin);
    session.send(b"/main_target\r");
    session.wait_text("Search candidates");
    session.send(b"\r");
    session.wait_text("Captured old target.rs");
    session.send(b"\x1b."); // M-.
    session.wait_text("Definitions candidates");
    session.send(b"\x1b,"); // M-,
    session.wait_text("Captured old target.rs");
    session.send(b"\x1b");
    assert!(session.poll().is_none(), "Emacs Escape exited");
    session.send(b",q");
    session.finish(0);

    // CLI style wins discovery, with readline disabled independently of Vim.
    let mut session = ctx.session(&["--keymap", "vim", "--", "HEAD"]);
    session.wait_text("Files");
    session.send(b"\r");
    session.wait_text("Diff");
    let origin = cursor(&session);
    session.send(b"\x0e");
    assert_eq!(
        cursor(&session),
        origin,
        "disabled readline moved the cursor"
    );
    session.send(b"j");
    assert_eq!(
        session.row,
        origin.0 + 1,
        "CLI keymap did not override discovered Emacs map"
    );
    session.send(b",q");
    session.finish(0);

    // Supplemental readline works in Vim when its action is not overridden.
    let readline_config = root.join("readline.toml");
    write(
        &readline_config,
        "[features]\nkeymap=\"vim\"\nreadline=true\n",
    );
    let mut session = ctx.session(&["--config", path_str(&readline_config), "--", "HEAD"]);
    session.wait_text("Files");
    session.send(b"\r");
    session.wait_text("Diff");
    let origin = cursor(&session);
    session.send(b"\x0e");
    assert_eq!(
        session.row,
        origin.0 + 1,
        "enabled readline failed in Vim diff"
    );
    session.send(b",q");
    session.finish(0);

    // Overrides replace defaults, support sequences and update the legend.
    let explicit_config = root.join("custom.toml");
    let explicit = path_str(&explicit_config);
    write(
        &explicit_config,
        "[features]\nkeymap = \"vim\"\nreadline = true\n[keybindings]\nclose_diff_view = \"x\"\ncursor_down = \"z\"\ngo_to_definition = \"gD\"\n",
    );
    let mut session = ctx.session(&["--config", explicit, "--", "HEAD"]);
    session.wait_text("Files");
    session.send(b"\r");
    session.wait_text("Diff");
    let origin = cursor(&session);
    session.send(b"j");
    assert_eq!(cursor(&session), origin, "overridden j binding survived");
    session.send(b"z");
    assert_eq!(session.row, origin.0 + 1);
    session.send(b"\x0e");
    assert_eq!(
        session.row,
        origin.0 + 1,
        "overridden action retained its readline alias"
    );
    session.send(b"/main_target\r");
    session.wait_text("Search candidates");
    session.send(b"\r");
    session.wait_text("Captured old target.rs");
    session.send(b"gd");
    assert!(
        !session.text().contains("Definitions candidates"),
        "overridden gd binding survived"
    );
    session.send(b"gD");
    session.wait_text("Definitions candidates");
    session.send(b"x");
    session.wait_text("Files");
    assert!(!session.text().contains("┌Diff"));
    assert!(
        session.text().contains("x close"),
        "footer ignored close override"
    );
    session.send(b",q");
    session.finish(0);

    let help = ctx.help(&["--config", explicit, "--help"]);
    assert!(help.contains("close_diff_view: x\n"));
    assert!(help.contains("cursor_down: z\n"));
    assert!(help.contains("go_to_definition: gD\n"));

    // An empty value disables its action; explicit config also wins discovery.
    write(
        &explicit_config,
        "[features]\nkeymap=\"vim\"\nreadline=false\n[keybindings]\ncursor_down=\"\"\n",
    );
    let mut session = ctx.session(&["--config", explicit, "--", "HEAD"]);
    session.wait_text("Files");
    session.send(b"\r");
    session.wait_text("Diff");
    let origin = cursor(&session);
    session.send(b"j\x1b[B");
    assert_eq!(
        cursor(&session),
        origin,
        "disabled cursor_down action remained active"
    );
    session.send(b",q");
    session.finish(0);

    for invalid in [
        "[features]\nkeymap=\"unknown\"\n",
        "[features]\nreadline=\"yes\"\n",
        "[unknown]\nx=true\n",
        "[keybindings]\nunknown=\"j\"\n",
        "[keybindings]\ncursor_down=\"ctrl-unknown\"\n",
        "[keybindings]\ncursor_down=\"k\"\n",
        "[keybindings]\ncursor_down=\"g\"\n",
        "[keybindings]\ngo_to_definition=\"g esc\"\n",
        "[keybindings]\nopen_diff_view=\"ctrl-i\"\n",
        "[keybindings]\ngo_to_definition=\"ctrl-[\"\n",
    ] {
        write(&explicit_config, invalid);
        let result = ctx.run(&["--config", explicit, "--", "HEAD"], Some(repo));
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            !result.status.success() && stderr.contains("review:"),
            "{invalid:?} {stderr}"
        );
        assert!(
            !stderr.contains("interactive terminal"),
            "config was not validated before terminal setup"
        );
    }
    let missing = root.join("missing.toml");
    let result = ctx.run(&["--config", path_str(&missing), "--", "HEAD"], Some(repo));
    assert!(
        !result.status.success()
            && !String::from_utf8_lossy(&result.stderr).contains("interactive terminal")
    );
    write(&explicit_config, "[keybindings]\ncursor_down=\"\"\n");
    let help = ctx.help(&["--help", "--config", explicit, "--keymap", "vim"]);
    assert!(help.contains("cursor_down: (disabled)\n"));
    assert!(help.contains("Active bindings (Vim)"));
    write(&ctx.config_file, CLASSIC_CONFIG);
    assert_same(
        &config_before,
        &ctx.repo_snapshot(),
        "config sessions changed repository bytes",
    );
    pty_passed(
        "discovered/explicit config, Vim/Emacs, overrides, readline, disabled/removed keys and invalid config",
    );
}

fn documentation(ctx: &Ctx) {
    // Captured documentation preserves candidate origin and never writes marks.
    let docs_before = ctx.repo_snapshot();
    for dismiss in [b"q", b"\x1b", b"\x0f"] {
        let mut session = ctx.default_session();
        session.wait_text("Files");
        session.send(b"/main_target\r");
        session.wait_text("Search candidates");
        session.send(b"K");
        session.wait_text("Captured terminal documentation.");
        session.send(b"m");
        session.resize(3, 8);
        session.resize(30, 140);
        session.send(dismiss);
        session.wait_text("Search candidates");
        assert!(session.poll().is_none());
        session.send(b"\r");
        session.wait_text("Captured old target.rs");
        session.send(b"K");
        session.wait_text("Function documentation");
        session.send(b"q");
        session.wait_text("Captured old target.rs");
        session.send(b"/caller\r");
        session.wait_text("Search candidates");
        session.send(b"\r");
        session.wait_text("Captured old caller.rs");
        session.send(b"014lK");
        session.wait_text("Function documentation");
        session.send(b"q");
        session.wait_text("Captured old caller.rs");
        session.send(b"K");
        session.wait_text("Captured terminal documentation.");
        session.send(b",q");
        session.finish(0);
    }
    let mut session = ctx.default_session();
    session.wait_text("Files");
    session.send(b"/docs_caller\r");
    session.wait_text("Search candidates");
    session.send(b"\r");
    session.wait_text("Captured old docs.rs");
    session.send(b"019lK");
    session.wait_text("Documentation candidates");
    session.wait_text("2 results");
    session.send(b"j\r");
    session.wait_text("Documentation unavailable");
    session.send(b"k ");
    session.wait_text("Captured ambiguous documentation.");
    session.send(b",q");
    session.finish(0);
    let docs_config = ctx.root.join("documentation.toml");
    for style in ["vim", "emacs"] {
        for binding in ["D", ""] {
            write(
                &docs_config,
                format!(
                    "[features]\nkeymap=\"{style}\"\n[keybindings]\nshow_documentation=\"{binding}\"\n"
                ),
            );
            let config = path_str(&docs_config);
            let help = ctx.help(&["--config", config, "--help"]);
            let shown = if binding.is_empty() {
                "(disabled)"
            } else {
                binding
            };
            assert!(help.contains(&format!("show_documentation: {shown}\n")));
            let mut session = ctx.session(&["--config", config, "--", "HEAD"]);
            session.wait_text("Files");
            session.send(b"/main_target\r");
            session.wait_text("Search candidates");
            let origin = session.text();
            session.send(b"K");
            assert!(
                session.text() == origin,
                "replaced default K remained active: {style}, {binding:?}"
            );
            session.send(b"D");
            if !binding.is_empty() {
                session.wait_text("Function documentation");
                session.wait_text("Captured terminal documentation.");
                session.send(b"q");
                session.wait_text("Search candidates");
            } else {
                assert!(
                    session.text() == origin,
                    "disabled documentation action ran: {style}"
                );
            }
            session.send(b",q");
            session.finish(0);
        }
    }
    std::fs::remove_file(&docs_config).unwrap();
    assert_same(
        &docs_before,
        &ctx.repo_snapshot(),
        "documentation sessions changed repository bytes",
    );
    pty_passed(
        "captured function documentation, custom/disabled keys in both styles, effective help, dismissal, tiny terminals and global quit",
    );
}

fn previews(ctx: &Ctx) {
    // Definition previews use captured function/type targets and reversible views.
    let preview_before = ctx.repo_snapshot();
    let preview_marks = ctx.state_snapshot();
    for (target_row, name) in [(1i64, "start"), (50, "middle"), (99, "end")] {
        let mut session = ctx.default_session();
        session.wait_text("Files");
        session.send(b"/centered_caller\r");
        session.wait_text("Search candidates");
        session.send(b"\r");
        session.wait_text("Captured old centered.rs");
        let column = CENTERED_CALLER.find(&format!("centered_{name}")).unwrap();
        session.send(format!("0{column}l").as_bytes());
        let origin = view(&session);
        session.send(b"P");
        session.wait_text("Definition preview");
        session.wait_text("Captured old centered.rs");
        let content_height = 25; // terminal rows minus borders, status and global rows
        let top = (target_row - content_height / 2)
            .max(0)
            .min(101 - content_height);
        assert!(session.cursor_visible);
        assert_eq!(
            session.row,
            1 + target_row - top,
            "{name}\n{}",
            session.text()
        );
        assert_eq!(
            session.cells.get(&cursor(&session)),
            Some(&'c'),
            "{}",
            session.text()
        );
        let opening_row = session.row;
        session.send(b"j");
        assert_eq!(
            session.row,
            opening_row + 1,
            "ordinary motion recentered preview"
        );
        session.resize(3, 8);
        assert!(!session.cursor_visible);
        session.resize(6, 140);
        assert!(session.cursor_visible && session.row == 1);
        session.resize(30, 140);
        assert!(session.cursor_visible && 1 <= session.row && session.row <= content_height);
        session.send(b"m");
        session.send(b"q");
        session.wait_text("Captured old centered.rs");
        assert!(view(&session) == origin, "centered preview lost origin");
        session.send(b",q");
        session.finish(0);
    }
    for symbol in ["preview_unique", "PreviewType"] {
        for dismiss in [b"q", b"\x1b", b"\x0f"] {
            let mut session = ctx.default_session();
            session.wait_text("Files");
            session.send(b"/preview_caller\r");
            session.wait_text("Search candidates");
            session.send(b"\r");
            session.wait_text("Captured old preview.rs");
            let column = PREVIEW_CALLER.find(symbol).unwrap();
            session.send(format!("0{column}l").as_bytes());
            let origin = view(&session);
            session.send(b"P");
            session.wait_text("Definition preview");
            session.wait_text("Captured old preview.rs");
            session.send(b"m"); // Viewing never toggles the selected file's mark.
            session.resize(3, 8);
            session.resize(30, 140);
            session.send(dismiss);
            session.wait_text("Captured old preview.rs");
            assert!(view(&session) == origin, "P dismissal lost source origin");
            session.send(b",q");
            session.finish(0);
        }
    }
    let mut session = ctx.default_session();
    session.wait_text("Files");
    session.send(b"/preview_ambiguous_caller\r");
    session.wait_text("Search candidates");
    session.send(b"\r");
    session.wait_text("Captured old preview.rs");
    let column = PREVIEW_AMBIGUOUS_CALLER
        .find("preview_ambiguous();")
        .unwrap();
    session.send(format!("0{column}lP").as_bytes());
    session.wait_text("PreviewDefinition candidates");
    session.wait_text("2 results");
    session.send(b"j");
    let origin = view(&session);
    session.send(b"\r");
    session.wait_text("Definition preview");
    session.send(b"q");
    session.wait_text("PreviewDefinition candidates");
    assert!(
        view(&session) == origin,
        "P lost ambiguous candidate selection"
    );
    session.send(b"q");
    session.wait_text("Captured old preview.rs");
    session.send(b",q");
    session.finish(0);
    let preview_config = ctx.root.join("preview.toml");
    for style in ["vim", "emacs"] {
        for binding in ["D", ""] {
            write(
                &preview_config,
                format!(
                    "[features]\nkeymap=\"{style}\"\n[keybindings]\npreview_definition=\"{binding}\"\n"
                ),
            );
            let config = path_str(&preview_config);
            let help = ctx.help(&["--config", config, "--help"]);
            let shown = if binding.is_empty() {
                "(disabled)"
            } else {
                binding
            };
            assert!(help.contains(&format!("preview_definition: {shown}\n")));
            let mut session = ctx.session(&["--config", config, "--", "HEAD"]);
            session.wait_text("Files");
            session.send(b"/PreviewType\r");
            session.wait_text("Search candidates");
            let origin = view(&session);
            session.send(b"P");
            assert!(
                view(&session) == origin,
                "replaced default P remained active"
            );
            session.send(b"D");
            if !binding.is_empty() {
                session.wait_text("Definition preview");
                session.wait_text("Captured old preview.rs");
                session.send(b"q");
                session.wait_text("Search candidates");
                assert!(view(&session) == origin, "P lost selected exact candidate");
            } else {
                assert!(view(&session) == origin, "disabled P action ran");
            }
            session.send(b",q");
            session.finish(0);
        }
    }
    std::fs::remove_file(&preview_config).unwrap();
    let mut session = ctx.default_session();
    session.wait_text("Files");
    session.send(b"/preview_variable\r");
    session.wait_text("Search candidates");
    session.send(b"P");
    session.wait_text("Preview unavailable");
    session.wait_text("Search candidates");
    session.send(b",q");
    session.finish(0);
    assert_same(
        &preview_before,
        &ctx.repo_snapshot(),
        "previews changed repository bytes",
    );
    assert_same(
        &preview_marks,
        &ctx.state_snapshot(),
        "previews changed review marks",
    );
    pty_passed(
        "captured function/type previews, exact return, ambiguity, unavailable targets, custom/disabled keys, read-only marks, tiny terminals and global quit",
    );
}

fn quitting(ctx: &Ctx) {
    // q closes a view first; leader-q exits directly from every context.
    for source_view in [false, true] {
        let mut session = ctx.default_session();
        session.wait_text("Files");
        if source_view {
            session.send(b"/main_target\r");
            session.wait_text("Search candidates");
            session.send(b"\r");
            session.wait_text("Captured old target.rs");
        } else {
            session.send(b"\r");
            session.wait_text("Diff");
        }
        session.send(b"q");
        session.wait_text("Files");
        assert!(
            !session.text().contains("┌Diff") && !session.text().contains("Captured old target.rs")
        );
        assert!(session.poll().is_none(), "q exited before closing the view");
        session.send(b"q");
        session.finish(0);
    }
    for context in ["files", "diff", "candidates", "source", "query", "original"] {
        let mut session = if context == "original" {
            ctx.session(&["--", "--stat", "HEAD"])
        } else {
            ctx.default_session()
        };
        session.wait_text(if context == "original" {
            "Original Git output"
        } else {
            "Files"
        });
        match context {
            "diff" => {
                session.send(b"\r");
                session.wait_text("Diff");
            }
            "candidates" | "source" => {
                session.send(b"/main_target\r");
                session.wait_text("Search candidates");
                if context == "source" {
                    session.send(b"\r");
                    session.wait_text("Captured old target.rs");
                }
            }
            "query" => {
                session.send(b"/main");
                session.wait_text("Search/outline: main");
            }
            _ => {}
        }
        session.send(b",q");
        session.finish(0);
    }
    pty_passed(
        "q closes then exits; leader-q quits from files, diff, candidates, source, query and original output",
    );
}

/// Story 6: run product acceptance through the installed executable, keeping
/// setup mutations distinct from read-only review sessions.
fn acceptance_setup(ctx: &Ctx) -> (PathBuf, &'static str, &'static str) {
    let acceptance = ctx.root.join("acceptance");
    std::fs::create_dir(&acceptance).unwrap();
    let setup_git = |words: &[&str]| ctx.git_in(&acceptance, words);
    setup_git(&["init", "-q"]);
    setup_git(&["config", "user.name", "Acceptance"]);
    setup_git(&["config", "user.email", "acceptance@example.invalid"]);
    for name in ["one.rs", "two.rs"] {
        write(
            &acceptance.join(name),
            format!("fn {}() {{ let value = 1; }}\n", &name[..name.len() - 3]),
        );
    }
    setup_git(&["add", "."]);
    setup_git(&["commit", "-qm", "base"]);
    let (left, right) = ("feat/base-work", "feat/next-work");
    setup_git(&["branch", left]);
    for name in ["one.rs", "two.rs"] {
        write(
            &acceptance.join(name),
            format!("fn {}() {{ let value = 2; }}\n", &name[..name.len() - 3]),
        );
    }
    setup_git(&["add", "."]);
    setup_git(&["commit", "-qm", "both changed"]);
    setup_git(&["branch", right]);
    (acceptance, left, right)
}

fn closed(session: &mut Session) {
    session.wait_text("Files");
    assert!(!session.text().contains("┌Diff"), "{}", session.text());
    assert!(!session.text().contains("let value"), "{}", session.text());
}

fn marks(session: &mut Session, expected: &[(&str, bool)]) {
    closed(session);
    for &(name, marked) in expected {
        session.wait_text(&format!("{}{name}", if marked { "+ " } else { "- " }));
        if marked {
            // Reviewed filenames must be dark gray (ANSI bright black),
            // including after restart, rather than merely showing a plus.
            let lines = session.lines();
            if let Some((row, line)) = lines
                .iter()
                .enumerate()
                .find(|(_, line)| line.contains(&format!("+ {name}")))
            {
                let col = char_index(line, name).unwrap();
                assert_eq!(session.color(row, col), idx(8), "{name}");
            }
        }
    }
}

fn quit_unchanged(session: &mut Session, before: &Snapshot, cwd: &Path, expected: i32) {
    session.send(b",q");
    session.finish(expected);
    assert!(
        snapshot(cwd) == *before,
        "installed review modified comparison inputs"
    );
}

fn indexing(ctx: &Ctx) {
    // Previously omitted bindings and generic trait calls remain navigable even
    // when non-code files sort before unchanged source in the captured catalog.
    let indexing_repo = ctx.root.join("indexing");
    std::fs::create_dir(&indexing_repo).unwrap();
    let indexing_git = |words: &[&str]| ctx.git_in(&indexing_repo, words);
    indexing_git(&["init", "-q"]);
    indexing_git(&["config", "user.name", "Index acceptance"]);
    indexing_git(&["config", "user.email", "index@example.invalid"]);
    let docs = indexing_repo.join("a-docs");
    std::fs::create_dir(&docs).unwrap();
    let documentation = "Documentation only.\n".repeat(460);
    for index in 0..1030 {
        write(&docs.join(format!("{index:04}.md")), &documentation);
    }
    let trait_source = "pub trait Convert<T> {
    fn adapt<U>(&self, input: U) -> T;
}
pub struct Worker;
impl Convert<u32> for Worker {
    fn adapt<U>(&self, _input: U) -> u32 { 7 }
}
pub fn convert_value<U>(value: U) -> U { value }
pub fn invoke(worker: Worker) -> u32 {
    let local_value = 3;
    let callback = convert_value::<u32>;
    let after = callback(local_value);
    // adapt local_value convert_value
    let ignored = \"adapt local_value convert_value\";
    worker.adapt::<u32>(after)
}
";
    write(&indexing_repo.join("zzz-traits.rs"), trait_source);
    write(
        &indexing_repo.join("caller.rs"),
        "fn caller() { invoke(Worker); }\n",
    );
    indexing_git(&["add", "."]);
    indexing_git(&["commit", "-qm", "generic trait and late source"]);
    write(
        &indexing_repo.join("caller.rs"),
        "fn caller() { let result = invoke(Worker); }\n",
    );
    let indexing_before = snapshot(&indexing_repo);
    let index_state_before = ctx.state_snapshot();
    let mut session = ctx.session_in(&indexing_repo, &["--", "HEAD", "--", "caller.rs"]);
    closed(&mut session);
    session.send(b"/adapt\r");
    session.wait_text("4 results"); // trait declaration and impl, on both sides
    session.send(b"\r");
    session.wait_text("Captured old zzz-traits.rs");
    let (symbol_x, symbol_y) = locate(&session, "adapt<U>");
    assert!(
        session.color(symbol_y, symbol_x).is_some(),
        "generic trait declaration lost highlighting"
    );
    session.send(b"gr");
    session.wait_text("1 results"); // excludes same text in comment and string
    session.send(b"\r");
    session.wait_text("Captured old zzz-traits.rs");
    let (symbol_x, symbol_y) = locate(&session, "adapt::<u32>");
    assert_eq!(
        cursor(&session),
        (symbol_y as i64, symbol_x as i64),
        "generic call opened at wrong scalar position"
    );
    let read_cursor = cursor(&session);
    session.send(b"gd");
    session.wait_text("2 results"); // ambiguity retains trait declaration and impl
    session.send(b"\x0f");
    session.wait_text("Captured old zzz-traits.rs");
    assert_eq!(cursor(&session), read_cursor);
    session.send(b"]");
    session.wait_text("Captured new zzz-traits.rs");
    session.send(b"gd");
    session.wait_text("2 results");
    session.send(b"\x1b");
    closed(&mut session);

    for (symbol, occurrence) in [
        ("local_value", "local_value);"),
        ("convert_value", "convert_value::<u32>"),
    ] {
        session.send(format!("/{symbol}\r").as_bytes());
        session.wait_text("2 results");
        session.send(b"\r");
        session.wait_text("Captured old zzz-traits.rs");
        session.send(b"gr");
        session.wait_text("1 results");
        session.send(b"\r");
        session.wait_text("Captured old zzz-traits.rs");
        let (symbol_x, symbol_y) = locate(&session, occurrence);
        assert_eq!(
            cursor(&session),
            (symbol_y as i64, symbol_x as i64),
            "{symbol}\n{}",
            session.text()
        );
        session.send(b"gd");
        session.wait_text("1 results");
        session.send(b"\x1b");
        closed(&mut session);
    }
    assert!(!session.text().contains("Partial name/syntax"));
    session.send(b",q");
    session.finish(0);
    assert!(
        snapshot(&indexing_repo) == indexing_before,
        "indexing/navigation changed repository bytes"
    );
    assert!(
        ctx.state_snapshot() == index_state_before,
        "indexing/navigation implicitly changed marks"
    );
    pty_passed(
        "late unchanged sources, generic trait declarations/turbofish calls, local bindings, function references and exact read-only navigation",
    );
}

fn cursor_and_mouse(ctx: &Ctx) {
    // R13: installed cursor/mouse behavior, independently captured immutable inputs.
    let mouse_repo = ctx.root.join("cursor-mouse");
    std::fs::create_dir(&mouse_repo).unwrap();
    let mouse_git = |words: &[&str]| ctx.git_in(&mouse_repo, words);
    mouse_git(&["init", "-q"]);
    mouse_git(&["config", "user.name", "Cursor acceptance"]);
    mouse_git(&["config", "user.email", "cursor@example.invalid"]);
    for index in 0..15 {
        write(
            &mouse_repo.join(format!("a{index:02}.rs")),
            format!("\tfn old_symbol_{index:02}() {{}}\n") + &"\t// 界e\u{301}\\ old\n".repeat(45),
        );
    }
    mouse_git(&["add", "."]);
    mouse_git(&["commit", "-qm", "captured old sides"]);
    for index in 0..15 {
        write(
            &mouse_repo.join(format!("a{index:02}.rs")),
            format!("\tfn new_symbol_{index:02}() {{}}\n") + &"\t// 界e\u{301}\\ new\n".repeat(45),
        );
    }
    let mouse_before = snapshot(&mouse_repo);
    let state_before_mouse = ctx.state_snapshot();
    let mut session = ctx.session_in(&mouse_repo, &["--", "HEAD"]);
    closed(&mut session);
    // File wheel scrolling does not select: Enter still opens the initial file.
    session.resize(12, 140);
    session.send(&pointer(65, 2, 2, false));
    closed(&mut session);
    session.send(b"\r");
    session.wait_text("old_symbol_00");
    session.resize(30, 140);
    assert_same(
        &state_before_mouse,
        &ctx.state_snapshot(),
        "file wheel changed review marks",
    );
    // Select a visible file once: preview closes. Two same-file presses open it.
    let (x, y) = locate(&session, "a03.rs");
    session.send(&click(x, y));
    closed(&mut session);
    session.pump(0.5);
    session.send(&[click(x, y), click(x, y)].concat());
    session.wait_text("old_symbol_03");
    // Scroll to the added rows, then click the exact transformed/tabbed symbol.
    // New rows follow 46 removed rows; Vim counts reach them independently of pane height.
    session.send(b"gg60j");
    session.wait_text("new_symbol_03");
    let (x, y) = locate(&session, "new_symbol_03");
    session.send(&[click(x + 2, y), b"gd".to_vec()].concat());
    session.wait_text("Definitions candidates for \"new_symbol_03\"");
    session.wait_text("1 results");
    session.send(b"\r");
    session.wait_text("Captured new a03.rs");
    assert!(!session.text().contains("Partial name/syntax"));
    assert!(!session.text().contains("never binding"));
    let (symbol_x, symbol_y) = locate(&session, "new_symbol_03");
    assert!(
        session.color(symbol_y, symbol_x).is_some(),
        "jump target lost syntax highlighting"
    );
    let origin = cursor(&session);
    session.send(b"l");
    assert_eq!(
        cursor(&session),
        (origin.0, origin.1 + 1),
        "{origin:?} {:?}",
        cursor(&session)
    );
    session.send(b"h");
    assert_eq!(cursor(&session), origin);
    // Vim * searches this captured buffer, highlights matches, wraps and repeats.
    session.send(b"G$*");
    session.wait_text("/new · 1/45");
    let first_match = cursor(&session);
    session.send(b"n");
    session.wait_text("/new · 2/45");
    assert_eq!(cursor(&session), (first_match.0 + 1, first_match.1));
    session.send(b"N");
    assert_eq!(cursor(&session), first_match);
    session.send(b"gg0ww");
    assert_eq!(cursor(&session), origin);
    session.send(b"gd");
    session.wait_text("Definitions candidates for \"new_symbol_03\"");
    session.send(b"\x0f");
    session.wait_text("Captured new a03.rs");
    assert_eq!(cursor(&session), origin, "back did not restore the cursor");
    session.send(b"[gd");
    session.wait_text("Definitions candidates for \"old_symbol_03\"");
    session.send(b"\x0f");
    session.wait_text("Captured old a03.rs");
    // Wheel the preview while files hold focus, then move files to prove focus stayed.
    session.send(b"\t");
    session.send(&pointer(65, 80, 5, false));
    session.send(b"j");
    closed(&mut session);
    // Reviewed files have the same single/double click opening contract.
    session.send(b"m");
    session.wait_text("Review mark saved");
    let state_after_mark = ctx.state_snapshot();
    let (x, y) = locate(&session, "a04.rs");
    session.send(&click(x, y));
    closed(&mut session);
    session.pump(0.5);
    session.send(&[click(x, y), click(x, y)].concat());
    session.wait_text("Diff");
    session.send(b"a");
    session.wait_text("After changes");
    assert!(
        ctx.state_snapshot() == state_after_mark,
        "toggle changed an explicit saved mark"
    );
    session.send(b"a");
    session.wait_text("Diff");
    assert_same(
        &state_after_mark,
        &ctx.state_snapshot(),
        "toggle changed an explicit saved mark",
    );
    session.send(b"\x1b[6~\x1b[5~");
    session.send(b"\x1b");
    closed(&mut session);
    assert!(
        session.poll().is_none(),
        "Escape exited instead of closing the diff"
    );
    session.send(b"\x1b");
    assert!(session.poll().is_none(), "Escape exited from the file list");
    session.send(b"\r");
    session.wait_text("Diff");
    for (rows, cols) in [(1, 1), (3, 8), (30, 140)] {
        session.resize(rows, cols);
    }
    session.send(b",q");
    session.finish(0);
    assert!(
        snapshot(&mouse_repo) == mouse_before,
        "cursor/mouse review changed repository bytes"
    );
    assert!(
        ctx.state_snapshot() == state_after_mark,
        "cursor/mouse input changed marks"
    );
    pty_passed(
        "R13 exact cursor navigation, file clicks, wheels, history, side switching, resize and read-only state",
    );
}

fn diff_status(ctx: &Ctx) {
    // Status uses the displayed captured buffer and the actual content viewport.
    let status_repo = ctx.root.join("diff-status");
    std::fs::create_dir(&status_repo).unwrap();
    let status_git = |words: &[&str]| ctx.git_in(&status_repo, words);
    status_git(&["init", "-q"]);
    status_git(&["config", "user.name", "Status acceptance"]);
    status_git(&["config", "user.email", "status@example.invalid"]);
    write(&status_repo.join("status.rs"), "fn before() {}\n");
    status_git(&["add", "."]);
    status_git(&["commit", "-qm", "initial"]);
    let rows: String = (0..100)
        .map(|index| format!("// row {index:03} captured\n"))
        .collect();
    write(&status_repo.join("status.rs"), rows);
    let total = String::from_utf8_lossy(&status_git(&["diff", "HEAD", "--", "status.rs"]))
        .lines()
        .count();
    let status_before = snapshot(&status_repo);
    let marks_before = ctx.state_snapshot();
    let mut session = ctx.session_in(&status_repo, &["--", "HEAD"]);
    closed(&mut session);
    session.send(b"\r");
    let status_line = |session: &Session, rows: i64| {
        let cols = 140;
        let x = cols * 30 / 100 + 1;
        (x..cols - 1)
            .map(|col| session.cells.get(&(rows - 4, col)).copied().unwrap_or(' '))
            .collect::<String>()
            .trim_end()
            .to_owned()
    };
    assert_eq!(
        status_line(&session, 30),
        format!("{total} lines  1:1  Top"),
        "{}",
        session.text()
    );
    session.send(b"gg40j0");
    assert_eq!(
        status_line(&session, 30),
        format!("{total} lines  41:1  {}%", 16 * 100 / (total - 25)),
        "{}",
        session.text()
    );
    assert_eq!(cursor(&session), (25, 43));
    session.send(&pointer(65, 80, 5, false));
    assert_eq!(
        status_line(&session, 30),
        format!("{total} lines  41:1  {}%", 19 * 100 / (total - 25)),
        "{}",
        session.text()
    );
    assert_eq!(cursor(&session), (22, 43));
    // Neither clicks nor wheels on status may move the source cursor/viewport.
    let (saved_status, saved_cursor) = (status_line(&session, 30), cursor(&session));
    session.send(&[pointer(65, 80, 26, false), pointer(0, 80, 26, false)].concat());
    assert_eq!(status_line(&session, 30), saved_status);
    assert_eq!(cursor(&session), saved_cursor);
    session.send(b"\x1b[6~");
    assert!(
        status_line(&session, 30).contains('%'),
        "{}",
        session.text()
    );
    session.send(b"G0");
    assert_eq!(
        status_line(&session, 30),
        format!("{total} lines  {total}:1  Bot"),
        "{}",
        session.text()
    );
    assert_eq!(cursor(&session), (25, 43));
    session.resize(12, 140);
    assert_eq!(
        status_line(&session, 12),
        format!("{total} lines  {total}:1  Bot"),
        "{}",
        session.text()
    );
    assert_eq!(cursor(&session), (7, 43));
    session.resize(5, 8);
    assert!(
        !session.cursor_visible,
        "zero-content-height pane retained hardware cursor"
    );
    let first_digit = total.to_string().chars().next().unwrap();
    assert!(
        session.lines()[1].contains(first_digit),
        "{}",
        session.text()
    );
    for (rows, cols) in [(1, 1), (3, 8), (30, 140)] {
        session.resize(rows, cols);
    }
    assert!(session.cursor_visible);
    assert_eq!(
        status_line(&session, 30),
        format!("{total} lines  {total}:1  Bot"),
        "{}",
        session.text()
    );
    session.send(b"gg");
    session.resize(total as u16 + 5, 140);
    assert_eq!(
        status_line(&session, total as i64 + 5),
        format!("{total} lines  1:1  All"),
        "{}",
        session.text()
    );
    session.send(b",q");
    session.finish(0);
    assert!(
        snapshot(&status_repo) == status_before,
        "status navigation changed captured repository"
    );
    assert!(
        ctx.state_snapshot() == marks_before,
        "status navigation changed review marks"
    );
    pty_passed(
        "diff status top/middle/bottom/fit, cursor cells, wheel/page, status mouse exclusion and tiny resize",
    );
}

fn file_statistics(ctx: &Ctx) {
    // Per-file statistics come from captured hunks, including mixed graphs,
    // binary evidence, metadata zeros and omitted-content uncertainty.
    let stats_repo = ctx.root.join("file-statistics");
    std::fs::create_dir(&stats_repo).unwrap();
    let stats_git = |words: &[&str]| ctx.git_in(&stats_repo, words);
    stats_git(&["init", "-q"]);
    stats_git(&["config", "user.name", "Statistics acceptance"]);
    stats_git(&["config", "user.email", "statistics@example.invalid"]);
    write(&stats_repo.join("a.txt"), "old\nkeep\n");
    write(&stats_repo.join("binary.dat"), b"old\0binary");
    write(&stats_repo.join("mode.txt"), "same\n");
    stats_git(&["add", "."]);
    stats_git(&["commit", "-qm", "base"]);
    write(&stats_repo.join("a.txt"), "new\nextra\nkeep\n");
    write(&stats_repo.join("binary.dat"), b"new\0binary");
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            stats_repo.join("mode.txt"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let oracle = String::from_utf8(stats_git(&["diff", "--numstat", "HEAD"])).unwrap();
    assert!(
        oracle.contains("2\t1\ta.txt") && oracle.contains("-\t-\tbinary.dat"),
        "{oracle}"
    );
    let stats_before = snapshot(&stats_repo);
    let marks_before = ctx.state_snapshot();
    let mut session = ctx.session_in(&stats_repo, &["--", "HEAD"]);
    closed(&mut session);
    session.wait_text("3 ++-");
    session.wait_text("binary");
    // The whole-diff total sums the numstat oracle: +2 -1 and one binary.
    session.wait_text("Σ +2 −1 · 1 binary");
    let stats_row = |session: &Session, name: &str| find_line(session, name).1;
    assert!(stats_row(&session, "binary.dat").contains("binary"));
    // Python: re.search(r"mode.txt +0", row).
    let mode_row = stats_row(&session, "mode.txt");
    assert!(
        mode_row.match_indices("mode.txt").any(|(at, name)| {
            let after = &mode_row[at + name.len()..];
            let digits = after.trim_start_matches(' ');
            digits.len() < after.len() && digits.starts_with('0')
        }),
        "{}",
        session.text()
    );
    // An open diff hides per-file statistics and the totals line.
    session.send(b"\r");
    session.wait_text("+extra");
    let end = Instant::now() + Duration::from_secs(10);
    while session.text().contains("3 ++-") || session.text().contains('Σ') {
        session.pump(0.1);
        assert!(Instant::now() < end, "{}", session.text());
    }
    session.send(b"\x1b");
    session.wait_text("Σ +2 −1 · 1 binary");
    session.wait_text("3 ++-");
    session.send(b"jk");
    for (rows, cols) in [(30, 28), (3, 8), (1, 1), (30, 140)] {
        session.resize(rows, cols);
    }
    session.wait_text("3 ++-");
    quit_unchanged(&mut session, &stats_before, &stats_repo, 0);
    assert!(
        ctx.state_snapshot() == marks_before,
        "statistics navigation changed marks"
    );
    write(&stats_repo.join("mode.txt"), "same   \n");
    let stats_before = snapshot(&stats_repo);
    let mut session = ctx.session_in(&stats_repo, &["--", "-w", "HEAD", "--", "mode.txt"]);
    closed(&mut session);
    session.wait_text("unavailable");
    quit_unchanged(&mut session, &stats_before, &stats_repo, 0);
    pty_passed("captured file statistics, binary/metadata/unavailable, open pane and tiny resize");
}

fn marks_invalidation_and_compatibility(ctx: &Ctx, acceptance: &Path, left: &str, right: &str) {
    let setup_git = |words: &[&str]| ctx.git_in(acceptance, words);
    let start = |words: &[&str], cwd: &Path| {
        let args: Vec<&str> = std::iter::once("--").chain(words.iter().copied()).collect();
        ctx.session_in(cwd, &args)
    };
    // R2-R10: exact ticket invocation, explicit marks, closed previews and gray
    // names after restart, selective invalidation, ref movement and isolation.
    let branch_args = [left, right];
    let mut baseline = snapshot(acceptance);
    let state_before = ctx.state_snapshot();
    let mut session = start(&branch_args, acceptance);
    marks(&mut session, &[("one.rs", false), ("two.rs", false)]);
    session.send(b"j");
    closed(&mut session);
    session.send(b"k ");
    session.wait_text("let value = 2;");
    session.wait_text("Files");
    assert!(
        !session.text().contains("+ one.rs"),
        "opening a diff implicitly marked it"
    );
    assert!(
        ctx.state_snapshot() == state_before,
        "viewing wrote review state"
    );
    session.send(b"\x1b");
    session.send(b"m");
    session.wait_text("Review mark saved");
    session.send(b"jm");
    session.wait_text("+ two.rs");
    quit_unchanged(&mut session, &baseline, acceptance, 0);
    let state_files: Vec<PathBuf> = std::fs::read_dir(ctx.state.join("review"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    assert!(state_files.iter().any(|path| {
        let value: serde_json::Value = serde_json::from_str(&read(path)).unwrap();
        let marks = &value["marks"];
        marks
            .as_array()
            .map(Vec::len)
            .or_else(|| marks.as_object().map(serde_json::Map::len))
            == Some(2)
    }));

    let mut session = start(&branch_args, acceptance);
    marks(&mut session, &[("one.rs", true), ("two.rs", true)]);
    session.send(b"m");
    session.wait_text("- one.rs");
    quit_unchanged(&mut session, &baseline, acceptance, 0);
    let mut session = start(&branch_args, acceptance);
    marks(&mut session, &[("one.rs", false), ("two.rs", true)]);
    closed(&mut session);
    session.send(b"m");
    session.wait_text("+ one.rs");
    session.send(b"\r");
    session.wait_text("let value = 2;");
    quit_unchanged(&mut session, &baseline, acceptance, 0);
    let mut session = start(&[left, right, "--", "one.rs"], acceptance);
    marks(&mut session, &[("one.rs", false)]);
    assert!(!session.text().contains("two.rs"));
    quit_unchanged(&mut session, &baseline, acceptance, 0);

    write(&acceptance.join("one.rs"), "fn one() { let value = 3; }\n");
    setup_git(&["add", "."]);
    setup_git(&["commit", "-qm", "only one changed"]);
    setup_git(&["branch", "-f", right]);
    baseline = snapshot(acceptance);
    let mut session = start(&branch_args, acceptance);
    marks(&mut session, &[("one.rs", false), ("two.rs", true)]);
    session.send(b"m");
    session.wait_text("Review mark saved");
    quit_unchanged(&mut session, &baseline, acceptance, 0);
    setup_git(&["commit", "--allow-empty", "-qm", "commit identity only"]);
    setup_git(&["branch", "-f", right]);
    baseline = snapshot(acceptance);
    let mut session = start(&branch_args, acceptance);
    marks(&mut session, &[("one.rs", true), ("two.rs", true)]);
    quit_unchanged(&mut session, &baseline, acceptance, 0);

    // R1/R9/R12: native Git is the oracle for every short displayed patch or
    // original-output line; process status is compared independently.
    write(&acceptance.join("one.rs"), "fn one() { let value = 4; }\n");
    setup_git(&["add", "one.rs"]);
    write(&acceptance.join("two.rs"), "fn two() { let value = 5; }\n");
    let two_dots = format!("{left}..{right}");
    let three_dots = format!("{left}...{right}");
    let left_blob = format!("{left}:one.rs");
    let right_blob = format!("{right}:one.rs");
    let cases: Vec<(Vec<&str>, bool)> = vec![
        (vec!["--", "two.rs"], false),
        (vec!["HEAD", "--", "two.rs"], false),
        (vec!["--cached", "--", "one.rs"], false),
        (vec![left, right, "--", "one.rs"], false),
        (vec![&two_dots, "--", "one.rs"], false),
        (vec![&three_dots, "--", "one.rs"], false),
        (vec!["--merge-base", left, right, "--", "one.rs"], false),
        (vec![&left_blob, &right_blob], false),
        (vec!["--exit-code", "HEAD", "--", "one.rs"], false),
        (vec!["--stat", "HEAD"], true),
        (vec!["--word-diff=porcelain", "HEAD", "--", "one.rs"], true),
        (
            vec!["--output-indicator-new=>", "HEAD", "--", "one.rs"],
            true,
        ),
        (vec!["--no-patch", "HEAD"], true),
        (vec!["--quiet", "HEAD"], true),
    ];

    let check_native = |words: &[&str], original: bool, cwd: &Path| {
        let before = snapshot(cwd);
        let mut command = Command::new(&ctx.git);
        command
            .arg("diff")
            .args(words)
            .current_dir(cwd)
            .stdin(Stdio::null());
        ctx.env.apply(&mut command);
        let oracle = command.output().unwrap();
        assert!(
            snapshot(cwd) == before,
            "oracle unexpectedly mutated fixture"
        );
        let mut session = start(words, cwd);
        session.wait_text(if original {
            "Original Git output"
        } else {
            "Files"
        });
        if !original {
            session.send(b"\r");
        }
        for line in String::from_utf8(oracle.stdout).unwrap().lines() {
            if !line.trim().is_empty() {
                session.wait_text(line.trim());
            }
        }
        if original {
            let saved_before = ctx.state_snapshot();
            session.send(b"m");
            session.wait_text("Marks are unavailable");
            session.send(b"/");
            session.wait_text("Navigation unavailable");
            assert!(
                !session.text().contains("Search/outline:"),
                "{}",
                session.text()
            );
            assert!(
                ctx.state_snapshot() == saved_before,
                "opaque output wrote review marks"
            );
        }
        let code = oracle.status.code().expect("git diff killed by a signal");
        quit_unchanged(&mut session, &before, cwd, code);
    };

    for (words, original) in &cases {
        check_native(words, *original, acceptance);
    }

    // Blob/no-index status and read-only filesystem behavior outside a repo.
    let outside = ctx.root.join("outside");
    std::fs::create_dir(&outside).unwrap();
    write(&outside.join("old.rs"), "fn outside() { let value = 1; }\n");
    write(&outside.join("new.rs"), "fn outside() { let value = 2; }\n");
    let no_index = ["--no-index", "old.rs", "new.rs"];
    check_native(&no_index, false, &outside);
    for (iteration, changed) in [false, false, true].into_iter().enumerate() {
        if changed {
            write(&outside.join("new.rs"), "fn outside() { let value = 3; }\n");
        }
        let before = snapshot(&outside);
        let mut session = start(&no_index, &outside);
        closed(&mut session);
        session.wait_text(if changed || iteration == 0 {
            "- "
        } else {
            "+ "
        });
        if changed || iteration == 0 {
            session.send(b"m");
            session.wait_text("Review mark saved");
        }
        quit_unchanged(&mut session, &before, &outside, 1);
    }

    // Staged/worktree mark invalidation uses the installed UI too.
    for (words, name) in [
        (&["--cached"][..], "one.rs"),
        (&["--", "two.rs"][..], "two.rs"),
    ] {
        let before = snapshot(acceptance);
        let mut session = start(words, acceptance);
        session.wait_text(&format!("- {name}"));
        session.send(b"m");
        session.wait_text("Review mark saved");
        quit_unchanged(&mut session, &before, acceptance, 0);
        let mut session = start(words, acceptance);
        marks(&mut session, &[(name, true)]);
        quit_unchanged(&mut session, &before, acceptance, 0);
        write(&acceptance.join(name), "fn changed_again() {}\n");
        if words.contains(&"--cached") {
            setup_git(&["add", name]);
        }
        let before = snapshot(acceptance);
        let mut session = start(words, acceptance);
        marks(&mut session, &[(name, false)]);
        quit_unchanged(&mut session, &before, acceptance, 0);
    }

    // A genuine conflict produces combined output; preserve it without inventing
    // ordinary file marks or navigation, and retain Git's status.
    setup_git(&["reset", "--hard", "-q", "HEAD"]);
    setup_git(&["checkout", "-qb", "conflicting", left]);
    write(&acceptance.join("one.rs"), "fn conflict() {}\n");
    setup_git(&["add", "."]);
    setup_git(&["commit", "-qm", "conflicting side"]);
    let mut merge = Command::new(&ctx.git);
    merge
        .args(["merge", "--no-edit", right])
        .current_dir(acceptance)
        .stdin(Stdio::null());
    ctx.env.apply(&mut merge);
    let merge = merge.output().unwrap();
    assert_eq!(
        merge.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&merge.stdout)
    );
    check_native(&["--cc"], true, acceptance);
}

fn comments(ctx: &Ctx) {
    // Story 18: persistent review comments in the launch directory.
    let comments_repo = ctx.root.join("comments");
    std::fs::create_dir(&comments_repo).unwrap();
    let comments_git = |words: &[&str]| ctx.git_in(&comments_repo, words);
    comments_git(&["init", "-q"]);
    comments_git(&["config", "user.name", "Comments acceptance"]);
    comments_git(&["config", "user.email", "comments@example.invalid"]);
    for name in ["one.rs", "two.rs"] {
        write(
            &comments_repo.join(name),
            format!(
                "fn {}() {{\n    let answer = 1;\n}}\n",
                &name[..name.len() - 3]
            ),
        );
    }
    comments_git(&["add", "."]);
    comments_git(&["commit", "-qm", "base"]);
    for name in ["one.rs", "two.rs"] {
        write(
            &comments_repo.join(name),
            format!(
                "fn {}() {{\n    let answer = 100;\n}}\n",
                &name[..name.len() - 3]
            ),
        );
    }
    let comments_file = comments_repo.join("review-HEAD.comments");
    let mut comments_before = snapshot(&comments_repo);
    let comments_state = ctx.state_snapshot();
    let comment_session = |options: &[&str]| {
        let args: Vec<&str> = options.iter().copied().chain(["--", "HEAD"]).collect();
        ctx.session_in(&comments_repo, &args)
    };
    let save_comment = |session: &mut Session, title: &str, text: &str| {
        session.wait_text(&format!("Comment · {title}"));
        session.send(text.as_bytes());
        session.send(b"\r");
        session.wait_text("Comment saved");
        assert!(!session.text().contains("Comment · "), "{}", session.text());
    };
    let icons = |session: &Session| -> Vec<(usize, Color)> {
        session
            .lines()
            .iter()
            .enumerate()
            .filter_map(|(row, line)| {
                char_index(line, "💬").map(|col| (row, session.color(row, col)))
            })
            .collect()
    };
    // one.rs rows: 0 diff --git .. 4 @@, 5 context, 6 removed, 7 added, 8 context.
    let mut session = comment_session(&[]);
    session.wait_text("Files");
    session.send(b"\r");
    session.wait_text("let answer = 100;");
    session.send(b"jjjjjjj");
    session.send(b"c");
    save_comment(&mut session, "one.rs:2 (new)", "Line note");
    assert_eq!(icons(&session).len(), 1, "{}", session.text());
    assert!(comments_file.exists());
    session.send(b"gg");
    session.send(b"jjjjj");
    session.send(b"v");
    session.send(b"j");
    session.send(b"c");
    save_comment(&mut session, "one.rs:old 2, new 1 (mixed)", "Range note");
    session.send(b"gg");
    session.send(b"c");
    save_comment(&mut session, "one.rs (file)", "File note");
    assert_eq!(icons(&session).len(), 7, "{:?}", icons(&session));
    // A range overlapping an existing comment is refused; editing prefills.
    session.send(b"jjjjjv");
    session.send(b"jjc");
    session.wait_text("overlaps an existing comment");
    session.send(b"c");
    session.wait_text("Comment · one.rs:2 (new)");
    session.wait_text("Line note");
    // Alt+Enter (ESC CR) and Ctrl-J (LF) insert newlines; Ctrl-S is a save alias.
    session.send(b" edited\x1b\rsecond\nthird");
    session.wait_text("third");
    assert!(
        session.text().contains("Comment · one.rs:2 (new)"),
        "{}",
        session.text()
    );
    session.send(b"\x13");
    session.wait_text("Comment saved");
    session.send(b"c");
    session.wait_text("Comment · one.rs:2 (new)");
    session.send(b"\x1b");
    session.wait_text("Comment edit cancelled");
    assert!(
        session.text().contains("┌Diff"),
        "Escape in the editor closed the diff"
    );
    session.send(b",q");
    session.finish(0);
    assert!(
        !contains(&session.output, b"\x1b[>"),
        "keyboard enhancement pushed without terminal support"
    );
    let text = read(&comments_file);
    assert!(text.starts_with("---\nreview-comments: v1\n"), "{text}");
    for heading in [
        "## one.rs (file)",
        "## one.rs:old 2, new 1 (mixed)",
        "## one.rs:2 (new)",
    ] {
        assert!(
            text.contains(&format!("\n{heading}\n")),
            "{heading}\n{text}"
        );
    }
    assert!(
        text.contains("Line note edited\nsecond\nthird\n")
            && text.contains("comments: 3\noutdated: 0\n"),
        "{text}"
    );
    assert!(text.contains("command: \"git diff HEAD\""), "{text}");
    // Emacs controls in a restarted session reuse the same file.
    let mut session = comment_session(&["--keymap", "emacs"]);
    session.wait_text("Files");
    session.send(b"\x0e");
    session.send(b"\r");
    session.wait_text("fn two()");
    session.send(&b"\x0e".repeat(6));
    session.send(b"\x00");
    session.send(b"\x0e");
    session.send(b"c");
    session.wait_text("Comment · two.rs:old 2, new 2 (mixed)");
    session.send(b"discard");
    session.send(b"\x07");
    session.wait_text("Comment edit cancelled");
    session.send(b"\x00");
    session.send(b"\x10");
    session.send(b"\x07");
    session.wait_text("Selection cancelled");
    session.send(b"\x00\x0ec");
    save_comment(&mut session, "two.rs:old 2, new 2 (mixed)", "Emacs note");
    session.send(b",q");
    session.finish(0);
    assert!(read(&comments_file).contains("comments: 4\n"));
    // Restart restores icons without changing marks.
    let mut session = comment_session(&[]);
    session.wait_text("4 comment(s) loaded");
    session.send(b"\r");
    session.wait_text("let answer = 100;");
    assert_eq!(icons(&session).len(), 7, "{:?}", icons(&session));
    session.send(b",q");
    session.finish(0);
    // Changed code: the line comment becomes outdated and its icon gray.
    write(
        &comments_repo.join("one.rs"),
        "fn one() {\n    let answer = 101;\n}\n",
    );
    let mut session = comment_session(&[]);
    session.wait_text("4 comment(s) loaded, 1 outdated");
    session.send(b"\r");
    session.wait_text("let answer = 101;");
    let found = icons(&session);
    assert_eq!(found.len(), 7, "{found:?}");
    let current: std::collections::HashSet<Color> = found[..found.len() - 1]
        .iter()
        .map(|&(_, color)| color)
        .collect();
    assert!(
        current.len() == 1 && !current.contains(&found[found.len() - 1].1),
        "{found:?}"
    );
    assert!(read(&comments_file).contains("## one.rs:2 (new) [outdated]"));
    // Deleting every comment removes the owned file.
    session.send(b"ggdc");
    session.wait_text("Comment deleted");
    session.send(b"jjjjjdc");
    session.send(b"jjdc");
    session.send(b"\x1b");
    session.send(b"j");
    session.send(b"\r");
    session.wait_text("fn two()");
    session.send(b"jjjjjjdc");
    session.wait_text("comments file removed");
    assert!(!comments_file.exists());
    session.send(b",q");
    session.finish(0);
    // A kitty-protocol terminal gets disambiguated keys: Ctrl+Enter and
    // Shift+Enter insert newlines, CSI-u Escape still closes, and the flags
    // are popped on exit.
    let mut session = Session::new(
        &ctx.binary,
        &["--", "HEAD"],
        &comments_repo,
        &ctx.env,
        None,
        true,
    );
    session.wait_text("Files");
    assert!(
        contains(&session.output, b"\x1b[>5u"),
        "{:?}",
        tail(&session.output, 300)
    );
    session.send(b"\r");
    session.wait_text("let answer = 101;");
    session.send(b"jjjjjjjc");
    session.wait_text("Comment · one.rs:2 (new)");
    session.send(b"kitty\x1b[13;5uctrl\x1b[13;2ushift");
    session.wait_text("shift");
    session.send(b"\r");
    session.wait_text("Comment saved");
    assert!(read(&comments_file).contains("kitty\nctrl\nshift\n"));
    session.send(b"dc");
    session.wait_text("comments file removed");
    session.send(b"\x1b[27u");
    session.wait_text("Files");
    session.pump(0.3);
    assert!(!session.text().contains("┌Diff"), "{}", session.text());
    session.send(b",q");
    session.finish(0);
    assert!(
        contains(&session.output, b"\x1b[<1u"),
        "keyboard enhancement flags not popped"
    );
    assert!(!comments_file.exists());
    let mut after = snapshot(&comments_repo);
    assert!(after.remove("one.rs") != comments_before.remove("one.rs"));
    assert!(
        after == comments_before,
        "comments changed repository bytes beyond the owned file"
    );
    assert!(
        ctx.state_snapshot() == comments_state,
        "comments changed review marks"
    );
    pty_passed(
        "line/range/header comments in Vim and Emacs keymaps, Enter save, Ctrl/Shift/Alt+Enter and Ctrl-J newlines, kitty keyboard push/pop, icons, edit, restart, outdated detection and file removal",
    );
}

/// A failed assertion inside a session must not leave its child running.
#[test]
fn session_kills_child_on_panic() {
    let pid = std::cell::Cell::new(0);
    let started = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let session = Session::new(
            &which("sleep"),
            &["60"],
            &std::env::temp_dir(),
            &TestEnv { vars: Vec::new() },
            None,
            false,
        );
        pid.set(session.id());
        panic!("injected assertion failure");
    }));
    assert!(result.is_err());
    // Without the kill, Drop would wait for `sleep 60` to exit on its own.
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "Drop did not kill the child: unwinding took {:?}",
        started.elapsed()
    );
    let alive = unsafe { libc::kill(pid.get() as libc::pid_t, 0) } == 0;
    assert!(!alive, "child {} survived the panic", pid.get());
}

use super::*;
use app::{App, Focus};
use crossterm::event::KeyCode;
use ratatui::{
    Terminal,
    backend::TestBackend,
    style::{Color, Modifier},
};
use std::{path::Path, process::Command};

fn git_run(cwd: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn fixture() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    git_run(directory.path(), &["init", "-q"]);
    git_run(directory.path(), &["config", "user.name", "Review Test"]);
    git_run(
        directory.path(),
        &["config", "user.email", "review@example.invalid"],
    );
    std::fs::write(
        directory.path().join("one.rs"),
        "fn one() {\n\tlet answer = 1;\n}\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("two.rs"),
        "fn two() {\n\tlet answer = 2;\n}\n",
    )
    .unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "initial"]);
    directory
}

fn change(directory: &Path) {
    std::fs::write(
        directory.join("one.rs"),
        "fn one() {\n\tlet answer = 100;\n}\n",
    )
    .unwrap();
    std::fs::write(
        directory.join("two.rs"),
        "fn two() {\n\tlet answer = 200;\n}\n",
    )
    .unwrap();
}

fn capture(directory: &Path, args: &[&str]) -> git::Capture {
    git::capture(
        directory,
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
    )
    .unwrap()
}

fn application(directory: &Path, base: &Path) -> App {
    App::new(
        capture(directory, &["HEAD"]),
        state::Store::at(base, directory, &["HEAD".into()]),
    )
}

fn screen(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| ui::draw(frame, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn screen_text(buffer: &ratatui::buffer::Buffer) -> String {
    buffer
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
}

#[test]
fn outer_and_inner_argument_boundaries_are_native() {
    let args = ["--", "HEAD", "--", "a file", "--stat"].map(OsString::from);
    assert_eq!(
        parse_args(args).unwrap(),
        Cli::Review(
            ["HEAD", "--", "a file", "--stat"]
                .map(OsString::from)
                .to_vec()
        )
    );
    assert!(parse_args(["HEAD".into()]).is_err());
    assert_eq!(parse_args(["--help".into()]).unwrap(), Cli::Help);
}

#[test]
fn native_argument_bytes_and_length_delimiters_do_not_alias() {
    use std::os::unix::ffi::OsStringExt;
    let raw = OsString::from_vec(vec![b'p', 0xff]);
    assert_eq!(
        parse_args(["--".into(), raw.clone()]).unwrap(),
        Cli::Review(vec![raw.clone()])
    );
    let one = state::Store::at(Path::new("/tmp"), Path::new("/repository"), &[raw]);
    let two = state::Store::at(Path::new("/tmp"), Path::new("/repository"), &["p�".into()]);
    assert_ne!(one.path, two.path);
    assert_ne!(
        state::digest([b"a".as_slice(), b"bc"]),
        state::digest([b"ab".as_slice(), b"c"])
    );
}

#[test]
fn navigation_preview_focus_mark_and_restart() {
    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    change(directory.path());
    let mut app = application(directory.path(), state.path());
    assert_eq!(app.capture.items.len(), 2, "{:?}", app.capture.unavailable);
    assert!(app.preview.is_none());
    assert!(!screen_text(&screen(&mut app, 100, 24)).contains("@@"));
    app.key(KeyCode::Down);
    assert!(app.preview.is_none());
    app.key(KeyCode::Up);
    app.key(KeyCode::Enter);
    assert_eq!(app.focus, Focus::Preview);
    let rendered = screen(&mut app, 100, 24);
    assert!(screen_text(&rendered).contains("Files"));
    assert!(screen_text(&rendered).contains("Diff"));
    app.key(KeyCode::Tab);
    assert_eq!(app.focus, Focus::Files);
    app.key(KeyCode::Down);
    assert!(app.preview.is_none());
    app.key(KeyCode::Char(' '));
    assert_eq!(app.marks, [false, true]);
    let mut restarted = application(directory.path(), state.path());
    assert_eq!(restarted.marks, [false, true]);
    assert!(restarted.preview.is_none());
    assert!(
        screen(&mut restarted, 100, 24)
            .content
            .iter()
            .any(|cell| cell.symbol() == "t" && cell.fg == Color::DarkGray)
    );
}

#[test]
fn mapped_source_and_styling_preserve_the_captured_lines() {
    let directory = fixture();
    change(directory.path());
    let captured = capture(directory.path(), &["HEAD"]);
    let item = &captured.items[0];
    let sources = source::load(directory.path(), item).unwrap();
    assert!(sources.old.content.contains("answer = 1;"));
    assert!(sources.new.content.contains("answer = 100;"));
    // Mutating the checkout after capture cannot change compared source.
    std::fs::write(
        directory.path().join("one.rs"),
        "invalid replacement checkout\n",
    )
    .unwrap();
    assert_eq!(
        source::load(directory.path(), item).unwrap().new.content,
        sources.new.content
    );
    let preview = preview::Preview::item(directory.path(), item);
    assert_eq!(preview.reason, None);
    let plain: Vec<_> = preview.lines.iter().map(ToString::to_string).collect();
    let expected: Vec<_> = std::str::from_utf8(&item.patch)
        .unwrap()
        .lines()
        .map(|line| line.replace('\t', "    "))
        .collect();
    assert_eq!(plain, expected);
    assert!(
        preview
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.style.fg == Some(Color::Magenta))
    );
    assert!(
        preview
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.style.add_modifier.contains(Modifier::UNDERLINED))
    );
}

#[test]
fn global_selection_is_not_recomputed_per_path() {
    let directory = fixture();
    std::fs::write(
        directory.path().join("one.rs"),
        "fn one() { let needle = 100; }\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("two.rs"),
        "fn two() { let other = 200; }\n",
    )
    .unwrap();
    let captured = capture(directory.path(), &["-Sneedle", "--pickaxe-all", "HEAD"]);
    assert_eq!(captured.items.len(), 2);
    assert!(
        captured.items[1]
            .patch
            .windows(5)
            .any(|window| window == b"other")
    );
}

#[test]
fn original_output_is_faithful_and_marks_are_disabled() {
    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    change(directory.path());
    for options in [
        &["--stat", "HEAD"][..],
        &["--word-diff", "HEAD"],
        &["--no-patch", "HEAD"],
        &["--name-only", "HEAD"],
    ] {
        let expected = git_run(directory.path(), &[&["diff"][..], options].concat());
        let captured = capture(directory.path(), options);
        assert!(captured.unavailable.is_some());
        assert_eq!(captured.original, expected);
        let mut app = App::new(
            captured,
            state::Store::at(state.path(), directory.path(), &[]),
        );
        app.key(KeyCode::Char(' '));
        assert!(app.marks.is_empty());
        assert!(app.status.contains("unavailable"));
        assert!(!app.store.path.exists());
    }
}

#[test]
fn binary_unproven_structure_and_unsupported_source_stay_readable() {
    let directory = fixture();
    std::fs::write(directory.path().join("one.rs"), b"binary\0data").unwrap();
    let captured = capture(directory.path(), &["HEAD"]);
    assert!(captured.unavailable.is_none());
    assert_eq!(captured.items.len(), 1);
    assert!(source::load(directory.path(), &captured.items[0]).is_err());
    assert_eq!(
        captured.original,
        git_run(directory.path(), &["diff", "HEAD"])
    );
    std::fs::write(
        directory.path().join("one.rs"),
        b"fn one() {\n\tlet answer = 9;\n}",
    )
    .unwrap();
    let captured = capture(directory.path(), &["HEAD"]);
    assert_eq!(captured.items.len(), 1);
    let preview = preview::Preview::item(directory.path(), &captured.items[0]);
    assert!(preview.reason.unwrap().contains("final newlines"));
    assert!(!preview.lines.is_empty());
}

#[test]
fn empty_comparison_controls_are_safe() {
    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    assert!(app.capture.items.is_empty());
    assert!(app.capture.unavailable.is_none());
    for key in [
        KeyCode::Enter,
        KeyCode::Tab,
        KeyCode::Down,
        KeyCode::Up,
        KeyCode::Left,
        KeyCode::Right,
        KeyCode::Char(' '),
    ] {
        app.key(key);
    }
    assert!(screen_text(&screen(&mut app, 60, 12)).contains("no changes"));
    for size in [(1, 1), (10, 3), (0, 0)] {
        screen(&mut app, size.0, size.1);
    }
}

#[test]
fn save_failure_preserves_visible_marks_and_prior_state() {
    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    change(directory.path());
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Char(' '));
    assert!(app.marks[0]);
    let prior_path = app.store.path.clone();
    let prior = std::fs::read(&prior_path).unwrap();
    // A regular file as a parent fails regardless of test-user permissions.
    let obstacle = state.path().join("regular-file");
    std::fs::write(&obstacle, b"occupied").unwrap();
    app.store.path = obstacle.join("state.json");
    app.key(KeyCode::Char(' '));
    assert!(app.marks[0]);
    assert!(app.status.contains("not saved"));
    assert_eq!(std::fs::read(prior_path).unwrap(), prior);
}

#[test]
fn corrupt_state_is_diagnosed_and_preserved() {
    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    change(directory.path());
    let store = state::Store::at(state.path(), directory.path(), &["HEAD".into()]);
    std::fs::create_dir_all(store.path.parent().unwrap()).unwrap();
    std::fs::write(&store.path, b"invalid json").unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Char(' '));
    assert_eq!(app.marks, [false, false]);
    assert!(app.status.contains("corrupt"));
    assert_eq!(std::fs::read(store.path).unwrap(), b"invalid json");
}

#[test]
fn staged_refs_paths_git_diagnostics_and_read_only_capture() {
    let directory = fixture();
    change(directory.path());
    git_run(directory.path(), &["add", "."]);
    let index_before = std::fs::read(directory.path().join(".git/index")).unwrap();
    let config_before = std::fs::read(directory.path().join(".git/config")).unwrap();
    assert_eq!(capture(directory.path(), &["--cached"]).items.len(), 2);
    assert_eq!(
        capture(directory.path(), &["HEAD", "--", "one.rs"])
            .items
            .len(),
        1
    );
    git_run(directory.path(), &["commit", "-qm", "changed"]);
    assert_eq!(capture(directory.path(), &["HEAD~", "HEAD"]).items.len(), 2);
    let args = vec![OsString::from("unknown-ref")];
    let failed = git::capture(directory.path(), &args).unwrap();
    assert_eq!(failed.status.code(), Some(128));
    assert!(failed.diagnostic.contains("unknown revision"));
    assert!(!failed.stderr.is_empty());
    assert!(failed.unavailable.is_some());
    assert!(git::capture(directory.path(), &["--output=unwanted".into()]).is_err());
    assert!(!directory.path().join("unwanted").exists());
    assert_eq!(
        std::fs::read(directory.path().join(".git/config")).unwrap(),
        config_before
    );
    // Check a second capture against a stable index (commit itself changes it).
    let stable = std::fs::read(directory.path().join(".git/index")).unwrap();
    capture(directory.path(), &["HEAD~", "HEAD"]);
    assert_eq!(
        std::fs::read(directory.path().join(".git/index")).unwrap(),
        stable
    );
    assert!(!index_before.is_empty());
}

#[test]
fn pathname_bytes_and_nested_separator_select_exactly() {
    use std::os::unix::ffi::OsStringExt;
    let directory = fixture();
    let names = [
        OsString::from("a space.rs"),
        OsString::from("a\tline\n.rs"),
        OsString::from_vec(b"bad-\xff.rs".to_vec()),
        OsString::from("--stat"),
    ];
    for name in &names {
        std::fs::write(directory.path().join(name), "fn sample() { let x = 1; }\n").unwrap();
    }
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "paths"]);
    for name in &names {
        std::fs::write(directory.path().join(name), "fn sample() { let x = 2; }\n").unwrap();
        let captured = git::capture(
            directory.path(),
            &["HEAD".into(), "--".into(), name.clone()],
        )
        .unwrap();
        assert_eq!(
            captured.items.len(),
            1,
            "{name:?}: {:?}",
            captured.unavailable
        );
        assert_eq!(captured.items[0].new_path, name.as_encoded_bytes());
    }
}

#[test]
fn unicode_tabs_clipping_and_scroll_bounds_use_screen_coordinates() {
    let preview = preview::Preview::original("a\t界é🙂z\nsecond\n".as_bytes());
    assert_eq!(preview.lines[0].to_string(), "a   界é🙂z");
    let visible = preview.visible(0, 5, 1, 4);
    assert_eq!(visible[0].to_string(), " é🙂");
    assert_eq!(visible[0].width(), 4);
    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    change(directory.path());
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Enter);
    screen(&mut app, 40, 10);
    for _ in 0..1000 {
        app.key(KeyCode::Down);
        app.key(KeyCode::Right);
    }
    let preview = app.preview.as_ref().unwrap();
    assert_eq!(app.top, preview.lines.len().saturating_sub(app.viewport.0));
    assert_eq!(app.left, preview.width.saturating_sub(app.viewport.1));
}

#[test]
fn added_deleted_and_multiple_hunks_have_verified_sources() {
    let directory = fixture();
    std::fs::write(directory.path().join("added.rs"), "fn added() {}\n").unwrap();
    std::fs::remove_file(directory.path().join("one.rs")).unwrap();
    git_run(directory.path(), &["add", "."]);
    let captured = capture(directory.path(), &["--cached"]);
    assert_eq!(captured.items.len(), 2);
    for item in &captured.items {
        source::load(directory.path(), item).unwrap();
    }
    let original = (0..30)
        .map(|n| format!("let x{n} = {n};\n"))
        .collect::<String>();
    std::fs::write(directory.path().join("long.rs"), &original).unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "long"]);
    let changed = original
        .replace("x2 = 2", "x2 = 200")
        .replace("x26 = 26", "x26 = 2600");
    std::fs::write(directory.path().join("long.rs"), &changed).unwrap();
    let captured = capture(directory.path(), &["HEAD"]);
    assert_eq!(
        source::load(directory.path(), &captured.items[0])
            .unwrap()
            .new
            .content,
        changed
    );
}

#[test]
fn external_helpers_are_diagnosed_without_running() {
    let directory = fixture();
    change(directory.path());
    for option in ["--ext-diff", "--ext-di", "--textconv", "--textcon"] {
        assert!(git::capture(directory.path(), &[option.into()]).is_err());
    }
    git_run(
        directory.path(),
        &["config", "filter.test.clean", "touch helper-ran"],
    );
    assert!(git::capture(directory.path(), &["HEAD".into()]).is_ok());
    std::fs::write(
        directory.path().join(".gitattributes"),
        "one.rs filter=test\n",
    )
    .unwrap();
    assert!(
        git::capture(directory.path(), &["HEAD".into()])
            .unwrap_err()
            .to_string()
            .contains("clean/process filters")
    );
    assert!(!directory.path().join("helper-ran").exists());
}

#[test]
fn nominal_changed_patch_clears_only_its_mark_and_sessions_are_separate() {
    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    change(directory.path());
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Char(' '));
    app.key(KeyCode::Down);
    app.key(KeyCode::Char(' '));
    std::fs::write(
        directory.path().join("one.rs"),
        "fn one() { let changed = 9; }\n",
    )
    .unwrap();
    assert_eq!(
        application(directory.path(), state.path()).marks,
        [false, true]
    );
    let distinct = App::new(
        capture(directory.path(), &[]),
        state::Store::at(state.path(), directory.path(), &[]),
    );
    assert_eq!(distinct.marks, [false, false]);
}

#[test]
fn preview_preserves_carriage_returns_in_structured_and_original_output() {
    let original = preview::Preview::original(b"plain\r\n");
    assert_eq!(original.lines[0].to_string(), "plain\\r");

    let directory = fixture();
    let path = directory.path().join("one.rs");
    let old = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, old.replace('\n', "\r\n")).unwrap();
    let captured = capture(directory.path(), &["HEAD"]);
    let preview = preview::Preview::item(directory.path(), &captured.items[0]);
    assert_eq!(preview.reason, None);
    let removed = preview
        .lines
        .iter()
        .find(|line| line.to_string().starts_with("-fn one"))
        .unwrap();
    let added = preview
        .lines
        .iter()
        .find(|line| line.to_string().starts_with("+fn one"))
        .unwrap();
    assert!(!removed.to_string().ends_with("\\r"));
    assert!(added.to_string().ends_with("\\r"));
    assert!(added.spans.iter().any(
        |span| span.content == "\\r" && span.style.add_modifier.contains(Modifier::UNDERLINED)
    ));
}

#[test]
fn preview_graphemes_survive_composition_and_test_backend_clipping() {
    let preview = preview::Preview::original("e\u{301}👩‍💻Z\n".as_bytes());
    assert_eq!(preview.width, 4);
    for (left, width, expected) in [
        (0, 4, vec!["e\u{301}", "👩‍💻", " ", "Z"]),
        (1, 3, vec!["👩‍💻", " ", "Z"]),
        (2, 2, vec![" ", "Z"]),
        (0, 2, vec!["e\u{301}", " "]),
    ] {
        let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(
                    ratatui::widgets::Paragraph::new(preview.visible(0, left, 1, width as usize)),
                    frame.area(),
                )
            })
            .unwrap();
        for (column, expected) in expected.into_iter().enumerate() {
            assert_eq!(
                terminal.backend().buffer()[(column as u16, 0)].symbol(),
                expected,
                "left={left}, width={width}, column={column}"
            );
        }
    }
}

#[test]
fn preview_intraline_emphasizes_source_starting_with_header_like_dashes() {
    let directory = fixture();
    let path = directory.path().join("one.rs");
    let old = "fn one() {\n    let text = r#\"\n--old\n\"#;\n}\n";
    std::fs::write(&path, old).unwrap();
    git_run(directory.path(), &["add", "one.rs"]);
    git_run(directory.path(), &["commit", "-qm", "raw string"]);
    std::fs::write(path, old.replace("--old", "--new")).unwrap();
    let captured = capture(directory.path(), &["HEAD"]);
    let preview = preview::Preview::item(directory.path(), &captured.items[0]);
    assert_eq!(preview.reason, None);
    for expected in ["---old", "+--new"] {
        let line = preview
            .lines
            .iter()
            .find(|line| line.to_string() == expected)
            .unwrap();
        assert!(
            line.spans
                .iter()
                .any(|span| span.style.add_modifier.contains(Modifier::UNDERLINED))
        );
    }
    let header = preview
        .lines
        .iter()
        .find(|line| line.to_string().starts_with("--- a/"))
        .unwrap();
    assert!(
        header
            .spans
            .iter()
            .all(|span| !span.style.add_modifier.contains(Modifier::UNDERLINED))
    );
}

#[test]
fn pickaxe_separate_flag_like_values_are_patterns_and_real_requests_stay_blocked() {
    let directory = fixture();
    for option in ["-S", "-G"] {
        for pattern in ["--output", "--ext-diff", "--textconv"] {
            std::fs::write(
                directory.path().join("one.rs"),
                format!("fn one() {{ /* {pattern} */ }}\n"),
            )
            .unwrap();
            let captured = capture(directory.path(), &[option, pattern, "HEAD"]);
            assert_eq!(
                captured.items.len(),
                1,
                "{option} {pattern}: {:?}",
                captured.unavailable
            );
            for actual in ["--output=unwanted", "--ext-diff", "--textconv"] {
                let args = [option, pattern, actual, "HEAD"].map(OsString::from);
                assert!(
                    git::capture(directory.path(), &args)
                        .unwrap_err()
                        .to_string()
                        .contains("conflicts with read-only")
                );
            }
        }
    }
    assert!(!directory.path().join("unwanted").exists());
}

fn assert_native(cwd: &Path, args: &[&str]) -> git::Capture {
    let expected = Command::new("git")
        .current_dir(cwd)
        .args(["--no-pager", "diff"])
        .args(args)
        .output()
        .unwrap();
    let captured = capture(cwd, args);
    assert_eq!(captured.original, expected.stdout, "stdout for {args:?}");
    assert_eq!(captured.stderr, expected.stderr, "stderr for {args:?}");
    assert_eq!(
        captured.status.code(),
        expected.status.code(),
        "status for {args:?}"
    );
    captured
}

#[test]
fn native_formats_negations_and_unknown_valid_options_use_one_faithful_capture() {
    let directory = fixture();
    change(directory.path());
    for args in [
        vec!["--stat", "HEAD"],
        vec!["--raw", "-z", "HEAD"],
        vec!["--numstat", "-z", "HEAD"],
        vec!["--word-diff=porcelain", "HEAD"],
        vec!["--src-prefix=old/", "--dst-prefix=new/", "HEAD"],
        vec!["--output-indicator-new=>", "HEAD"],
        vec!["--color=always", "HEAD"],
        vec!["--no-patch", "HEAD"],
        vec!["--check", "HEAD"],
        vec!["--no-patch", "--patch", "HEAD"],
        vec!["--stat", "--no-stat", "HEAD"],
        vec!["--word-diff=none", "HEAD"],
        vec!["--histogram", "--minimal", "-U1", "HEAD"],
        vec!["--ext-diff", "--no-ext-diff", "HEAD"],
        vec!["--textconv", "--no-textconv", "HEAD"],
    ] {
        assert_native(directory.path(), &args);
    }
    assert_eq!(
        capture(directory.path(), &["--no-patch", "--patch", "HEAD"])
            .items
            .len(),
        2
    );
}

#[test]
fn separate_native_option_values_own_flag_like_tokens_and_separator() {
    let directory = fixture();
    change(directory.path());
    for option in [
        "--anchored",
        "--src-prefix",
        "--dst-prefix",
        "--line-prefix",
        "--output-indicator-new",
        "--output-indicator-old",
        "--output-indicator-context",
        "--diff-algorithm",
        "--word-diff-regex",
        "--find-object",
        "-S",
        "-G",
        "-O",
    ] {
        for value in ["--output=unwanted", "--ext-diff", "--textconv", "--"] {
            assert_native(directory.path(), &[option, value, "HEAD"]);
        }
    }
    assert!(!directory.path().join("unwanted").exists());
    for output in ["--output=unwanted", "--out=unwanted", "--output"] {
        assert!(git::capture(directory.path(), &[output.into()]).is_err());
    }
}

#[test]
fn statuses_preserve_differences_findings_and_execution_errors() {
    let directory = fixture();
    change(directory.path());
    assert_eq!(
        assert_native(directory.path(), &["--exit-code"])
            .status
            .code(),
        Some(1)
    );
    assert_eq!(
        assert_native(directory.path(), &["--quiet"]).status.code(),
        Some(1)
    );
    assert_eq!(
        assert_native(directory.path(), &["--exit-code", "--no-exit-code"])
            .status
            .code(),
        Some(0)
    );
    std::fs::write(directory.path().join("one.rs"), "bad trailing space \n").unwrap();
    let findings = assert_native(directory.path(), &["--check"]);
    assert_ne!(findings.status.code(), Some(0));
    assert!(!findings.original.is_empty());
    assert_native(directory.path(), &["--definitely-invalid-option"]);
    assert_native(directory.path(), &["unknown-ref"]);
}

#[test]
fn explicit_implicit_no_index_files_and_directories_work_outside_repositories() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("left"), "one\n").unwrap();
    std::fs::write(directory.path().join("right"), "two\n").unwrap();
    for args in [
        &["left", "right"][..],
        &["--no-index", "left", "right"],
        &["--no-index", "--quiet", "left", "right"],
    ] {
        let captured = assert_native(directory.path(), args);
        assert_eq!(captured.status.code(), Some(1));
        for item in &captured.items {
            assert!(source::load(directory.path(), item).is_err());
        }
    }
    std::fs::create_dir(directory.path().join("a")).unwrap();
    std::fs::create_dir(directory.path().join("b")).unwrap();
    std::fs::write(directory.path().join("a/file"), "a\n").unwrap();
    std::fs::write(directory.path().join("b/file"), "b\n").unwrap();
    assert_native(directory.path(), &["a", "b"]);
    assert_native(directory.path(), &["--no-index", "missing", "right"]);
}

#[test]
fn refs_ranges_merge_base_blob_and_reverse_comparisons_match_git() {
    let directory = fixture();
    change(directory.path());
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "second"]);
    for args in [
        vec!["HEAD~..HEAD"],
        vec!["HEAD~...HEAD"],
        vec!["--merge-base", "HEAD~", "HEAD"],
        vec!["HEAD~:one.rs", "HEAD:two.rs"],
        vec!["-R", "HEAD~", "HEAD"],
        vec!["--cached"],
        vec!["--staged", "HEAD~"],
    ] {
        assert_native(directory.path(), &args);
    }
}

#[test]
fn metadata_only_items_renames_and_binary_changes_can_be_marked_and_restarted() {
    use std::os::unix::fs::PermissionsExt;
    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    std::fs::set_permissions(
        directory.path().join("one.rs"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let captured = assert_native(directory.path(), &["HEAD"]);
    assert_eq!(captured.items.len(), 1);
    assert!(
        captured.items[0]
            .patch
            .windows(8)
            .any(|part| part == b"old mode")
    );
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Enter);
    assert!(app.preview.is_some());
    app.key(KeyCode::Char(' '));
    assert_eq!(application(directory.path(), state.path()).marks, [true]);
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "mode"]);
    git_run(directory.path(), &["mv", "one.rs", "renamed.rs"]);
    let renamed = assert_native(directory.path(), &["--cached", "-M"]);
    assert_eq!(renamed.items.len(), 1);
    assert_eq!(renamed.items[0].status, "R100");
    assert_ne!(renamed.items[0].old_path, renamed.items[0].new_path);
    std::fs::write(directory.path().join("two.rs"), b"binary\0bytes").unwrap();
    let binary = assert_native(directory.path(), &["HEAD"]);
    assert!(!binary.items.is_empty());
    for item in &binary.items {
        assert!(source::load(directory.path(), item).is_err());
    }
}

#[test]
fn malformed_sections_never_authorize_file_actions() {
    let directory = fixture();
    change(directory.path());
    let original = capture(directory.path(), &["HEAD"]).original;
    for malformed in [
        b"diff --git a/a b/a\n".to_vec(),
        b"diff --git a/a b/a\nindex 1234..5678\n@@ -1,2 +1,2 @@\n-a\n+b\n".to_vec(),
        b"diff --git a/a b/a\nunknown metadata\n".to_vec(),
        b"diff --git a/a b/a b/b\nold mode 100644\nnew mode 100755\n".to_vec(),
    ] {
        assert!(git::parse(directory.path(), &malformed).is_none());
    }
    let missing = original.iter().rposition(|byte| *byte == b'\n').unwrap();
    assert!(git::parse(directory.path(), &original[..missing.saturating_sub(2)]).is_none());
}

#[test]
fn invalid_native_bytes_have_distinct_visible_names_and_output() {
    use std::os::unix::ffi::OsStringExt;
    let directory = fixture();
    let names = [
        OsString::from_vec(b"bad-\xff.rs".to_vec()),
        OsString::from("bad-�.rs"),
        OsString::from("bad-\\xff.rs"),
    ];
    for name in &names {
        std::fs::write(directory.path().join(name), "old\n").unwrap();
    }
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "native"]);
    for name in &names {
        std::fs::write(directory.path().join(name), "new\n").unwrap();
    }
    let captured = capture(directory.path(), &["HEAD"]);
    let visible: std::collections::BTreeSet<_> =
        captured.items.iter().map(|item| item.name()).collect();
    assert_eq!(visible.len(), 3);
    let rendered: std::collections::BTreeSet<_> = [b"\xff".as_slice(), "�".as_bytes(), b"\\xff"]
        .into_iter()
        .map(|bytes| preview::Preview::original(bytes).lines[0].to_string())
        .collect();
    assert_eq!(rendered.len(), 3);
}

#[test]
fn nominal_saved_marks_from_full_index_capture_are_restored() {
    let directory = fixture();
    let base = tempfile::tempdir().unwrap();
    change(directory.path());
    let captured = capture(directory.path(), &["HEAD"]);
    let output = Command::new("git")
        .current_dir(directory.path())
        .args([
            "diff",
            "--raw",
            "-z",
            "--patch",
            "--full-index",
            "--no-abbrev",
            "--no-color",
            "HEAD",
        ])
        .output()
        .unwrap();
    let mut bytes = output.stdout.as_slice();
    let mut metadata = Vec::new();
    while bytes.starts_with(b":") {
        let end = bytes.iter().position(|byte| *byte == 0).unwrap();
        metadata.push(bytes[..end].to_vec());
        bytes = &bytes[end + 1..];
        let end = bytes.iter().position(|byte| *byte == 0).unwrap();
        bytes = &bytes[end + 1..];
    }
    bytes = &bytes[1..];
    let second = bytes
        .windows(11)
        .position(|part| part == b"diff --git " && part.as_ptr() != bytes.as_ptr())
        .unwrap();
    let patches = [&bytes[..second], &bytes[second..]];
    let store = state::Store::at(base.path(), directory.path(), &["HEAD".into()]);
    let marks: std::collections::BTreeMap<_, _> = captured
        .items
        .iter()
        .zip(metadata.iter().zip(patches))
        .map(|(item, (metadata, patch))| {
            (
                state::digest([&item.new_path]),
                state::digest([item.old_path.as_slice(), &item.new_path, metadata, patch]),
            )
        })
        .collect();
    std::fs::create_dir_all(store.path.parent().unwrap()).unwrap();
    std::fs::write(
        &store.path,
        serde_json::to_vec(&serde_json::json!({"version": 1, "marks": marks})).unwrap(),
    )
    .unwrap();
    assert_eq!(store.load(&captured.items).unwrap(), [true, true]);
}

fn snapshot(root: &Path) -> std::collections::BTreeMap<std::path::PathBuf, Vec<u8>> {
    fn visit(
        root: &Path,
        current: &Path,
        result: &mut std::collections::BTreeMap<std::path::PathBuf, Vec<u8>>,
    ) {
        for entry in std::fs::read_dir(current).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                visit(root, &path, result);
            } else if entry.file_type().unwrap().is_symlink() {
                result.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read_link(&path)
                        .unwrap()
                        .as_os_str()
                        .as_encoded_bytes()
                        .to_vec(),
                );
            } else {
                result.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut result = std::collections::BTreeMap::new();
    visit(root, root, &mut result);
    result
}

#[test]
fn helper_policy_is_applicable_ordered_and_preserves_all_repository_bytes() {
    let directory = fixture();
    change(directory.path());
    git_run(
        directory.path(),
        &["config", "filter.unused.clean", "touch helper-ran; cat"],
    );
    git_run(
        directory.path(),
        &["config", "filter.used.clean", "touch helper-ran; cat"],
    );
    git_run(
        directory.path(),
        &["config", "core.fsmonitor", "touch monitor-ran"],
    );
    let before = snapshot(directory.path());
    capture(directory.path(), &["HEAD"]);
    assert_eq!(snapshot(directory.path()), before);
    std::fs::write(
        directory.path().join(".gitattributes"),
        "two.rs filter=used\n",
    )
    .unwrap();
    let before = snapshot(directory.path());
    // Git pathspec selection proves the configured filter irrelevant.
    capture(directory.path(), &["HEAD", "--", "one.rs"]);
    capture(directory.path(), &["HEAD", "one.rs"]);
    capture(directory.path(), &["--cached"]);
    capture(directory.path(), &["HEAD", "HEAD"]);
    capture(directory.path(), &["HEAD..HEAD"]);
    assert!(git::capture(directory.path(), &["HEAD".into()]).is_err());
    assert_eq!(snapshot(directory.path()), before);
    assert!(!directory.path().join("helper-ran").exists());
    assert!(!directory.path().join("monitor-ran").exists());
    std::fs::write(
        directory.path().join(".gitattributes"),
        "one.rs diff=unsafe\n",
    )
    .unwrap();
    git_run(
        directory.path(),
        &["config", "diff.unsafe.textconv", "touch converter-ran; cat"],
    );
    let before = snapshot(directory.path());
    assert!(git::capture(directory.path(), &["HEAD".into()]).is_err());
    capture(directory.path(), &["--no-textconv", "HEAD"]);
    capture(directory.path(), &["--textconv", "--no-textconv", "HEAD"]);
    assert_eq!(snapshot(directory.path()), before);
    git_run(
        directory.path(),
        &["config", "diff.unsafe.command", "touch external-ran"],
    );
    let before = snapshot(directory.path());
    assert!(git::capture(directory.path(), &["--no-textconv".into(), "HEAD".into()]).is_err());
    capture(
        directory.path(),
        &["--no-textconv", "--no-ext-diff", "HEAD"],
    );
    assert_eq!(snapshot(directory.path()), before);
}

#[test]
fn optional_values_never_hide_actual_output_destinations() {
    let directory = fixture();
    change(directory.path());
    for option in [
        "--src-p",
        "--dst-p",
        "--line-p",
        "--anch",
        "--unified",
        "-U",
        "--relative",
        "--abbrev",
        "--find-renames",
        "-M",
        "-B",
        "-C",
        "--word-diff",
        "--color-moved",
        "--submodule",
        "--ignore-submodules",
    ] {
        assert!(
            git::capture(
                directory.path(),
                &[option.into(), "--output=unwanted".into(), "HEAD".into()]
            )
            .is_err(),
            "{option}"
        );
    }
    assert!(!directory.path().join("unwanted").exists());
    for option in [
        "-pS",
        "-pG",
        "-l",
        "-I",
        "--color-moved-ws",
        "--ws-error-highlight",
    ] {
        assert_native(directory.path(), &[option, "--output=unwanted", "HEAD"]);
    }
    assert!(!directory.path().join("unwanted").exists());
}

#[test]
fn merge_conflicts_and_combined_sections_retain_every_native_side() {
    let directory = fixture();
    git_run(directory.path(), &["checkout", "-qb", "other"]);
    std::fs::write(directory.path().join("one.rs"), "fn other() {}\n").unwrap();
    git_run(directory.path(), &["commit", "-qam", "other"]);
    git_run(directory.path(), &["checkout", "-q", "-"]);
    std::fs::write(directory.path().join("one.rs"), "fn primary() {}\n").unwrap();
    git_run(directory.path(), &["commit", "-qam", "primary"]);
    let merge = Command::new("git")
        .current_dir(directory.path())
        .args(["merge", "other"])
        .output()
        .unwrap();
    assert!(!merge.status.success());
    let before = snapshot(directory.path());
    for args in [
        &[][..],
        &["--cc"],
        &["--raw", "-z"],
        &["--base"],
        &["--ours"],
        &["--theirs"],
        &["--combined-all-paths", "--cc"],
    ] {
        let captured = assert_native(directory.path(), args);
        if args.is_empty() || args.contains(&"--cc") {
            assert!(captured.unavailable.is_some());
            assert!(captured.items.is_empty());
        }
    }
    assert_eq!(snapshot(directory.path()), before);
}

#[test]
fn copies_type_changes_submodules_and_nested_paths_keep_native_output() {
    let directory = fixture();
    std::fs::copy(
        directory.path().join("one.rs"),
        directory.path().join("copy.rs"),
    )
    .unwrap();
    git_run(directory.path(), &["add", "."]);
    let copied = assert_native(
        directory.path(),
        &["--cached", "-C", "--find-copies-harder"],
    );
    assert_eq!(copied.items[0].status, "C100");
    git_run(directory.path(), &["commit", "-qm", "copy"]);
    std::fs::remove_file(directory.path().join("one.rs")).unwrap();
    std::os::unix::fs::symlink("two.rs", directory.path().join("one.rs")).unwrap();
    let changed = assert_native(directory.path(), &["HEAD"]);
    assert!(changed.unavailable.is_some()); // Git emits two sections for one type change.
    let oid = String::from_utf8(git_run(directory.path(), &["rev-parse", "HEAD"])).unwrap();
    git_run(
        directory.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},sub", oid.trim()),
        ],
    );
    assert_native(directory.path(), &["--cached", "--submodule=log"]);
    assert_native(directory.path(), &["--cached"]);
}

#[test]
fn no_index_abbreviated_values_follow_its_distinct_git_parser() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("a"), "a\n").unwrap();
    std::fs::write(directory.path().join("b"), "b\n").unwrap();
    for option in ["--src-p", "--dst-p", "--line-p", "--anch"] {
        assert_native(directory.path(), &[option, "--output=unwanted", "a", "b"]);
    }
    assert!(!directory.path().join("unwanted").exists());
}

#[test]
fn nested_submodule_helpers_cannot_escape_the_read_only_policy() {
    let directory = fixture();
    let nested_path = directory.path().join("nested");
    std::fs::create_dir(&nested_path).unwrap();
    git_run(&nested_path, &["init", "-q"]);
    std::fs::write(nested_path.join("file"), "old\n").unwrap();
    git_run(&nested_path, &["add", "."]);
    git_run(
        &nested_path,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "nested",
        ],
    );
    let oid = String::from_utf8(git_run(&nested_path, &["rev-parse", "HEAD"])).unwrap();
    git_run(
        directory.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},nested", oid.trim()),
        ],
    );
    git_run(directory.path(), &["commit", "-qm", "submodule"]);
    std::fs::write(nested_path.join(".gitattributes"), "file filter=unsafe\n").unwrap();
    git_run(
        &nested_path,
        &["config", "filter.unsafe.clean", "touch helper-ran; cat"],
    );
    std::fs::write(nested_path.join("file"), "new\n").unwrap();
    let before = snapshot(directory.path());
    assert!(git::capture(directory.path(), &[]).is_err());
    capture(directory.path(), &["HEAD", "--", "one.rs"]);
    capture(directory.path(), &["HEAD", "one.rs"]);
    capture(directory.path(), &["--ignore-submodules=all"]);
    assert_eq!(snapshot(directory.path()), before);
}

#[test]
fn empty_added_files_are_items_even_without_hunks() {
    let directory = fixture();
    std::fs::write(directory.path().join("empty"), "").unwrap();
    git_run(directory.path(), &["add", "empty"]);
    let captured = assert_native(directory.path(), &["--cached"]);
    assert_eq!(captured.items.len(), 1);
    assert_eq!(captured.items[0].status, "A");
    assert!(source::load(directory.path(), &captured.items[0]).is_err());
}

// Write the exact v1 mark fingerprints that the original raw/full-index
// capture produced. The compatibility oracle is Git's baseline byte stream,
// independent of the new reconstruction and fingerprint implementation.
fn save_baseline_marks(cwd: &Path, base: &Path, args: &[&str]) -> state::Store {
    let output = Command::new("git")
        .current_dir(cwd)
        .args([
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--raw",
            "-z",
            "--patch",
            "--full-index",
            "--no-abbrev",
            "--no-color",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success());
    let mut bytes = output.stdout.as_slice();
    let mut records = Vec::new();
    while bytes.starts_with(b":") {
        let end = bytes.iter().position(|byte| *byte == 0).unwrap();
        let metadata = bytes[..end].to_vec();
        bytes = &bytes[end + 1..];
        let end = bytes.iter().position(|byte| *byte == 0).unwrap();
        let path = bytes[..end].to_vec();
        bytes = &bytes[end + 1..];
        assert!(matches!(metadata.last(), Some(b'M' | b'A' | b'D' | b'T')));
        records.push((metadata, path));
    }
    assert_eq!(bytes.first(), Some(&0));
    bytes = &bytes[1..];
    let mut starts: Vec<_> = bytes
        .windows(11)
        .enumerate()
        .filter_map(|(index, part)| {
            (part == b"diff --git " && (index == 0 || bytes[index - 1] == b'\n')).then_some(index)
        })
        .collect();
    assert_eq!(starts.len(), records.len());
    starts.push(bytes.len());
    let marks: std::collections::BTreeMap<_, _> = records
        .iter()
        .zip(starts.windows(2))
        .map(|((metadata, path), bounds)| {
            let patch = &bytes[bounds[0]..bounds[1]];
            assert!(patch.windows(3).any(|part| part == b"@@ "));
            (
                state::digest([path]),
                state::digest([path.as_slice(), path, metadata, patch]),
            )
        })
        .collect();
    let store = state::Store::at(
        base,
        cwd,
        &args.iter().map(OsString::from).collect::<Vec<_>>(),
    );
    std::fs::create_dir_all(store.path.parent().unwrap()).unwrap();
    std::fs::write(
        &store.path,
        serde_json::to_vec(&serde_json::json!({"version": 1, "marks": marks})).unwrap(),
    )
    .unwrap();
    store
}

#[test]
fn legacy_marks_restore_non_utf8_and_missing_final_newline_sources() {
    for (old, new) in [
        (b"old\n".as_slice(), b"new".as_slice()),
        (b"old", b"new\n"),
        (b"old", b"new"),
        (b"old-\xff\n", b"new\n"),
        (b"old\n", b"new-\xfe\n"),
        (b"old-\xff", b"new-\xfe"),
        (
            b"invalid-\xff\nunchanged\nold\n",
            b"invalid-\xff\nunchanged\nnew\n",
        ),
    ] {
        let directory = fixture();
        let base = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("one.rs"), old).unwrap();
        git_run(directory.path(), &["add", "one.rs"]);
        git_run(directory.path(), &["commit", "-qm", "byte source"]);
        std::fs::write(directory.path().join("one.rs"), new).unwrap();
        let args = ["HEAD", "one.rs"];
        let store = save_baseline_marks(directory.path(), base.path(), &args);
        let captured = capture(directory.path(), &args);
        assert_eq!(captured.items.len(), 1, "{old:?} -> {new:?}");
        let item = &captured.items[0];
        let reconstructed = source::reconstruct(directory.path(), item).unwrap();
        assert_eq!(reconstructed.old, old);
        assert_eq!(reconstructed.new, new);
        assert!(
            source::load(directory.path(), item).is_err(),
            "enrichment limitation must stay visible"
        );
        assert_eq!(
            store.load(&captured.items).unwrap(),
            [true],
            "{old:?} -> {new:?}"
        );
        let mut app = App::new(captured, store);
        app.key(KeyCode::Enter);
        assert!(app.preview.as_ref().unwrap().reason.is_some());
        assert_eq!(app.marks, [true]);
    }
}

#[test]
fn legacy_byte_marks_restore_for_staged_refs_additions_and_deletions() {
    let directory = fixture();
    let base = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("one.rs"), b"old-\xff").unwrap();
    git_run(directory.path(), &["add", "one.rs"]);
    git_run(directory.path(), &["commit", "-qm", "old bytes"]);
    std::fs::write(directory.path().join("one.rs"), b"new-\xfe").unwrap();
    std::fs::write(directory.path().join("added.rs"), b"added-\xff").unwrap();
    std::fs::remove_file(directory.path().join("two.rs")).unwrap();
    git_run(directory.path(), &["add", "."]);
    let store = save_baseline_marks(directory.path(), base.path(), &["--cached"]);
    let captured = capture(directory.path(), &["--cached"]);
    assert_eq!(captured.items.len(), 3);
    assert_eq!(store.load(&captured.items).unwrap(), [true; 3]);
    // HEAD-to-worktree can reuse full index IDs in its raw records even
    // though the command is not --cached. Preserve that baseline form too.
    let store = save_baseline_marks(directory.path(), base.path(), &["HEAD"]);
    let captured = capture(directory.path(), &["HEAD"]);
    assert_eq!(store.load(&captured.items).unwrap(), [true; 3]);
    git_run(directory.path(), &["commit", "-qm", "new bytes"]);
    for args in [&["HEAD~", "HEAD"][..], &["HEAD~..HEAD"]] {
        let store = save_baseline_marks(directory.path(), base.path(), args);
        let captured = capture(directory.path(), args);
        assert_eq!(store.load(&captured.items).unwrap(), [true; 3]);
    }
}

#[test]
fn legacy_symlink_marks_restore_without_enabling_text_enrichment() {
    let directory = fixture();
    let base = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink("one.rs", directory.path().join("link.rs")).unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "link"]);
    std::fs::remove_file(directory.path().join("link.rs")).unwrap();
    std::os::unix::fs::symlink("two.rs", directory.path().join("link.rs")).unwrap();
    let store = save_baseline_marks(directory.path(), base.path(), &["HEAD"]);
    let captured = capture(directory.path(), &["HEAD"]);
    assert_eq!(captured.items.len(), 1);
    assert_eq!(store.load(&captured.items).unwrap(), [true]);
    let reconstructed = source::reconstruct(directory.path(), &captured.items[0]).unwrap();
    assert_eq!(reconstructed.old, b"one.rs");
    assert_eq!(reconstructed.new, b"two.rs");
    assert!(
        source::load(directory.path(), &captured.items[0])
            .unwrap_err()
            .to_string()
            .contains("ordinary file modes")
    );
}

#[test]
fn policy_historical_and_root_relative_paths_block_applicable_drivers() {
    let directory = fixture();
    std::fs::write(directory.path().join("historical.txt"), "old\n").unwrap();
    std::fs::write(
        directory.path().join(".gitattributes"),
        "*.txt diff=unsafe\n",
    )
    .unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "historical"]);
    git_run(directory.path(), &["rm", "-q", "historical.txt"]);
    git_run(directory.path(), &["commit", "-qm", "delete"]);
    git_run(
        directory.path(),
        &["config", "diff.unsafe.textconv", "touch driver-ran; cat"],
    );
    let native = Command::new("git")
        .current_dir(directory.path())
        .args(["diff", "HEAD~", "HEAD"])
        .output()
        .unwrap();
    assert!(native.status.success());
    assert!(directory.path().join("driver-ran").exists());
    std::fs::remove_file(directory.path().join("driver-ran")).unwrap();
    let before = snapshot(directory.path());
    assert!(git::capture(directory.path(), &["HEAD~".into(), "HEAD".into()]).is_err());
    assert_eq!(snapshot(directory.path()), before);
    // Original Git pathspec semantics still prove an unrelated file safe.
    assert_native(directory.path(), &["HEAD~", "HEAD", "--", ":(top)one.rs"]);
    std::fs::create_dir(directory.path().join("sub")).unwrap();
    std::fs::write(directory.path().join("outside.txt"), "old\n").unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "outside"]);
    std::fs::write(directory.path().join("outside.txt"), "new\n").unwrap();
    let cwd = directory.path().join("sub");
    let native = Command::new("git")
        .current_dir(&cwd)
        .args(["diff", "HEAD"])
        .output()
        .unwrap();
    assert!(native.status.success());
    assert!(directory.path().join("driver-ran").exists());
    std::fs::remove_file(directory.path().join("driver-ran")).unwrap();
    let before = snapshot(directory.path());
    assert!(git::capture(&cwd, &["HEAD".into()]).is_err());
    assert!(
        git::capture(
            &cwd,
            &["HEAD".into(), "--".into(), ":(exclude)ignored.txt".into()]
        )
        .is_err()
    );
    assert_native(&cwd, &["HEAD", "--", ":(top)one.rs"]);
    assert_eq!(snapshot(directory.path()), before);
}

fn bounded_policy_worker(cwd: &Path, name: &str, extra: &[(&str, &Path)]) {
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args(["--exact", name, "--nocapture"])
        .env("REVIEW_POLICY_FIXTURE", cwd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in extra {
        child.env(key, value);
    }
    let mut child = child.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let output = child.wait_with_output().unwrap();
            assert!(
                status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("helper policy timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn policy_no_index_global_external_worker() {
    let Some(cwd) = std::env::var_os("REVIEW_POLICY_FIXTURE") else {
        return;
    };
    let cwd = Path::new(&cwd);
    let native = Command::new("git")
        .current_dir(cwd)
        .args(["diff", "--no-index", "a", "b"])
        .output()
        .unwrap();
    assert!(native.status.code().is_some_and(|code| code <= 1));
    assert!(cwd.join("external-ran").exists());
    std::fs::remove_file(cwd.join("external-ran")).unwrap();
    assert!(git::capture(cwd, &["--no-index".into(), "a".into(), "b".into()]).is_err());
    assert!(!cwd.join("external-ran").exists());
}

#[test]
fn policy_no_index_outside_repo_checks_global_external_before_enumeration() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("a"), "old\n").unwrap();
    std::fs::write(directory.path().join("b"), "new\n").unwrap();
    let global = directory.path().join("global-config");
    std::fs::write(&global, "[diff]\n external = touch external-ran\n").unwrap();
    bounded_policy_worker(
        directory.path(),
        "tests::policy_no_index_global_external_worker",
        &[("GIT_CONFIG_GLOBAL", &global)],
    );
}

#[test]
fn policy_large_candidates_worker() {
    let Some(cwd) = std::env::var_os("REVIEW_POLICY_FIXTURE") else {
        return;
    };
    let captured = git::capture(Path::new(&cwd), &[]).unwrap();
    assert!(captured.original.is_empty());
    assert!(captured.status.success());
}

#[test]
fn policy_large_candidate_sets_complete_without_pipe_deadlock() {
    let directory = fixture();
    for index in 0..5000 {
        std::fs::write(
            directory.path().join(format!(
                "long-candidate-{index:05}-padding-for-attribute-pipes.txt"
            )),
            "unchanged\n",
        )
        .unwrap();
    }
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "large"]);
    git_run(
        directory.path(),
        &["config", "filter.unused.clean", "touch helper-ran; cat"],
    );
    bounded_policy_worker(
        directory.path(),
        "tests::policy_large_candidates_worker",
        &[],
    );
    assert!(!directory.path().join("helper-ran").exists());
}

#[test]
fn policy_effective_submodule_ignores_skip_unused_worktree_filters() {
    let directory = fixture();
    let nested = directory.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    git_run(&nested, &["init", "-q"]);
    std::fs::write(nested.join("file"), "old\n").unwrap();
    git_run(&nested, &["add", "."]);
    git_run(
        &nested,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "nested",
        ],
    );
    let oid = String::from_utf8(git_run(&nested, &["rev-parse", "HEAD"])).unwrap();
    git_run(
        directory.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{},nested", oid.trim()),
        ],
    );
    std::fs::write(
        directory.path().join(".gitmodules"),
        "[submodule \"example\"]\n path = nested\n url = ./nested\n",
    )
    .unwrap();
    git_run(directory.path(), &["add", ".gitmodules"]);
    git_run(directory.path(), &["commit", "-qm", "module"]);
    std::fs::write(nested.join(".gitattributes"), "file filter=unsafe\n").unwrap();
    git_run(
        &nested,
        &["config", "filter.unsafe.clean", "touch helper-ran; cat"],
    );
    std::fs::write(nested.join("file"), "new\n").unwrap();
    for args in [
        &["--ignore-submodules=dirty"][..],
        &["--ignore-submodules=all"],
    ] {
        let captured = assert_native(directory.path(), args);
        assert!(captured.original.is_empty());
        assert!(captured.status.success());
    }
    for (file, key) in [
        (None, "diff.ignoreSubmodules"),
        (None, "submodule.example.ignore"),
        (Some(".gitmodules"), "submodule.example.ignore"),
    ] {
        let mut args = vec!["config"];
        if let Some(file) = file {
            args.extend(["--file", file]);
        }
        args.extend([key, "dirty"]);
        git_run(directory.path(), &args);
        let captured = assert_native(directory.path(), &[]);
        assert!(captured.status.success());
        // Explicit none overrides every configured ignore and must diagnose.
        assert!(git::capture(directory.path(), &["--ignore-submodules=none".into()]).is_err());
        let mut args = vec!["config"];
        if let Some(file) = file {
            args.extend(["--file", file]);
        }
        args.extend(["--unset", key]);
        git_run(directory.path(), &args);
    }
    assert!(!nested.join("helper-ran").exists());
}

#[test]
fn structured_warning_bytes_remain_scrollable_after_marking_and_reopening() {
    let directory = fixture();
    let base = tempfile::tempdir().unwrap();
    change(directory.path());
    let mut captured = capture(directory.path(), &["HEAD"]);
    let patch = captured.items[0].patch.clone();
    captured.stderr = (0..80)
        .map(|index| format!("warning row {index:02}\n"))
        .collect::<String>()
        .into_bytes();
    captured.stderr.extend_from_slice(b"last-warning-\xff\n");
    captured.diagnostic = git::visible_bytes(&captured.stderr);
    let mut app = App::new(
        captured,
        state::Store::at(base.path(), directory.path(), &["HEAD".into()]),
    );
    assert!(app.preview.is_none());
    app.key(KeyCode::Enter);
    app.key(KeyCode::Char(' '));
    assert_eq!(app.status, "Review mark saved");
    assert!(app.marks[0]);
    assert_eq!(app.capture.items[0].patch, patch);
    assert!(
        app.preview
            .as_ref()
            .unwrap()
            .lines
            .iter()
            .any(|line| line.to_string() == "warning row 79")
    );
    screen(&mut app, 100, 12);
    for _ in 0..200 {
        app.key(KeyCode::Down);
    }
    assert!(screen_text(&screen(&mut app, 100, 12)).contains("last-warning-\\xff"));
    app.key(KeyCode::Tab);
    app.key(KeyCode::Down);
    app.key(KeyCode::Up);
    assert!(app.preview.is_none());
    app.key(KeyCode::Enter);
    assert!(
        app.preview
            .as_ref()
            .unwrap()
            .lines
            .iter()
            .any(|line| line.to_string() == "warning row 79")
    );
}

#[test]
fn policy_staged_deletions_include_implicit_and_explicit_head_driver_candidates() {
    let directory = fixture();
    std::fs::write(directory.path().join("deleted.txt"), "old\n").unwrap();
    std::fs::write(
        directory.path().join(".gitattributes"),
        "deleted.txt diff=unsafe\n",
    )
    .unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "deletion source"]);
    git_run(directory.path(), &["rm", "-q", "deleted.txt"]);
    git_run(
        directory.path(),
        &[
            "config",
            "diff.unsafe.textconv",
            "touch staged-driver-ran; cat",
        ],
    );
    for args in [
        &["--cached"][..],
        &["--staged"],
        &["--cached", "HEAD"],
        &["--staged", "HEAD"],
    ] {
        let native = Command::new("git")
            .current_dir(directory.path())
            .arg("diff")
            .args(args)
            .output()
            .unwrap();
        assert!(native.status.success());
        assert!(directory.path().join("staged-driver-ran").exists());
        std::fs::remove_file(directory.path().join("staged-driver-ran")).unwrap();
        let before = snapshot(directory.path());
        assert!(
            git::capture(
                directory.path(),
                &args.iter().map(OsString::from).collect::<Vec<_>>()
            )
            .is_err()
        );
        assert_eq!(snapshot(directory.path()), before);
    }
    let unborn = tempfile::tempdir().unwrap();
    git_run(unborn.path(), &["init", "-q"]);
    std::fs::write(unborn.path().join("added"), "new\n").unwrap();
    git_run(unborn.path(), &["add", "added"]);
    for args in [&["--cached"][..], &["--staged"]] {
        let captured = assert_native(unborn.path(), args);
        assert!(captured.status.success());
        assert_eq!(captured.items.len(), 1);
    }
}

#[test]
fn policy_assigned_sentinel_names_cannot_hide_filter_or_diff_helpers() {
    for name in ["set", "unset", "unspecified"] {
        for attr in ["filter", "diff"] {
            let directory = fixture();
            change(directory.path());
            std::fs::write(
                directory.path().join(".gitattributes"),
                format!("one.rs {attr}={name}\n"),
            )
            .unwrap();
            let key = format!(
                "{attr}.{name}.{}",
                if attr == "filter" {
                    "clean"
                } else {
                    "textconv"
                }
            );
            git_run(
                directory.path(),
                &["config", &key, "touch sentinel-helper-ran; cat"],
            );
            let native = Command::new("git")
                .current_dir(directory.path())
                .args(["diff", "HEAD"])
                .output()
                .unwrap();
            assert!(
                native.status.success(),
                "{attr}={name}: {:?}",
                native.stderr
            );
            assert!(
                directory.path().join("sentinel-helper-ran").exists(),
                "{attr}={name}"
            );
            std::fs::remove_file(directory.path().join("sentinel-helper-ran")).unwrap();
            let before = snapshot(directory.path());
            assert!(
                git::capture(directory.path(), &["HEAD".into()]).is_err(),
                "{attr}={name}"
            );
            assert_eq!(snapshot(directory.path()), before, "{attr}={name}");
        }
    }
}

#[test]
fn no_index_unique_short_values_preserve_literal_flags_and_real_destinations() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("left"), "left\n").unwrap();
    std::fs::write(directory.path().join("right"), "right\n").unwrap();
    for option in ["--li", "--ds"] {
        let captured = assert_native(
            directory.path(),
            &["--no-index", option, "--output=literal", "left", "right"],
        );
        assert_eq!(captured.status.code(), Some(1));
        assert!(!directory.path().join("literal").exists());
        assert!(
            git::capture(
                directory.path(),
                &[
                    "--no-index".into(),
                    option.into(),
                    "--output=literal".into(),
                    "--output=real".into(),
                    "left".into(),
                    "right".into()
                ]
            )
            .is_err()
        );
    }
    for option in [
        "--a",
        "--in",
        "--unified",
        "--relative",
        "--di",
        "--word-diff",
    ] {
        assert!(
            git::capture(
                directory.path(),
                &[
                    "--no-index".into(),
                    option.into(),
                    "--output=real".into(),
                    "left".into(),
                    "right".into()
                ]
            )
            .is_err(),
            "{option}"
        );
    }
    assert!(!directory.path().join("real").exists());
}

#[test]
fn custom_a_b_shaped_prefixes_never_create_fictitious_item_identities() {
    let directory = fixture();
    let base = tempfile::tempdir().unwrap();
    change(directory.path());
    for args in [
        vec!["--src-prefix=a/old/", "--dst-prefix=b/new/", "HEAD"],
        vec!["--src-prefix", "a/old/", "--dst-prefix", "b/new/", "HEAD"],
        vec!["--default-prefix", "--src-prefix=a/old/", "HEAD"],
    ] {
        let captured = assert_native(directory.path(), &args);
        assert!(captured.unavailable.is_some());
        assert!(captured.items.is_empty());
        let mut app = App::new(
            captured,
            state::Store::at(base.path(), directory.path(), &[]),
        );
        app.key(KeyCode::Char(' '));
        assert!(app.marks.is_empty());
        assert!(app.status.contains("unavailable"));
    }
    for args in [
        vec![
            "--src-prefix=a/old/",
            "--dst-prefix=b/new/",
            "--default-prefix",
            "HEAD",
        ],
        vec!["--no-prefix", "--default-prefix", "HEAD"],
        vec![
            "--src-prefix=a/old/",
            "--src-prefix=a/",
            "--dst-prefix=b/new/",
            "--dst-prefix=b/",
            "HEAD",
        ],
    ] {
        let captured = assert_native(directory.path(), &args);
        assert!(captured.unavailable.is_none());
        assert_eq!(captured.items.len(), 2);
        assert_eq!(captured.items[0].old_path, b"one.rs");
        assert_eq!(captured.items[0].new_path, b"one.rs");
    }
    git_run(directory.path(), &["config", "diff.srcPrefix", "a/custom/"]);
    git_run(directory.path(), &["config", "diff.dstPrefix", "b/custom/"]);
    let captured = assert_native(directory.path(), &["HEAD"]);
    assert!(captured.unavailable.is_some());
    assert!(captured.items.is_empty());
    for args in [
        vec!["--default-prefix", "HEAD"],
        vec!["--src-prefix", "a/", "--dst-prefix", "b/", "HEAD"],
    ] {
        let captured = assert_native(directory.path(), &args);
        assert!(captured.unavailable.is_none());
        assert_eq!(captured.items[0].new_path, b"one.rs");
    }
    git_run(directory.path(), &["config", "diff.srcPrefix", "a/"]);
    git_run(directory.path(), &["config", "diff.dstPrefix", "b/"]);
    assert_eq!(assert_native(directory.path(), &["HEAD"]).items.len(), 2);
}

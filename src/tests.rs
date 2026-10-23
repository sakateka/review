use super::*;
#[path = "quit_tests.rs"]
mod quit_tests;
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
        Cli::Review {
            args: ["HEAD", "--", "a file", "--stat"]
                .map(OsString::from)
                .to_vec(),
            keymap: None,
            config: None
        }
    );
    assert!(parse_args(["HEAD".into()]).is_err());
    assert_eq!(
        parse_args(["--help".into()]).unwrap(),
        Cli::Help {
            keymap: None,
            config: None
        }
    );
}

#[test]
fn native_argument_bytes_and_length_delimiters_do_not_alias() {
    use std::os::unix::ffi::OsStringExt;
    let raw = OsString::from_vec(vec![b'p', 0xff]);
    assert_eq!(
        parse_args(["--".into(), raw.clone()]).unwrap(),
        Cli::Review {
            args: vec![raw.clone()],
            keymap: None,
            config: None
        }
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
    app.key(KeyCode::Char('m'));
    assert_eq!(app.marks, [false, true]);
    let mut restarted = application(directory.path(), state.path());
    restarted.set_theme(theme::Theme::classic());
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
    let preview = preview::Preview::item(directory.path(), item, &theme::Theme::classic());
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
        app.key(KeyCode::Char('m'));
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
    let preview = preview::Preview::item(
        directory.path(),
        &captured.items[0],
        &theme::Theme::default(),
    );
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
        KeyCode::Char('m'),
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
    app.key(KeyCode::Char('m'));
    assert!(app.marks[0]);
    let prior_path = app.store.path.clone();
    let prior = std::fs::read(&prior_path).unwrap();
    // A regular file as a parent fails regardless of test-user permissions.
    let obstacle = state.path().join("regular-file");
    std::fs::write(&obstacle, b"occupied").unwrap();
    app.store.path = obstacle.join("state.json");
    app.key(KeyCode::Char('m'));
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
    app.key(KeyCode::Char('m'));
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
    let preview =
        preview::Preview::original("a\t界é🙂z\nsecond\n".as_bytes(), &theme::Theme::default());
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
    assert_eq!(app.cursor.0, preview.lines.len() - 1);
    assert_eq!(app.cursor.1, preview.lines.last().unwrap().width() - 1);
    assert!(app.cursor.1 >= app.left && app.cursor.1 < app.left + app.viewport.1);
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
    app.key(KeyCode::Char('m'));
    app.key(KeyCode::Down);
    app.key(KeyCode::Char('m'));
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
    let original = preview::Preview::original(b"plain\r\n", &theme::Theme::default());
    assert_eq!(original.lines[0].to_string(), "plain\\r");

    let directory = fixture();
    let path = directory.path().join("one.rs");
    let old = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, old.replace('\n', "\r\n")).unwrap();
    let captured = capture(directory.path(), &["HEAD"]);
    let preview = preview::Preview::item(
        directory.path(),
        &captured.items[0],
        &theme::Theme::default(),
    );
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
    let preview = preview::Preview::original("e\u{301}👩‍💻Z\n".as_bytes(), &theme::Theme::default());
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
    let preview = preview::Preview::item(
        directory.path(),
        &captured.items[0],
        &theme::Theme::default(),
    );
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
fn no_index_marks_follow_captured_file_bytes_and_invalidate_on_change() {
    let directory = tempfile::tempdir().unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let left = directory.path().join("left");
    let right = directory.path().join("right");
    std::fs::write(&left, b"old side\n").unwrap();
    std::fs::write(&right, b"new side\n").unwrap();
    let args = vec!["--no-index".into(), "left".into(), "right".into()];
    let captured = git::capture(directory.path(), &args).unwrap();
    assert_eq!(captured.items.len(), 1);
    assert!(captured.items[0].canonical_identity.is_some());
    let store = state::Store::at(state_dir.path(), directory.path(), &args);
    let mut app = App::new(captured, store);
    app.key(KeyCode::Char('m'));
    assert_eq!(app.marks, [true]);
    assert_eq!(
        App::new(
            git::capture(directory.path(), &args).unwrap(),
            state::Store::at(state_dir.path(), directory.path(), &args)
        )
        .marks,
        [true]
    );
    let absolute_args = vec![
        "--no-index".into(),
        left.as_os_str().to_owned(),
        right.as_os_str().to_owned(),
    ];
    let absolute = git::capture(directory.path(), &absolute_args).unwrap();
    assert!(absolute.items[0].canonical_identity.is_some());
    std::fs::write(&right, b"changed side\n").unwrap();
    assert_eq!(
        App::new(
            git::capture(directory.path(), &args).unwrap(),
            state::Store::at(state_dir.path(), directory.path(), &args)
        )
        .marks,
        [false]
    );
}

#[test]
fn identical_mode_only_patch_with_changed_sides_clears_mark() {
    use std::os::unix::fs::PermissionsExt;
    let directory = fixture();
    let state_dir = tempfile::tempdir().unwrap();
    git_run(directory.path(), &["switch", "-c", "left"]);
    git_run(directory.path(), &["update-ref", "refs/heads/left", "HEAD"]);
    git_run(directory.path(), &["switch", "-c", "right"]);
    std::fs::set_permissions(
        directory.path().join("one.rs"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    git_run(directory.path(), &["add", "one.rs"]);
    git_run(directory.path(), &["commit", "-qm", "executable side"]);
    git_run(
        directory.path(),
        &["update-ref", "refs/heads/right", "HEAD"],
    );
    let args = vec!["left".into(), "right".into()];
    let first = git::capture(directory.path(), &args).unwrap();
    assert_eq!(first.items.len(), 1);
    assert!(first.items[0].patch.windows(9).any(|w| w == b"old mode "));
    let first_patch = first.items[0].patch.clone();
    let store = state::Store::at(state_dir.path(), directory.path(), &args);
    let mut app = App::new(first, store);
    app.key(KeyCode::Char('m'));
    assert!(app.marks[0]);

    git_run(directory.path(), &["switch", "left"]);
    std::fs::write(directory.path().join("one.rs"), b"same new bytes\n").unwrap();
    git_run(directory.path(), &["add", "one.rs"]);
    git_run(directory.path(), &["commit", "-qm", "left content"]);
    git_run(directory.path(), &["update-ref", "refs/heads/left", "HEAD"]);
    git_run(directory.path(), &["switch", "right"]);
    std::fs::write(directory.path().join("one.rs"), b"same new bytes\n").unwrap();
    git_run(directory.path(), &["add", "one.rs"]);
    git_run(directory.path(), &["commit", "-qm", "right content"]);
    git_run(
        directory.path(),
        &["update-ref", "refs/heads/right", "HEAD"],
    );
    let second = git::capture(directory.path(), &args).unwrap();
    assert_eq!(second.items[0].patch, first_patch);
    assert_eq!(
        App::new(
            second,
            state::Store::at(state_dir.path(), directory.path(), &args)
        )
        .marks,
        [false]
    );
}

#[test]
fn ignored_content_changes_cannot_use_metadata_only_identity() {
    use std::os::unix::fs::PermissionsExt;
    let directory = fixture();
    std::fs::set_permissions(
        directory.path().join("one.rs"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(
        directory.path().join("one.rs"),
        "fn one() {\n\tlet    answer = 1;\n}\n",
    )
    .unwrap();
    let args = ["-w".into(), "HEAD".into()];
    let first = git::capture(directory.path(), &args).unwrap();
    assert_eq!(first.items.len(), 1);
    let first_patch = first.items[0].patch.clone();
    let first_identity = first.items[0].canonical_identity.clone().unwrap();
    std::fs::write(
        directory.path().join("one.rs"),
        "fn one() {\n\tlet      answer = 1;\n}\n",
    )
    .unwrap();
    let second = git::capture(directory.path(), &args).unwrap();
    let strip_index = |patch: &[u8]| {
        patch
            .split_inclusive(|byte| *byte == b'\n')
            .filter(|line| !line.starts_with(b"index "))
            .flatten()
            .copied()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        strip_index(&second.items[0].patch),
        strip_index(&first_patch)
    );
    assert!(second.items[0].canonical_identity.is_some());
    assert_ne!(
        second.items[0].canonical_identity.as_ref(),
        Some(&first_identity)
    );
}

#[test]
fn unrelated_commit_movement_preserves_unchanged_item_mark() {
    let directory = fixture();
    let state_dir = tempfile::tempdir().unwrap();
    git_run(directory.path(), &["tag", "review-base"]);
    std::fs::write(directory.path().join("one.rs"), b"committed change\n").unwrap();
    git_run(directory.path(), &["add", "one.rs"]);
    git_run(directory.path(), &["commit", "-qm", "reviewed change"]);
    let args = vec!["review-base".into(), "HEAD".into()];
    let mut app = App::new(
        git::capture(directory.path(), &args).unwrap(),
        state::Store::at(state_dir.path(), directory.path(), &args),
    );
    assert_eq!(app.capture.items.len(), 1);
    app.key(KeyCode::Char('m'));
    assert!(app.marks[0]);

    std::fs::write(directory.path().join("unrelated.txt"), b"unrelated\n").unwrap();
    git_run(directory.path(), &["add", "unrelated.txt"]);
    git_run(directory.path(), &["commit", "-qm", "unrelated movement"]);
    let restarted = App::new(
        git::capture(directory.path(), &args).unwrap(),
        state::Store::at(state_dir.path(), directory.path(), &args),
    );
    assert_eq!(restarted.capture.items.len(), 2);
    let reviewed = restarted
        .capture
        .items
        .iter()
        .position(|item| item.new_path == b"one.rs")
        .unwrap();
    assert!(restarted.marks[reviewed]);
}

#[test]
fn staged_content_change_clears_only_its_mark() {
    let directory = fixture();
    let state_dir = tempfile::tempdir().unwrap();
    change(directory.path());
    git_run(directory.path(), &["add", "."]);
    let args = vec!["--cached".into()];
    let mut app = App::new(
        git::capture(directory.path(), &args).unwrap(),
        state::Store::at(state_dir.path(), directory.path(), &args),
    );
    app.key(KeyCode::Char('m'));
    app.key(KeyCode::Down);
    app.key(KeyCode::Char('m'));
    assert_eq!(app.marks, [true, true]);
    std::fs::write(directory.path().join("one.rs"), b"different staged bytes\n").unwrap();
    git_run(directory.path(), &["add", "one.rs"]);
    assert_eq!(
        App::new(
            git::capture(directory.path(), &args).unwrap(),
            state::Store::at(state_dir.path(), directory.path(), &args)
        )
        .marks,
        [false, true]
    );
}

#[test]
fn mutations_at_capture_boundary_disable_proof_but_post_capture_marks_stay_bound() {
    let directory = fixture();
    let state_dir = tempfile::tempdir().unwrap();
    change(directory.path());
    let args = vec![OsString::from("HEAD")];
    let changed_path = directory.path().join("one.rs");
    let unstable = git::capture_with_hook(directory.path(), &args, || {
        std::fs::write(&changed_path, b"raced after Git compared the file\n").unwrap();
    })
    .unwrap();
    let item = unstable
        .items
        .iter()
        .find(|item| item.new_path == b"one.rs")
        .unwrap();
    assert!(item.canonical_identity.is_none());
    let mut app = App::new(
        unstable,
        state::Store::at(state_dir.path(), directory.path(), &args),
    );
    app.key(KeyCode::Char('m'));
    assert!(!app.marks[app.selected]);
    assert!(app.status.contains("could not be proven"));

    std::fs::write(&changed_path, b"captured before later mutation\n").unwrap();
    let captured = git::capture(directory.path(), &args).unwrap();
    assert!(captured.items[0].canonical_identity.is_some());
    let mut app = App::new(
        captured,
        state::Store::at(state_dir.path(), directory.path(), &args),
    );
    std::fs::write(&changed_path, b"changed after capture\n").unwrap();
    app.key(KeyCode::Char('m'));
    assert!(app.marks[0]);
    assert_eq!(
        App::new(
            git::capture(directory.path(), &args).unwrap(),
            state::Store::at(state_dir.path(), directory.path(), &args)
        )
        .marks,
        [false, false]
    );
}

#[test]
fn legacy_session_path_is_read_and_unproven_marks_stay_pending() {
    let directory = fixture();
    let base = tempfile::tempdir().unwrap();
    change(directory.path());
    let args = vec![OsString::from("HEAD")];
    let store = state::Store::at(base.path(), directory.path(), &args);
    let captured = git::capture(directory.path(), &args).unwrap();
    let legacy_marks: std::collections::BTreeMap<_, _> = captured
        .items
        .iter()
        .map(|item| (state::digest([&item.new_path]), "unverifiable-legacy-mark"))
        .collect();
    std::fs::create_dir_all(store.legacy_path.parent().unwrap()).unwrap();
    std::fs::write(
        &store.legacy_path,
        serde_json::to_vec(&serde_json::json!({"version": 1, "marks": legacy_marks})).unwrap(),
    )
    .unwrap();
    let loaded = store.load_report(&captured.items).unwrap();
    assert_eq!(loaded.marks, [false, false]);
    assert!(loaded.pending_legacy);
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
    app.key(KeyCode::Char('m'));
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
        .map(|bytes| {
            preview::Preview::original(bytes, &theme::Theme::default()).lines[0].to_string()
        })
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
    app.key(KeyCode::Char('m'));
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
        app.key(KeyCode::Char('m'));
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

#[test]
fn staged_metadata_marks_use_base_tree_and_index_for_modes_and_exact_renames() {
    use std::os::unix::fs::PermissionsExt;
    for reverse in [false, true] {
        let directory = fixture();
        let state_dir = tempfile::tempdir().unwrap();
        let args: Vec<OsString> = if reverse {
            vec!["--cached".into(), "-R".into(), "HEAD".into()]
        } else {
            vec!["--cached".into()]
        };
        for name in ["one.rs", "two.rs"] {
            std::fs::set_permissions(
                directory.path().join(name),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        git_run(directory.path(), &["add", "."]);
        let before = snapshot(directory.path());
        let first = git::capture(directory.path(), &args).unwrap();
        assert_eq!(snapshot(directory.path()), before);
        if reverse {
            // Native reverse headers use b/ then a/: keep faithful fallback.
            assert!(first.unavailable.is_some());
            continue;
        }
        assert!(
            first
                .items
                .iter()
                .all(|item| item.canonical_identity.is_some())
        );
        let patches: Vec<_> = first.items.iter().map(|item| item.patch.clone()).collect();
        let mut app = App::new(
            first,
            state::Store::at(state_dir.path(), directory.path(), &args),
        );
        app.key(KeyCode::Char('m'));
        app.key(KeyCode::Down);
        app.key(KeyCode::Char('m'));
        assert_eq!(app.marks, [true, true]);
        assert_eq!(
            state::Store::at(state_dir.path(), directory.path(), &args)
                .load(&git::capture(directory.path(), &args).unwrap().items)
                .unwrap(),
            [true, true]
        );
        // Move the base and index to new bytes while preserving both patches.
        for name in ["one.rs", "two.rs"] {
            std::fs::set_permissions(
                directory.path().join(name),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
        }
        std::fs::write(
            directory.path().join("one.rs"),
            b"different complete base bytes\n",
        )
        .unwrap();
        git_run(directory.path(), &["add", "."]);
        git_run(directory.path(), &["commit", "-qm", "new base contents"]);
        for name in ["one.rs", "two.rs"] {
            std::fs::set_permissions(
                directory.path().join(name),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        git_run(directory.path(), &["add", "."]);
        let changed = git::capture(directory.path(), &args).unwrap();
        assert_eq!(
            changed
                .items
                .iter()
                .map(|item| item.patch.clone())
                .collect::<Vec<_>>(),
            patches
        );
        assert_eq!(
            state::Store::at(state_dir.path(), directory.path(), &args)
                .load(&changed.items)
                .unwrap(),
            [false, true]
        );
    }
    let directory = fixture();
    let state_dir = tempfile::tempdir().unwrap();
    git_run(directory.path(), &["mv", "one.rs", "renamed.rs"]);
    let args = vec!["--cached".into(), "HEAD".into()];
    let mut app = App::new(
        git::capture(directory.path(), &args).unwrap(),
        state::Store::at(state_dir.path(), directory.path(), &args),
    );
    assert!(app.capture.items[0].status.starts_with('R'));
    assert!(app.capture.items[0].canonical_identity.is_some());
    app.key(KeyCode::Char('m'));
    let store = state::Store::at(state_dir.path(), directory.path(), &args);
    assert_eq!(
        store
            .load(&git::capture(directory.path(), &args).unwrap().items)
            .unwrap(),
        [true]
    );
    std::fs::write(
        directory.path().join("renamed.rs"),
        b"changed rename destination\n",
    )
    .unwrap();
    git_run(directory.path(), &["add", "renamed.rs"]);
    assert!(
        store
            .load(&git::capture(directory.path(), &args).unwrap().items)
            .unwrap()
            .iter()
            .all(|mark| !mark)
    );
}

#[test]
fn subdirectory_worktree_capture_uses_native_root_relative_paths() {
    use std::os::unix::ffi::OsStringExt;
    let directory = fixture();
    let state_dir = tempfile::tempdir().unwrap();
    let sub = directory.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let name = OsString::from_vec(b" leading-\xff trailing ".to_vec());
    let file = sub.join(&name);
    std::fs::write(&file, b"old\n").unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "nested native file"]);
    std::fs::write(&file, b"captured\n").unwrap();
    let args = vec!["HEAD".into()];
    let mut app = App::new(
        git::capture(&sub, &args).unwrap(),
        state::Store::at(state_dir.path(), &sub, &args),
    );
    assert_eq!(app.capture.items.len(), 1);
    assert!(app.capture.items[0].canonical_identity.is_some());
    app.key(KeyCode::Char('m'));
    assert_eq!(app.marks, [true]);
    let store = state::Store::at(state_dir.path(), &sub, &args);
    assert_eq!(
        store
            .load(&git::capture(&sub, &args).unwrap().items)
            .unwrap(),
        [true]
    );
    std::fs::write(&file, b"changed\n").unwrap();
    assert_eq!(
        store
            .load(&git::capture(&sub, &args).unwrap().items)
            .unwrap(),
        [false]
    );
}

#[test]
fn pending_legacy_survives_v2_saves_and_migrated_unmarks_never_resurrect() {
    let directory = fixture();
    let state_dir = tempfile::tempdir().unwrap();
    change(directory.path());
    let args = vec!["HEAD".into()];
    let captured = git::capture(directory.path(), &args).unwrap();
    let store = state::Store::at(state_dir.path(), directory.path(), &args);
    let mut legacy = std::collections::BTreeMap::new();
    for item in &captured.items {
        legacy.insert(
            state::digest([&item.new_path]),
            item.legacy_fingerprints[0].clone(),
        );
    }
    legacy.insert(
        "absent-unverifiable-item".to_owned(),
        "abbreviated-patch-only".to_owned(),
    );
    let bytes = serde_json::to_vec(&serde_json::json!({"version": 1, "marks": legacy})).unwrap();
    std::fs::create_dir_all(store.legacy_path.parent().unwrap()).unwrap();
    std::fs::write(&store.legacy_path, &bytes).unwrap();
    git_run(directory.path(), &["restore", "two.rs"]);
    let mut app = App::new(git::capture(directory.path(), &args).unwrap(), store);
    assert_eq!(app.marks, [true]);
    assert!(app.status.contains("pending"));
    app.key(KeyCode::Char('m')); // Persist explicit unmark of migrated one.rs.
    assert_eq!(app.marks, [false]);
    assert!(app.store.path.exists());
    let loaded = app
        .store
        .load_report(&git::capture(directory.path(), &args).unwrap().items)
        .unwrap();
    assert_eq!(loaded.marks, [false]);
    assert!(loaded.pending_legacy);
    change(directory.path()); // Previously absent two.rs can now prove its legacy identity.
    let loaded = app
        .store
        .load_report(&git::capture(directory.path(), &args).unwrap().items)
        .unwrap();
    assert_eq!(loaded.marks, [false, true]);
    assert!(loaded.pending_legacy);
    assert_eq!(std::fs::read(&app.store.legacy_path).unwrap(), bytes);
    app.store
        .save(
            &git::capture(directory.path(), &args).unwrap().items,
            &[false, false],
        )
        .unwrap();
    assert_eq!(
        app.store
            .load(&git::capture(directory.path(), &args).unwrap().items)
            .unwrap(),
        [false, false]
    );
    // A corrupt legacy file is still a protected input after v2 exists.
    let v2_bytes = std::fs::read(&app.store.path).unwrap();
    std::fs::write(&app.store.legacy_path, b"unknown corrupt input").unwrap();
    assert!(app.store.load_report(&captured.items).is_err());
    assert!(app.store.save(&captured.items, &[true, true]).is_err());
    assert_eq!(std::fs::read(&app.store.path).unwrap(), v2_bytes);
    assert_eq!(
        std::fs::read(&app.store.legacy_path).unwrap(),
        b"unknown corrupt input"
    );
}

#[test]
fn capture_boundary_index_and_metadata_ref_movement_leave_proof_unavailable() {
    use std::os::unix::fs::PermissionsExt;
    let directory = fixture();
    std::fs::set_permissions(
        directory.path().join("one.rs"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    git_run(directory.path(), &["add", "one.rs"]);
    let captured = git::capture_with_hook(directory.path(), &["--cached".into()], || {
        git_run(directory.path(), &["update-index", "--chmod=-x", "one.rs"]);
    })
    .unwrap();
    assert_eq!(captured.items.len(), 1);
    assert!(captured.items[0].canonical_identity.is_none());
    git_run(directory.path(), &["tag", "before"]);
    git_run(directory.path(), &["add", "one.rs"]);
    git_run(directory.path(), &["commit", "-qm", "executable"]);
    let captured =
        git::capture_with_hook(directory.path(), &["before".into(), "HEAD".into()], || {
            git_run(
                directory.path(),
                &["update-ref", "refs/tags/before", "HEAD"],
            );
        })
        .unwrap();
    assert_eq!(captured.items.len(), 1);
    assert!(captured.items[0].canonical_identity.is_none());
    assert!(
        captured
            .diagnostic
            .contains("complete captured-content proof")
    );
}

#[test]
fn implicit_no_index_and_symlink_operands_have_captured_identities() {
    use std::os::unix::fs::symlink;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("left"), b"left\n").unwrap();
    std::fs::write(directory.path().join("right"), b"right\n").unwrap();
    for args in [
        &["left", "right"][..],
        &["--no-index", "--", "left", "right"],
    ] {
        let captured = capture(directory.path(), args);
        assert!(captured.items[0].canonical_identity.is_some());
    }
    std::fs::create_dir(directory.path().join("target")).unwrap();
    symlink("missing-target", directory.path().join("link-left")).unwrap();
    symlink("other-target", directory.path().join("link-right")).unwrap();
    let captured = capture(directory.path(), &["--no-index", "link-left", "link-right"]);
    assert_eq!(captured.items.len(), 1);
    assert_eq!(captured.items[0].old_mode, "120000");
    assert!(captured.items[0].canonical_identity.is_some());
}

#[test]
fn relative_git_context_worker() {
    let Some(cwd) = std::env::var_os("REVIEW_RELATIVE_CONTEXT_FIXTURE") else {
        return;
    };
    let cwd = Path::new(&cwd);
    let captured = capture(cwd, &["--cached", "HEAD"]);
    assert_eq!(captured.items.len(), 1);
    assert_eq!(captured.items[0].new_path, b"sub/file");
    assert!(captured.items[0].canonical_identity.is_some());
    let captured = capture(cwd, &[]);
    assert_eq!(captured.items.len(), 1);
    assert_eq!(captured.items[0].new_path, b"sub/file");
    assert!(captured.items[0].canonical_identity.is_some());
}

#[test]
fn subdirectory_relative_alternate_index_and_git_paths_keep_original_context() {
    use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};
    let directory = fixture();
    let sub = directory.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("file"), b"base\n").unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "nested"]);
    let native_index = OsString::from_vec(b" index-\xff ".to_vec());
    std::fs::copy(
        directory.path().join(".git/index"),
        directory.path().join(&native_index),
    )
    .unwrap();
    std::fs::set_permissions(sub.join("file"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let status = Command::new("git")
        .current_dir(&sub)
        .env("GIT_INDEX_FILE", &native_index)
        .env("GIT_DIR", "../.git")
        .env("GIT_WORK_TREE", "..")
        .args(["add", "file"])
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::write(sub.join("file"), b"working tree change\n").unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "tests::relative_git_context_worker",
            "--nocapture",
        ])
        .env("REVIEW_RELATIVE_CONTEXT_FIXTURE", &sub)
        .env("GIT_INDEX_FILE", &native_index)
        .env("GIT_DIR", "../.git")
        .env("GIT_WORK_TREE", "..")
        .current_dir(&sub)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn no_index_mode_only_and_empty_files_prove_complete_sides() {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("left"), b"same bytes\n").unwrap();
    std::fs::write(directory.path().join("right"), b"same bytes\n").unwrap();
    std::fs::set_permissions(
        directory.path().join("right"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let captured = capture(directory.path(), &["--no-index", "left", "right"]);
    assert_eq!(captured.items.len(), 1);
    assert!(captured.items[0].canonical_identity.is_some());
    std::fs::create_dir(directory.path().join("old")).unwrap();
    std::fs::create_dir(directory.path().join("new")).unwrap();
    std::fs::write(directory.path().join("new/empty"), []).unwrap();
    let captured = capture(directory.path(), &["--no-index", "old", "new"]);
    assert_eq!(captured.items.len(), 1);
    assert!(captured.items[0].canonical_identity.is_some());
}

#[test]
fn worktree_exact_rename_mark_restarts_and_invalidates_changed_destination() {
    let directory = fixture();
    let state_dir = tempfile::tempdir().unwrap();
    git_run(directory.path(), &["mv", "one.rs", "renamed.rs"]);
    let args = vec!["HEAD".into()];
    let captured = git::capture(directory.path(), &args).unwrap();
    assert_eq!(captured.items.len(), 1);
    assert_eq!(captured.items[0].status, "R100");
    assert!(captured.items[0].old_oid.is_empty());
    assert!(captured.items[0].new_oid.is_empty());
    let mut app = App::new(
        captured,
        state::Store::at(state_dir.path(), directory.path(), &args),
    );
    app.key(KeyCode::Char('m'));
    assert_eq!(app.marks, [true]);
    let store = state::Store::at(state_dir.path(), directory.path(), &args);
    assert_eq!(
        store
            .load(&git::capture(directory.path(), &args).unwrap().items)
            .unwrap(),
        [true]
    );
    std::fs::write(
        directory.path().join("renamed.rs"),
        b"changed rename destination\n",
    )
    .unwrap();
    let changed = git::capture(directory.path(), &args).unwrap();
    assert!(!changed.items.is_empty());
    assert!(store.load(&changed.items).unwrap().iter().all(|mark| !mark));
}

#[test]
fn captured_permissions_match_git_owner_execute_for_worktree_and_no_index() {
    use std::os::unix::fs::PermissionsExt;
    let directory = fixture();
    std::fs::write(directory.path().join("one.rs"), b"changed content\n").unwrap();
    std::fs::set_permissions(
        directory.path().join("one.rs"),
        std::fs::Permissions::from_mode(0o654),
    )
    .unwrap();
    let captured = capture(directory.path(), &["HEAD"]);
    assert_eq!(captured.items.len(), 1);
    assert_eq!(captured.items[0].new_mode, "100644");
    assert!(captured.items[0].canonical_identity.is_some());

    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("left"), b"old content\n").unwrap();
    std::fs::write(directory.path().join("right"), b"new content\n").unwrap();
    for name in ["left", "right"] {
        std::fs::set_permissions(
            directory.path().join(name),
            std::fs::Permissions::from_mode(0o654),
        )
        .unwrap();
    }
    let captured = capture(directory.path(), &["--no-index", "left", "right"]);
    assert_eq!(captured.items.len(), 1);
    assert_eq!(captured.items[0].old_mode, "100644");
    assert_eq!(captured.items[0].new_mode, "100644");
    assert!(captured.items[0].canonical_identity.is_some());
}

fn navigation_query(
    catalog: &source::Catalog,
    operation: navigation::Operation,
    name: &str,
) -> navigation::Results {
    navigation::Analysis::build(catalog).query(
        catalog,
        navigation::Request {
            generation: 1,
            position: None,
            operation,
            query: name.into(),
            side: None,
        },
    )
}
#[test]
fn navigation_fixture_operations_cover_every_declared_language_and_shell_dialect() {
    for name in [
        "sample.rs",
        "sample.c",
        "sample.h",
        "sample.cpp",
        "sample.go",
        "sample.zig",
        "sample.py",
        "sample.sh",
        "sample.bash",
    ] {
        let bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/navigation")
                .join(name),
        )
        .unwrap();
        let mut catalog = source::Catalog::default();
        catalog.insert(
            source::ComparedSide::Old,
            name.as_bytes().to_vec(),
            bytes.clone(),
        );
        catalog.insert(
            source::ComparedSide::New,
            name.as_bytes().to_vec(),
            bytes.clone(),
        );
        for operation in [
            navigation::Operation::Search,
            navigation::Operation::Definitions,
            navigation::Operation::References,
        ] {
            let result = navigation_query(&catalog, operation, "target");
            assert!(
                result.candidates.len() >= 2,
                "{name} {operation:?}: {result:?}"
            );
            assert!(result.coverage.contains("Partial"));
            for candidate in result.candidates {
                let text = std::str::from_utf8(&catalog.sources[candidate.source].bytes).unwrap();
                let actual: String = text
                    .lines()
                    .nth(candidate.tag.line as usize - 1)
                    .unwrap()
                    .chars()
                    .skip(candidate.tag.col as usize)
                    .take(candidate.tag.len as usize)
                    .collect();
                assert_eq!(actual, "target", "{name} {operation:?}");
            }
        }
    }
}
#[test]
fn navigation_formats_keep_nested_arrays_duplicate_positions_and_yaml_aliases() {
    for name in ["sample.yaml", "sample.json"] {
        let mut catalog = source::Catalog::default();
        catalog.insert(
            source::ComparedSide::New,
            name.as_bytes().to_vec(),
            std::fs::read(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/navigation")
                    .join(name),
            )
            .unwrap(),
        );
        let ports = navigation_query(&catalog, navigation::Operation::Definitions, "port");
        assert!(ports.candidates.len() >= 2);
        let positions: std::collections::BTreeSet<_> = ports
            .candidates
            .iter()
            .map(|c| (c.tag.line, c.tag.col))
            .collect();
        assert_eq!(positions.len(), ports.candidates.len());
        assert!(
            ports.candidates[0]
                .tag
                .context
                .as_deref()
                .unwrap()
                .contains("[\"service\"][\"port\"]")
        );
        let names = navigation_query(&catalog, navigation::Operation::Definitions, "name");
        assert_eq!(names.candidates.len(), 2);
        assert!(
            names.candidates[0]
                .tag
                .context
                .as_deref()
                .unwrap()
                .contains("[0]")
        );
        assert!(
            names.candidates[1]
                .tag
                .context
                .as_deref()
                .unwrap()
                .contains("[1]")
        );
        if name.ends_with("yaml") {
            assert_eq!(
                navigation_query(&catalog, navigation::Operation::Definitions, "shared")
                    .candidates
                    .len(),
                1
            );
            assert_eq!(
                navigation_query(&catalog, navigation::Operation::References, "shared")
                    .candidates
                    .len(),
                1
            );
            assert_eq!(
                navigation_query(&catalog, navigation::Operation::Definitions, "line\nkey")
                    .candidates
                    .len(),
                1
            );
        } else {
            assert!(
                navigation_query(&catalog, navigation::Operation::References, "port")
                    .candidates
                    .is_empty()
            );
        }
    }
}
#[test]
fn navigation_reports_unsupported_recovered_invalid_and_resource_limited_sources() {
    let mut catalog = source::Catalog::default();
    for (name, bytes) in [
        ("other.sh", b"#!/bin/zsh -f\ntarget() {}".to_vec()),
        (
            "env.sh",
            b"#!/usr/bin/env -S fish -x\nfunction target; end".to_vec(),
        ),
        ("file.rs", vec![0xff]),
        ("huge.rs", vec![b'a'; navigation::MAX_FILE_BYTES + 1]),
        ("broken.json", b"{\"port\": 1, broken".to_vec()),
    ] {
        catalog.insert(source::ComparedSide::New, name.as_bytes().to_vec(), bytes);
    }
    let result = navigation_query(&catalog, navigation::Operation::Search, "");
    for reason in [
        "unsupported language/shell dialect",
        "invalid UTF-8",
        "resource limit",
        "recovered syntax errors",
    ] {
        assert!(
            result.coverage.contains(reason),
            "{reason}: {}",
            result.coverage
        );
    }
}
#[test]
fn captured_navigation_searches_unchanged_both_sides_and_survives_later_mutation() {
    let directory = fixture();
    change(directory.path());
    let captured = capture(directory.path(), &["HEAD", "--", "one.rs"]);
    std::fs::write(directory.path().join("two.rs"), "fn moved() {}\n").unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "moved"]);
    let results = navigation_query(&captured.catalog, navigation::Operation::Definitions, "two");
    assert_eq!(results.candidates.len(), 2, "{results:?}");
    assert_ne!(
        captured.catalog.sources[results.candidates[0].source].side,
        captured.catalog.sources[results.candidates[1].source].side
    );
    assert!(
        results
            .candidates
            .iter()
            .all(|c| captured.catalog.sources[c.source].path == b"two.rs")
    );
    assert!(
        navigation_query(
            &captured.catalog,
            navigation::Operation::Definitions,
            "moved"
        )
        .candidates
        .is_empty()
    );
}
#[test]
fn no_index_navigation_includes_unchanged_directory_targets() {
    let directory = tempfile::tempdir().unwrap();
    for side in ["left", "right"] {
        std::fs::create_dir(directory.path().join(side)).unwrap();
        std::fs::write(
            directory.path().join(side).join("target.rs"),
            "fn target() {}\n",
        )
        .unwrap();
    }
    std::fs::write(directory.path().join("left/change.rs"), "fn old() {}\n").unwrap();
    std::fs::write(directory.path().join("right/change.rs"), "fn new() {}\n").unwrap();
    let captured = capture(directory.path(), &["--no-index", "left", "right"]);
    let results = navigation_query(
        &captured.catalog,
        navigation::Operation::Definitions,
        "target",
    );
    assert_eq!(results.candidates.len(), 2, "{results:?}");
    assert_eq!(
        captured.catalog.sources[results.candidates[0].source].path,
        b"left/target.rs"
    );
    assert_eq!(
        captured.catalog.sources[results.candidates[1].source].path,
        b"right/target.rs"
    );
}
fn await_navigation(app: &mut App) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app
        .navigation
        .as_ref()
        .is_some_and(|nav| nav.coverage.is_empty())
    {
        app.poll_analysis();
        assert!(std::time::Instant::now() < deadline, "worker timed out");
        std::thread::yield_now();
    }
}
#[test]
fn navigation_controls_open_candidates_go_back_choose_old_side_and_never_mark() {
    let directory = fixture();
    change(directory.path());
    let state_dir = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state_dir.path());
    app.key(KeyCode::Enter);
    app.key(KeyCode::Char('/'));
    for character in "two".chars() {
        app.key(KeyCode::Char(character));
    }
    app.key(KeyCode::Enter);
    await_navigation(&mut app);
    assert_eq!(app.navigation.as_ref().unwrap().candidates.len(), 4);
    app.key(KeyCode::Enter);
    assert!(
        app.preview
            .as_ref()
            .unwrap()
            .reason
            .as_deref()
            .unwrap()
            .contains("old two.rs")
    );
    app.key(KeyCode::Char(']'));
    assert!(
        app.preview
            .as_ref()
            .unwrap()
            .reason
            .as_deref()
            .unwrap()
            .contains("new two.rs")
    );
    app.key(KeyCode::Char('g'));
    app.key(KeyCode::Char('d'));
    await_navigation(&mut app);
    assert_eq!(app.navigation.as_ref().unwrap().candidates.len(), 1);
    app.key_event(crossterm::event::KeyEvent::new(
        KeyCode::Char('o'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    app.key_event(crossterm::event::KeyEvent::new(
        KeyCode::Char('o'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    assert!(app.navigation.is_some());
    app.key_event(crossterm::event::KeyEvent::new(
        KeyCode::Char('o'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    assert!(app.preview.is_some());
    assert!(app.marks.iter().all(|mark| !mark));
    assert!(!state_dir.path().read_dir().unwrap().any(|_| true));
}
#[test]
fn navigation_selection_changes_cannot_publish_obsolete_worker_results() {
    let directory = fixture();
    change(directory.path());
    let state_dir = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state_dir.path());
    app.key(KeyCode::Char('/'));
    app.key(KeyCode::Enter);
    app.key(KeyCode::Esc);
    app.key(KeyCode::Char('j'));
    for _ in 0..1000 {
        app.poll_analysis();
        std::thread::yield_now();
    }
    assert!(app.preview.is_none());
    assert!(app.navigation.is_none());
    assert_eq!(app.selected, 1);
    assert!(app.marks.iter().all(|mark| !mark));
    for (width, height) in [(0, 0), (1, 1), (4, 2), (12, 6), (120, 40)] {
        screen(&mut app, width, height);
    }
}

#[test]
fn terminal_lifecycle_worker() {
    let Ok(scenario) = std::env::var("REVIEW_TERMINAL_SCENARIO") else {
        return;
    };
    let cwd = std::env::current_dir().unwrap();
    let capture = capture(&cwd, &["HEAD"]);
    let state_dir = tempfile::tempdir().unwrap();
    let mut app = App::new(
        capture,
        state::Store::at(state_dir.path(), &cwd, &["HEAD".into()]),
    );
    let result = ui::run(&mut app);
    if scenario == "error" {
        assert!(result.unwrap_err().to_string().contains("injected"));
    } else {
        result.unwrap();
    }
}
#[test]
fn navigation_deleted_renamed_native_paths_and_missing_newlines_keep_captured_targets() {
    use std::os::unix::ffi::OsStringExt;
    let directory = fixture();
    let name = OsString::from_vec(b"odd-\xff.rs".to_vec());
    std::fs::write(directory.path().join(&name), "fn unique() {}").unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "native"]);
    std::fs::remove_file(directory.path().join(&name)).unwrap();
    std::fs::rename(
        directory.path().join("two.rs"),
        directory.path().join("renamed.rs"),
    )
    .unwrap();
    git_run(directory.path(), &["add", "-A"]);
    let captured = capture(directory.path(), &["--cached"]);
    let result = navigation_query(
        &captured.catalog,
        navigation::Operation::Definitions,
        "unique",
    );
    assert_eq!(result.candidates.len(), 1);
    let target = &captured.catalog.sources[result.candidates[0].source];
    assert_eq!(target.path, b"odd-\xff.rs");
    assert_eq!(target.side, source::ComparedSide::Old);
    assert_eq!(target.bytes.as_ref(), b"fn unique() {}");
    let twos = navigation_query(&captured.catalog, navigation::Operation::Definitions, "two");
    assert_eq!(twos.candidates.len(), 2);
    assert!(
        twos.candidates
            .iter()
            .any(|c| captured.catalog.sources[c.source].path == b"two.rs")
    );
    assert!(
        twos.candidates
            .iter()
            .any(|c| captured.catalog.sources[c.source].path == b"renamed.rs")
    );
}
#[test]
fn navigation_range_and_merge_base_catalogs_include_unchanged_sources() {
    let directory = fixture();
    change(directory.path());
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "second"]);
    for args in [
        vec!["HEAD~..HEAD"],
        vec!["HEAD~...HEAD"],
        vec!["--merge-base", "HEAD~", "HEAD"],
    ] {
        let captured = capture(directory.path(), &args);
        let result = navigation_query(&captured.catalog, navigation::Operation::Definitions, "two");
        assert_eq!(result.candidates.len(), 2, "{args:?}: {result:?}");
    }
}

#[test]
fn navigation_search_accepts_native_path_substrings_without_collapsing_candidates() {
    let mut catalog = source::Catalog::default();
    catalog.insert(
        source::ComparedSide::Old,
        b"nested/definition.rs".to_vec(),
        b"fn target() {}\n".to_vec(),
    );
    catalog.insert(
        source::ComparedSide::New,
        b"nested/definition.rs".to_vec(),
        b"fn target() {}\n".to_vec(),
    );
    let result = navigation_query(&catalog, navigation::Operation::Search, "nested/def");
    assert_eq!(result.candidates.len(), 2);
    assert_eq!(
        catalog.sources[result.candidates[0].source].side,
        source::ComparedSide::Old
    );
    assert_eq!(
        catalog.sources[result.candidates[1].source].side,
        source::ComparedSide::New
    );
}

#[test]
fn navigation_catalog_acquisition_limits_unchanged_blobs_before_retention() {
    let directory = fixture();
    for index in 0..9 {
        std::fs::write(
            directory.path().join(format!("a{index}.rs")),
            vec![b' '; 1024 * 1024],
        )
        .unwrap();
    }
    std::fs::write(
        directory.path().join("a-large.rs"),
        vec![b'x'; 1024 * 1024 + 1],
    )
    .unwrap();
    for index in 0..1030 {
        std::fs::write(
            directory.path().join(format!("b{index:04}.rs")),
            "fn tiny() {}\n",
        )
        .unwrap();
    }
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "catalog limits"]);
    change(directory.path());
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "small change"]);
    let captured = assert_native(directory.path(), &["HEAD~", "HEAD", "--", "one.rs"]);
    assert_eq!(captured.items.len(), 1);
    assert!(captured.items[0].canonical_identity.is_some());
    assert!(
        captured
            .catalog
            .sources
            .iter()
            .all(|s| s.bytes.len() <= 1024 * 1024)
    );
    assert!(
        !captured
            .catalog
            .sources
            .iter()
            .any(|s| s.path == b"a-large.rs")
    );
    assert!(
        captured
            .catalog
            .gaps
            .iter()
            .any(|gap| gap.contains("byte limit"))
    );
    let late = navigation_query(&captured.catalog, navigation::Operation::Search, "b1029.rs");
    assert_eq!(late.candidates.len(), 2, "{late:?}");
    // Supported targets beyond the former 1024-entry cutoff survive.
    assert!(
        captured
            .catalog
            .sources
            .iter()
            .any(|source| source.path == b"b1029.rs")
    );
    assert!(
        captured
            .catalog
            .sources
            .iter()
            .map(|s| s.bytes.len())
            .sum::<usize>()
            <= 128 * 1024 * 1024
    );
}

#[test]
fn every_preview_reports_omissions_and_excluded_targets_are_refused() {
    let many_rows = "x\n".repeat(20001);
    let original = preview::Preview::original(many_rows.as_bytes(), &theme::Theme::default());
    assert!(original.reason.unwrap().contains("omitted rows"));
    let directory = fixture();
    change(directory.path());
    let mut opaque = capture(directory.path(), &["HEAD"]);
    opaque.unavailable = Some("Original output: file actions unavailable".into());
    let state_dir = tempfile::tempdir().unwrap();
    let mut opaque_app = App::new(
        opaque,
        state::Store::at(state_dir.path(), directory.path(), &[]),
    );
    opaque_app.preview = Some(preview::Preview::original(
        many_rows.as_bytes(),
        &theme::Theme::default(),
    ));
    assert!(screen_text(&screen(&mut opaque_app, 120, 20)).contains("Display omitted rows"));
    let mut captured = capture(directory.path(), &["HEAD"]);
    captured.items[0].patch = format!("{}\n", "界".repeat(4100)).into_bytes();
    let basic = preview::Preview::item(
        directory.path(),
        &captured.items[0],
        &theme::Theme::default(),
    );
    assert!(basic.reason.unwrap().contains("omitted columns"));
    let original_wide =
        preview::Preview::original("界".repeat(4100).as_bytes(), &theme::Theme::default());
    assert!(original_wide.reason.unwrap().contains("omitted columns"));
    std::fs::write(
        directory.path().join("one.rs"),
        format!("// {}\nfn wide() {{}}\n", "界".repeat(4100)),
    )
    .unwrap();
    let enriched_capture = capture(directory.path(), &["HEAD"]);
    let enriched = preview::Preview::item(
        directory.path(),
        &enriched_capture.items[0],
        &theme::Theme::default(),
    );
    let reason = enriched.reason.unwrap();
    assert!(reason.contains("omitted columns"));
    assert!(!reason.contains("unavailable"));
    captured.items[0].patch = "x\n".repeat(140000).into_bytes();
    let large_basic = preview::Preview::item(
        directory.path(),
        &captured.items[0],
        &theme::Theme::default(),
    );
    let reason = large_basic.reason.unwrap();
    assert!(reason.contains("omitted rows") && reason.contains("Large patch"));
    let mut catalog = source::Catalog::default();
    catalog.insert(
        source::ComparedSide::New,
        b"late.rs".to_vec(),
        format!("{}fn late() {{}}\n", "\n".repeat(20000)).into_bytes(),
    );
    assert!(
        preview::Preview::target(&catalog.sources[0], 20001, 3, 4, &theme::Theme::default())
            .unwrap_err()
            .to_string()
            .contains("20000-row")
    );
    catalog.insert(
        source::ComparedSide::New,
        b"wide.rs".to_vec(),
        format!("{}fn late() {{}}\n", " ".repeat(4100)).into_bytes(),
    );
    assert!(
        preview::Preview::target(&catalog.sources[1], 1, 4103, 4, &theme::Theme::default())
            .unwrap_err()
            .to_string()
            .contains("4096-grapheme")
    );
}

#[test]
fn navigation_new_search_editing_rejects_inflight_results() {
    let directory = fixture();
    change(directory.path());
    let state_dir = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state_dir.path());
    app.key(KeyCode::Char('/'));
    app.key(KeyCode::Char('t'));
    app.key(KeyCode::Enter);
    app.key(KeyCode::Char('/'));
    app.key(KeyCode::Char('z'));
    // Let the old worker answer while the new query is still being edited.
    let until = std::time::Instant::now() + std::time::Duration::from_millis(100);
    while std::time::Instant::now() < until {
        app.poll_analysis();
        std::thread::yield_now();
    }
    let nav = app.navigation.as_ref().unwrap();
    assert!(nav.editing);
    assert_eq!(nav.query, "z");
    assert!(nav.candidates.is_empty());
    assert!(nav.coverage.is_empty());
}

#[test]
fn navigation_horizontal_movement_uses_cursor_after_target_opening() {
    let directory = fixture();
    std::fs::write(
        directory.path().join("one.rs"),
        "fn first() {} fn second() {}\n",
    )
    .unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state_dir.path());
    app.key(KeyCode::Char('/'));
    for ch in "first".chars() {
        app.key(KeyCode::Char(ch));
    }
    app.key(KeyCode::Enter);
    await_navigation(&mut app);
    app.key(KeyCode::Enter);
    app.viewport = (1, 1);
    for _ in 0..14 {
        app.key(KeyCode::Right);
    }
    app.key(KeyCode::Char('g'));
    app.key(KeyCode::Char('d'));
    await_navigation(&mut app);
    assert_eq!(app.navigation.as_ref().unwrap().query, "second");
    app.key(KeyCode::Enter);
    for _ in 0..14 {
        app.key(KeyCode::Left);
    }
    app.key(KeyCode::Char('g'));
    app.key(KeyCode::Char('d'));
    await_navigation(&mut app);
    assert_eq!(app.navigation.as_ref().unwrap().query, "first");
}

#[test]
fn navigation_side_switch_discards_previous_target_coordinates() {
    let directory = fixture();
    std::fs::write(
        directory.path().join("one.rs"),
        format!("{}fn late() {{}}\n", "\n".repeat(30)),
    )
    .unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "old location"]);
    std::fs::write(directory.path().join("one.rs"), "fn early() {}\n").unwrap();
    let state_dir = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state_dir.path());
    app.key(KeyCode::Char('/'));
    for ch in "late".chars() {
        app.key(KeyCode::Char(ch));
    }
    app.key(KeyCode::Enter);
    await_navigation(&mut app);
    app.key(KeyCode::Enter);
    assert_eq!(app.top, 30);
    app.viewport = (1, 1);
    app.key(KeyCode::Char(']'));
    assert_eq!(app.top, 0);
    app.key(KeyCode::Char('g'));
    app.key(KeyCode::Char('d'));
    await_navigation(&mut app);
    assert_eq!(app.navigation.as_ref().unwrap().query, "early");
}

#[test]
fn review_fix_target_retention_counts_escaped_backslashes() {
    let mut catalog = source::Catalog::default();
    catalog.insert(
        source::ComparedSide::New,
        b"slashes.rs".to_vec(),
        format!("{}fn target() {{}}\n", "\\".repeat(2500)).into_bytes(),
    );
    let source = &catalog.sources[0];
    let preview = preview::Preview::source(source, &theme::Theme::default());
    assert!(!preview.lines[0].to_string().contains("target"));
    let error = preview::Preview::target(source, 1, 2503, 6, &theme::Theme::default()).unwrap_err();
    assert!(error.to_string().contains("4096-grapheme"));
    catalog.insert(
        source::ComparedSide::New,
        b"retained.rs".to_vec(),
        format!("{}fn target() {{}}\n", "\\".repeat(2000)).into_bytes(),
    );
    let (preview, left) =
        preview::Preview::target(&catalog.sources[1], 1, 2003, 6, &theme::Theme::default())
            .unwrap();
    assert_eq!(left, 4003);
    assert!(
        preview.visible(0, left, 1, 20)[0]
            .to_string()
            .starts_with("target")
    );
}

#[test]
fn review_fix_multiline_candidate_fields_keep_one_row_and_selection_visible() {
    let directory = fixture();
    let path = "multi\nname.json";
    std::fs::write(
        directory.path().join(path),
        r#"{"key\nline":0,"second":1,"third":2,"fourth":3}"#,
    )
    .unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "multiline candidates"]);
    change(directory.path());
    let state_dir = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state_dir.path());
    app.key(KeyCode::Char('/'));
    app.key(KeyCode::Enter);
    await_navigation(&mut app);
    let nav = app.navigation.as_ref().unwrap();
    assert!(
        nav.candidates
            .iter()
            .any(|c| c.tag.name.as_ref() == "key\nline")
    );
    assert!(
        app.capture
            .catalog
            .sources
            .iter()
            .any(|s| s.path == path.as_bytes())
    );
    assert_eq!(
        app.preview.as_ref().unwrap().lines.len(),
        nav.candidates.len() + 2
    );
    assert!(
        app.preview
            .as_ref()
            .unwrap()
            .lines
            .iter()
            .any(|line| line.to_string().contains("multi\\\\nname.json"))
    );
    screen(&mut app, 120, 12);
    let count = app.navigation.as_ref().unwrap().candidates.len();
    for _ in 1..count {
        app.key(KeyCode::Down);
    }
    let nav = app.navigation.as_ref().unwrap();
    assert_eq!(nav.selected, count - 1);
    let selected_row = nav.selected + 2;
    assert!(selected_row >= app.top && selected_row < app.top + app.viewport.0);
    assert!(screen_text(&screen(&mut app, 120, 12)).contains("> new"));
}

#[test]
fn review_fix_structural_depth_limit_is_explicit() {
    let mut catalog = source::Catalog::default();
    catalog.insert(
        source::ComparedSide::New,
        b"deep.json".to_vec(),
        format!("{}{{\"key\":0}}{}", "[".repeat(140), "]".repeat(140)).into_bytes(),
    );
    let results = navigation_query(&catalog, navigation::Operation::Search, "");
    assert!(
        results
            .coverage
            .contains("structural depth limit (128), truncated")
    );
    assert!(results.candidates.is_empty());
}

#[test]
fn review_fix_structural_tag_limit_is_explicit_at_exact_cap() {
    let mut catalog = source::Catalog::default();
    let json = format!("{{\n{}\"end\":0\n}}", "\"k\":0,\n".repeat(100000));
    assert!(json.len() < navigation::MAX_FILE_BYTES);
    catalog.insert(
        source::ComparedSide::New,
        b"tags.json".to_vec(),
        json.into_bytes(),
    );
    let results = navigation_query(&catalog, navigation::Operation::Search, "end");
    assert!(
        results
            .coverage
            .contains("structural tag limit (100000), truncated")
    );
    assert!(results.candidates.is_empty());
}

fn mouse_event(
    kind: crossterm::event::MouseEventKind,
    x: u16,
    y: u16,
) -> crossterm::event::MouseEvent {
    crossterm::event::MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: crossterm::event::KeyModifiers::NONE,
    }
}

#[test]
fn cursor_cell_maps_share_tabs_unicode_escapes_and_clipping() {
    let mut catalog = source::Catalog::default();
    catalog.insert(
        source::ComparedSide::New,
        b"cells.rs".to_vec(),
        "\t界e\u{301}\\\u{1b}a\n\n".as_bytes().to_vec(),
    );
    let preview = preview::Preview::source(&catalog.sources[0], &theme::Theme::default());
    assert_eq!(preview.lines[0].to_string(), "    界e\u{301}\\\\\\u{1b}a");
    for (cell, scalar) in [
        (0, 0),
        (3, 0),
        (4, 1),
        (5, 1),
        (6, 2),
        (7, 4),
        (8, 4),
        (9, 5),
        (14, 5),
        (15, 6),
    ] {
        assert_eq!(preview.hit_test(0, cell), Some(scalar), "cell {cell}");
    }
    assert_eq!(preview.hit_test(0, 16), None);
    assert_eq!(preview.hit_test(1, 0), None);
    assert_eq!(preview.hit_test(2, 0), None);
    assert_eq!(preview.visible_hit(0, 5, 5, 10), None);
    assert_eq!(preview.visible_hit(0, 4, 4, 1), None);
    assert_eq!(preview.visible_hit(0, 5, 4, 2), Some(1));
    catalog.insert(
        source::ComparedSide::New,
        b"bad.rs".to_vec(),
        vec![0xff, b'a', b'\n'],
    );
    let invalid = preview::Preview::source(&catalog.sources[1], &theme::Theme::default());
    assert!(
        invalid
            .locations
            .iter()
            .all(|position| *position == (None, None))
    );
    assert!(invalid.reason.unwrap().contains("invalid UTF-8"));
    catalog.insert(
        source::ComparedSide::New,
        b"omitted.rs".to_vec(),
        "界".repeat(4100).into_bytes(),
    );
    let omitted = preview::Preview::source(&catalog.sources[2], &theme::Theme::default());
    assert_eq!(omitted.hit_test(0, 8191), Some(4095));
    assert_eq!(omitted.hit_test(0, 8192), None);
    assert!(omitted.reason.unwrap().contains("omitted columns"));
    catalog.insert(
        source::ComparedSide::New,
        b"combining.rs".to_vec(),
        "\\\u{301}x".as_bytes().to_vec(),
    );
    let combining = preview::Preview::source(&catalog.sources[3], &theme::Theme::default());
    assert_eq!(combining.lines[0].to_string(), "\\\\\u{301}x");
    assert_eq!(combining.hit_test(0, 2), Some(2));
}

#[test]
fn diff_cursor_navigation_uses_exact_captured_side_and_history() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let directory = fixture();
    std::fs::write(directory.path().join("one.rs"), "\tfn old_symbol() {}\n").unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "old"]);
    std::fs::write(directory.path().join("one.rs"), "\tfn new_symbol() {}\n").unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Enter);
    screen(&mut app, 100, 24);
    let old_row = app
        .preview
        .as_ref()
        .unwrap()
        .locations
        .iter()
        .position(|row| *row == (Some(1), None))
        .unwrap();
    let new_row = app
        .preview
        .as_ref()
        .unwrap()
        .locations
        .iter()
        .position(|row| *row == (None, Some(1)))
        .unwrap();
    app.cursor = (0, 0);
    app.key(KeyCode::Char('g'));
    app.key(KeyCode::Char('d'));
    assert!(app.navigation.is_none());
    assert!(app.status.contains("no mapped"));
    // The prefix is unmapped, including when a side is available.
    app.cursor = (old_row, 0);
    assert_eq!(app.side, source::ComparedSide::New);
    app.key(KeyCode::Char('g'));
    app.key(KeyCode::Char('d'));
    assert!(app.navigation.is_none());
    assert!(app.status.contains("no mapped"));
    // A tab starts after the diff prefix: 1 + 4 + "fn ".
    let x = app.preview_area.x + 8;
    let y = app.preview_area.y + old_row as u16;
    app.mouse(
        mouse_event(MouseEventKind::Down(MouseButton::Left), x, y),
        std::time::Instant::now(),
    );
    let saved = (app.cursor, app.top, app.left);
    app.mouse(
        mouse_event(
            MouseEventKind::Down(MouseButton::Left),
            app.preview_area.right() - 1,
            y,
        ),
        std::time::Instant::now(),
    );
    assert_eq!((app.cursor, app.top, app.left), saved);
    assert!(app.status.contains("No retained text"));
    app.mouse(
        mouse_event(
            MouseEventKind::Down(MouseButton::Left),
            x,
            app.preview_area.bottom() - 1,
        ),
        std::time::Instant::now(),
    );
    assert_eq!((app.cursor, app.top, app.left), saved);
    // A live edit cannot replace the captured symbol.
    std::fs::write(directory.path().join("one.rs"), "fn unrelated_live() {}\n").unwrap();
    app.key(KeyCode::Char('g'));
    app.key(KeyCode::Char('d'));
    await_navigation(&mut app);
    assert_eq!(app.navigation.as_ref().unwrap().query, "old_symbol");
    assert_eq!(app.navigation.as_ref().unwrap().candidates.len(), 1);
    app.key_event(crossterm::event::KeyEvent::new(
        KeyCode::Char('o'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    assert_eq!((app.cursor, app.top, app.left), saved);
    app.cursor = (new_row, 8);
    app.key(KeyCode::Char(']'));
    app.key(KeyCode::Char('g'));
    app.key(KeyCode::Char('r'));
    await_navigation(&mut app);
    assert_eq!(app.navigation.as_ref().unwrap().query, "new_symbol");
    app.key_event(crossterm::event::KeyEvent::new(
        KeyCode::Char('o'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    // A mapped whitespace cell cannot search forward to the next identifier.
    app.cursor = (new_row, 5 + 2);
    app.key(KeyCode::Char('g'));
    app.key(KeyCode::Char('d'));
    await_navigation(&mut app);
    assert!(app.navigation.as_ref().unwrap().query.is_empty());
    assert!(app.navigation.as_ref().unwrap().candidates.is_empty());
    assert!(app.status.contains("No symbol at cursor"));
    assert!(app.marks.iter().all(|mark| !mark));
    assert!(state.path().read_dir().unwrap().next().is_none());
    assert_eq!(
        std::fs::read_to_string(directory.path().join("one.rs")).unwrap(),
        "fn unrelated_live() {}\n"
    );
}

#[test]
fn mouse_files_scroll_without_selecting_and_require_timed_same_file_opening() {
    use crossterm::event::{MouseButton, MouseEventKind};
    use std::time::{Duration, Instant};
    let directory = fixture();
    for index in 0..15 {
        std::fs::write(
            directory.path().join(format!("extra{index:02}.rs")),
            "fn old() {}\n",
        )
        .unwrap();
    }
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "many files"]);
    for index in 0..15 {
        std::fs::write(
            directory.path().join(format!("extra{index:02}.rs")),
            "fn new() {}\n",
        )
        .unwrap();
    }
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    screen(&mut app, 80, 12);
    let (x, y) = (app.file_area.x, app.file_area.y);
    let now = Instant::now();
    app.mouse(mouse_event(MouseEventKind::ScrollDown, x, y), now);
    assert_eq!(app.file_offset, 3);
    assert_eq!(app.selected, 0);
    assert_eq!(app.focus, Focus::Files);
    assert!(app.preview.is_none());
    screen(&mut app, 80, 12);
    let click = mouse_event(MouseEventKind::Down(MouseButton::Left), x, y);
    app.mouse(click, now);
    assert_eq!(app.selected, 3);
    assert!(app.preview.is_none());
    app.mouse(click, now + Duration::from_millis(401));
    assert!(app.preview.is_none());
    app.mouse(click, now + Duration::from_millis(700));
    assert!(app.preview.is_some());
    assert_eq!(app.focus, Focus::Preview);
    screen(&mut app, 80, 12);
    app.mouse(mouse_event(MouseEventKind::ScrollDown, x, y), now);
    assert_eq!(app.focus, Focus::Preview);
    assert_eq!(app.selected, 3);
    assert!(app.preview.is_some());
    app.mouse(click, now + Duration::from_secs(1));
    assert!(app.preview.is_none());
    assert_eq!(app.focus, Focus::Files);
    assert!(app.marks.iter().all(|mark| !mark));
}

#[test]
fn cursor_wheel_resize_and_focus_movement_preserve_viewport_bounds() {
    use crossterm::event::MouseEventKind;
    let directory = fixture();
    std::fs::write(
        directory.path().join("one.rs"),
        "fn target() {}\n".repeat(50),
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Enter);
    screen(&mut app, 80, 12);
    let before = app.cursor;
    app.key(KeyCode::Char('l'));
    assert_eq!(app.cursor.1, before.1 + 1);
    app.key(KeyCode::Char('h'));
    assert_eq!(app.cursor, before);
    assert!(app.preview.is_some());
    app.key(KeyCode::Tab);
    let now = std::time::Instant::now();
    app.mouse(
        mouse_event(
            MouseEventKind::ScrollDown,
            app.preview_area.x,
            app.preview_area.y,
        ),
        now,
    );
    assert_eq!(app.focus, Focus::Files);
    assert_eq!(app.selected, 0);
    assert_eq!(app.top, 3);
    assert!(app.cursor.0 >= app.top && app.cursor.0 < app.top + app.viewport.0);
    for (width, height) in [(1, 1), (3, 2), (10, 7), (100, 30)] {
        screen(&mut app, width, height);
        assert!(app.cursor.0 < app.preview.as_ref().unwrap().lines.len());
        if app.viewport.0 > 0 {
            assert!(app.cursor.0 >= app.top && app.cursor.0 < app.top + app.viewport.0);
        }
        if app.viewport.1 > 0 {
            assert!(app.cursor.1 >= app.left && app.cursor.1 < app.left + app.viewport.1);
        }
    }
    app.key(KeyCode::Esc);
    assert!(app.preview.is_none());
    assert!(app.marks.iter().all(|mark| !mark));
}

#[test]
fn review_fix_target_retention_uses_expanded_tab_mapping() {
    let mut catalog = source::Catalog::default();
    for tabs in [1024, 1000] {
        catalog.insert(
            source::ComparedSide::New,
            format!("tabs{tabs}.rs").into_bytes(),
            format!("{}fn target() {{}}\n", "\t".repeat(tabs)).into_bytes(),
        );
    }
    let excluded = &catalog.sources[0];
    assert!(
        !preview::Preview::source(excluded, &theme::Theme::default()).lines[0]
            .to_string()
            .contains("target")
    );
    assert!(
        preview::Preview::target(excluded, 1, 1027, 6, &theme::Theme::default())
            .unwrap_err()
            .to_string()
            .contains("4096-grapheme")
    );
    let (preview, left) =
        preview::Preview::target(&catalog.sources[1], 1, 1003, 6, &theme::Theme::default())
            .unwrap();
    assert_eq!(left, 4003);
    assert!(
        preview.visible(0, left, 1, 20)[0]
            .to_string()
            .starts_with("target")
    );
}

#[test]
fn vim_motions_counts_and_escape_preserve_read_only_state() {
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.preview = Some(preview::Preview::original(
        b"  alpha.beta  gamma\n  second line\nlast\n",
        &theme::Theme::default(),
    ));
    app.focus = Focus::Preview;
    app.viewport = (2, 12);
    app.key(KeyCode::Char('w'));
    assert_eq!(app.cursor, (0, 2));
    app.key(KeyCode::Char('e'));
    assert_eq!(app.cursor, (0, 6));
    app.key(KeyCode::Char('w'));
    assert_eq!(app.cursor, (0, 7));
    app.key(KeyCode::Char('b'));
    assert_eq!(app.cursor, (0, 2));
    app.key(KeyCode::Char('W'));
    assert_eq!(app.cursor, (0, 14));
    app.key(KeyCode::Char('B'));
    assert_eq!(app.cursor, (0, 2));
    app.key(KeyCode::Char('$'));
    assert_eq!(app.cursor, (0, 18));
    app.key(KeyCode::Char('0'));
    assert_eq!(app.cursor, (0, 0));
    app.key(KeyCode::Char('^'));
    assert_eq!(app.cursor, (0, 2));
    app.key_event(crossterm::event::KeyEvent::new(
        KeyCode::Char('d'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    assert_eq!(app.top, 1);
    app.key_event(crossterm::event::KeyEvent::new(
        KeyCode::Char('u'),
        crossterm::event::KeyModifiers::CONTROL,
    ));
    assert_eq!(app.top, 0);
    app.key(KeyCode::Char('g'));
    app.key(KeyCode::Char('g'));
    for c in "2j".chars() {
        app.key(KeyCode::Char(c));
    }
    assert_eq!(app.cursor.0, 2);
    for c in "gg".chars() {
        app.key(KeyCode::Char(c));
    }
    assert_eq!(app.cursor.0, 0);
    for c in "2gg".chars() {
        app.key(KeyCode::Char(c));
    }
    assert_eq!(app.cursor.0, 1);
    app.key(KeyCode::Char('G'));
    assert_eq!(app.cursor.0, 2);
    assert!(!app.key(KeyCode::Esc));
    assert!(app.preview.is_none());
    assert_eq!(app.focus, Focus::Files);
    assert!(!app.key(KeyCode::Esc));
    assert!(app.key(KeyCode::Char('q')));
    assert!(app.marks.iter().all(|mark| !mark));
    assert!(state.path().read_dir().unwrap().next().is_none());
}

#[test]
fn source_targets_highlight_every_supported_language_without_changing_cell_maps() {
    for name in [
        "sample.rs",
        "sample.c",
        "sample.h",
        "sample.cpp",
        "sample.go",
        "sample.py",
        "sample.zig",
        "sample.sh",
        "sample.bash",
        "sample.yaml",
        "sample.json",
    ] {
        let bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/navigation")
                .join(name),
        )
        .unwrap();
        let mut catalog = source::Catalog::default();
        catalog.insert(
            source::ComparedSide::Old,
            name.as_bytes().to_vec(),
            bytes.clone(),
        );
        let rendered = preview::Preview::source(&catalog.sources[0], &theme::Theme::default());
        assert!(
            rendered
                .lines
                .iter()
                .flat_map(|line| &line.spans)
                .any(|span| span.style.fg.is_some()),
            "{name} has no syntax colors"
        );
        assert!(rendered.title.as_ref().unwrap().contains(name));
        for (row, cells) in rendered.cells.iter().enumerate() {
            assert_eq!(cells.len(), rendered.lines[row].width());
        }
    }
}

#[test]
fn vim_definition_jump_is_highlighted_and_footer_keeps_analysis_details_on_demand() {
    use crossterm::event::{KeyEvent, KeyModifiers};
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Char('/'));
    for ch in "two".chars() {
        app.key(KeyCode::Char(ch));
    }
    app.key(KeyCode::Enter);
    await_navigation(&mut app);
    let text = screen_text(&screen(&mut app, 120, 24));
    assert!(!text.contains("Partial name/syntax"));
    assert!(!text.contains("never binding"));
    app.key(KeyCode::Enter);
    assert!(
        app.preview
            .as_ref()
            .unwrap()
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.style.fg.is_some())
    );
    for c in "gd".chars() {
        app.key(KeyCode::Char(c));
    }
    await_navigation(&mut app);
    assert_eq!(app.navigation.as_ref().unwrap().candidates.len(), 1);
    app.key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    assert!(app.navigation.is_none());
    assert!(
        app.preview
            .as_ref()
            .unwrap()
            .title
            .as_ref()
            .unwrap()
            .contains("two.rs")
    );
    for c in "gr".chars() {
        app.key(KeyCode::Char(c));
    }
    await_navigation(&mut app);
    assert_eq!(
        app.navigation.as_ref().unwrap().operation,
        navigation::Operation::References
    );
    app.key(KeyCode::Char('?'));
    assert!(screen_text(&screen(&mut app, 120, 24)).contains("Partial name/syntax"));
    assert!(!app.key(KeyCode::Esc));
    assert!(app.preview.is_none());
}

#[test]
fn vim_star_search_wraps_whole_words_with_counts_and_keeps_source_styles() {
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    let mut catalog = source::Catalog::default();
    catalog.insert(
        source::ComparedSide::New,
        b"search.rs".to_vec(),
        "界e\u{301} foo foo_bar foo\nfoo\nFOO\n".as_bytes().to_vec(),
    );
    app.preview = Some(preview::Preview::source(
        &catalog.sources[0],
        &theme::Theme::named("gruvbox-dark").unwrap(),
    ));
    app.focus = Focus::Preview;
    app.viewport = (2, 10);
    app.cursor = (0, 5);
    let original_foregrounds: Vec<_> = app.preview.as_ref().unwrap().visible(0, 0, 3, 30)[0]
        .spans
        .iter()
        .map(|span| span.style.fg)
        .collect();
    app.key(KeyCode::Char('*'));
    assert_eq!(app.cursor, (0, 16));
    let preview = app.preview.as_ref().unwrap();
    assert_eq!(preview.search.as_ref().unwrap().matches.len(), 3);
    let displayed = preview.visible(0, 0, 3, 30);
    assert!(
        displayed[0]
            .spans
            .iter()
            .any(|span| span.style.bg.is_some())
    );
    assert_eq!(
        displayed[0]
            .spans
            .iter()
            .map(|span| span.style.fg)
            .collect::<Vec<_>>(),
        original_foregrounds
    );
    assert_eq!(preview.hit_test(0, 16), Some(16));
    app.key(KeyCode::Char('n'));
    assert_eq!(app.cursor, (1, 0));
    app.key(KeyCode::Char('n'));
    assert_eq!(app.cursor, (0, 4));
    app.key(KeyCode::Char('N'));
    assert_eq!(app.cursor, (1, 0));
    app.cursor = (0, 18);
    app.key(KeyCode::Char('#'));
    assert_eq!(app.cursor, (0, 4));
    app.key(KeyCode::Char('n'));
    assert_eq!(app.cursor, (1, 0));
    app.key(KeyCode::Char('N'));
    assert_eq!(app.cursor, (0, 4));
    for c in "2*".chars() {
        app.key(KeyCode::Char(c));
    }
    assert_eq!(app.cursor, (1, 0));
    assert!(app.marks.iter().all(|m| !m));
    assert!(state.path().read_dir().unwrap().next().is_none());
    assert!(app.navigation.is_none());
}

#[test]
fn buffer_search_is_local_to_the_retained_view_and_restored_with_history() {
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.preview = Some(preview::Preview::original(
        b"foo foo\n",
        &theme::Theme::default(),
    ));
    app.focus = Focus::Preview;
    app.viewport = (10, 30);
    app.key(KeyCode::Char('*'));
    assert_eq!(app.cursor, (0, 4));
    app.key(KeyCode::Char('/'));
    app.key(KeyCode::Esc);
    app.key(KeyCode::Char('n'));
    assert_eq!(app.cursor, (0, 0));
    app.key(KeyCode::Esc);
    app.key(KeyCode::Enter);
    app.key(KeyCode::Char('n'));
    assert_eq!(app.status, "No previous buffer search");
    assert!(app.preview.as_ref().unwrap().search.is_none());
    app.preview = Some(preview::Preview::original(b" \n", &theme::Theme::default()));
    app.key(KeyCode::Char('*'));
    assert_eq!(app.status, "No word under cursor");
    assert!(app.preview.as_ref().unwrap().search.is_none());
    app.preview = Some(preview::Preview::original(
        b"\xce\xb1\xce\xb2 \xce\xb1\xce\xb2 \xce\xb1\xce\xb2_\xce\xb3\n",
        &theme::Theme::default(),
    ));
    app.cursor = (0, 0);
    app.key(KeyCode::Char('*'));
    assert_eq!(app.cursor, (0, 3));
    assert_eq!(
        app.preview
            .as_ref()
            .unwrap()
            .search
            .as_ref()
            .unwrap()
            .matches
            .len(),
        2
    );
    app.key(KeyCode::Char('n'));
    assert_eq!(app.cursor, (0, 0));
}

#[test]
fn star_search_uses_vim_keyword_boundaries_including_underscores() {
    let lines = [ratatui::text::Line::from("foo'bar foo foo_bar _ _")];
    let (search, origin) = crate::search::BufferSearch::at_cursor(&lines, (0, 1), false).unwrap();
    assert_eq!(search.word, "foo");
    assert_eq!(search.matches.len(), 2);
    assert_eq!(search.next(origin, false, 1).unwrap().start, 8);
    let (search, origin) = crate::search::BufferSearch::at_cursor(&lines, (0, 20), false).unwrap();
    assert_eq!(search.word, "_");
    assert_eq!(search.next(origin, false, 1).unwrap().start, 22);
}

#[test]
fn configurable_keymaps_validate_and_replace_every_default() {
    use crossterm::event::{KeyEvent, KeyModifiers};
    use keybindings::{Action, Keymap, Resolution};
    let config = config::Config::parse("[features]\nkeymap = 'emacs'\nreadline = false\n[keybindings]\ncursor_down = 'z'\nclose_diff_view = ''\ngo_to_definition = 'gD'\n").unwrap();
    let mut bindings = config.bindings(Some(Keymap::Vim)).unwrap();
    assert_eq!(bindings.keymap, Keymap::Vim);
    for key in [
        KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL),
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    ] {
        assert!(matches!(bindings.resolve(key, true), Resolution::Unbound));
    }
    assert!(matches!(
        bindings.resolve(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE), true),
        Resolution::Action(Action::CursorDown)
    ));
    assert!(matches!(
        bindings.resolve(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE), true),
        Resolution::Pending
    ));
    assert!(matches!(
        bindings.resolve(KeyEvent::new(KeyCode::Char('D'), KeyModifiers::SHIFT), true),
        Resolution::Action(Action::GoToDefinition)
    ));
    for keymap in [Keymap::Vim, Keymap::Emacs] {
        let mut defaults = keybindings::Bindings::new(keymap, true, &Default::default()).unwrap();
        for (code, expected) in [
            (KeyCode::Enter, Action::OpenDiffView),
            (KeyCode::Char(' '), Action::OpenDiffView),
            (KeyCode::Char('m'), Action::ToggleMark),
        ] {
            assert!(
                matches!(defaults.resolve(KeyEvent::new(code, KeyModifiers::NONE), false), Resolution::Action(found) if found == expected)
            );
        }
        assert!(defaults.help().contains("open_diff_view: enter, space"));
        assert!(defaults.help().contains("toggle_mark: m"));
        let mut custom =
            config::Config::parse("[keybindings]\nopen_diff_view='o'\ntoggle_mark='space'")
                .unwrap()
                .bindings(Some(keymap))
                .unwrap();
        assert!(matches!(
            custom.resolve(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), false),
            Resolution::Unbound
        ));
        assert!(matches!(
            custom.resolve(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE), false),
            Resolution::Unbound
        ));
        assert!(matches!(
            custom.resolve(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), false),
            Resolution::Action(Action::ToggleMark)
        ));
        let mut disabled =
            config::Config::parse("[keybindings]\nopen_diff_view=''\ntoggle_mark=''")
                .unwrap()
                .bindings(Some(keymap))
                .unwrap();
        for code in [KeyCode::Enter, KeyCode::Char(' '), KeyCode::Char('m')] {
            assert!(matches!(
                disabled.resolve(KeyEvent::new(code, KeyModifiers::NONE), false),
                Resolution::Unbound
            ));
        }
    }
    for invalid in [
        "[features]\nkeymap='unknown'",
        "[features]\nunknown=true",
        "[keybindings]\nunknown='x'",
        "[keybindings]\ncursor_down='k'",
        "[keybindings]\ntoggle_mark='space'",
        "[keybindings]\nopen_diff_view='m'",
        "[keybindings]\ncursor_down='g'",
        "[keybindings]\ncursor_down='ctrl-bogus'",
        "[keybindings]\nquit='ctrl-c'",
    ] {
        assert!(
            config::Config::parse(invalid)
                .and_then(|c| c.bindings(None))
                .is_err(),
            "{invalid}"
        );
    }
}

#[test]
fn configurable_actions_preserve_query_mouse_and_cancelled_prefixes() {
    use crossterm::event::{KeyEvent, KeyModifiers, MouseButton, MouseEventKind};
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.bindings = config::Config::parse(
        "[keybindings]\nopen_diff_view='o'\nclose_diff_view='x'\nsearch_symbols='f'\n",
    )
    .unwrap()
    .bindings(None)
    .unwrap();
    app.key(KeyCode::Enter);
    assert!(app.preview.is_none());
    screen(&mut app, 100, 24);
    let click = mouse_event(
        MouseEventKind::Down(MouseButton::Left),
        app.file_area.x,
        app.file_area.y,
    );
    let now = std::time::Instant::now();
    app.mouse(click, now);
    app.mouse(click, now + std::time::Duration::from_millis(100));
    assert!(app.preview.is_some());
    app.key(KeyCode::Char('g'));
    app.key(KeyCode::Esc);
    assert!(app.preview.is_some());
    app.key(KeyCode::Char('d'));
    assert!(app.navigation.is_none());
    app.key(KeyCode::Esc);
    assert!(app.preview.is_some());
    app.key(KeyCode::Char('f'));
    for c in "gd/qx".chars() {
        app.key(KeyCode::Char(c));
    }
    for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
        app.key_event(KeyEvent::new(KeyCode::Char('z'), modifiers));
    }
    assert_eq!(app.navigation.as_ref().unwrap().query, "gd/qx");
    app.key(KeyCode::Esc);
    assert!(app.preview.is_some());
    app.key(KeyCode::Char('x'));
    assert!(app.preview.is_none());
    assert!(app.marks.iter().all(|mark| !mark));
    let legend = app.bindings.legend();
    assert!(legend.contains("o open") && legend.contains("x close"));
    assert!(!legend.contains("esc"));
}

#[test]
fn emacs_and_optional_readline_have_distinct_precedence() {
    use crossterm::event::{KeyEvent, KeyModifiers};
    use keybindings::{Action, Keymap, Resolution};
    for readline in [true, false] {
        let mut emacs =
            keybindings::Bindings::new(Keymap::Emacs, readline, &Default::default()).unwrap();
        for (ch, action) in [
            ('b', Action::CursorLeft),
            ('f', Action::CursorRight),
            ('a', Action::LineStart),
            ('n', Action::CursorDown),
        ] {
            assert!(
                matches!(emacs.resolve(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL), true), Resolution::Action(found) if found == action)
            );
        }
        let mut vim =
            keybindings::Bindings::new(Keymap::Vim, readline, &Default::default()).unwrap();
        assert!(matches!(
            vim.resolve(
                KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
                true
            ),
            Resolution::Action(Action::PageUp)
        ));
        let resolved = vim.resolve(
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
            true,
        );
        assert_eq!(
            matches!(resolved, Resolution::Action(Action::LineStart)),
            readline
        );
        assert!(matches!(
            vim.resolve(
                KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
                false
            ),
            Resolution::Unbound
        ));
    }
}

#[test]
fn keymap_regression_terminal_equivalent_chords_collide_and_dispatch() {
    use crossterm::event::{KeyEvent, KeyModifiers};
    use keybindings::{Action, Resolution, parse_keys};
    for (chord, named, code, action) in [
        ("ctrl-i", "tab", KeyCode::Tab, Action::SwitchPane),
        ("ctrl-m", "enter", KeyCode::Enter, Action::OpenDiffView),
    ] {
        assert_eq!(parse_keys(chord).unwrap(), parse_keys(named).unwrap());
        let invalid = format!("[keybindings]\nquit='{chord}'");
        let error = config::Config::parse(&invalid)
            .unwrap()
            .bindings(None)
            .err()
            .unwrap();
        assert!(error.to_string().contains("conflicts"), "{error}");
        let valid = format!("[keybindings]\n{}='{chord}'", action.name());
        let mut bindings = config::Config::parse(&valid)
            .unwrap()
            .bindings(None)
            .unwrap();
        assert!(
            matches!(bindings.resolve(KeyEvent::new(code, KeyModifiers::NONE), false), Resolution::Action(found) if found == action)
        );
        let character = if chord == "ctrl-i" { 'i' } else { 'm' };
        assert!(
            matches!(bindings.resolve(KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL), false), Resolution::Action(found) if found == action)
        );
    }
    for unsupported in [
        "ctrl-[", "ctrl-?", "ctrl-@", "ctrl-2", "ctrl-3", "ctrl-8", "ctrl-!", "ctrl-é",
    ] {
        let error = parse_keys(unsupported).unwrap_err();
        assert!(
            error.to_string().contains("unsupported Ctrl character"),
            "{unsupported}: {error}"
        );
    }
    for supported in [
        "ctrl-a",
        "ctrl-Z",
        "ctrl-space",
        "ctrl-4",
        "ctrl-5",
        "ctrl-6",
        "ctrl-7",
        "ctrl-left",
        "alt-[",
        "alt-?",
        "alt-@",
    ] {
        assert!(parse_keys(supported).is_ok(), "{supported}");
    }
    assert_ne!(parse_keys("ctrl-j").unwrap(), parse_keys("enter").unwrap());
    let mut bindings = config::Config::parse("[keybindings]\nquit='ctrl-j'\nnext_file_diff=''")
        .unwrap()
        .bindings(None)
        .unwrap();
    assert!(matches!(
        bindings.resolve(
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
            false
        ),
        Resolution::Action(Action::Quit)
    ));
    assert!(matches!(
        bindings.resolve(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), false),
        Resolution::Action(Action::OpenDiffView)
    ));
}

#[test]
fn keymap_regression_escape_cannot_complete_a_pending_sequence() {
    for sequence in ["g esc", "g escape", "g d esc"] {
        let text = format!("[keybindings]\ngo_to_definition='{sequence}'");
        let error = config::Config::parse(&text)
            .unwrap()
            .bindings(None)
            .err()
            .unwrap();
        assert!(
            error
                .to_string()
                .contains("Escape cancels pending key sequences"),
            "{error}"
        );
    }
    assert!(keybindings::parse_keys("esc").is_ok());
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Enter);
    app.key(KeyCode::Char('g'));
    app.key(KeyCode::Esc);
    assert!(app.preview.is_some());
    app.key(KeyCode::Char('d'));
    assert!(app.navigation.is_none());
    // `d` now starts the `dc` (delete_comment) sequence; Escape cancels it.
    app.key(KeyCode::Esc);
    assert!(app.preview.is_some());
    app.key(KeyCode::Esc);
    assert!(app.preview.is_none());
}

#[test]
fn keymap_regression_help_retains_options_and_prints_effective_bindings() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custom.toml");
    std::fs::write(&path, "[features]\nkeymap='emacs'\n[keybindings]\ncursor_down='z'\nclose_diff_view=''\ngo_to_definition='gD'\n").unwrap();
    for args in [
        vec![
            "--config".into(),
            path.clone().into_os_string(),
            "--help".into(),
        ],
        vec![
            "--help".into(),
            "--config".into(),
            path.clone().into_os_string(),
        ],
    ] {
        let Cli::Help { keymap, config } = parse_args(args).unwrap() else {
            panic!("help options lost")
        };
        assert_eq!(config.as_deref(), Some(path.as_path()));
        let text = help(keymap, config.as_deref()).unwrap();
        assert!(text.contains("Active bindings (Emacs)"));
        assert!(text.contains("cursor_down: z\n"));
        assert!(text.contains("close_diff_view: (disabled)\n"));
        assert!(text.contains("go_to_definition: gD\n"));
        assert!(text.contains("cursor_left: left, ctrl-b\n"));
        assert!(!text.contains("Ctrl-N/P/B/F move"));
        assert!(!text.contains("Escape closes"));
        assert!(!text.contains("ctrl-n"));
    }
    let Cli::Help { keymap, config } = parse_args([
        "--config".into(),
        path.clone().into_os_string(),
        "--keymap".into(),
        "vim".into(),
        "--help".into(),
    ])
    .unwrap() else {
        panic!("help options lost")
    };
    assert_eq!(keymap, Some(keybindings::Keymap::Vim));
    let text = help(keymap, config.as_deref()).unwrap();
    assert!(text.contains("Active bindings (Vim)"));
    assert!(text.contains("cursor_left: left, h\n"));
    assert!(text.contains("cursor_down: z\n"));
    assert!(text.contains("line_end: $, ctrl-e (buffers)\n"));
    assert!(help(None, Some(&directory.path().join("missing.toml"))).is_err());
}

fn navigation_at(
    catalog: &source::Catalog,
    index: &navigation::Analysis,
    source_id: usize,
    line: usize,
    column: usize,
    operation: navigation::Operation,
) -> navigation::Results {
    index.query(
        catalog,
        navigation::Request {
            generation: 1,
            position: Some((source_id, line, column)),
            operation,
            query: String::new(),
            side: Some(catalog.sources[source_id].side),
        },
    )
}

#[test]
fn navigation_syntax_bindings_generic_trait_methods_and_reads_have_exact_scalar_spans() {
    // Positions are independently specified by fixture row and occurrence in that row.
    for (name, declarations, reads) in [
        (
            "bindings.rs",
            vec![
                (2, "convert"),
                (6, "convert"),
                (7, "local"),
                (8, "left"),
                (8, "right"),
                (13, "value"),
                (14, "callback"),
                (18, "café"),
            ],
            vec![
                (7, "input"),
                (8, "local"),
                (9, "left"),
                (14, "transform"),
                (15, "convert"),
                (16, "convert"),
                (17, "callback"),
                (19, "café"),
            ],
        ),
        (
            "bindings.py",
            vec![(1, "parameter"), (2, "local"), (3, "left"), (3, "right")],
            vec![
                (2, "parameter"),
                (3, "local"),
                (4, "local"),
                (4, "left"),
                (4, "right"),
            ],
        ),
        (
            "bindings.go",
            vec![(2, "parameter"), (3, "local"), (4, "left"), (4, "right")],
            vec![
                (3, "parameter"),
                (4, "local"),
                (5, "local"),
                (5, "left"),
                (5, "right"),
            ],
        ),
        (
            "bindings.cpp",
            vec![
                (1, "Alias"),
                (2, "field"),
                (3, "parameter"),
                (4, "local"),
                (5, "record"),
            ],
            vec![(4, "parameter"), (6, "field"), (6, "local"), (7, "local")],
        ),
        (
            "bindings.zig",
            vec![(1, "parameter"), (2, "local")],
            vec![(2, "parameter"), (3, "local")],
        ),
    ] {
        let bytes = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/navigation")
                .join(name),
        )
        .unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        let mut catalog = source::Catalog::default();
        catalog.insert(source::ComparedSide::New, name.as_bytes().to_vec(), bytes);
        let index = navigation::Analysis::build(&catalog);
        for &(line, symbol) in &declarations {
            let row = text.lines().nth(line - 1).unwrap();
            let column = row[..row.find(symbol).unwrap()].chars().count();
            let result = navigation_at(
                &catalog,
                &index,
                0,
                line,
                column,
                navigation::Operation::Definitions,
            );
            assert_eq!(result.query, symbol, "{name}:{line}:{column} {result:?}");
            assert!(
                result.candidates.iter().any(|c| c.tag.line as usize == line
                    && c.tag.col as usize == column
                    && c.tag.len as usize == symbol.chars().count()),
                "{name}:{line} {symbol}: {result:?}"
            );
        }
        for &(line, symbol) in &reads {
            let row = text.lines().nth(line - 1).unwrap();
            let column = row[..row.find(symbol).unwrap()].chars().count();
            let result = navigation_at(
                &catalog,
                &index,
                0,
                line,
                column,
                navigation::Operation::References,
            );
            assert_eq!(result.query, symbol, "{name}:{line} {result:?}");
            assert!(
                result.candidates.iter().any(|c| c.tag.line as usize == line
                    && c.tag.col as usize == column
                    && c.tag.len as usize == symbol.chars().count()),
                "{name}:{line} {symbol}: {result:?}"
            );
            let definitions = navigation_at(
                &catalog,
                &index,
                0,
                line,
                column,
                navigation::Operation::Definitions,
            );
            assert!(
                !definitions.candidates.is_empty(),
                "{name}:{line} gd {symbol}: {definitions:?}"
            );
            if symbol == "convert" {
                assert_eq!(
                    definitions
                        .candidates
                        .iter()
                        .map(|c| c.tag.line)
                        .collect::<Vec<_>>(),
                    vec![2, 6]
                );
            }
        }
        for (line, row) in text.lines().enumerate() {
            if let Some(byte_column) = row.find("ghost") {
                let column = row[..byte_column].chars().count();
                let result = navigation_at(
                    &catalog,
                    &index,
                    0,
                    line + 1,
                    column,
                    navigation::Operation::References,
                );
                assert!(
                    result.query.is_empty() && result.candidates.is_empty(),
                    "{name} comment/string {result:?}"
                );
            }
        }
        let empty = navigation_at(
            &catalog,
            &index,
            0,
            1,
            text.lines().next().unwrap().chars().count(),
            navigation::Operation::Definitions,
        );
        assert!(empty.query.is_empty() && empty.candidates.is_empty());
    }
}

#[test]
fn navigation_acquisition_skips_docs_and_assets_and_shares_detection_across_capture_modes() {
    let directory = fixture();
    for id in 0..1100 {
        std::fs::write(
            directory.path().join(format!("a{id:04}.md")),
            "documentation local target",
        )
        .unwrap();
    }
    std::fs::write(directory.path().join("a-invalid.bin"), [0xff, 0, 0xff]).unwrap();
    for suffix in ["hh", "h++", "ipp", "tpp", "C", "H"] {
        std::fs::write(
            directory.path().join(format!("z-target.{suffix}")),
            "int target() { int local = 1; return local; }\n",
        )
        .unwrap();
    }
    std::fs::write(
        directory.path().join("z-script"),
        "#!/usr/bin/env -S python3 -u\ndef script_target(parameter):\n    return parameter\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("z-shell"),
        "#!/bin/bash\nshell_target() { echo okay; }\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("z-unsupported"),
        "#!/usr/bin/env fish\nfunction excluded; end\n",
    )
    .unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "late sources"]);
    change(directory.path());
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "second"]);
    change(directory.path());
    std::fs::write(
        directory.path().join("one.rs"),
        "fn changed_priority() {}\n",
    )
    .unwrap();
    for args in [
        vec!["HEAD~", "HEAD", "--", "one.rs"],
        vec!["--cached", "--", "one.rs"],
        vec!["HEAD", "--", "one.rs"],
    ] {
        let captured = capture(directory.path(), &args);
        for symbol in ["target", "script_target", "shell_target"] {
            let result = navigation_query(
                &captured.catalog,
                navigation::Operation::Definitions,
                symbol,
            );
            assert!(
                result.candidates.len() >= 2,
                "{args:?} {symbol}: {result:?}"
            );
        }
        assert!(
            !captured
                .catalog
                .sources
                .iter()
                .any(|source| source.path.ends_with(b".md")
                    || source.path.ends_with(b".bin")
                    || source.path == b"z-unsupported")
        );
        if args[0] == "HEAD" {
            assert_eq!(captured.catalog.sources[0].path, b"one.rs");
            assert!(
                navigation_query(
                    &captured.catalog,
                    navigation::Operation::Definitions,
                    "changed_priority"
                )
                .candidates
                .iter()
                .any(|candidate| captured.catalog.sources[candidate.source].side
                    == source::ComparedSide::New)
            );
        }
    }
}

#[test]
fn no_index_navigation_late_sources_survive_documentation_catalogs() {
    let directory = tempfile::tempdir().unwrap();
    for side in ["left", "right"] {
        let root = directory.path().join(side);
        std::fs::create_dir(&root).unwrap();
        for id in 0..1100 {
            std::fs::write(root.join(format!("a{id:04}.md")), "documentation").unwrap();
        }
        std::fs::write(root.join("z-target.rs"), "fn late_target() {}\n").unwrap();
        std::fs::write(root.join("change.rs"), format!("fn {side}() {{}}\n")).unwrap();
    }
    let captured = capture(directory.path(), &["--no-index", "left", "right"]);
    let result = navigation_query(
        &captured.catalog,
        navigation::Operation::Definitions,
        "late_target",
    );
    assert_eq!(result.candidates.len(), 2, "{result:?}");
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|candidate| candidate.tag.line)
            .collect::<Vec<_>>(),
        vec![1, 1]
    );
    assert!(
        captured
            .catalog
            .sources
            .iter()
            .all(|source| !source.path.ends_with(b".md"))
    );
}

fn assert_navigation_occurrences(
    catalog: &source::Catalog,
    index: &navigation::Analysis,
    name: &str,
    definitions: &[(u32, u32)],
    references: &[(u32, u32)],
) {
    for (operation, expected) in [
        (navigation::Operation::Definitions, definitions),
        (navigation::Operation::References, references),
    ] {
        let result = index.query(
            catalog,
            navigation::Request {
                generation: 1,
                position: None,
                operation,
                query: name.into(),
                side: Some(source::ComparedSide::New),
            },
        );
        let actual: Vec<_> = result
            .candidates
            .iter()
            .map(|candidate| {
                assert_eq!(candidate.tag.len as usize, name.chars().count());
                (candidate.tag.line, candidate.tag.col)
            })
            .collect();
        assert_eq!(actual, expected, "{name} {operation:?}: {result:?}");
        for &(line, column) in expected {
            let at_cursor =
                navigation_at(catalog, index, 0, line as usize, column as usize, operation);
            assert_eq!(at_cursor.query, name);
            assert_eq!(
                at_cursor
                    .candidates
                    .iter()
                    .map(|candidate| (candidate.tag.line, candidate.tag.col))
                    .collect::<Vec<_>>(),
                expected
            );
        }
    }
}

#[test]
fn navigation_rust_shorthand_reads_and_patterns_preserve_explicit_field_references() {
    let text = r#"struct Record { field: u32, local: u32 }
fn run(record: Record, local: u32) {
    let value = Record { field: local, local };
    let Record { field: renamed, local } = record;
    consume(renamed, local, value.field);
}
"#;
    let mut catalog = source::Catalog::default();
    catalog.insert(
        source::ComparedSide::New,
        b"shorthand.rs".to_vec(),
        text.as_bytes().to_vec(),
    );
    let index = navigation::Analysis::build(&catalog);
    assert_navigation_occurrences(
        &catalog,
        &index,
        "field",
        &[(1, 16)],
        &[(3, 25), (4, 17), (5, 34)],
    );
    assert_navigation_occurrences(
        &catalog,
        &index,
        "local",
        &[(1, 28), (2, 23), (4, 33)],
        &[(3, 32), (3, 39), (5, 21)],
    );
    assert_navigation_occurrences(&catalog, &index, "renamed", &[(4, 24)], &[(5, 12)]);
}

#[test]
fn navigation_go_range_assignment_is_read_and_short_declaration_introduces_name() {
    let text = r#"package main
func run(values []int, key int) {
    for key = range values { consume(key) }
    for next := range values { consume(next) }
}
"#;
    let mut catalog = source::Catalog::default();
    catalog.insert(
        source::ComparedSide::New,
        b"range.go".to_vec(),
        text.as_bytes().to_vec(),
    );
    let index = navigation::Analysis::build(&catalog);
    assert_navigation_occurrences(&catalog, &index, "key", &[(2, 23)], &[(3, 8), (3, 37)]);
    assert_navigation_occurrences(&catalog, &index, "next", &[(4, 8)], &[(4, 39)]);
}

#[test]
fn navigation_c_locals_parameters_and_pointer_fields_have_exact_declarations_and_reads() {
    let text = r#"struct Record { int *field; };
int run(int parameter) {
    int local = parameter;
    struct Record value;
    value.field = &local;
    return *value.field;
}
"#;
    let mut catalog = source::Catalog::default();
    catalog.insert(
        source::ComparedSide::New,
        b"bindings.c".to_vec(),
        text.as_bytes().to_vec(),
    );
    let index = navigation::Analysis::build(&catalog);
    assert_navigation_occurrences(&catalog, &index, "parameter", &[(2, 12)], &[(3, 16)]);
    assert_navigation_occurrences(&catalog, &index, "local", &[(3, 8)], &[(5, 19)]);
    assert_navigation_occurrences(&catalog, &index, "field", &[(1, 21)], &[(5, 10), (6, 18)]);
}

#[test]
fn navigation_code_parser_recovery_does_not_erase_other_valid_sources() {
    let mut catalog = source::Catalog::default();
    catalog.insert(
        source::ComparedSide::New,
        b"valid.rs".to_vec(),
        b"fn valid(parameter: u32) {\n    let local = parameter;\n    consume(local);\n}\n"
            .to_vec(),
    );
    catalog.insert(
        source::ComparedSide::New,
        b"broken.rs".to_vec(),
        b"fn broken( { let lost = ; }\n".to_vec(),
    );
    catalog.insert(
        source::ComparedSide::New,
        b"invalid.rs".to_vec(),
        vec![0xff],
    );
    let index = navigation::Analysis::build(&catalog);
    assert_navigation_occurrences(&catalog, &index, "local", &[(2, 8)], &[(3, 12)]);
    let result = navigation_at(
        &catalog,
        &index,
        0,
        3,
        12,
        navigation::Operation::Definitions,
    );
    assert!(
        result
            .coverage
            .contains("broken.rs: recovered syntax errors"),
        "{result:?}"
    );
    assert!(
        result.coverage.contains("invalid.rs: invalid UTF-8"),
        "{result:?}"
    );
}

#[test]
fn navigation_dependency_preflight_limits_dense_deep_and_large_sources_without_erasing_valid_code()
{
    let dense = format!(
        "fn dense(value: u32) {{\n{}}}\n",
        "consume(value);\n".repeat(10_000)
    );
    let deep = format!(
        "fn deep(value: u32) {{\nconsume({}value{});\n}}\n",
        "(".repeat(512),
        ")".repeat(512)
    );
    let large = format!("fn large() {{}}\n{}", " ".repeat(256 * 1024));
    for text in [&dense, &deep, &large] {
        assert!(text.len() < navigation::MAX_FILE_BYTES);
        let (_, coverage) = symbols::extract("buffer.rs", text);
        assert!(
            coverage.contains("rich riptags extraction skipped"),
            "{coverage}"
        );
        assert!(coverage.contains("bounded syntax supplementation"));
    }
    let (rich, coverage) = symbols::extract("buffer.rs", "fn valid() { valid(); }\n");
    assert!(!coverage.contains("rich riptags extraction skipped"));
    assert!(rich.iter().any(|tag| tag.name.as_ref() == "valid"
        && tag.kind == riptags::OccKind::Definition
        && tag.sym_kind == riptags::SymKind::Function));
    assert!(
        rich.iter().any(
            |tag| tag.kind == riptags::OccKind::Call && tag.context.as_deref() == Some("valid")
        )
    );
    let mut catalog = source::Catalog::default();
    catalog.insert(
        source::ComparedSide::New,
        b"valid.rs".to_vec(),
        b"fn valid() { valid(); }\n".to_vec(),
    );
    catalog.insert(
        source::ComparedSide::New,
        b"dense.rs".to_vec(),
        dense.into_bytes(),
    );
    catalog.insert(
        source::ComparedSide::New,
        b"deep.rs".to_vec(),
        deep.into_bytes(),
    );
    catalog.insert(
        source::ComparedSide::New,
        b"large.rs".to_vec(),
        large.into_bytes(),
    );
    let index = navigation::Analysis::build(&catalog);
    assert_navigation_occurrences(&catalog, &index, "valid", &[(1, 3)], &[(1, 13)]);
    let late_read = navigation_at(
        &catalog,
        &index,
        1,
        10_001,
        8,
        navigation::Operation::Definitions,
    );
    assert_eq!(late_read.query, "value");
    assert!(
        late_read
            .candidates
            .iter()
            .any(|candidate| candidate.source == 1
                && candidate.tag.line == 1
                && candidate.tag.col == 9)
    );
    let deep_read = navigation_at(
        &catalog,
        &index,
        2,
        2,
        8 + 512,
        navigation::Operation::Definitions,
    );
    assert_eq!(deep_read.query, "value");
    assert!(
        deep_read
            .candidates
            .iter()
            .any(|candidate| candidate.source == 2
                && candidate.tag.line == 1
                && candidate.tag.col == 8)
    );
}

#[test]
fn navigation_large_flat_arguments_keep_exact_reads_without_declarations() {
    let arguments = vec!["value"; 10_000].join(", ");
    let text = format!("fn flat(value: u32) {{\n    consume({arguments});\n}}\n");
    let (tags, _) = symbols::extract("buffer.rs", &text);
    let declarations: Vec<_> = tags
        .iter()
        .filter(|tag| tag.name.as_ref() == "value" && tag.kind.is_definition())
        .map(|tag| (tag.line, tag.col))
        .collect();
    assert_eq!(declarations, vec![(1, 8)]);
    let reads: Vec<_> = tags
        .iter()
        .filter(|tag| tag.name.as_ref() == "value" && !tag.kind.is_definition())
        .map(|tag| {
            assert_eq!(tag.len, 5);
            (tag.line, tag.col)
        })
        .collect();
    assert_eq!(
        reads,
        (0..10_000)
            .map(|argument| (2, 12 + argument * 7))
            .collect::<Vec<_>>()
    );
}

#[test]
fn navigation_rust_captured_patterns_declare_whole_and_nested_names() {
    let text = "fn capture(pair: (u32, u32)) {\n    let whole @ (left, right) = pair;\n    consume(whole, left, right);\n}\n";
    let mut catalog = source::Catalog::default();
    catalog.insert(
        source::ComparedSide::New,
        b"captured.rs".to_vec(),
        text.as_bytes().to_vec(),
    );
    let index = navigation::Analysis::build(&catalog);
    assert_navigation_occurrences(&catalog, &index, "whole", &[(2, 8)], &[(3, 12)]);
    assert_navigation_occurrences(&catalog, &index, "left", &[(2, 17)], &[(3, 19)]);
    assert_navigation_occurrences(&catalog, &index, "right", &[(2, 23)], &[(3, 25)]);
    assert_navigation_occurrences(&catalog, &index, "pair", &[(1, 11)], &[(2, 32)]);
}

#[test]
fn documentation_grammar_forms_and_nonfunction_exclusions() {
    for (selector, text, expected) in [
        (
            "buffer.rs",
            "/// Captured help\n#[inline]\nfn target() {}",
            true,
        ),
        ("buffer.rs", "/*! container */\nfn target() {}", false),
        (
            "buffer.py",
            "@decorator\ndef target():\n    \"\"\"Captured help\"\"\"\n    pass\n",
            true,
        ),
        (
            "buffer.py",
            "def target():\n    f\"Captured {help}\"\n",
            false,
        ),
        (
            "buffer.py",
            "def target():\n    b\"Captured help\"\n",
            false,
        ),
        ("buffer.go", "// Captured help\nfunc target() {}", true),
        ("buffer.c", "/** Captured help */\nint target(void);", true),
        (
            "buffer.cpp",
            "// Captured help\ntemplate<class T> void target(T value) {}",
            true,
        ),
        (
            "buffer.zig",
            "/// Captured help\npub fn target() void {}",
            true,
        ),
        ("buffer.sh", "# Captured help\ntarget() { :; }", true),
        ("buffer.sh", "#!/bin/bash\ntarget() { :; }", false),
        (
            "buffer.rs",
            "/// Unrelated\nconst X: u8 = 0;\nfn target() {}",
            false,
        ),
    ] {
        let (_, _, functions) = symbols::extract_functions(selector, text);
        let target = functions
            .iter()
            .find(|f| f.tag.name.as_ref() == "target")
            .unwrap_or_else(|| panic!("missing {selector}: {text}; {functions:?}"));
        assert_eq!(
            target
                .documentation
                .as_deref()
                .is_some_and(|s| s.contains("Captured help")),
            expected,
            "{selector}: {target:?}"
        );
    }
    for text in [
        "// variable\nint (*target)(void);",
        "// variable\nint target;",
    ] {
        assert!(symbols::extract_functions("buffer.c", text).2.is_empty());
    }
}

#[test]
fn documentation_exact_identity_ambiguity_and_captured_sides() {
    let mut catalog = source::Catalog::default();
    catalog.insert(
        source::ComparedSide::New,
        b"a.rs".to_vec(),
        b"/// New help\nfn target() {}\nfn caller() { target(); }\nlet target = 1;".to_vec(),
    );
    catalog.insert(
        source::ComparedSide::New,
        b"b.rs".to_vec(),
        b"fn target() {}".to_vec(),
    );
    catalog.insert(
        source::ComparedSide::Old,
        b"a.rs".to_vec(),
        b"/// Old help\nfn target() {}".to_vec(),
    );
    let index = navigation::Analysis::build(&catalog);
    let exact = navigation_at(
        &catalog,
        &index,
        0,
        2,
        3,
        navigation::Operation::Documentation,
    );
    assert_eq!(exact.documentation.as_deref(), Some("New help"));
    let ambiguous = navigation_at(
        &catalog,
        &index,
        0,
        3,
        14,
        navigation::Operation::Documentation,
    );
    assert_eq!(ambiguous.candidates.len(), 2, "{ambiguous:?}");
    assert!(ambiguous.documentation.is_none());
    let old = navigation_at(
        &catalog,
        &index,
        2,
        2,
        3,
        navigation::Operation::Documentation,
    );
    assert_eq!(old.documentation.as_deref(), Some("Old help"));
    let undocumented = navigation_at(
        &catalog,
        &index,
        1,
        1,
        3,
        navigation::Operation::Documentation,
    );
    assert!(undocumented.documentation.is_none());
    assert!(undocumented.coverage.contains("no supported captured"));
}

#[test]
fn documentation_view_returns_exact_origin_and_respects_keys_marks_and_stale_results() {
    let directory = fixture();
    std::fs::write(
        directory.path().join("one.rs"),
        include_str!("../tests/fixtures/navigation/documentation.rs"),
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    for style in ["vim", "emacs"] {
        std::fs::write(
            directory.path().join("one.rs"),
            include_str!("../tests/fixtures/navigation/documentation.rs"),
        )
        .unwrap();
        let mut app = application(directory.path(), state.path());
        app.bindings = config::Config::parse(&format!("[features]\nkeymap='{style}'"))
            .unwrap()
            .bindings(None)
            .unwrap();
        app.key(KeyCode::Char('/'));
        for c in "documented".chars() {
            app.key(KeyCode::Char(c));
        }
        app.key(KeyCode::Enter);
        await_navigation(&mut app);
        std::fs::write(
            directory.path().join("one.rs"),
            "/// Live replacement must not leak\nfn documented() {}\n",
        )
        .unwrap();
        let selected = app.navigation.as_ref().unwrap().selected;
        let origin = app.preview.as_ref().unwrap().lines.clone();
        let origin_cursor = app.cursor;
        let origin_status = app.status.clone();
        app.key(KeyCode::Char('K'));
        await_navigation(&mut app);
        assert!(app.documentation);
        assert!(
            app.preview
                .as_ref()
                .unwrap()
                .lines
                .iter()
                .any(|line| line.to_string().contains("Captured documentation for"))
        );
        app.key(KeyCode::Char('m'));
        assert!(app.marks.iter().all(|mark| !mark));
        app.key(KeyCode::Char(']'));
        assert!(app.documentation);
        screen(&mut app, 3, 1);
        assert!(!app.key(KeyCode::Char('q')));
        assert!(!app.documentation);
        assert_eq!(app.preview.as_ref().unwrap().lines, origin);
        assert_eq!(app.cursor, origin_cursor);
        assert_eq!(app.status, origin_status);
        assert_eq!(app.navigation.as_ref().unwrap().selected, selected);
        app.key(KeyCode::Char('K'));
        app.key(KeyCode::Esc);
        for _ in 0..20 {
            app.poll_analysis();
        }
        assert!(!app.documentation);
        app.key(KeyCode::Char('K'));
        await_navigation(&mut app);
        app.key(KeyCode::Char(','));
        assert!(app.key(KeyCode::Char('q')));
        assert!(state.path().read_dir().unwrap().next().is_none());
    }
    let mut app = application(directory.path(), state.path());
    app.bindings = config::Config::parse("[keybindings]\nshow_documentation='D'")
        .unwrap()
        .bindings(None)
        .unwrap();
    app.key(KeyCode::Char('K'));
    assert!(app.status.is_empty());
    app.key(KeyCode::Char('D'));
    assert!(app.status.contains("unavailable"));
    app.key(KeyCode::Char('/'));
    app.key(KeyCode::Char('K'));
    assert_eq!(app.navigation.as_ref().unwrap().query, "K");
}

#[test]
fn documentation_bounds_unicode_unavailable_and_source_return() {
    let mut catalog = source::Catalog::default();
    let text = format!(
        "/// {}\nfn target() {{}}\nfn caller() {{ let target = 1; println!(\"{{}}\", target); }}\nfn broken(\n",
        "界\t captured ".repeat(9000)
    );
    catalog.insert(
        source::ComparedSide::New,
        b"bounded.rs".to_vec(),
        text.into_bytes(),
    );
    let index = navigation::Analysis::build(&catalog);
    let documented = navigation_at(
        &catalog,
        &index,
        0,
        2,
        3,
        navigation::Operation::Documentation,
    );
    let documentation = documented.documentation.unwrap();
    assert!(documentation.contains("Documentation truncated"));
    assert!(documentation.len() < 66 * 1024);
    let unavailable = navigation_at(
        &catalog,
        &index,
        0,
        3,
        48,
        navigation::Operation::Documentation,
    );
    assert!(unavailable.documentation.is_none());
    assert!(unavailable.candidates.is_empty());

    let directory = fixture();
    std::fs::write(
        directory.path().join("one.rs"),
        include_str!("../tests/fixtures/navigation/documentation.rs"),
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Char('/'));
    for c in "documented".chars() {
        app.key(KeyCode::Char(c));
    }
    app.key(KeyCode::Enter);
    await_navigation(&mut app);
    app.key(KeyCode::Enter);
    app.key(KeyCode::Char('*'));
    let origin_cursor = app.cursor;
    let origin_top = app.top;
    let origin_left = app.left;
    let origin_search = format!("{:?}", app.preview.as_ref().unwrap().search);
    app.key(KeyCode::Char('K'));
    await_navigation(&mut app);
    assert!(app.documentation);
    app.key(KeyCode::Esc);
    assert_eq!(app.cursor, origin_cursor);
    assert_eq!(app.top, origin_top);
    assert_eq!(app.left, origin_left);
    assert_eq!(
        format!("{:?}", app.preview.as_ref().unwrap().search),
        origin_search
    );
    app.capture.unavailable = Some("opaque".into());
    let origin = app.preview.as_ref().unwrap().lines.clone();
    app.key(KeyCode::Char('K'));
    assert!(app.status.contains("opaque"));
    assert_eq!(app.preview.as_ref().unwrap().lines, origin);
    assert!(!app.documentation);
}

#[test]
fn documentation_calls_are_syntax_verified_in_every_supported_language() {
    for (path, text) in [
        (
            "a.rs",
            "/// Call help\nfn target() {}\nfn caller() { target(); }",
        ),
        ("a.py", "def target():\n    \"Call help\"\n\ntarget()"),
        (
            "a.go",
            "package a\n// Call help\nfunc target() {}\nfunc caller() { target() }",
        ),
        (
            "a.c",
            "// Call help\nvoid target(void);\nvoid caller(void) { target(); }",
        ),
        (
            "a.cpp",
            "// Call help\nvoid target();\nvoid caller() { target(); }",
        ),
        (
            "a.zig",
            "/// Call help\nfn target() void {}\nfn caller() void { target(); }",
        ),
        ("a.sh", "# Call help\ntarget() { :; }\ntarget"),
    ] {
        let mut catalog = source::Catalog::default();
        catalog.insert(
            source::ComparedSide::New,
            path.as_bytes().to_vec(),
            text.as_bytes().to_vec(),
        );
        let index = navigation::Analysis::build(&catalog);
        let byte = text.rfind("target").unwrap();
        let line = text[..byte].bytes().filter(|b| *b == b'\n').count() + 1;
        let col = text[..byte].rsplit('\n').next().unwrap().chars().count();
        let result = navigation_at(
            &catalog,
            &index,
            0,
            line,
            col,
            navigation::Operation::Documentation,
        );
        assert_eq!(
            result.documentation.as_deref(),
            Some("Call help"),
            "{path}: {result:?}"
        );
    }
}

#[test]
fn documentation_review_fix_c_repeated_declarators_keep_docs_and_exclude_pointer_variables() {
    for extension in ["c", "cpp"] {
        let text = "/** Shared help */\nint first(void), (*pointer)(void), target(void);\nvoid caller(void) { target(); }";
        let mut catalog = source::Catalog::default();
        catalog.insert(
            source::ComparedSide::New,
            format!("a.{extension}").into_bytes(),
            text.as_bytes().to_vec(),
        );
        let index = navigation::Analysis::build(&catalog);
        for name in ["first", "target"] {
            let col = text.lines().nth(1).unwrap().find(name).unwrap();
            let result = navigation_at(
                &catalog,
                &index,
                0,
                2,
                col,
                navigation::Operation::Documentation,
            );
            assert_eq!(
                result.documentation.as_deref(),
                Some("Shared help"),
                "{extension} {name}: {result:?}"
            );
        }
        let pointer = navigation_at(
            &catalog,
            &index,
            0,
            2,
            19,
            navigation::Operation::Documentation,
        );
        assert!(pointer.candidates.is_empty(), "{extension}: {pointer:?}");
        let call = navigation_at(
            &catalog,
            &index,
            0,
            3,
            20,
            navigation::Operation::Documentation,
        );
        assert_eq!(
            call.documentation.as_deref(),
            Some("Shared help"),
            "{extension}: {call:?}"
        );
    }
}

#[test]
fn documentation_review_fix_comment_scan_limit_reports_omitted_prefix() {
    let text = format!(
        "/// omitted prefix\n{}fn target() {{}}",
        "/// x\n".repeat(1024)
    );
    let (_, _, functions) = symbols::extract_functions("buffer.rs", &text);
    let documentation = functions[0].documentation.as_deref().unwrap();
    assert!(!documentation.contains("omitted prefix"));
    assert!(documentation.contains("Documentation truncated by comment scan limit"));
    assert!(documentation.len() < 64 * 1024);
    let exact = format!("{}fn target() {{}}", "/// x\n".repeat(1024));
    let (_, _, functions) = symbols::extract_functions("buffer.rs", &exact);
    assert!(
        !functions[0]
            .documentation
            .as_deref()
            .unwrap()
            .contains("truncated")
    );
}

#[test]
fn documentation_review_fix_candidate_truncation_is_visible() {
    let directory = fixture();
    let definitions = "fn target() {}\n".repeat(1001);
    std::fs::write(
        directory.path().join("one.rs"),
        format!("{definitions}fn caller() {{ target(); }}\n"),
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Char('/'));
    for c in "caller".chars() {
        app.key(KeyCode::Char(c));
    }
    app.key(KeyCode::Enter);
    await_navigation(&mut app);
    assert_eq!(app.status, "1 results");
    app.key(KeyCode::Enter);
    app.key(KeyCode::Char('0'));
    for _ in 0..14 {
        app.key(KeyCode::Char('l'));
    }
    app.key(KeyCode::Char('K'));
    await_navigation(&mut app);
    assert_eq!(app.navigation.as_ref().unwrap().candidates.len(), 1000);
    assert_eq!(app.status, "1000 results (truncated)");
    assert!(screen_text(&screen(&mut app, 100, 20)).contains("1000 results (truncated)"));
}

#[test]
fn preview_definitions_cover_verified_function_and_type_forms() {
    let cases = [
        (
            "types.rs",
            "struct Struct; enum Enum {} trait Trait {} type Alias = Struct; fn function() {}",
            &["Struct", "Enum", "Trait", "Alias", "function"][..],
        ),
        (
            "types.py",
            "class Class:\n    pass\ndef function():\n    pass\n",
            &["Class", "function"][..],
        ),
        (
            "types.go",
            "package p\ntype Struct struct {}\ntype Alias = Struct\nfunc function() {}",
            &["Struct", "Alias", "function"][..],
        ),
        (
            "types.c",
            "struct Struct {}; union Union {}; enum Enum { Value }; typedef int Alias; int function(void) {}",
            &["Struct", "Union", "Enum", "Alias", "function"][..],
        ),
        (
            "types.cpp",
            "class Class {}; struct Struct {}; union Union {}; enum Enum {}; typedef int Alias; using Using = Alias; void function() {}",
            &[
                "Class", "Struct", "Union", "Enum", "Alias", "Using", "function",
            ][..],
        ),
        (
            "types.zig",
            "const Struct = struct {}; const Union = union { value: i32 }; const Enum = enum { value }; const Opaque = opaque {}; fn function() void {}",
            &["Struct", "Union", "Enum", "Opaque", "function"][..],
        ),
        ("types.sh", "function function() { :; }", &["function"][..]),
    ];
    for (path, text, names) in cases {
        let mut catalog = source::Catalog::default();
        catalog.insert(
            source::ComparedSide::New,
            path.as_bytes().to_vec(),
            text.as_bytes().to_vec(),
        );
        let index = navigation::Analysis::build(&catalog);
        for name in names {
            let result = index.query(
                &catalog,
                navigation::Request {
                    generation: 0,
                    position: None,
                    operation: navigation::Operation::PreviewDefinition,
                    query: (*name).into(),
                    side: Some(source::ComparedSide::New),
                },
            );
            assert_eq!(result.candidates.len(), 1, "{path} {name}: {result:?}");
            assert!(result.candidates[0].tag.kind.is_definition());
        }
    }
}

#[test]
fn preview_exact_identity_side_ambiguity_and_ineligible_definitions() {
    let mut catalog = source::Catalog::default();
    catalog.insert(source::ComparedSide::New, b"a.rs".to_vec(), b"struct Target;\nfn function() {}\nfn caller() { function(); }\nfn variable() { let Target = 1; }\n".to_vec());
    catalog.insert(
        source::ComparedSide::New,
        b"b.rs".to_vec(),
        b"fn function() {}\n".to_vec(),
    );
    catalog.insert(
        source::ComparedSide::Old,
        b"a.rs".to_vec(),
        b"struct Target;\nfn function() {}\n".to_vec(),
    );
    let index = navigation::Analysis::build(&catalog);
    let exact = navigation_at(
        &catalog,
        &index,
        0,
        2,
        3,
        navigation::Operation::PreviewDefinition,
    );
    assert_eq!(exact.candidates.len(), 1);
    assert_eq!(exact.candidates[0].source, 0);
    let ambiguous = navigation_at(
        &catalog,
        &index,
        0,
        3,
        14,
        navigation::Operation::PreviewDefinition,
    );
    assert_eq!(ambiguous.candidates.len(), 2);
    let old = navigation_at(
        &catalog,
        &index,
        2,
        2,
        3,
        navigation::Operation::PreviewDefinition,
    );
    assert_eq!(old.candidates.len(), 1);
    assert_eq!(old.candidates[0].source, 2);
    let variable = navigation_at(
        &catalog,
        &index,
        0,
        4,
        20,
        navigation::Operation::PreviewDefinition,
    );
    assert!(variable.candidates.is_empty(), "{variable:?}");
    let unmapped = navigation_at(
        &catalog,
        &index,
        0,
        1,
        0,
        navigation::Operation::PreviewDefinition,
    );
    assert!(unmapped.candidates.is_empty());
    for (path, text, name) in [
        ("x.rs", "mod Module {}", "Module"),
        ("x.py", "Variable = 1", "Variable"),
        ("x.zig", "const Variable = 1;", "Variable"),
        ("x.json", "{\"Target\": 1}", "Target"),
        ("x.yaml", "Target: 1", "Target"),
    ] {
        let mut catalog = source::Catalog::default();
        catalog.insert(
            source::ComparedSide::New,
            path.as_bytes().to_vec(),
            text.as_bytes().to_vec(),
        );
        let result = navigation::Analysis::build(&catalog).query(
            &catalog,
            navigation::Request {
                generation: 0,
                position: None,
                operation: navigation::Operation::PreviewDefinition,
                query: name.into(),
                side: None,
            },
        );
        assert!(result.candidates.is_empty(), "{path}: {result:?}");
    }
}

#[test]
fn preview_returns_snapshot_origin_and_respects_configured_keys() {
    let directory = fixture();
    let text =
        "struct CapturedType;\nfn captured_function() {}\nfn caller() { captured_function(); }\n";
    std::fs::write(directory.path().join("one.rs"), text).unwrap();
    let state = tempfile::tempdir().unwrap();
    for style in ["vim", "emacs"] {
        let mut app = application(directory.path(), state.path());
        app.bindings = config::Config::parse(&format!("[features]\nkeymap='{style}'"))
            .unwrap()
            .bindings(None)
            .unwrap();
        app.key(KeyCode::Char('/'));
        for c in "captured_function".chars() {
            app.key(KeyCode::Char(c));
        }
        app.key(KeyCode::Enter);
        await_navigation(&mut app);
        let origin = app.preview.as_ref().unwrap().clone();
        let cursor = app.cursor;
        let status = app.status.clone();
        let selected = app.navigation.as_ref().unwrap().selected;
        std::fs::write(
            directory.path().join("one.rs"),
            "fn live_replacement() {}\n",
        )
        .unwrap();
        app.key(KeyCode::Char('P'));
        await_navigation(&mut app);
        assert!(
            app.definition_preview,
            "{} {:?}",
            app.status,
            app.navigation
                .as_ref()
                .map(|nav| (&nav.query, &nav.coverage, nav.candidates.len()))
        );
        assert!(
            app.preview
                .as_ref()
                .unwrap()
                .title
                .as_ref()
                .unwrap()
                .contains("Definition preview")
        );
        assert!(
            app.preview
                .as_ref()
                .unwrap()
                .lines
                .iter()
                .any(|line| line.to_string().contains("captured_function"))
        );
        assert!(
            !app.preview
                .as_ref()
                .unwrap()
                .lines
                .iter()
                .any(|line| line.to_string().contains("live_replacement"))
        );
        app.key(KeyCode::Char('m'));
        assert!(app.marks.iter().all(|mark| !mark));
        app.key(KeyCode::Char('q'));
        assert!(!app.definition_preview);
        assert_eq!(app.preview.as_ref().unwrap().lines, origin.lines);
        assert_eq!(app.cursor, cursor);
        assert_eq!(app.status, status);
        assert_eq!(app.navigation.as_ref().unwrap().selected, selected);
        app.key(KeyCode::Char('P'));
        app.key(KeyCode::Esc);
        for _ in 0..20 {
            app.poll_analysis();
        }
        assert!(!app.definition_preview);
        app.key(KeyCode::Char('P'));
        await_navigation(&mut app);
        app.key(KeyCode::Char(','));
        assert!(app.key(KeyCode::Char('q')));
        std::fs::write(directory.path().join("one.rs"), text).unwrap();
    }
    let mut app = application(directory.path(), state.path());
    app.bindings = config::Config::parse("[keybindings]\npreview_definition='D'")
        .unwrap()
        .bindings(None)
        .unwrap();
    app.key(KeyCode::Char('P'));
    assert!(app.navigation.is_none());
    app.key(KeyCode::Char('D'));
    assert!(app.status.contains("unavailable"));
    app.key(KeyCode::Char('/'));
    app.key(KeyCode::Char('P'));
    assert_eq!(app.navigation.as_ref().unwrap().query, "P");
    assert!(state.path().read_dir().unwrap().next().is_none());
}

#[test]
fn preview_limits_unicode_recovery_and_unrenderable_target_retain_origin() {
    let mut catalog = source::Catalog::default();
    let unicode = "\tstruct Café;\nfn broken( {\n";
    catalog.insert(
        source::ComparedSide::New,
        b"unicode.rs".to_vec(),
        unicode.as_bytes().to_vec(),
    );
    let index = navigation::Analysis::build(&catalog);
    let result = navigation_at(
        &catalog,
        &index,
        0,
        1,
        10,
        navigation::Operation::PreviewDefinition,
    );
    assert_eq!(result.candidates.len(), 1, "{result:?}");
    assert_eq!(result.candidates[0].tag.col, 8);
    assert!(result.coverage.contains("recovered"));
    let (_, left) =
        preview::Preview::target(&catalog.sources[0], 1, 8, 4, &theme::Theme::default()).unwrap();
    assert!(left > 8);
    for id in (0..1100).rev() {
        catalog.insert(
            source::ComparedSide::New,
            format!("{id:04}.rs").into_bytes(),
            b"struct Target;".to_vec(),
        );
    }
    let result = navigation::Analysis::build(&catalog).query(
        &catalog,
        navigation::Request {
            generation: 0,
            position: None,
            operation: navigation::Operation::PreviewDefinition,
            query: "Target".into(),
            side: Some(source::ComparedSide::New),
        },
    );
    assert_eq!(result.candidates.len(), 1000);
    assert_eq!(
        catalog.sources[result.candidates[0].source].path,
        b"0000.rs"
    );
    assert!(result.coverage.contains("truncated"));

    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("one.rs"),
        format!("{}struct TooFar;", "\n".repeat(21000)),
    )
    .unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Char('/'));
    for c in "TooFar".chars() {
        app.key(KeyCode::Char(c));
    }
    app.key(KeyCode::Enter);
    await_navigation(&mut app);
    assert_eq!(app.navigation.as_ref().unwrap().candidates.len(), 1);
    let origin = app.preview.as_ref().unwrap().lines.clone();
    let cursor = app.cursor;
    app.key(KeyCode::Char('P'));
    await_navigation(&mut app);
    assert!(!app.definition_preview);
    assert_eq!(app.preview.as_ref().unwrap().lines, origin);
    assert_eq!(app.cursor, cursor);
    assert!(app.status.contains("unavailable"), "{}", app.status);
}

#[test]
fn preview_opaque_nested_navigation_and_file_selection_preserve_lifecycle() {
    use crossterm::event::{KeyEvent, KeyModifiers};
    let directory = fixture();
    std::fs::write(
        directory.path().join("one.rs"),
        "/// Captured help\nfn target() {}\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Char('P'));
    assert!(app.status.contains("no mapped"));
    assert!(app.preview.is_none());
    app.capture.unavailable = Some("opaque".into());
    app.key(KeyCode::Char('P'));
    assert!(app.status.contains("opaque"));
    assert!(app.preview.is_none());
    app.capture.unavailable = None;
    app.key(KeyCode::Char('/'));
    for c in "target".chars() {
        app.key(KeyCode::Char(c));
    }
    app.key(KeyCode::Enter);
    await_navigation(&mut app);
    let origin = app.preview.as_ref().unwrap().lines.clone();
    app.key(KeyCode::Char('P'));
    await_navigation(&mut app);
    assert!(app.definition_preview);
    let definition = app.preview.as_ref().unwrap().lines.clone();
    let cursor = app.cursor;
    for nested in ["P", "K", "gd"] {
        for c in nested.chars() {
            app.key(KeyCode::Char(c));
        }
        await_navigation(&mut app);
        if nested == "gd" {
            app.key(KeyCode::Enter);
            app.key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        }
        app.key_event(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert!(app.definition_preview, "{nested}");
        assert_eq!(app.preview.as_ref().unwrap().lines, definition, "{nested}");
        assert_eq!(app.cursor, cursor);
    }
    app.key(KeyCode::Char('q'));
    assert!(!app.definition_preview);
    assert_eq!(app.preview.as_ref().unwrap().lines, origin);
    app.key(KeyCode::Char('P'));
    // A file click must invalidate even a reply already queued by the worker.
    app.file_area = ratatui::layout::Rect::new(0, 0, 10, 5);
    app.mouse(
        crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        },
        std::time::Instant::now(),
    );
    for _ in 0..50 {
        app.poll_analysis();
        std::thread::yield_now();
    }
    assert!(!app.definition_preview);
    assert!(app.navigation.is_none());
    assert!(app.preview.is_none());
}

#[test]
fn preview_cpp_template_type_parameter_names_are_ineligible_definitions() {
    let text = "struct Target {};\ntemplate<class Target> void ordinary() {}\ntemplate<typename... Target> void variadic() {}\ntemplate<class Target = int> void defaulted() {}\ntemplate<class Other = Target> void default_reference() {}\n";
    let mut catalog = source::Catalog::default();
    catalog.insert(
        source::ComparedSide::New,
        b"template.cpp".to_vec(),
        text.as_bytes().to_vec(),
    );
    let index = navigation::Analysis::build(&catalog);
    for line in [2, 3, 4] {
        let row = text.lines().nth(line - 1).unwrap();
        let col = row.find("Target").unwrap();
        let result = navigation_at(
            &catalog,
            &index,
            0,
            line,
            col,
            navigation::Operation::PreviewDefinition,
        );
        assert!(result.candidates.is_empty(), "line {line}: {result:?}");
        assert!(result.coverage.contains("Preview unavailable"));
    }
    for (line, name) in [
        (1, "Target"),
        (2, "ordinary"),
        (3, "variadic"),
        (4, "defaulted"),
        (5, "default_reference"),
        (5, "Target"),
    ] {
        let row = text.lines().nth(line - 1).unwrap();
        let col = row.find(name).unwrap();
        let result = navigation_at(
            &catalog,
            &index,
            0,
            line,
            col,
            navigation::Operation::PreviewDefinition,
        );
        assert_eq!(result.candidates.len(), 1, "line {line} {name}: {result:?}");
        assert_eq!(result.candidates[0].tag.name.as_ref(), name);
    }
}

fn preview_status_row(app: &mut App, width: u16, height: u16) -> String {
    let buffer = screen(app, width, height);
    let area = app.preview_area;
    (area.x..area.right())
        .map(|x| buffer[(x, area.bottom())].symbol())
        .collect::<String>()
        .trim_end()
        .to_owned()
}

#[test]
fn preview_status_tracks_actual_viewport_cursor_and_excludes_mouse_hits() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let directory = fixture();
    std::fs::write(
        directory.path().join("one.rs"),
        "fn target() {}\n".repeat(100),
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Enter);
    let total = app.preview.as_ref().unwrap().lines.len();
    assert_eq!(
        preview_status_row(&mut app, 100, 15),
        format!("{total} lines  1:1  Top")
    );
    assert_eq!(app.viewport, (10, 68));
    app.key(KeyCode::PageDown);
    let range = total - app.viewport.0;
    let expected = format!(
        "{total} lines  {}:{}  {}%",
        app.cursor.0 + 1,
        app.cursor.1 + 1,
        app.top * 100 / range
    );
    assert_eq!(preview_status_row(&mut app, 100, 15), expected);
    app.mouse(
        mouse_event(
            MouseEventKind::ScrollDown,
            app.preview_area.x,
            app.preview_area.y,
        ),
        std::time::Instant::now(),
    );
    assert!(app.top > 0);
    let saved = (app.cursor, app.top, app.left);
    app.key(KeyCode::Tab);
    for kind in [
        MouseEventKind::ScrollDown,
        MouseEventKind::Down(MouseButton::Left),
    ] {
        app.mouse(
            mouse_event(kind, app.preview_area.x, app.preview_area.bottom()),
            std::time::Instant::now(),
        );
        assert_eq!((app.cursor, app.top, app.left), saved);
        assert_eq!(app.focus, Focus::Files);
    }
    app.key(KeyCode::Tab);
    app.key(KeyCode::Char('G'));
    let expected = format!(
        "{total} lines  {}:{}  Bot",
        app.cursor.0 + 1,
        app.cursor.1 + 1
    );
    assert_eq!(preview_status_row(&mut app, 100, 15), expected);
    assert_eq!(app.top, total - app.viewport.0);
    assert!(app.marks.iter().all(|mark| !mark));
    assert!(state.path().read_dir().unwrap().next().is_none());
}

#[test]
fn preview_status_empty_fit_cells_fallback_and_tiny_resize() {
    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.preview = Some(preview::Preview::original(b"", &theme::Theme::default()));
    app.focus = Focus::Preview;
    app.cursor = (100, 100);
    assert_eq!(preview_status_row(&mut app, 100, 15), "0 lines  0:0  All");
    assert_eq!(app.cursor, (0, 0));
    app.preview = Some(preview::Preview::original(
        "a\t界e\u{301}🙂z\n\n".as_bytes(),
        &theme::Theme::default(),
    ));
    app.cursor = (0, 7);
    assert_eq!(preview_status_row(&mut app, 100, 7), "2 lines  1:8  All");
    // A displayed column counts terminal cells after tab expansion, including wide glyphs.
    app.cursor = (0, 9);
    screen(&mut app, 12, 7);
    assert!(app.left > 0);
    assert_eq!(preview_status_row(&mut app, 100, 7), "2 lines  1:10  All");
    app.cursor = (1, 99);
    assert_eq!(preview_status_row(&mut app, 100, 7), "2 lines  2:1  All");
    app.capture.unavailable = Some("Unstructured comparison".into());
    app.status = "Unstructured comparison".into();
    app.preview.as_mut().unwrap().reason = Some("Source enrichment unavailable".into());
    let buffer = screen(&mut app, 100, 7);
    assert!(screen_text(&buffer).contains("Original Git output"));
    assert!(screen_text(&buffer).contains("Unstructured comparison"));
    assert_eq!(preview_status_row(&mut app, 100, 7), "2 lines  2:1  All");
    // One interior row is entirely status; smaller panes have no content or status.
    assert_eq!(preview_status_row(&mut app, 8, 5), "2 line");
    assert_eq!(app.viewport, (0, 6));
    for (width, height) in [(1, 1), (3, 2), (2, 5), (100, 30)] {
        screen(&mut app, width, height);
    }
    assert_eq!(preview_status_row(&mut app, 100, 30), "2 lines  2:1  All");
}

#[test]
fn preview_status_source_documentation_and_definition_restore_drawn_origin() {
    let directory = fixture();
    std::fs::write(
        directory.path().join("one.rs"),
        "/// Captured help\nfn target() {}\nfn caller() { target(); }\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Char('/'));
    for c in "caller".chars() {
        app.key(KeyCode::Char(c));
    }
    app.key(KeyCode::Enter);
    await_navigation(&mut app);
    app.key(KeyCode::Enter);
    assert_eq!(preview_status_row(&mut app, 100, 15), "3 lines  3:4  All");
    app.key(KeyCode::Char('0'));
    for _ in 0..14 {
        app.key(KeyCode::Char('l'));
    }
    let origin_status = preview_status_row(&mut app, 100, 15);
    let origin = (app.cursor, app.top, app.left);
    for key in ['P', 'K'] {
        app.key(KeyCode::Char(key));
        await_navigation(&mut app);
        assert!(
            app.definition_preview || app.documentation,
            "{}",
            app.status
        );
        let total = app.preview.as_ref().unwrap().lines.len();
        let expected = format!(
            "{total} lines  {}:{}  All",
            app.cursor.0 + 1,
            app.cursor.1 + 1
        );
        assert_eq!(preview_status_row(&mut app, 100, 15), expected);
        app.key(KeyCode::Char('q'));
        assert_eq!(preview_status_row(&mut app, 100, 15), origin_status);
        assert_eq!((app.cursor, app.top, app.left), origin);
    }
    assert!(app.marks.iter().all(|mark| !mark));
    assert!(state.path().read_dir().unwrap().next().is_none());
}

#[test]
fn file_statistics_follow_native_captured_hunks_and_metadata_evidence() {
    use git::Statistics::{Binary, Text, Unavailable};
    let directory = fixture();
    let root = directory.path();
    std::fs::write(root.join("text.txt"), "--- header\nkeep\nold\nend").unwrap();
    std::fs::write(root.join("binary.dat"), b"old\0binary").unwrap();
    std::fs::write(root.join("empty.txt"), "").unwrap();
    git_run(root, &["add", "."]);
    git_run(root, &["commit", "-qm", "statistics fixture"]);
    std::fs::write(root.join("text.txt"), "+++ header\nkeep\nnew\nextra\nend").unwrap();
    std::fs::write(root.join("binary.dat"), b"new\0binary").unwrap();
    std::fs::remove_file(root.join("empty.txt")).unwrap();
    git_run(root, &["mv", "two.rs", "renamed.rs"]);
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(root.join("one.rs"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(root.join("new-empty.txt"), "").unwrap();
    git_run(root, &["add", "."]);
    for words in [
        vec!["HEAD"],
        vec!["--cached"],
        vec!["-w", "HEAD"],
        vec!["--unified=0", "HEAD"],
        vec!["--no-renames", "HEAD"],
    ] {
        let captured = capture(root, &words);
        assert!(captured.unavailable.is_none());
        let mut oracle_args = vec!["diff", "--numstat"];
        oracle_args.extend(&words);
        let oracle = String::from_utf8(git_run(root, &oracle_args)).unwrap();
        for line in oracle.lines() {
            let fields: Vec<_> = line.splitn(3, '\t').collect();
            if fields.len() != 3 {
                continue;
            }
            let item = captured
                .items
                .iter()
                .find(|item| {
                    fields[2].ends_with(std::str::from_utf8(&item.new_path).unwrap())
                        || fields[2] == "two.rs => renamed.rs" && item.new_path == b"renamed.rs"
                })
                .unwrap();
            let expected = if fields[0] == "-" {
                Binary
            } else {
                Text {
                    additions: fields[0].parse().unwrap(),
                    deletions: fields[1].parse().unwrap(),
                }
            };
            if words.contains(&"-w") && item.statistics() == Unavailable {
                assert_eq!(
                    expected,
                    Text {
                        additions: 0,
                        deletions: 0
                    }
                );
            } else {
                assert_eq!(item.statistics(), expected, "{words:?}: {line}");
            }
        }
    }
    let captured = capture(root, &["HEAD"]);
    let text = captured
        .items
        .iter()
        .find(|item| item.new_path == b"text.txt")
        .unwrap();
    assert_eq!(
        text.statistics(),
        Text {
            additions: 3,
            deletions: 2
        }
    );
    std::fs::write(root.join("text.txt"), "completely different\n").unwrap();
    assert_eq!(
        text.statistics(),
        Text {
            additions: 3,
            deletions: 2
        }
    );
    // A mode-only section with suppressed whitespace changes cannot prove zero.
    std::fs::write(root.join("one.rs"), "fn one() {\n    let answer = 1;\n}\n").unwrap();
    let omitted = capture(root, &["-w", "HEAD", "--", "one.rs"]);
    assert_eq!(omitted.items[0].statistics(), Unavailable);
    assert!(capture(root, &["--binary", "HEAD"]).unavailable.is_some());
    assert!(capture(root, &["--stat", "HEAD"]).unavailable.is_some());
    git_run(root, &["commit", "-qm", "second"]);
    let refs = capture(root, &["HEAD~", "HEAD"]);
    assert!(refs.items.iter().any(|item| item.statistics() == Binary));
}

#[test]
fn file_statistics_rows_keep_names_styles_marks_and_resize_safe() {
    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    change(directory.path());
    let mut app = application(directory.path(), state.path());
    app.set_theme(theme::Theme::classic());
    let rendered = screen(&mut app, 100, 24);
    let text = screen_text(&rendered);
    assert!(text.contains("one.rs"));
    assert!(text.contains("2 +-"), "{text}");
    assert!(
        rendered
            .content
            .iter()
            .any(|cell| cell.symbol() == "+" && cell.fg == Color::Green)
    );
    assert!(
        rendered
            .content
            .iter()
            .any(|cell| cell.symbol() == "-" && cell.fg == Color::Red)
    );
    app.key(KeyCode::Char('m'));
    app.key(KeyCode::Enter);
    let text = screen_text(&screen(&mut app, 100, 24));
    assert!(!text.contains("2 +-") && !text.contains('Σ'), "{text}");
    app.key(KeyCode::Esc);
    assert!(screen_text(&screen(&mut app, 100, 24)).contains("2 +-"));
    app.key(KeyCode::Down);
    app.key(KeyCode::Up);
    assert_eq!(app.marks, [true, false]);
    let rendered = screen(&mut app, 100, 24);
    assert!(
        rendered
            .content
            .iter()
            .any(|cell| cell.symbol() == "o" && cell.fg == Color::DarkGray)
    );
    let item = &mut app.capture.items[0];
    item.old_path = "old/界界界界界界界界界界.rs".as_bytes().to_vec();
    item.new_path = "new/e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}e\u{301}.rs"
        .as_bytes()
        .to_vec();
    let text = screen_text(&screen(&mut app, 45, 24));
    assert!(text.contains(" → "), "{text}");
    assert!(text.contains("old/") && text.contains("new/"), "{text}");
    for width in 0..50 {
        for height in [0, 1, 3, 10] {
            screen(&mut app, width, height);
        }
    }
    assert_eq!(app.marks, [true, false]);
}

#[test]
fn file_statistics_no_index_add_delete_and_multiple_hunks_match_git() {
    use git::Statistics::Text;
    let directory = fixture();
    let root = directory.path();
    let old = (0..30).map(|n| format!("line {n}\n")).collect::<String>();
    std::fs::write(root.join("many.txt"), &old).unwrap();
    std::fs::write(root.join("deleted.txt"), "first\nsecond").unwrap();
    git_run(root, &["add", "."]);
    git_run(root, &["commit", "-qm", "hunks"]);
    std::fs::write(
        root.join("many.txt"),
        old.replace("line 1\n", "new first\n")
            .replace("line 28\n", "new last\n"),
    )
    .unwrap();
    std::fs::remove_file(root.join("deleted.txt")).unwrap();
    std::fs::write(root.join("added.txt"), "first\nsecond\nthird").unwrap();
    git_run(root, &["add", "."]);
    let captured = capture(root, &["HEAD"]);
    for (path, additions, deletions) in [
        ("many.txt", 2, 2),
        ("deleted.txt", 0, 2),
        ("added.txt", 3, 0),
    ] {
        let item = captured
            .items
            .iter()
            .find(|item| item.new_path == path.as_bytes())
            .unwrap();
        assert_eq!(
            item.statistics(),
            Text {
                additions,
                deletions
            }
        );
        let oracle = git_run(root, &["diff", "--numstat", "HEAD", "--", path]);
        assert!(
            String::from_utf8(oracle)
                .unwrap()
                .starts_with(&format!("{additions}\t{deletions}\t"))
        );
        if path == "many.txt" {
            assert_eq!(
                item.patch
                    .split(|byte| *byte == b'\n')
                    .filter(|line| line.starts_with(b"@@ "))
                    .count(),
                2
            );
        }
    }
    let outside = tempfile::tempdir().unwrap();
    std::fs::write(outside.path().join("old.txt"), "--- header\nold").unwrap();
    std::fs::write(outside.path().join("new.txt"), "+++ header\nnew\nextra").unwrap();
    let args = ["--no-index", "old.txt", "new.txt"];
    let captured = capture(outside.path(), &args);
    assert_eq!(
        captured.items[0].statistics(),
        Text {
            additions: 3,
            deletions: 2
        }
    );
    let oracle = Command::new("git")
        .current_dir(outside.path())
        .args(["diff", "--numstat"])
        .args(args)
        .output()
        .unwrap();
    assert_eq!(oracle.status.code(), Some(1));
    assert!(oracle.stdout.starts_with(b"3\t2\t"));
}

#[test]
fn after_changes_projects_captured_rows_and_preserves_navigation_history() {
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Char('a'));
    assert!(app.preview.is_none());
    app.key(KeyCode::Enter);
    screen(&mut app, 110, 25);
    let original = app.preview.as_ref().unwrap().clone();
    let removed = original
        .locations
        .iter()
        .position(|row| row.0.is_some() && row.1.is_none())
        .unwrap();
    app.cursor = (removed, 5);
    app.key(KeyCode::Char('*'));
    let marks = app.marks.clone();
    let patch = app.capture.items[0].patch.clone();
    std::fs::write(directory.path().join("one.rs"), "fn later_live_edit() {}\n").unwrap();
    app.cursor.0 = removed;
    app.key(KeyCode::Char('a'));
    let projected = app.preview.as_ref().unwrap().clone();
    assert!(
        projected
            .title
            .as_ref()
            .unwrap()
            .contains("captured diff context")
    );
    assert!(
        !projected
            .locations
            .iter()
            .any(|row| row.0.is_some() && row.1.is_none())
    );
    assert!(
        projected
            .lines
            .iter()
            .any(|line| line.to_string().starts_with("@@"))
    );
    assert_eq!(projected.lines.len(), projected.locations.len());
    assert_eq!(projected.lines.len(), projected.cells.len());
    assert!(
        projected
            .search
            .as_ref()
            .unwrap()
            .matches
            .iter()
            .all(|hit| hit.row < projected.lines.len())
    );
    for (row, location) in projected.locations.iter().enumerate() {
        if location.1.is_some() {
            let old = original
                .locations
                .iter()
                .position(|old| old == location)
                .unwrap();
            assert_eq!(projected.lines[row], original.lines[old]);
            assert_eq!(projected.cells[row], original.cells[old]);
        }
    }
    assert!(screen_text(&screen(&mut app, 110, 25)).contains("After changes"));
    app.key(KeyCode::Char('/'));
    app.key(KeyCode::Char('a')); // Search input retains its ordinary typing behavior.
    assert_eq!(app.navigation.as_ref().unwrap().query, "a");
    app.key(KeyCode::Esc);
    assert_eq!(app.preview.as_ref().unwrap().lines, projected.lines);
    app.key(KeyCode::Tab);
    app.key(KeyCode::Char('a'));
    assert_eq!(app.preview.as_ref().unwrap().lines, projected.lines);
    app.key(KeyCode::Tab);
    app.key(KeyCode::Char('a'));
    assert_eq!(app.preview.as_ref().unwrap().lines, original.lines);
    assert_eq!(app.preview.as_ref().unwrap().locations, original.locations);
    assert_eq!(app.marks, marks);
    assert_eq!(app.capture.items[0].patch, patch);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("one.rs")).unwrap(),
        "fn later_live_edit() {}\n"
    );
    app.key(KeyCode::Esc);
    app.key(KeyCode::Enter);
    assert!(app.preview.as_ref().unwrap().title.is_none());
}

#[test]
fn after_changes_classifies_hunks_with_headers_annotations_invalid_and_bounded_output() {
    let directory = fixture();
    change(directory.path());
    let mut captured = capture(directory.path(), &["HEAD"]);
    let patch = "diff --git a/one.rs b/one.rs\n--- a/one.rs\n+++ b/one.rs\n@@ -1,2 +1,2 @@\n--- header-like removed source\n+++ header-like added source\n \\t界\n@@ -20 +20 @@\n-deleted\n\\ No newline at end of file\n+retained\n\\ No newline at end of file\n-metadata outside hunk\n";
    for suffix in [Vec::new(), vec![b'\xff'], vec![b'x'; 270000]] {
        captured.items[0].patch = [patch.as_bytes(), &suffix].concat();
        let mut preview = preview::Preview::item(
            directory.path(),
            &captured.items[0],
            &theme::Theme::default(),
        );
        let original = preview.lines.clone();
        preview.toggle_after_changes(8, 0);
        let text = preview
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("--- a/one.rs"));
        assert!(text.contains("+++ header-like added source"));
        assert!(text.contains("-metadata outside hunk"));
        assert!(!text.contains("header-like removed source"));
        assert!(!text.contains("-deleted"));
        assert_eq!(text.matches("@@").count(), 4);
        assert_eq!(text.matches("No newline at end of file").count(), 2);
        assert!(text.contains("No newline at end of file (removed old-side line)\n+retained\n"));
        assert!(text.contains("+retained\n"));
        assert_eq!(text.matches("(removed old-side line)").count(), 1);
        let annotation = preview
            .lines
            .iter()
            .position(|line| line.to_string().contains("(removed old-side line)"))
            .unwrap();
        assert!(
            preview.cells[annotation]
                .iter()
                .rev()
                .take(" (removed old-side line)".len())
                .all(Option::is_none)
        );
        assert_eq!(
            preview.cells[annotation].len(),
            preview.lines[annotation].width()
        );
        assert_eq!(
            preview.width,
            preview.lines.iter().map(|line| line.width()).max().unwrap()
        );
        if !suffix.is_empty() {
            assert!(preview.locations.iter().all(|row| *row == (None, None)));
        }
        preview.toggle_after_changes(8, 0);
        assert_eq!(preview.lines, original);
    }
    let mut opaque = preview::Preview::original(patch.as_bytes(), &theme::Theme::default());
    let original = opaque.lines.clone();
    opaque.toggle_after_changes(8, 0);
    assert_eq!(opaque.lines, original);
    assert!(opaque.title.is_none());
}

#[test]
fn after_changes_binding_supports_both_maps_remapping_disabling_and_collisions() {
    use crossterm::event::{KeyEvent, KeyModifiers};
    use keybindings::{Action, Keymap, Resolution};
    for keymap in [Keymap::Vim, Keymap::Emacs] {
        let mut bindings = keybindings::Bindings::new(keymap, true, &Default::default()).unwrap();
        assert!(matches!(
            bindings.resolve(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE), true),
            Resolution::Action(Action::ToggleAfterChanges)
        ));
        assert!(bindings.help().contains("toggle_after_changes: a"));
    }
    for binding in ["z", ""] {
        let mut bindings =
            config::Config::parse(&format!("[keybindings]\ntoggle_after_changes='{binding}'"))
                .unwrap()
                .bindings(None)
                .unwrap();
        assert!(matches!(
            bindings.resolve(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE), true),
            Resolution::Unbound
        ));
        assert_eq!(bindings.label(Action::ToggleAfterChanges), binding);
    }
    assert!(
        config::Config::parse("[keybindings]\ntoggle_after_changes='j'")
            .unwrap()
            .bindings(None)
            .is_err()
    );
}

#[test]
fn after_changes_deleted_binary_metadata_and_other_views_remain_truthful() {
    let directory = fixture();
    std::fs::write(directory.path().join("binary"), b"old\0binary").unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "binary"]);
    std::fs::remove_file(directory.path().join("one.rs")).unwrap();
    std::fs::write(directory.path().join("binary"), b"new\0binary").unwrap();
    git_run(directory.path(), &["update-index", "--chmod=+x", "two.rs"]);
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    for item in &app.capture.items {
        let mut preview = preview::Preview::item(directory.path(), item, &theme::Theme::default());
        let original = preview.lines.clone();
        preview.toggle_after_changes(0, 0);
        assert!(
            !preview
                .locations
                .iter()
                .any(|row| row.0.is_some() && row.1.is_none())
        );
        if item.new_path == b"binary" || item.new_path == b"two.rs" {
            assert_eq!(preview.lines, original);
        } else {
            assert!(
                preview
                    .lines
                    .iter()
                    .any(|line| line.to_string() == "+++ /dev/null")
            );
            assert!(
                preview
                    .lines
                    .iter()
                    .any(|line| line.to_string().starts_with("@@"))
            );
        }
    }
    app.key(KeyCode::Enter);
    for (documentation, definition_preview) in [(true, false), (false, true)] {
        app.documentation = documentation;
        app.definition_preview = definition_preview;
        let before = app.preview.as_ref().unwrap().clone();
        app.key(KeyCode::Char('a'));
        assert_eq!(app.preview.as_ref().unwrap().lines, before.lines);
        assert_eq!(app.preview.as_ref().unwrap().title, before.title);
    }
    let captured = &app.capture.catalog.sources[0];
    let mut source = preview::Preview::source(captured, &theme::Theme::default());
    let original = source.lines.clone();
    source.toggle_after_changes(0, 0);
    assert_eq!(source.lines, original);
}

#[test]
fn after_changes_search_reports_hidden_matches_and_restores_results() {
    let directory = fixture();
    std::fs::write(
        directory.path().join("one.rs"),
        "fn deleted_only_word() {}\n",
    )
    .unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "deleted search word"]);
    std::fs::write(directory.path().join("one.rs"), "fn replacement() {}\n").unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Enter);
    screen(&mut app, 100, 24);
    app.cursor = (
        app.preview
            .as_ref()
            .unwrap()
            .locations
            .iter()
            .position(|row| *row == (Some(1), None))
            .unwrap(),
        4,
    );
    app.key(KeyCode::Char('*'));
    assert_eq!(app.status, "/deleted_only_word · 1/1");
    app.key(KeyCode::Char('a'));
    assert_eq!(app.status, "/deleted_only_word · no displayed matches");
    assert!(
        app.preview
            .as_ref()
            .unwrap()
            .search
            .as_ref()
            .unwrap()
            .matches
            .is_empty()
    );
    app.status = "Other transient status".into();
    app.key(KeyCode::Char('n'));
    assert_eq!(app.status, "/deleted_only_word · no displayed matches");
    app.key(KeyCode::Char('a'));
    assert_eq!(app.status, "/deleted_only_word · 1 displayed match");
    app.key(KeyCode::Char('n'));
    assert_eq!(app.status, "/deleted_only_word · 1/1");
    assert_eq!(
        app.preview.as_ref().unwrap().locations[app.cursor.0],
        (Some(1), None)
    );
}

#[test]
fn yaml_colors_distinguish_tokens_in_captured_source_and_both_diff_sides() {
    let text = include_str!("../tests/fixtures/yaml-colors.yaml");
    for name in ["colors.yaml", "colors.yml"] {
        let directory = fixture();
        std::fs::write(directory.path().join(name), text).unwrap();
        git_run(directory.path(), &["add", "."]);
        git_run(directory.path(), &["commit", "-qm", "yaml"]);
        std::fs::write(directory.path().join(name), text.replace("42", "43")).unwrap();
        let captured = capture(directory.path(), &["HEAD", "--", name]);
        let sources = source::load(directory.path(), &captured.items[0]).unwrap();
        let mut catalog = source::Catalog::default();
        catalog.insert(
            source::ComparedSide::Old,
            name.as_bytes().to_vec(),
            text.as_bytes().to_vec(),
        );
        let source_preview =
            preview::Preview::source(&catalog.sources[0], &theme::Theme::classic());
        let diff = preview::Preview::item(
            directory.path(),
            &captured.items[0],
            &theme::Theme::classic(),
        );
        assert_eq!(diff.reason, None);
        let style_at = |view: &preview::Preview, token: &str| {
            let line = view
                .lines
                .iter()
                .find(|line| line.to_string().contains(token))
                .unwrap();
            let start = line.to_string().find(token).unwrap();
            let mut offset = 0;
            line.spans
                .iter()
                .find_map(|span| {
                    let contains = offset <= start && start < offset + span.content.len();
                    offset += span.content.len();
                    contains.then_some(span.style)
                })
                .unwrap()
        };
        for (token, color) in [
            ("service", Color::Cyan),
            ("name", Color::Cyan),
            ("café", Color::Green),
            ("42", Color::Yellow),
            ("true", Color::Magenta),
            ("null", Color::Yellow),
            ("# review", Color::DarkGray),
            ("shared", Color::Blue),
            ("!!str", Color::Cyan),
            ("%YAML", Color::Magenta),
            ("nested", Color::Cyan),
            ("plain", Color::Cyan),
            ("value", Color::Green),
            ("block content", Color::Green),
            ("&", Color::DarkGray),
            ("*", Color::DarkGray),
        ] {
            assert_eq!(style_at(&source_preview, token).fg, Some(color), "{token}");
        }
        assert_eq!(style_at(&diff, "42").fg, Some(Color::Yellow));
        assert_eq!(style_at(&diff, "43").fg, Some(Color::Yellow));
        for token in ["42", "43"] {
            assert!(style_at(&diff, token).bg.is_some());
            let changed_digit = diff.lines.iter().flat_map(|line| &line.spans).any(|span| {
                span.content.contains(if token == "42" { '2' } else { '3' })
                    && span
                        .style
                        .add_modifier
                        .contains(Modifier::UNDERLINED | Modifier::BOLD)
            });
            assert!(changed_digit);
        }
        for view in [&source_preview, &diff] {
            for (row, cells) in view.cells.iter().enumerate() {
                assert_eq!(cells.len(), view.lines[row].width());
            }
        }
        assert_eq!(sources.old.content, text);
        assert_eq!(sources.new.content, text.replace("42", "43"));
    }
}

#[test]
fn toml_colors_preserve_captured_sources_coordinates_and_diff_emphasis() {
    let old = include_str!("../tests/fixtures/toml-colors.toml");
    let _: toml::Value = toml::from_str(old).unwrap();
    let new = old.replace("42", "43").replace("🙂 old", "🙂 new");
    // Keep the entire construct matrix present on both changed sides.
    let directory = fixture();
    let path = directory.path().join("colors.toml");
    std::fs::write(&path, old).unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "toml"]);
    std::fs::write(&path, &new).unwrap();
    let captured = capture(
        directory.path(),
        &["HEAD", "--unified=100", "--", "colors.toml"],
    );
    let sources = source::load(directory.path(), &captured.items[0]).unwrap();
    let mut catalog = source::Catalog::default();
    for (side, content) in [
        (source::ComparedSide::Old, old),
        (source::ComparedSide::New, new.as_str()),
    ] {
        catalog.insert(side, b"colors.toml".to_vec(), content.as_bytes().to_vec());
    }
    let identities: Vec<_> = catalog.sources.iter().map(|s| s.identity.clone()).collect();
    // Rendering must continue to use the captured patch and catalog after mutation.
    std::fs::write(&path, "unrelated = 'checkout edit'\n").unwrap();
    let checkout = std::fs::read(&path).unwrap();
    let diff = preview::Preview::item(
        directory.path(),
        &captured.items[0],
        &theme::Theme::classic(),
    );
    assert_eq!(diff.reason, None);
    let style_at = |view: &preview::Preview, token: &str| {
        let line = view
            .lines
            .iter()
            .enumerate()
            .find(|(row, line)| {
                view.locations[*row] != (None, None) && line.to_string().contains(token)
            })
            .unwrap()
            .1;
        let start = line.to_string().find(token).unwrap();
        let mut offset = 0;
        line.spans
            .iter()
            .find_map(|span| {
                let contains = offset <= start && start < offset + span.content.len();
                offset += span.content.len();
                contains.then_some(span.style)
            })
            .unwrap()
    };
    for (index, source) in catalog.sources.iter().enumerate() {
        let view = preview::Preview::source(source, &theme::Theme::classic());
        for (token, color) in [
            ("# review", Color::DarkGray),
            ("title", Color::Cyan),
            ("café", Color::Green),
            ("enabled", Color::Cyan),
            ("true", Color::Magenta),
            ("false", Color::Magenta),
            ("1979-05-27T07:32:00Z", Color::Blue),
            ("1979-05-27T07:32:00", Color::Blue),
            ("local_date", Color::Cyan),
            ("07:32:00", Color::Blue),
            ("array value", Color::Green),
            ("inline value", Color::Green),
            ("\"quoted key\"", Color::Cyan),
            ("'literal key'", Color::Cyan),
            ("dotted key", Color::Cyan),
            ("owner", Color::Cyan),
            ("literal value", Color::Green),
            ("basic value", Color::Green),
            ("0xDEAD_BEEF", Color::Yellow),
            ("0o755", Color::Yellow),
            ("0b1101", Color::Yellow),
            ("-1.2e+3", Color::Yellow),
            ("+inf", Color::Yellow),
            ("nan", Color::Yellow),
            ("basic café", Color::Green),
            ("literal café", Color::Green),
            ("server", Color::Cyan),
            ("public host", Color::Cyan),
            ("products", Color::Cyan),
            ("[", Color::DarkGray),
            ("{", Color::DarkGray),
            (",", Color::DarkGray),
            ("]]", Color::DarkGray),
            ("=", Color::Cyan),
        ] {
            assert_eq!(style_at(&view, token).fg, Some(color), "{token}");
            assert_eq!(style_at(&diff, token).fg, Some(color), "diff {token}");
        }
        for key in ["released", "local_date_time", "local_date", "local_time"] {
            let line = view
                .lines
                .iter()
                .find(|line| line.to_string().starts_with(&format!("{key} =")))
                .unwrap();
            assert_eq!(
                line.spans.last().unwrap().style.fg,
                Some(Color::Blue),
                "{key}"
            );
        }
        let row = old
            .lines()
            .position(|line| line.starts_with("tabbed"))
            .unwrap();
        let expected = vec![
            Some(0),
            Some(1),
            Some(2),
            Some(3),
            Some(4),
            Some(5),
            Some(6),
            Some(7),
            Some(8),
            Some(8),
            Some(8),
            Some(8),
            Some(9),
            Some(10),
            Some(10),
            Some(11),
            Some(13),
            Some(13),
            Some(14),
            Some(15),
            Some(16),
            Some(17),
            Some(18),
        ];
        assert_eq!(view.cells[row], expected);
        let location = if index == 0 {
            (Some(row + 1), None)
        } else {
            (None, Some(row + 1))
        };
        assert_eq!(view.locations[row], location);
        let diff_row = diff
            .locations
            .iter()
            .position(|&value| value == location)
            .unwrap();
        assert_eq!(diff.cells[diff_row], [vec![None], expected].concat());
        assert_eq!(source.identity, identities[index]);
        assert_eq!(
            source.bytes.as_ref(),
            if index == 0 {
                old.as_bytes()
            } else {
                new.as_bytes()
            }
        );
        for (row, cells) in view.cells.iter().enumerate() {
            assert_eq!(cells.len(), view.lines[row].width());
        }
    }
    for (token, digit) in [("42", '2'), ("43", '3')] {
        assert_eq!(style_at(&diff, token).fg, Some(Color::Yellow));
        assert!(style_at(&diff, token).bg.is_some());
        assert!(diff.lines.iter().flat_map(|line| &line.spans).any(|span| {
            span.content.contains(digit)
                && span
                    .style
                    .add_modifier
                    .contains(Modifier::UNDERLINED | Modifier::BOLD)
        }));
    }
    assert_eq!(sources.old.content, old);
    assert_eq!(sources.new.content, new);
    assert_eq!(std::fs::read(&path).unwrap(), checkout);
    assert_eq!(
        source::load(directory.path(), &captured.items[0])
            .unwrap()
            .new
            .content,
        new
    );
}

#[test]
fn definition_preview_centers_once_with_rendered_geometry_and_restores_origin() {
    use source::ComparedSide;
    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    for short in [false, true] {
        let lines: usize = if short { 6 } else { 101 };
        let mut text = String::new();
        for row in 0..lines {
            text.push_str(&format!("fn center_target_{row:03}() {{}}\n"));
        }
        std::fs::write(directory.path().join("one.rs"), &text).unwrap();
        git_run(directory.path(), &["add", "."]);
        git_run(
            directory.path(),
            &["commit", "-qm", "captured centering fixture"],
        );
        // A same-length live edit captures both sides with identical rows.
        let text = text.replacen("fn center_target_000() {}", "fn center_target_000() { }", 1);
        std::fs::write(directory.path().join("one.rs"), &text).unwrap();
        for side in [ComparedSide::Old, ComparedSide::New] {
            for target in [1, lines / 2, lines - 2] {
                // Both sides are captured before replacing the live checkout.
                let mut app = application(directory.path(), state.path());
                app.key(KeyCode::Char('/'));
                for c in format!("center_target_{target:03}").chars() {
                    app.key(KeyCode::Char(c));
                }
                app.key(KeyCode::Enter);
                await_navigation(&mut app);
                let index = app
                    .navigation
                    .as_ref()
                    .unwrap()
                    .candidates
                    .iter()
                    .position(|c| app.capture.catalog.sources[c.source].side == side)
                    .unwrap();
                for _ in 0..index {
                    app.key(KeyCode::Char('j'));
                }
                let origin = screen(&mut app, 100, 24);
                let origin_position = (app.cursor, app.top, app.left, app.side);
                let selection = app.navigation.as_ref().unwrap().selected;
                std::fs::write(
                    directory.path().join("one.rs"),
                    "fn checkout_replaced() {}\n",
                )
                .unwrap();
                app.key(KeyCode::Char('P'));
                await_navigation(&mut app);
                assert!(app.definition_preview, "{}", app.status);
                // The first content layout differs from the origin dimensions.
                screen(&mut app, 100, 16);
                let height = app.viewport.0;
                assert_eq!(height, app.preview_area.height as usize);
                assert_eq!(height, 11); // borders, pane status and global rows excluded
                let expected_top = target
                    .saturating_sub(height / 2)
                    .min(lines.saturating_sub(height));
                assert_eq!(app.top, expected_top, "{side:?} target {target}");
                assert_eq!(app.cursor.0, target);
                assert_eq!(app.side, side);
                let mut terminal = Terminal::new(TestBackend::new(100, 16)).unwrap();
                let frame = terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
                assert!(!screen_text(frame.buffer).contains("checkout_replaced"));
                use ratatui::backend::Backend;
                assert_eq!(
                    terminal.backend_mut().get_cursor_position().unwrap().y,
                    app.preview_area.y + (target - expected_top) as u16
                );
                let opening_top = app.top;
                app.key(KeyCode::Char('j'));
                screen(&mut app, 100, 16);
                assert_eq!(app.top, opening_top); // ordinary motion does not recenter
                if !short {
                    app.scroll_preview(3, 0);
                    let scrolled = app.top;
                    screen(&mut app, 100, 16);
                    assert_eq!(app.top, scrolled);
                }
                for (width, height) in [(0, 0), (1, 1), (8, 3), (8, 6), (100, 24)] {
                    screen(&mut app, width, height);
                    assert!(app.cursor.0 < lines);
                    if app.viewport.0 > 0 {
                        assert!(app.cursor.0 >= app.top);
                        assert!(app.cursor.0 - app.top < app.viewport.0);
                    }
                }
                app.key(KeyCode::Char('m'));
                app.key(KeyCode::Char('q'));
                assert_eq!(screen(&mut app, 100, 24), origin);
                assert_eq!((app.cursor, app.top, app.left, app.side), origin_position);
                assert_eq!(app.navigation.as_ref().unwrap().selected, selection);
                assert!(app.marks.iter().all(|mark| !mark));
                std::fs::write(directory.path().join("one.rs"), &text).unwrap();
            }
        }
    }
    assert!(state.path().read_dir().unwrap().next().is_none());
}

#[test]
fn definition_preview_zero_height_defers_centering_and_dismissal_clears_it() {
    let directory = fixture();
    let text = (0..80)
        .map(|i| format!("fn deferred_target_{i}() {{}}\n"))
        .collect::<String>();
    std::fs::write(directory.path().join("one.rs"), text).unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Char('/'));
    for c in "deferred_target_40".chars() {
        app.key(KeyCode::Char(c));
    }
    app.key(KeyCode::Enter);
    await_navigation(&mut app);
    let origin = screen(&mut app, 100, 24);
    for dismiss_before_layout in [true, false] {
        app.key(KeyCode::Char('P'));
        await_navigation(&mut app);
        screen(&mut app, 8, 3);
        assert_eq!(app.viewport.0, 0);
        if !dismiss_before_layout {
            screen(&mut app, 100, 24);
            assert_eq!(app.top, 40 - app.viewport.0 / 2);
            // Nested ordinary source navigation inherits definition_preview but
            // must retain ordinary opening positioning.
            app.key(KeyCode::Char('g'));
            app.key(KeyCode::Char('d'));
            await_navigation(&mut app);
            app.key(KeyCode::Enter);
            screen(&mut app, 100, 24);
            assert_eq!(app.top, 40);
            app.key(KeyCode::Char('q'));
            app.key(KeyCode::Char('q'));
        }
        app.key(KeyCode::Char('q'));
        assert_eq!(screen(&mut app, 100, 24), origin);
    }
}

#[test]
fn definition_preview_movement_before_layout_consumes_centering() {
    let directory = fixture();
    let text = (0..80)
        .map(|i| format!("fn consumed_target_{i}() {{}}\n"))
        .collect::<String>();
    std::fs::write(directory.path().join("one.rs"), text).unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Char('/'));
    for c in "consumed_target_40".chars() {
        app.key(KeyCode::Char(c));
    }
    app.key(KeyCode::Enter);
    await_navigation(&mut app);
    let origin = screen(&mut app, 100, 24);
    for motion in ["j", "scroll", "G"] {
        app.key(KeyCode::Char('P'));
        await_navigation(&mut app);
        assert!(app.definition_preview, "{}", app.status);
        screen(&mut app, 8, 3);
        assert_eq!(app.viewport.0, 0);
        match motion {
            "scroll" => app.scroll_preview(3, 0),
            "G" => {
                // A counted jump bypasses ordinary row motion.
                for c in "20G".chars() {
                    app.key(KeyCode::Char(c));
                }
            }
            _ => {
                app.key(KeyCode::Char('j'));
            }
        }
        let moved_top = app.top;
        let moved_cursor = app.cursor.0;
        screen(&mut app, 100, 24);
        let height = app.viewport.0;
        assert!(height > 0);
        assert_ne!(app.top, 40 - height / 2, "{motion} recentered");
        assert_ne!(
            app.top,
            moved_cursor.saturating_sub(height / 2).min(80 - height),
            "{motion} recentered on the moved cursor"
        );
        assert_eq!(app.top, moved_top.min(80 - height), "{motion}");
        assert!(app.cursor.0 >= app.top && app.cursor.0 - app.top < app.viewport.0);
        app.key(KeyCode::Char('q'));
        assert_eq!(screen(&mut app, 100, 24), origin);
    }
}

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.key(KeyCode::Char(c));
    }
}

fn run_command(app: &mut App, text: &str) {
    app.key(KeyCode::Char(':'));
    assert!(app.command.is_some(), "command line did not open");
    type_text(app, text);
    app.key(KeyCode::Enter);
    assert!(app.command.is_none(), "command line stayed open");
}

fn colors(preview: &preview::Preview) -> Vec<(Option<Color>, Option<Color>)> {
    preview
        .lines
        .iter()
        .flat_map(|line| &line.spans)
        .map(|span| (span.style.fg, span.style.bg))
        .collect()
}

#[test]
fn theme_registry_lists_bundled_names_and_classic_matches_literals() {
    let names = theme::Theme::names();
    for name in [
        "gruvbox-dark",
        "gruvbox-light",
        "Dracula",
        "review-classic",
        "gruvbox-baby",
        "ansi",
    ] {
        assert!(names.contains(&name), "{name}");
    }
    assert_eq!(theme::Theme::default().name, "gruvbox-baby");
    assert_eq!(theme::Theme::named("dracula").unwrap().name, "Dracula");
    assert!(theme::Theme::named("nope").is_none());
    let classic = theme::Theme::named("REVIEW-CLASSIC").unwrap();
    assert_eq!(classic, theme::Theme::classic());
    assert_eq!(classic.added_bg, Color::Rgb(10, 30, 10));
    assert_eq!(classic.removed_bg, Color::Rgb(35, 10, 10));
    assert_eq!(classic.search_bg, Color::Rgb(80, 65, 0));
    assert_eq!(classic.default_fg, None);
    // Every theme keeps diffs, marks and borders distinguishable.
    for name in names {
        let theme = theme::Theme::named(name).unwrap();
        assert_ne!(theme.added_bg, theme.removed_bg, "{name}");
        assert_ne!(theme.added_fg, theme.removed_fg, "{name}");
        assert_ne!(theme.reviewed_mark, theme.pending_mark, "{name}");
        assert_ne!(theme.reviewed_name, theme.pending_name, "{name}");
        assert_ne!(theme.border_focused, theme.border_unfocused, "{name}");
        assert_ne!(theme.search_bg, theme.added_bg, "{name}");
        assert_ne!(theme.search_bg, theme.removed_bg, "{name}");
    }
}

#[test]
fn gruvbox_baby_uses_the_upstream_tree_sitter_palette() {
    let theme = theme::Theme::named("Gruvbox-Baby").unwrap();
    assert_eq!(theme, theme::Theme::gruvbox_baby());
    assert_eq!(theme.name, "gruvbox-baby");
    assert_eq!(theme.default_fg, Some(Color::Rgb(0xeb, 0xdb, 0xb2)));
    assert_eq!(theme.added_bg, Color::Rgb(0x26, 0x33, 0x2c));
    assert_eq!(theme.removed_bg, Color::Rgb(0x57, 0x2e, 0x33));
    let mut catalog = source::Catalog::default();
    catalog.insert(
        source::ComparedSide::New,
        b"one.rs".to_vec(),
        b"// note\nfn one() -> u8 {\n\tlet answer = \"text\";\n\ttrue;\n\tlet n = 7;\n}\n".to_vec(),
    );
    let view = preview::Preview::source(&catalog.sources[0], &theme);
    let fg_of = |token: &str| {
        let line = view
            .lines
            .iter()
            .find(|line| line.to_string().contains(token))
            .unwrap_or_else(|| panic!("{token}"));
        let start = line.to_string().find(token).unwrap();
        let mut offset = 0;
        line.spans
            .iter()
            .find_map(|span| {
                let contains = offset <= start && start < offset + span.content.len();
                offset += span.content.len();
                contains.then_some(span.style.fg)
            })
            .unwrap()
    };
    for (token, color) in [
        ("// note", (0x66, 0x5c, 0x54)),
        ("fn", (0xfb, 0x49, 0x34)),
        ("one", (0xee, 0xbd, 0x35)),
        ("u8", (0x8e, 0xc0, 0x7c)),
        ("\"text\"", (0x98, 0x97, 0x1a)),
        // Rust literals are @constant.builtin, which gruvbox-baby colors pink.
        ("true", (0xd4, 0x87, 0x9c)),
        ("7;", (0xd4, 0x87, 0x9c)),
    ] {
        assert_eq!(
            fg_of(token),
            Some(Color::Rgb(color.0, color.1, color.2)),
            "{token}"
        );
    }
}

#[test]
fn gruvbox_dark_distinguishes_code_tokens_keys_and_diff_sides() {
    let theme = theme::Theme::named("gruvbox-dark").unwrap();
    let directory = fixture();
    let rust = "// note\nfn one() {\n\tlet answer = \"text\";\n\tlet number = 1;\n\tone();\n}\n";
    std::fs::write(directory.path().join("one.rs"), rust).unwrap();
    std::fs::write(directory.path().join("keys.yaml"), "key: 1\n").unwrap();
    std::fs::write(directory.path().join("keys.toml"), "key = 1\n").unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "tokens"]);
    std::fs::write(
        directory.path().join("one.rs"),
        rust.replace("= 1;", "= 2;"),
    )
    .unwrap();
    std::fs::write(directory.path().join("keys.yaml"), "key: 2\n").unwrap();
    std::fs::write(directory.path().join("keys.toml"), "key = 2\n").unwrap();
    let captured = capture(directory.path(), &["HEAD"]);
    let item = |name: &str| {
        captured
            .items
            .iter()
            .find(|item| item.new_path == name.as_bytes())
            .unwrap()
    };
    let fg_at = |view: &preview::Preview, row_text: &str, token: &str| {
        let line = view
            .lines
            .iter()
            .find(|line| line.to_string().contains(row_text))
            .unwrap_or_else(|| panic!("{row_text}"));
        let start = line.to_string().find(token).unwrap();
        let mut offset = 0;
        line.spans
            .iter()
            .find_map(|span| {
                let contains = offset <= start && start < offset + span.content.len();
                offset += span.content.len();
                contains.then_some(span.style)
            })
            .unwrap()
    };
    let diff = preview::Preview::item(directory.path(), item("one.rs"), &theme);
    assert_eq!(diff.reason, None);
    let comment = fg_at(&diff, "// note", "//").fg;
    let string = fg_at(&diff, "\"text\"", "\"text\"").fg;
    let keyword = fg_at(&diff, "let answer", "let").fg;
    let function = fg_at(&diff, "fn one", "one").fg;
    let number = fg_at(&diff, "+    let number", "2").fg;
    let yaml = preview::Preview::item(directory.path(), item("keys.yaml"), &theme);
    let toml = preview::Preview::item(directory.path(), item("keys.toml"), &theme);
    let yaml_key = fg_at(&yaml, "+key", "key").fg;
    let toml_key = fg_at(&toml, "+key", "key").fg;
    assert_eq!(yaml_key, toml_key);
    let distinct = [comment, string, keyword, function, number, yaml_key];
    for (index, color) in distinct.iter().enumerate() {
        assert!(color.is_some());
        assert!(
            !distinct[..index].contains(color),
            "{distinct:?} repeats {color:?}"
        );
    }
    let added = fg_at(&diff, "+    let number", "2");
    let removed = fg_at(&diff, "-    let number", "1");
    assert!(added.bg.is_some() && removed.bg.is_some());
    assert_ne!(added.bg, removed.bg);
    assert_eq!(added.bg, Some(theme.added_bg));
    assert!(
        added
            .add_modifier
            .contains(Modifier::BOLD | Modifier::UNDERLINED)
    );
    // The background of ordinary text is never painted.
    assert!(diff.lines.iter().flat_map(|line| &line.spans).all(|span| {
        span.style.bg.is_none()
            || span.style.bg == Some(theme.added_bg)
            || span.style.bg == Some(theme.removed_bg)
    }));
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    assert_eq!(app.theme.name, "gruvbox-baby");
    app.set_theme(theme);
    let rendered = screen(&mut app, 100, 24);
    assert!(
        rendered
            .content
            .iter()
            .any(|cell| cell.symbol() == "F" && cell.fg == theme.border_focused)
    );
}

#[test]
fn command_line_sets_queries_and_aliases_the_colorscheme() {
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Enter);
    screen(&mut app, 100, 24);
    let default = colors(app.preview.as_ref().unwrap());
    app.key(KeyCode::Char(':'));
    type_text(&mut app, "set appearance.colorscheme=dracula");
    let text = screen_text(&screen(&mut app, 100, 24));
    assert!(
        text.contains(":set appearance.colorscheme=dracula"),
        "{text}"
    );
    app.key(KeyCode::Enter);
    assert_eq!(app.theme.name, "Dracula");
    assert!(app.status.contains("Dracula"), "{}", app.status);
    let dracula = colors(app.preview.as_ref().unwrap());
    assert_ne!(dracula, default);
    run_command(&mut app, "colorscheme review-classic");
    assert_eq!(app.theme.name, "review-classic");
    let fresh = preview::Preview::item(
        directory.path(),
        &app.capture.items[0],
        &theme::Theme::classic(),
    );
    assert_eq!(app.preview.as_ref().unwrap().lines, fresh.lines);
    run_command(&mut app, "colorscheme");
    assert!(app.status.contains("review-classic"), "{}", app.status);
    run_command(&mut app, "set appearance.colorscheme");
    assert!(app.status.contains("review-classic"), "{}", app.status);
    run_command(&mut app, "  colorscheme   gruvbox-baby  ");
    assert_eq!(app.theme.name, "gruvbox-baby");
    assert_eq!(colors(app.preview.as_ref().unwrap()), default);
}

#[test]
fn command_line_errors_cancel_empty_and_quit_leave_theme_unchanged() {
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Enter);
    let before = app.preview.as_ref().unwrap().lines.clone();
    for (command, token) in [
        ("frob", "\"frob\""),
        ("set foo=1", "\"foo\""),
        ("colorscheme nope", "\"nope\""),
        ("set appearance.colorscheme=nope", "\"nope\""),
    ] {
        run_command(&mut app, command);
        assert_eq!(app.theme.name, "gruvbox-baby", "{command}");
        assert!(app.status.contains(token), "{command}: {}", app.status);
        assert_eq!(app.preview.as_ref().unwrap().lines, before);
    }
    app.status = "kept".into();
    app.key(KeyCode::Char(':'));
    type_text(&mut app, "colorscheme review-classic");
    app.key(KeyCode::Esc);
    assert!(app.command.is_none());
    assert_eq!(app.theme.name, "gruvbox-baby");
    assert_eq!(app.status, "kept");
    // Escape closed only the command line, not the diff.
    assert!(app.preview.is_some());
    run_command(&mut app, "");
    assert_eq!(app.status, "kept");
    assert!(app.preview.is_some());
    // Backspace edits; the 256-byte cap holds.
    app.key(KeyCode::Char(':'));
    type_text(&mut app, "ab");
    app.key(KeyCode::Backspace);
    assert_eq!(app.command.as_ref().unwrap().text, "a");
    type_text(&mut app, &"x".repeat(300));
    assert_eq!(app.command.as_ref().unwrap().text.len(), 256);
    app.key(KeyCode::Esc);
    // Command text never reaches other actions; the quit sequence still quits.
    app.key(KeyCode::Char(':'));
    type_text(&mut app, "mq");
    assert_eq!(app.marks, [false, false]);
    assert!(app.preview.is_some());
    assert!(!app.key(KeyCode::Char(',')));
    assert!(app.key(KeyCode::Char('q')));
    app.command = None;
    app.key(KeyCode::Char(':'));
    app.key(KeyCode::Char(','));
    app.key(KeyCode::Char('x'));
    assert_eq!(app.command.as_ref().unwrap().text, ",x");
}

#[test]
fn command_line_tab_completion_cycles_in_place() {
    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    let text = |app: &App| app.command.as_ref().unwrap().text.clone();
    app.key(KeyCode::Char(':'));
    type_text(&mut app, "colo");
    app.key(KeyCode::Tab);
    assert_eq!(text(&app), "colorscheme");
    type_text(&mut app, " gru");
    app.key(KeyCode::Tab);
    assert_eq!(text(&app), "colorscheme gruvbox-baby");
    app.key(KeyCode::Tab);
    assert_eq!(text(&app), "colorscheme gruvbox-dark");
    app.key(KeyCode::BackTab);
    assert_eq!(text(&app), "colorscheme gruvbox-baby");
    app.key(KeyCode::Tab);
    app.key(KeyCode::Tab);
    assert_eq!(text(&app), "colorscheme gruvbox-light");
    app.key(KeyCode::Tab);
    assert_eq!(text(&app), "colorscheme gruvbox-baby");
    app.key(KeyCode::Esc);
    app.key(KeyCode::Char(':'));
    type_text(&mut app, "set appe");
    app.key(KeyCode::Tab);
    assert_eq!(text(&app), "set appearance.colorscheme");
    type_text(&mut app, "=REVIEW-c");
    app.key(KeyCode::Tab);
    assert_eq!(text(&app), "set appearance.colorscheme=review-classic");
    app.key(KeyCode::Enter);
    assert_eq!(app.theme.name, "review-classic");
    // Whitespace around '=' is accepted by execution and kept by completion.
    app.key(KeyCode::Char(':'));
    type_text(&mut app, "set appearance.colorscheme = gru");
    app.key(KeyCode::Tab);
    assert_eq!(text(&app), "set appearance.colorscheme = gruvbox-baby");
    app.key(KeyCode::Enter);
    assert_eq!(app.theme.name, "gruvbox-baby");
    for unmatched in [
        "zzz",
        "colorscheme zzz",
        "set zzz",
        "set foo=gru",
        "frob gru",
    ] {
        app.key(KeyCode::Char(':'));
        type_text(&mut app, unmatched);
        app.key(KeyCode::Tab);
        assert_eq!(text(&app), unmatched);
        app.key(KeyCode::BackTab);
        assert_eq!(text(&app), unmatched);
        app.key(KeyCode::Esc);
    }
    // Editing after a completion starts a new completion.
    app.key(KeyCode::Char(':'));
    type_text(&mut app, "colorscheme Dra");
    app.key(KeyCode::Tab);
    assert_eq!(text(&app), "colorscheme Dracula");
    app.key(KeyCode::Backspace);
    app.key(KeyCode::Tab);
    assert_eq!(text(&app), "colorscheme Dracula");
}

#[test]
fn theme_switch_restyles_history_and_preserves_view_state_and_marks() {
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Down);
    app.key(KeyCode::Char('m'));
    app.key(KeyCode::Up);
    let marks = app.marks.clone();
    let store = std::fs::read(&app.store.path).unwrap();
    app.key(KeyCode::Enter);
    screen(&mut app, 100, 12);
    app.key(KeyCode::Char('a'));
    let added = app
        .preview
        .as_ref()
        .unwrap()
        .locations
        .iter()
        .position(|row| row.1 == Some(2))
        .unwrap();
    app.cursor = (added, 6);
    app.key(KeyCode::Char('*'));
    let diff_cursor = app.cursor;
    let diff_top = app.top;
    let diff_lines = app.preview.as_ref().unwrap().lines.len();
    let diff_title = app.preview.as_ref().unwrap().title.clone();
    // Navigate deeper: symbol search, then its captured source.
    app.key(KeyCode::Char('/'));
    type_text(&mut app, "one");
    app.key(KeyCode::Enter);
    await_navigation(&mut app);
    let list_text: Vec<_> = app
        .preview
        .as_ref()
        .unwrap()
        .lines
        .iter()
        .map(ToString::to_string)
        .collect();
    app.key(KeyCode::Enter);
    assert!(
        app.preview
            .as_ref()
            .unwrap()
            .title
            .as_deref()
            .unwrap()
            .starts_with("Captured")
    );
    let source_cursor = app.cursor;
    run_command(&mut app, "colorscheme review-classic");
    assert_eq!(app.cursor, source_cursor);
    let current = app.preview.as_ref().unwrap();
    let source = app
        .capture
        .catalog
        .sources
        .iter()
        .map(|source| preview::Preview::source(source, &theme::Theme::classic()))
        .find(|source| source.title == current.title)
        .unwrap();
    assert_eq!(current.lines, source.lines);
    let back = |app: &mut App| {
        app.key_event(crossterm::event::KeyEvent::new(
            KeyCode::Char('o'),
            crossterm::event::KeyModifiers::CONTROL,
        ))
    };
    back(&mut app);
    let list = app.preview.as_ref().unwrap();
    assert_eq!(
        list.lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
        list_text
    );
    assert!(
        list.lines
            .iter()
            .flat_map(|line| &line.spans)
            .all(|span| span.style.fg.is_none())
    );
    back(&mut app);
    let preview = app.preview.as_ref().unwrap();
    assert_eq!(preview.title, diff_title);
    assert_eq!(preview.lines.len(), diff_lines);
    assert_eq!((app.cursor, app.top), (diff_cursor, diff_top));
    assert!(preview.search.is_some());
    let classic = theme::Theme::classic();
    let spans: Vec<_> = preview.lines.iter().flat_map(|line| &line.spans).collect();
    assert!(
        spans
            .iter()
            .any(|span| span.style.bg == Some(classic.added_bg))
    );
    assert!(
        spans
            .iter()
            .any(|span| span.style.fg == Some(Color::Magenta))
    );
    let visible = preview.visible(app.top, app.left, app.viewport.0, app.viewport.1);
    assert!(
        visible
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.style.bg == Some(classic.search_bg))
    );
    // Leaving after-changes reveals the ordinary view in the new theme too.
    app.key(KeyCode::Char('a'));
    let ordinary = app.preview.as_ref().unwrap();
    assert!(ordinary.title.is_none());
    let fresh = preview::Preview::item(directory.path(), &app.capture.items[0], &classic);
    assert_eq!(ordinary.lines, fresh.lines);
    assert!(
        ordinary
            .lines
            .iter()
            .flat_map(|line| &line.spans)
            .any(|span| span.style.bg == Some(classic.removed_bg))
    );
    assert_eq!(app.marks, marks);
    assert_eq!(std::fs::read(&app.store.path).unwrap(), store);
}

#[test]
fn command_line_renders_in_status_row_with_cursor_and_rebinds() {
    use crossterm::event::{KeyEvent, KeyModifiers};
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Char(':'));
    type_text(&mut app, "colo");
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let status: String = (0..80)
        .map(|x| buffer[(x, 18)].symbol().to_string())
        .collect();
    assert!(status.starts_with(":colo "), "{status}");
    terminal.backend_mut().assert_cursor_position((5, 18));
    // The command line also takes priority over an editing search prompt.
    app.key(KeyCode::Esc);
    app.key(KeyCode::Enter);
    app.key(KeyCode::Char('/'));
    type_text(&mut app, "x");
    app.command = Some(Default::default());
    let text = screen_text(&screen(&mut app, 80, 20));
    assert!(!text.contains("Search/outline"), "{text}");
    app.command = None;
    app.navigation = None;
    // Emacs opens it with alt-x; ':' stays ordinary there.
    app.bindings = config::Config::parse("[features]\nkeymap = 'emacs'\n")
        .unwrap()
        .bindings(None)
        .unwrap();
    app.key(KeyCode::Char(':'));
    assert!(app.command.is_none());
    app.key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT));
    assert!(app.command.is_some());
    assert!(app.bindings.legend().contains("alt-x command"));
    app.key(KeyCode::Esc);
    // Rebound and disabled like any other action.
    app.bindings = config::Config::parse("[keybindings]\ncommand_line = 'ctrl-x'\n")
        .unwrap()
        .bindings(None)
        .unwrap();
    app.key(KeyCode::Char(':'));
    assert!(app.command.is_none());
    app.key_event(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
    assert!(app.command.is_some());
    app.key(KeyCode::Esc);
    app.bindings = config::Config::parse("[keybindings]\ncommand_line = ''\n")
        .unwrap()
        .bindings(None)
        .unwrap();
    assert!(app.bindings.help().contains("command_line: (disabled)\n"));
    app.key(KeyCode::Char(':'));
    assert!(app.command.is_none());
    assert!(
        keybindings::Bindings::default()
            .help()
            .contains("command_line: :\n")
    );
}

fn alt(character: char) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(
        KeyCode::Char(character),
        crossterm::event::KeyModifiers::ALT,
    )
}

fn ctrl(character: char) -> crossterm::event::KeyEvent {
    crossterm::event::KeyEvent::new(
        KeyCode::Char(character),
        crossterm::event::KeyModifiers::CONTROL,
    )
}

fn palette_texts(app: &App) -> Vec<String> {
    app.palette
        .as_ref()
        .unwrap()
        .matches()
        .into_iter()
        .map(|entry| entry.text.clone())
        .collect()
}

#[test]
fn palette_entries_follow_the_registry_order() {
    let names = theme::Theme::names();
    let entries = commands::entries();
    let mut expected = vec!["colorscheme".to_owned()];
    expected.extend(names.iter().map(|name| format!("colorscheme {name}")));
    expected.push("set appearance.colorscheme".into());
    expected.extend(
        names
            .iter()
            .map(|name| format!("set appearance.colorscheme={name}")),
    );
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.text.clone())
            .collect::<Vec<_>>(),
        expected
    );
    // Bare `set` only errors and is not listed.
    assert!(!entries.iter().any(|entry| entry.text == "set"));
    for entry in &entries {
        let summary = if entry.text.starts_with("set ") {
            commands::SETTINGS[0].summary
        } else {
            commands::COMMANDS[0].summary
        };
        assert_eq!(entry.summary, summary, "{}", entry.text);
    }
    // Every listed entry is a command the command line accepts.
    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    for entry in &entries {
        commands::execute(&mut app, &entry.text);
        assert!(
            !app.status.contains("Unknown") && !app.status.contains("requires"),
            "{}: {}",
            entry.text,
            app.status
        );
    }
}

#[test]
fn palette_filters_runs_like_the_command_line_and_cancels() {
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    let mut reference = application(directory.path(), state.path());
    app.key(KeyCode::Down);
    app.key(KeyCode::Char('m'));
    app.key(KeyCode::Up);
    let marks = app.marks.clone();
    let store = std::fs::read(&app.store.path).unwrap();
    app.key(KeyCode::Enter);
    reference.key(KeyCode::Enter);
    screen(&mut app, 100, 24);
    let default = colors(app.preview.as_ref().unwrap());
    // Filter a setting: only its entries remain.
    app.key_event(alt('p'));
    assert!(app.palette.is_some());
    assert_eq!(palette_texts(&app).len(), commands::entries().len());
    type_text(&mut app, "APPEARANCE");
    let texts = palette_texts(&app);
    assert!(!texts.is_empty());
    assert!(
        texts
            .iter()
            .all(|text| text.starts_with("set appearance.colorscheme")),
        "{texts:?}"
    );
    // Multi-word: every word must match the text or the summary.
    for _ in 0.."APPEARANCE".len() {
        app.key(KeyCode::Backspace);
    }
    type_text(&mut app, "set dracula");
    assert_eq!(palette_texts(&app), ["set appearance.colorscheme=Dracula"]);
    app.key(KeyCode::Esc);
    assert!(app.palette.is_none());
    // Run a theme: identical to the equivalent `:` command.
    app.key_event(alt('p'));
    type_text(&mut app, "classic");
    assert_eq!(
        palette_texts(&app),
        [
            "colorscheme review-classic",
            "set appearance.colorscheme=review-classic"
        ]
    );
    app.key(KeyCode::Enter);
    assert!(app.palette.is_none());
    run_command(&mut reference, "colorscheme review-classic");
    assert_eq!(app.theme.name, "review-classic");
    assert_eq!(
        app.status,
        "appearance.colorscheme=review-classic (this session only)"
    );
    assert_eq!(app.status, reference.status);
    assert_eq!(
        app.preview.as_ref().unwrap().lines,
        reference.preview.as_ref().unwrap().lines
    );
    // Cancel: theme, status, diff, focus and cursor untouched.
    app.cursor = (1, 2);
    let view = (
        app.focus,
        app.cursor,
        app.top,
        app.left,
        app.selected,
        app.preview.as_ref().unwrap().lines.clone(),
    );
    app.status = "kept".into();
    for cancel in [crossterm::event::KeyEvent::from(KeyCode::Esc), ctrl('g')] {
        app.key_event(alt('p'));
        type_text(&mut app, "dark");
        app.key_event(cancel);
        assert!(app.palette.is_none());
        assert_eq!(app.theme.name, "review-classic");
        assert_eq!(app.status, "kept");
        assert_eq!(
            (
                app.focus,
                app.cursor,
                app.top,
                app.left,
                app.selected,
                app.preview.as_ref().unwrap().lines.clone(),
            ),
            view
        );
    }
    // No match: Enter keeps the palette open and changes nothing.
    app.key_event(alt('p'));
    type_text(&mut app, "zzz");
    assert!(palette_texts(&app).is_empty());
    app.key(KeyCode::Enter);
    assert!(app.palette.is_some());
    assert_eq!(app.status, "kept");
    let text = screen_text(&screen(&mut app, 100, 24));
    assert!(text.contains("No matching commands"), "{text}");
    app.key(KeyCode::Esc);
    // Selection moves with arrows and Ctrl-N/Ctrl-P, clamps and resets on edits.
    app.key_event(alt('p'));
    type_text(&mut app, "gruvbox");
    let gruvbox = palette_texts(&app);
    assert_eq!(gruvbox[0], "colorscheme gruvbox-baby");
    let selected = |app: &App| app.palette.as_ref().unwrap().selected;
    app.key(KeyCode::Up);
    assert_eq!(selected(&app), 0);
    app.key(KeyCode::Down);
    app.key_event(ctrl('n'));
    assert_eq!(selected(&app), 2);
    app.key_event(ctrl('p'));
    assert_eq!(selected(&app), 1);
    for _ in 0..gruvbox.len() + 3 {
        app.key(KeyCode::Down);
    }
    assert_eq!(selected(&app), gruvbox.len() - 1);
    app.key(KeyCode::Backspace);
    assert_eq!(selected(&app), 0);
    app.key(KeyCode::Down);
    app.key(KeyCode::Char('x'));
    assert_eq!(selected(&app), 0);
    app.key(KeyCode::Backspace);
    app.key(KeyCode::Down);
    app.key(KeyCode::Enter);
    assert_eq!(app.theme.name, "gruvbox-dark");
    assert_eq!(
        app.status,
        "appearance.colorscheme=gruvbox-dark (this session only)"
    );
    run_command(&mut app, "colorscheme gruvbox-baby");
    assert_eq!(colors(app.preview.as_ref().unwrap()), default);
    // Query text never reaches other actions; the 256-byte cap holds.
    app.key_event(alt('p'));
    type_text(&mut app, "mqa:");
    assert!(app.command.is_none());
    type_text(&mut app, &"x".repeat(300));
    assert_eq!(app.palette.as_ref().unwrap().query.len(), 256);
    app.key(KeyCode::Esc);
    assert_eq!(app.marks, marks);
    assert_eq!(std::fs::read(&app.store.path).unwrap(), store);
    assert!(app.preview.is_some());
    // The quit sequence quits; a failed quit prefix becomes query text.
    app.key_event(alt('p'));
    app.key(KeyCode::Char(','));
    app.key(KeyCode::Char('x'));
    assert_eq!(app.palette.as_ref().unwrap().query, ",x");
    assert!(!app.key(KeyCode::Char(',')));
    assert!(app.key(KeyCode::Char('q')));
}

#[test]
fn palette_is_modal_and_opening_clears_pending_keys() {
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    screen(&mut app, 100, 24);
    // A pending count is dropped when the palette opens.
    app.key(KeyCode::Enter);
    screen(&mut app, 100, 24);
    app.cursor = (0, 0);
    app.key(KeyCode::Char('3'));
    app.key_event(alt('p'));
    assert!(app.palette.is_some());
    assert!(!app.bindings.pending());
    app.key(KeyCode::Esc);
    app.key(KeyCode::Char('j'));
    assert_eq!(app.cursor.0, 1);
    app.key(KeyCode::Char('q'));
    assert!(app.preview.is_none());
    let selected = app.selected;
    let offset = app.file_offset;
    app.key_event(alt('p'));
    let area = app.file_area;
    app.mouse(
        mouse_event(
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            area.x + 1,
            area.y + 1,
        ),
        std::time::Instant::now(),
    );
    app.mouse(
        mouse_event(
            crossterm::event::MouseEventKind::ScrollDown,
            area.x + 1,
            area.y + 1,
        ),
        std::time::Instant::now(),
    );
    assert_eq!((app.selected, app.file_offset), (selected, offset));
    assert!(app.palette.is_some());
    app.key(KeyCode::Esc);
    // The same click acts once the palette is closed.
    app.mouse(
        mouse_event(
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            area.x + 1,
            area.y + 1,
        ),
        std::time::Instant::now(),
    );
    assert_eq!(app.selected, 1);
}

#[test]
fn palette_renders_a_floating_box_with_cursor_and_selection() {
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Enter);
    let before = screen(&mut app, 100, 24);
    app.key_event(alt('p'));
    type_text(&mut app, "classic");
    app.key(KeyCode::Down);
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row = |y: u16| -> String {
        (0..100)
            .map(|x| buffer[(x, y)].symbol().to_string())
            .collect()
    };
    // 90x16 box centered on 100x24: x 5..95, y 4..20.
    assert!(row(4).contains("┌ Command palette "), "{}", row(4));
    assert_eq!(buffer[(5, 4)].fg, app.theme.border_focused);
    assert!(
        row(19).contains(" enter run · esc cancel · ↑/↓ select "),
        "{}",
        row(19)
    );
    assert!(row(5)[..].contains("│> classic "), "{}", row(5));
    terminal
        .backend_mut()
        .assert_cursor_position((6 + "> classic".len() as u16, 5));
    assert!(
        row(6).contains("│colorscheme review-classic  show or switch"),
        "{}",
        row(6)
    );
    assert!(
        row(7).contains("│set appearance.colorscheme=review-classic  color theme"),
        "{}",
        row(7)
    );
    // The selected row is highlighted across the inner width; others are not.
    let selection = app.theme.selection;
    assert!(!selection.add_modifier.is_empty());
    for x in 6..94 {
        assert!(
            buffer[(x, 7)].modifier.contains(selection.add_modifier),
            "{x}"
        );
        assert!(
            !buffer[(x, 6)].modifier.contains(selection.add_modifier),
            "{x}"
        );
    }
    assert_ne!(buffer[(6, 6)].style(), buffer[(6, 7)].style());
    assert!(!row(8).contains("colorscheme"), "{}", row(8));
    // The selection scrolls into view in a long list.
    app.palette = Some(commands::Palette::default());
    for _ in 0..30 {
        app.key(KeyCode::Down);
    }
    let text = screen_text(&screen(&mut app, 100, 24));
    assert!(
        text.contains(&format!("colorscheme {}", theme::Theme::names()[29])),
        "{text}"
    );
    assert!(!text.contains("colorscheme 1337"), "{text}");
    // Escape restores the screen behind it.
    app.key(KeyCode::Esc);
    assert_eq!(screen(&mut app, 100, 24), before);
    // Too small terminals skip drawing without panicking.
    app.key_event(alt('p'));
    for (width, height) in [(1, 1), (6, 6), (7, 7), (20, 8)] {
        screen(&mut app, width, height);
    }
    app.key(KeyCode::Esc);
}

#[test]
fn palette_opens_in_both_keymaps_and_rebinds() {
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key_event(alt('p'));
    assert!(app.palette.is_some());
    app.key(KeyCode::Esc);
    assert!(
        keybindings::Bindings::default()
            .help()
            .contains("command_palette: alt-p\n")
    );
    // Emacs: alt-p opens it; ctrl-p stays cursor up.
    app.bindings = config::Config::parse("[features]\nkeymap = 'emacs'\n")
        .unwrap()
        .bindings(None)
        .unwrap();
    app.key_event(ctrl('n'));
    assert_eq!(app.selected, 1);
    app.key_event(ctrl('p'));
    assert_eq!(app.selected, 0);
    assert!(app.palette.is_none());
    app.key_event(alt('p'));
    assert!(app.palette.is_some());
    app.key(KeyCode::Esc);
    // The footer legend is unchanged.
    assert!(!app.bindings.legend().contains("alt-p"));
    // Rebound and disabled like any other action.
    app.bindings = config::Config::parse(
        "[keybindings]\ncommand_palette = 'ctrl-k'\nprevious_file_diff = ''\n",
    )
    .unwrap()
    .bindings(None)
    .unwrap();
    app.key_event(alt('p'));
    assert!(app.palette.is_none());
    app.key_event(ctrl('k'));
    assert!(app.palette.is_some());
    app.key(KeyCode::Esc);
    app.bindings = config::Config::parse("[keybindings]\ncommand_palette = ''\n")
        .unwrap()
        .bindings(None)
        .unwrap();
    assert!(
        app.bindings
            .help()
            .contains("command_palette: (disabled)\n")
    );
    app.key_event(alt('p'));
    assert!(app.palette.is_none());
    let help = help(None, None).unwrap();
    assert!(
        help.contains(
            "Command palette (command_palette): type to filter, Up/Down select, Enter runs, Escape cancels.\n"
        ),
        "{help}"
    );
}

#[test]
fn appearance_config_selects_and_validates_the_startup_theme() {
    assert_eq!(
        config::Config::parse("").unwrap().theme().unwrap().name,
        "gruvbox-baby"
    );
    let classic = config::Config::parse("[appearance]\ncolorscheme = \"review-classic\"\n")
        .unwrap()
        .theme()
        .unwrap();
    assert_eq!(classic, theme::Theme::classic());
    let error = config::Config::parse("[appearance]\ncolorscheme = \"nope\"\n")
        .unwrap()
        .theme()
        .unwrap_err();
    assert!(format!("{error:#}").contains("\"nope\""), "{error:#}");
    assert!(config::Config::parse("[appearance]\nother = 1\n").is_err());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("bad.toml");
    std::fs::write(&path, "[appearance]\ncolorscheme = \"nope\"\n").unwrap();
    assert!(format!("{:#}", help(None, Some(&path)).unwrap_err()).contains("\"nope\""));
    let text = help(None, None).unwrap();
    assert!(text.contains(":colorscheme"), "{text}");
    assert!(text.contains("appearance.colorscheme"), "{text}");
    assert!(text.contains("gruvbox-baby"), "{text}");
}

#[test]
fn gruvbox_dark_colors_captured_source_views_for_rust_yaml_and_toml() {
    let theme = theme::Theme::named("gruvbox-dark").unwrap();
    assert_eq!(theme.name, "gruvbox-dark");
    let comment = Color::Rgb(146, 131, 116);
    let string = Color::Rgb(184, 187, 38);
    let keyword = Color::Rgb(251, 73, 52);
    let function = Color::Rgb(142, 192, 124);
    let number = Color::Rgb(211, 134, 155);
    let key = Color::Rgb(131, 165, 152);
    let rust = "// note\nfn one() {\n\tlet answer = \"text\";\n\tlet number = 1;\n}\n";
    let yaml = include_str!("../tests/fixtures/yaml-colors.yaml");
    let toml = include_str!("../tests/fixtures/toml-colors.toml");
    let mut catalog = source::Catalog::default();
    for (name, text) in [
        ("one.rs", rust),
        ("colors.yaml", yaml),
        ("colors.toml", toml),
    ] {
        catalog.insert(
            source::ComparedSide::New,
            name.as_bytes().to_vec(),
            text.as_bytes().to_vec(),
        );
    }
    let fg_at = |view: &preview::Preview, token: &str| {
        let line = view
            .lines
            .iter()
            .find(|line| line.to_string().contains(token))
            .unwrap_or_else(|| panic!("{token}"));
        let start = line.to_string().find(token).unwrap();
        let mut offset = 0;
        line.spans
            .iter()
            .find_map(|span| {
                let contains = offset <= start && start < offset + span.content.len();
                offset += span.content.len();
                contains.then_some(span.style.fg)
            })
            .unwrap()
    };
    let expected: [&[(&str, Color)]; 3] = [
        &[
            ("// note", comment),
            ("fn", keyword),
            ("one", function),
            ("\"text\"", string),
            ("1;", number),
        ],
        &[
            ("service", key),
            ("café", string),
            ("42", number),
            ("true", number),
            ("# review", comment),
        ],
        &[
            ("title", key),
            ("café", string),
            ("42", number),
            ("true", number),
            ("# review", comment),
        ],
    ];
    for (source, tokens) in catalog.sources.iter().zip(expected) {
        let view = preview::Preview::source(source, &theme);
        assert!(view.title.as_deref().unwrap().starts_with("Captured"));
        for (token, color) in tokens {
            assert_eq!(fg_at(&view, token), Some(*color), "{token}");
        }
        assert!(
            view.lines
                .iter()
                .flat_map(|line| &line.spans)
                .all(|span| span.style.bg.is_none())
        );
    }
}

mod review_comments {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};

    fn ctrl(app: &mut App, character: char) {
        app.key_event(KeyEvent::new(
            KeyCode::Char(character),
            KeyModifiers::CONTROL,
        ));
    }

    fn type_text(app: &mut App, text: &str) {
        for character in text.chars() {
            if character == '\n' {
                ctrl(app, 'j');
            } else {
                app.key(KeyCode::Char(character));
            }
        }
    }

    /// Open the first item's diff and put the cursor on display `row`.
    fn open_at(app: &mut App, row: usize) {
        app.key(KeyCode::Enter);
        assert_eq!(app.focus, Focus::Preview);
        for _ in 0..row {
            app.key(KeyCode::Char('j'));
        }
        assert_eq!(app.cursor.0, row);
    }

    fn comment_at(app: &mut App, row: usize, text: &str) {
        open_at(app, row);
        app.key(KeyCode::Char('c'));
        assert!(app.editor.is_some(), "{}", app.status);
        type_text(app, text);
        ctrl(app, 's');
        assert!(app.editor.is_none());
    }

    fn file(directory: &Path, name: &str) -> String {
        std::fs::read_to_string(directory.join(name)).unwrap()
    }

    fn icon_rows(app: &mut App) -> Vec<(usize, Color)> {
        let buffer = screen(app, 120, 30);
        let mut rows = Vec::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let cell = &buffer[(x, y)];
                if cell.symbol() == "💬" {
                    rows.push((y as usize, cell.fg));
                }
            }
        }
        rows
    }

    // one.rs patch rows: 0 diff --git, 1 index, 2 ---, 3 +++, 4 @@,
    // 5 context 1, 6 removed old 2, 7 added new 2, 8 context 3.

    #[test]
    fn editor_enter_saves_and_modified_enter_or_ctrl_j_inserts_newlines() {
        let directory = fixture();
        change(directory.path());
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        open_at(&mut app, 7);
        app.key(KeyCode::Char('c'));
        type_text(&mut app, "a");
        for modifiers in [
            KeyModifiers::CONTROL,
            KeyModifiers::SHIFT,
            KeyModifiers::ALT,
        ] {
            app.key_event(KeyEvent::new(KeyCode::Enter, modifiers));
            type_text(&mut app, "b");
        }
        ctrl(&mut app, 'j');
        type_text(&mut app, "c");
        assert_eq!(app.editor.as_ref().unwrap().text(), "a\nb\nb\nb\nc");
        app.key(KeyCode::Enter);
        assert!(app.editor.is_none());
        assert!(app.status.contains("Comment saved"), "{}", app.status);
        assert!(file(directory.path(), "review-HEAD.comments").contains("a\nb\nb\nb\nc\n"));
        // Escape cancels without changing the saved comment.
        app.key(KeyCode::Char('c'));
        type_text(&mut app, "x");
        app.key(KeyCode::Esc);
        assert!(app.editor.is_none() && app.preview.is_some());
        assert_eq!(app.comments.comments[0].text, "a\nb\nb\nb\nc");
        // Enter on emptied text deletes the existing comment.
        app.key(KeyCode::Char('c'));
        for _ in 0..9 {
            app.key(KeyCode::Backspace);
        }
        app.key(KeyCode::Enter);
        assert!(app.comments.comments.is_empty());
        assert!(!directory.path().join("review-HEAD.comments").exists());
    }

    #[test]
    fn owned_file_after_a_candidate_gap_is_loaded_and_reused() {
        let directory = fixture();
        change(directory.path());
        std::fs::write(directory.path().join("review-HEAD.comments"), "foreign\n").unwrap();
        std::fs::write(
            directory.path().join("review-HEAD-2.comments"),
            "foreign 2\n",
        )
        .unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        comment_at(&mut app, 7, "ours");
        assert!(directory.path().join("review-HEAD-3.comments").exists());
        std::fs::remove_file(directory.path().join("review-HEAD-2.comments")).unwrap();
        let mut again = application(directory.path(), state.path());
        assert_eq!(again.comments.comments.len(), 1);
        comment_at(&mut again, 0, "file");
        assert!(!directory.path().join("review-HEAD-2.comments").exists());
        assert!(file(directory.path(), "review-HEAD-3.comments").contains("comments: 2\n"));
        assert_eq!(file(directory.path(), "review-HEAD.comments"), "foreign\n");
    }

    #[test]
    fn tracked_owned_file_is_not_removed_and_mouse_waits_for_the_editor() {
        let directory = fixture();
        change(directory.path());
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        comment_at(&mut app, 0, "file");
        // `dc` on the @@ row deletes the file comment, like `c` edits it.
        for _ in 0..4 {
            app.key(KeyCode::Char('j'));
        }
        app.key(KeyCode::Char('d'));
        app.key(KeyCode::Char('c'));
        assert!(app.comments.comments.is_empty(), "{}", app.status);
        assert!(!directory.path().join("review-HEAD.comments").exists());
        comment_at_current(&mut app, 7, "line");
        git_run(directory.path(), &["add", "review-HEAD.comments"]);
        let before = file(directory.path(), "review-HEAD.comments");
        app.key(KeyCode::Char('d'));
        app.key(KeyCode::Char('c'));
        assert!(app.status.contains("tracked by Git"), "{}", app.status);
        assert_eq!(file(directory.path(), "review-HEAD.comments"), before);
        // Mouse events are ignored while the editor is open.
        let _ = screen(&mut app, 120, 30);
        app.key(KeyCode::Char('c'));
        let cursor = app.cursor;
        for kind in [
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            crossterm::event::MouseEventKind::ScrollDown,
        ] {
            app.mouse(mouse_event(kind, 50, 3), std::time::Instant::now());
            app.mouse(mouse_event(kind, 2, 2), std::time::Instant::now());
        }
        assert_eq!(app.cursor, cursor);
        assert_eq!((app.selected, app.top), (0, 0));
        assert!(app.editor.is_some());
    }

    #[test]
    fn remote_credentials_are_stripped() {
        assert_eq!(
            git::strip_userinfo("https://user:token@example.com/org/repo.git"),
            "https://example.com/org/repo.git"
        );
        assert_eq!(
            git::strip_userinfo("ssh://git@example.com:22/repo"),
            "ssh://example.com:22/repo"
        );
        assert_eq!(
            git::strip_userinfo("https://a@b@example.com/x@y"),
            "https://example.com/x@y"
        );
        assert_eq!(
            git::strip_userinfo("git@example.com:org/repo.git"),
            "git@example.com:org/repo.git"
        );
        assert_eq!(
            git::strip_userinfo("https://example.com/r"),
            "https://example.com/r"
        );
    }

    #[test]
    fn summaries_and_candidate_filenames() {
        let summary =
            |args: &[&str]| comments::summary(&args.iter().map(OsString::from).collect::<Vec<_>>());
        assert_eq!(summary(&["main..feat"]), "main..feat");
        assert_eq!(summary(&["main", "feat"]), "main-feat");
        assert_eq!(summary(&["main", "--", "a", "b/c"]), "main-a-b-c");
        assert_eq!(summary(&[]), "worktree");
        assert_eq!(summary(&["--", "//"]), "worktree");
        assert_eq!(summary(&["--cached"]), "cached");
        assert_eq!(summary(&["ä b"]), "b");
        let long = "x".repeat(300);
        assert_eq!(summary(&[long.as_str()]).len(), 100);
        assert_eq!(comments::filename("HEAD", 1), "review-HEAD.comments");
        assert_eq!(comments::filename("HEAD", 2), "review-HEAD-2.comments");
        assert_eq!(comments::shell_quote("main..feat"), "main..feat");
        assert_eq!(comments::shell_quote("a b"), "'a b'");
        assert_eq!(comments::shell_quote("it's"), r"'it'\''s'");
        assert_eq!(comments::shell_quote(""), "''");
        assert_eq!(comments::utc_for_test(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            comments::utc_for_test(1_792_670_400),
            "2026-10-22T12:00:00Z"
        );
    }

    #[test]
    fn line_comment_is_saved_shown_and_marks_stay_unchanged() {
        let directory = fixture();
        change(directory.path());
        let state = tempfile::tempdir().unwrap();
        let state_before = snapshot(state.path());
        let mut app = application(directory.path(), state.path());
        let marks = app.marks.clone();
        comment_at(&mut app, 7, "Rename `answer`\nsecond line");
        assert!(
            app.status.contains("review-HEAD.comments"),
            "{}",
            app.status
        );
        let text = file(directory.path(), "review-HEAD.comments");
        assert!(text.starts_with("---\nreview-comments: v1\n"), "{text}");
        assert!(text.contains("args: [\"HEAD\"]\n"), "{text}");
        assert!(text.contains("command: \"git diff HEAD\"\n"), "{text}");
        assert!(
            text.contains("label: \"working tree, uncommitted\""),
            "{text}"
        );
        let head = String::from_utf8(git_run(directory.path(), &["rev-parse", "HEAD"])).unwrap();
        assert!(
            text.contains(&format!("commit: \"{}\"", head.trim())),
            "{text}"
        );
        assert!(text.contains("comments: 1\noutdated: 0\n---\n"), "{text}");
        assert!(text.contains(comments::INSTRUCTION));
        assert!(text.contains("\n## one.rs:2 (new)\n"), "{text}");
        assert!(text.contains("  2 +\tlet answer = 100;\n"), "{text}");
        assert!(text.contains("Rename `answer`\nsecond line\n"), "{text}");
        // Icon on the commented display row only (preview rows start at y=1).
        let icons = icon_rows(&mut app);
        assert_eq!(icons.len(), 1, "{icons:?}");
        assert_eq!(icons[0].0, 1 + 7);
        assert_eq!(icons[0].1, app.theme.comment);
        assert_eq!(app.marks, marks);
        assert_eq!(snapshot(state.path()), state_before);
        // Restart with the same arguments restores the comment and marks.
        let mut restarted = application(directory.path(), state.path());
        assert_eq!(restarted.marks, marks);
        assert_eq!(restarted.comments.comments, app.comments.comments);
        assert!(restarted.status.contains("1 comment(s) loaded"));
        restarted.key(KeyCode::Enter);
        assert_eq!(icon_rows(&mut restarted).len(), 1);
        // Editing prefills the stored text and rewrites the same file.
        for _ in 0..7 {
            restarted.key(KeyCode::Char('j'));
        }
        restarted.key(KeyCode::Char('c'));
        let editor = restarted.editor.as_ref().unwrap();
        assert_eq!(editor.text(), "Rename `answer`\nsecond line");
        assert_eq!(editor.title, "one.rs:2 (new)");
        type_text(&mut restarted, "!");
        ctrl(&mut restarted, 's');
        assert!(file(directory.path(), "review-HEAD.comments").contains("second line!\n"));
        assert!(!directory.path().join("review-HEAD-2.comments").exists());
    }

    #[test]
    fn mixed_range_header_and_after_changes_anchors() {
        let directory = fixture();
        change(directory.path());
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        open_at(&mut app, 6);
        app.key(KeyCode::Char('v'));
        app.key(KeyCode::Char('j'));
        app.key(KeyCode::Char('j'));
        assert_eq!(app.selected_rows(), Some((6, 8)));
        app.key(KeyCode::Char('c'));
        assert_eq!(
            app.editor.as_ref().unwrap().title,
            "one.rs:old 2, new 2-3 (mixed)"
        );
        type_text(&mut app, "mixed");
        ctrl(&mut app, 's');
        assert!(app.selection.is_none());
        let text = file(directory.path(), "review-HEAD.comments");
        assert!(
            text.contains("## one.rs:old 2, new 2-3 (mixed)\n"),
            "{text}"
        );
        // Header row creates the file comment.
        app.key(KeyCode::Char('g'));
        app.key(KeyCode::Char('g'));
        app.key(KeyCode::Char('c'));
        type_text(&mut app, "file level");
        ctrl(&mut app, 's');
        let text = file(directory.path(), "review-HEAD.comments");
        assert!(text.contains("## one.rs (file)\n"), "{text}");
        assert!(
            text.find("## one.rs (file)").unwrap() < text.find("## one.rs:old").unwrap(),
            "file comments come first: {text}"
        );
        assert!(text.contains("comments: 2\n"));
        // Every header row and every anchored source row shows the icon.
        let icons = icon_rows(&mut app);
        assert_eq!(
            icons.iter().map(|(row, _)| row - 1).collect::<Vec<_>>(),
            [0, 1, 2, 3, 6, 7, 8]
        );
        // The @@ row of a file that already has a file comment edits it.
        for _ in 0..4 {
            app.key(KeyCode::Char('j'));
        }
        app.key(KeyCode::Char('c'));
        assert_eq!(app.editor.as_ref().unwrap().text(), "file level");
        app.key(KeyCode::Esc);
        assert!(app.editor.is_none());
        assert!(
            app.preview.is_some(),
            "Escape in the editor closed the diff"
        );
        // After-changes view: the context row anchors through locations.
        app.key(KeyCode::Char('a'));
        let second = &app.capture.items[1];
        assert!(second.name() == "two.rs");
        app.go_down_files_and_back();
        app.key(KeyCode::Enter);
        app.key(KeyCode::Char('a'));
        // two.rs after-changes rows: 0..=4 header/@@, 5 context, 6 added, 7 context.
        for _ in 0..6 {
            app.key(KeyCode::Char('j'));
        }
        app.key(KeyCode::Char('c'));
        assert_eq!(app.editor.as_ref().unwrap().title, "two.rs:2 (new)");
        type_text(&mut app, "after");
        ctrl(&mut app, 's');
        assert!(file(directory.path(), "review-HEAD.comments").contains("## two.rs:2 (new)\n"));
    }

    impl App {
        fn go_down_files_and_back(&mut self) {
            self.key(KeyCode::Esc);
            self.key(KeyCode::Char('j'));
        }
    }

    fn two_hunks(directory: &Path) {
        let lines: Vec<String> = (1..=20).map(|n| format!("line {n}")).collect();
        std::fs::write(directory.join("long.txt"), lines.join("\n") + "\n").unwrap();
        git_run(directory, &["add", "long.txt"]);
        git_run(directory, &["commit", "-qm", "long"]);
        let mut changed = lines.clone();
        changed[1] = "line two".into();
        changed[18] = "line nineteen".into();
        std::fs::write(directory.join("long.txt"), changed.join("\n") + "\n").unwrap();
    }

    #[test]
    fn cross_hunk_header_mix_and_trailing_hunk_header_are_refused() {
        let directory = fixture();
        two_hunks(directory.path());
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        let patch = String::from_utf8(app.capture.items[0].patch.clone()).unwrap();
        let rows = comments::rows(&patch);
        let second = rows
            .iter()
            .position(|row| row.kind == comments::RowKind::Hunk(1))
            .unwrap();
        // Range across hunks.
        open_at(&mut app, 6);
        app.key(KeyCode::Char('v'));
        for _ in 6..second + 2 {
            app.key(KeyCode::Char('j'));
        }
        app.key(KeyCode::Char('c'));
        assert!(app.editor.is_none());
        assert!(app.status.contains("within one hunk"), "{}", app.status);
        // Range ending on the second @@.
        app.key(KeyCode::Char('g'));
        app.key(KeyCode::Char('g'));
        for _ in 0..second - 1 {
            app.key(KeyCode::Char('j'));
        }
        app.key(KeyCode::Char('v'));
        app.key(KeyCode::Char('j'));
        app.key(KeyCode::Char('c'));
        assert!(app.editor.is_none());
        assert!(app.status.contains("hunk header"), "{}", app.status);
        // Range from the file header into a hunk.
        app.key(KeyCode::Char('g'));
        app.key(KeyCode::Char('g'));
        app.key(KeyCode::Char('v'));
        for _ in 0..6 {
            app.key(KeyCode::Char('j'));
        }
        app.key(KeyCode::Char('c'));
        assert!(app.editor.is_none());
        assert!(app.status.contains("within one hunk"), "{}", app.status);
        // Header-only range makes a file comment.
        app.key(KeyCode::Char('g'));
        app.key(KeyCode::Char('g'));
        app.key(KeyCode::Char('v'));
        app.key(KeyCode::Char('j'));
        app.key(KeyCode::Char('c'));
        assert_eq!(app.editor.as_ref().unwrap().title, "long.txt (file)");
        ctrl(&mut app, 'g');
        assert!(app.editor.is_none());
        // Escape cancels a selection without closing the diff.
        app.key(KeyCode::Char('v'));
        assert!(app.selection.is_some());
        app.key(KeyCode::Esc);
        assert!(app.selection.is_none() && app.preview.is_some());
        assert!(!directory.path().join("review-HEAD.comments").exists());
        // A new range overlapping an existing comment is refused.
        comment_at_current(&mut app, 6, "first");
        app.key(KeyCode::Char('k'));
        app.key(KeyCode::Char('v'));
        app.key(KeyCode::Char('j'));
        app.key(KeyCode::Char('c'));
        assert!(app.editor.is_none());
        assert!(app.status.contains("overlaps"), "{}", app.status);
        assert_eq!(app.comments.comments.len(), 1);
    }

    fn comment_at_current(app: &mut App, row: usize, text: &str) {
        app.key(KeyCode::Char('g'));
        app.key(KeyCode::Char('g'));
        for _ in 0..row {
            app.key(KeyCode::Char('j'));
        }
        app.key(KeyCode::Char('c'));
        type_text(app, text);
        ctrl(app, 's');
    }

    #[test]
    fn editor_cancel_empty_save_and_delete_last_remove_the_file() {
        let directory = fixture();
        change(directory.path());
        let state = tempfile::tempdir().unwrap();
        let before = snapshot(directory.path());
        let mut app = application(directory.path(), state.path());
        open_at(&mut app, 7);
        app.key(KeyCode::Char('c'));
        type_text(&mut app, "draft");
        app.key(KeyCode::Esc);
        assert!(app.comments.comments.is_empty());
        app.key(KeyCode::Char('c'));
        type_text(&mut app, "  \n");
        ctrl(&mut app, 's');
        assert!(app.status.contains("Empty comment discarded"));
        assert_eq!(snapshot(directory.path()), before);
        // Editor keys: arrows, backspace across lines.
        app.key(KeyCode::Char('c'));
        type_text(&mut app, "ab\ncd");
        app.key(KeyCode::Left);
        app.key(KeyCode::Left);
        app.key(KeyCode::Backspace);
        app.key(KeyCode::Up);
        app.key(KeyCode::Right);
        type_text(&mut app, "X");
        assert_eq!(app.editor.as_ref().unwrap().text(), "abcXd");
        ctrl(&mut app, 's');
        assert!(directory.path().join("review-HEAD.comments").exists());
        app.key(KeyCode::Char('d'));
        app.key(KeyCode::Char('c'));
        assert!(
            app.status.contains("comments file removed"),
            "{}",
            app.status
        );
        assert_eq!(snapshot(directory.path()), before);
        // dc on an uncommented line explains; an empty save deletes.
        app.key(KeyCode::Char('d'));
        app.key(KeyCode::Char('c'));
        assert_eq!(app.status, "No comment on this line");
        app.key(KeyCode::Char('c'));
        type_text(&mut app, "x");
        ctrl(&mut app, 's');
        app.key(KeyCode::Char('c'));
        app.key(KeyCode::Backspace);
        ctrl(&mut app, 's');
        assert!(app.comments.comments.is_empty());
        assert_eq!(snapshot(directory.path()), before);
    }

    #[test]
    fn foreign_and_tracked_files_are_never_touched() {
        let directory = fixture();
        std::fs::write(
            directory.path().join("review-HEAD-2.comments"),
            "---\nreview-comments: v1\nargs: [\"HEAD\"]\n---\n",
        )
        .unwrap();
        git_run(directory.path(), &["add", "review-HEAD-2.comments"]);
        git_run(directory.path(), &["commit", "-qm", "tracked comments"]);
        change(directory.path());
        std::fs::write(directory.path().join("review-HEAD.comments"), "foreign\n").unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        assert!(app.comments.comments.is_empty());
        comment_at(&mut app, 7, "ours");
        assert_eq!(file(directory.path(), "review-HEAD.comments"), "foreign\n");
        assert_eq!(
            file(directory.path(), "review-HEAD-2.comments"),
            "---\nreview-comments: v1\nargs: [\"HEAD\"]\n---\n"
        );
        assert!(file(directory.path(), "review-HEAD-3.comments").contains("ours"));
        // A different argument list never reuses this file.
        let other = App::new(
            capture(directory.path(), &["HEAD", "--", "one.rs"]),
            state::Store::at(state.path(), directory.path(), &["HEAD".into()]),
        );
        assert!(other.comments.comments.is_empty());
        // Same arguments reuse the owned file.
        let again = application(directory.path(), state.path());
        assert_eq!(again.comments.comments.len(), 1);
        assert_eq!(
            again.comments.path.as_deref(),
            Some(directory.path().join("review-HEAD-3.comments").as_path())
        );
    }

    #[test]
    fn changed_code_marks_comments_outdated_without_relocation() {
        let directory = fixture();
        change(directory.path());
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        comment_at(&mut app, 7, "on 100");
        app.key(KeyCode::Char('g'));
        app.key(KeyCode::Char('g'));
        app.key(KeyCode::Char('c'));
        type_text(&mut app, "file");
        ctrl(&mut app, 's');
        // A comment for a file that disappears from the comparison.
        app.key(KeyCode::Esc);
        app.key(KeyCode::Char('j'));
        comment_at(&mut app, 7, "two");
        std::fs::write(
            directory.path().join("one.rs"),
            "fn one() {\n\tlet answer = 101;\n}\n",
        )
        .unwrap();
        std::fs::write(
            directory.path().join("two.rs"),
            "fn two() {\n\tlet answer = 2;\n}\n",
        )
        .unwrap();
        let mut restarted = application(directory.path(), state.path());
        assert_eq!(restarted.capture.items.len(), 1);
        assert_eq!(restarted.comments.comments.len(), 3);
        assert!(
            restarted.status.contains("3 comment(s) loaded, 2 outdated"),
            "{}",
            restarted.status
        );
        let text = file(directory.path(), "review-HEAD.comments");
        assert!(text.contains("## one.rs:2 (new) [outdated]\n"), "{text}");
        assert!(text.contains("## one.rs (file)\n"), "{text}");
        assert!(text.contains("## two.rs:2 (new) [outdated]\n"), "{text}");
        assert!(text.contains("outdated: 2\n"), "{text}");
        // Absent files follow present ones; the reviewed excerpt is kept.
        assert!(text.find("## two.rs").unwrap() > text.find("## one.rs:2").unwrap());
        assert!(text.contains("\tlet answer = 100;"), "{text}");
        restarted.key(KeyCode::Enter);
        let icons = icon_rows(&mut restarted);
        let line = icons.iter().find(|(row, _)| *row == 1 + 7).unwrap();
        assert_eq!(line.1, restarted.theme.comment_outdated);
        assert!(
            icons
                .iter()
                .any(|(row, color)| *row == 1 && *color == restarted.theme.comment)
        );
    }

    #[test]
    fn no_index_comparison_records_unavailable_repository_fields() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(directory.path().join("b.rs"), "fn b() {}\n").unwrap();
        let args: Vec<OsString> = ["--no-index", "a.rs", "b.rs"].map(OsString::from).to_vec();
        let state = tempfile::tempdir().unwrap();
        let mut app = App::new(
            git::capture(directory.path(), &args).unwrap(),
            state::Store::at(state.path(), directory.path(), &args),
        );
        let patch = String::from_utf8(app.capture.items[0].patch.clone()).unwrap();
        let added = comments::rows(&patch)
            .iter()
            .position(|row| row.text.starts_with('+') && !row.text.starts_with("+++"))
            .unwrap();
        comment_at(&mut app, added, "no index");
        let text = file(directory.path(), "review-no-index-a.rs-b.rs.comments");
        assert!(
            text.contains("repository: unavailable\nremote: unavailable\n"),
            "{text}"
        );
        assert!(
            text.contains("label: \"old file a.rs\"\n  commit: unavailable\n"),
            "{text}"
        );
        assert!(text.contains("label: \"new file b.rs\""), "{text}");
        assert!(
            text.contains("command: \"git diff --no-index a.rs b.rs\""),
            "{text}"
        );
    }

    #[test]
    fn merge_base_ranges_record_full_shas_and_branches() {
        let directory = fixture();
        git_run(directory.path(), &["branch", "base"]);
        change(directory.path());
        git_run(directory.path(), &["commit", "-qam", "feature"]);
        git_run(
            directory.path(),
            &["remote", "add", "upstream", "https://example.invalid/u.git"],
        );
        let args: Vec<OsString> = ["base...HEAD"].map(OsString::from).to_vec();
        let state = tempfile::tempdir().unwrap();
        let mut app = App::new(
            git::capture(directory.path(), &args).unwrap(),
            state::Store::at(state.path(), directory.path(), &args),
        );
        comment_at(&mut app, 7, "range");
        let text = file(directory.path(), "review-base...HEAD.comments");
        let sha = |rev: &str| {
            String::from_utf8(git_run(directory.path(), &["rev-parse", rev]))
                .unwrap()
                .trim()
                .to_owned()
        };
        assert!(
            text.contains(&format!(
                "label: \"base\"\n  ref: \"refs/heads/base\"\n  commit: \"{}\"",
                sha("base")
            )),
            "{text}"
        );
        assert!(
            text.contains(&format!("commit: \"{}\"", sha("HEAD"))),
            "{text}"
        );
        assert!(
            text.contains(&format!("merge-base: \"{}\"", sha("base"))),
            "{text}"
        );
        assert!(
            text.contains("remote: \"https://example.invalid/u.git\""),
            "{text}"
        );
        let root = std::fs::canonicalize(directory.path()).unwrap();
        assert!(
            text.contains(&format!("repository: \"{}\"", root.display())),
            "{text}"
        );
    }

    #[test]
    fn unwritable_launch_directory_keeps_the_comment_in_memory() {
        use std::os::unix::fs::PermissionsExt;
        let directory = fixture();
        change(directory.path());
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        let mode = std::fs::metadata(directory.path()).unwrap().permissions();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        comment_at(&mut app, 7, "kept");
        std::fs::set_permissions(directory.path(), mode).unwrap();
        assert_eq!(app.comments.comments.len(), 1);
        assert!(app.status.contains("not written"), "{}", app.status);
        assert!(!directory.path().join("review-HEAD.comments").exists());
        assert_eq!(icon_rows(&mut app).len(), 1);
    }

    #[test]
    fn backticks_and_multiline_text_round_trip_as_valid_markdown() {
        let comment = comments::Comment {
            old_path: "a.md".into(),
            new_path: "b.md".into(),
            old: None,
            new: Some((1, 2)),
            anchors: vec![comments::Anchor {
                old: None,
                new: Some(1),
                text: "+```` --> <!-- x".into(),
            }],
            excerpt: vec!["  1 +```` --> <!-- x".into(), "".into()],
            text: "Use ```rust\nfences ``` here\n\n## not a heading\n".into(),
            outdated: true,
        };
        let comments = vec![comment];
        let text = comments::serialize(
            &comments,
            &[OsString::from("a b"), OsString::from("it's")],
            &git::Provenance::default(),
            std::time::UNIX_EPOCH,
        );
        assert_eq!(comments::parse(&text).unwrap(), comments);
        assert!(
            text.contains("\n`````\n  1 +```` --> <!-- x\n\n`````\n"),
            "{text}"
        );
        assert!(text.contains("\n````text\nUse ```rust\n"), "{text}");
        assert!(
            text.contains("## a.md → b.md:1-2 (new) [outdated]\n"),
            "{text}"
        );
        assert!(
            text.contains("command: \"git diff 'a b' 'it'\\\\''s'\"\n"),
            "{text}"
        );
        assert!(comments::owned(
            &text,
            &[OsString::from("a b"), OsString::from("it's")]
        ));
        assert!(!comments::owned(&text, &[OsString::from("a b")]));
        // The metadata comment cannot be closed early by content.
        let marker = text
            .lines()
            .find(|l| l.starts_with("<!-- review-comment "))
            .unwrap();
        assert_eq!(marker.matches("-->").count(), 1);
    }

    #[test]
    fn comments_need_a_structured_diff() {
        let directory = fixture();
        change(directory.path());
        let state = tempfile::tempdir().unwrap();
        let mut app = App::new(
            capture(directory.path(), &["--stat", "HEAD"]),
            state::Store::at(
                state.path(),
                directory.path(),
                &["--stat".into(), "HEAD".into()],
            ),
        );
        app.key(KeyCode::Char('c'));
        assert!(app.editor.is_none());
        assert!(
            app.status.contains("Comments are unavailable"),
            "{}",
            app.status
        );
        let mut app = application(directory.path(), state.path());
        app.key(KeyCode::Char('c'));
        assert!(app.editor.is_none());
        assert!(app.status.contains("Open a file diff"), "{}", app.status);
        let emacs =
            keybindings::Bindings::new(keybindings::Keymap::Emacs, true, &Default::default())
                .unwrap();
        assert!(emacs.help().contains("select_range: ctrl-space\n"));
        assert!(app.bindings.help().contains("select_range: v\n"));
        assert!(app.bindings.help().contains("delete_comment: dc\n"));
        assert!(app.bindings.legend().contains("c comment"));
    }
}

fn row_text(buffer: &ratatui::buffer::Buffer, row: u16) -> String {
    (0..buffer.area.width)
        .map(|x| buffer[(x, row)].symbol().to_string())
        .collect()
}

#[test]
fn file_list_totals_sum_text_statistics_in_the_bottom_row() {
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.set_theme(theme::Theme::classic());
    let rendered = screen(&mut app, 100, 24);
    // Rows: 0 top border, 1..=20 inner, 21 bottom border, 22-23 status.
    let totals = row_text(&rendered, 20);
    assert!(totals.starts_with("│Σ +2 −2 "), "{totals}");
    assert_eq!(app.file_area.height, 19);
    assert_eq!(rendered[(3, 20)].fg, Color::Green);
    assert_eq!(rendered[(6, 20)].fg, Color::Red);
    assert_eq!(rendered[(1, 20)].fg, app.theme.metrics);
    // Tiny panes clip the totals line and never panic.
    let rendered = screen(&mut app, 6, 8);
    assert!(row_text(&rendered, 4).starts_with("│Σ +2"));
    for width in 0..12 {
        for height in 0..6 {
            screen(&mut app, width, height);
        }
    }
}

#[test]
fn file_list_totals_count_binary_and_unavailable_items() {
    let directory = fixture();
    let root = directory.path();
    std::fs::write(root.join("binary.dat"), b"old\0binary").unwrap();
    git_run(root, &["add", "."]);
    git_run(root, &["commit", "-qm", "binary"]);
    std::fs::write(root.join("binary.dat"), b"new\0binary").unwrap();
    std::fs::write(
        root.join("one.rs"),
        "fn one() {\n\tlet answer = 1;\n}\na\nb\nc\n",
    )
    .unwrap();
    let state = tempfile::tempdir().unwrap();
    let mut app = application(root, state.path());
    let text = screen_text(&screen(&mut app, 100, 24));
    assert!(text.contains("Σ +3 −0 · 1 binary "), "{text}");
    assert!(!text.contains("unavailable"), "{text}");
    // A whitespace-suppressed mode change has no provable statistics.
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(root.join("two.rs"), "fn two() {\n    let answer = 2;\n}\n").unwrap();
    std::fs::set_permissions(root.join("two.rs"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut app = App::new(
        capture(root, &["-w", "HEAD"]),
        state::Store::at(state.path(), root, &["-w".into(), "HEAD".into()]),
    );
    let text = screen_text(&screen(&mut app, 100, 24));
    assert!(
        text.contains("Σ +3 −0 · 1 binary · 1 unavailable"),
        "{text}"
    );
}

#[test]
fn open_diff_hides_statistics_and_totals_until_closed() {
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Char('m'));
    app.key(KeyCode::Down);
    app.key(KeyCode::Enter);
    assert!(app.preview.is_some());
    let rendered = screen(&mut app, 100, 24);
    let text = screen_text(&rendered);
    assert!(!text.contains('Σ'), "{text}");
    // Rows hold only the mark and the name; the list keeps the full height.
    let files: String = (1..29).map(|x| rendered[(x, 2)].symbol()).collect();
    assert!(files.trim_end().ends_with("- two.rs"), "{files}");
    assert_eq!(app.file_area.height, 19);
    app.key(KeyCode::Esc);
    assert!(app.preview.is_none());
    let text = screen_text(&screen(&mut app, 100, 24));
    assert!(text.contains("Σ +2 −2") && text.contains("2 +-"), "{text}");
    assert_eq!(app.selected, 1);
    assert_eq!(app.marks, [true, false]);
}

#[test]
fn file_list_height_is_constant_and_double_click_opens_the_bottom_row_with_a_diff_open() {
    use crossterm::event::{MouseButton, MouseEventKind};
    use std::time::{Duration, Instant};
    let directory = fixture();
    for index in 0..30 {
        std::fs::write(directory.path().join(format!("f{index:02}.txt")), "x\n").unwrap();
    }
    git_run(directory.path(), &["add", "-N", "."]);
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    assert_eq!(app.capture.items.len(), 30);
    let rendered = screen(&mut app, 100, 13);
    let rows = |app: &App| (app.file_area.y, app.file_area.height, app.file_offset);
    let closed = rows(&app);
    assert_eq!(app.file_area.height, 8);
    assert!(row_text(&rendered, app.file_area.y + 8).contains('Σ'));
    app.key(KeyCode::Enter);
    let rendered = screen(&mut app, 100, 13);
    assert!(app.preview.is_some());
    assert_eq!(rows(&app), closed);
    // The reserved row stays blank while the diff is open.
    assert!(!row_text(&rendered, app.file_area.y + 8).contains('Σ'));
    // Double-clicking the bottom visible file row selects and opens it.
    let bottom = app.file_area.y + app.file_area.height - 1;
    let click = mouse_event(
        MouseEventKind::Down(MouseButton::Left),
        app.file_area.x,
        bottom,
    );
    let now = Instant::now();
    app.mouse(click, now);
    assert_eq!(app.selected, 7);
    screen(&mut app, 100, 13);
    assert_eq!(rows(&app), closed);
    app.mouse(click, now + Duration::from_millis(100));
    assert_eq!(app.selected, 7);
    assert!(app.preview.is_some());
    assert_eq!(app.focus, Focus::Preview);
    screen(&mut app, 100, 13);
    assert_eq!(rows(&app), closed);
    app.key(KeyCode::Esc);
    screen(&mut app, 100, 13);
    assert_eq!(rows(&app), closed);
}

#[test]
fn empty_capture_shows_no_totals_line() {
    let directory = fixture();
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    let text = screen_text(&screen(&mut app, 60, 12));
    assert!(text.contains("Files — no changes"), "{text}");
    assert!(!text.contains('Σ'), "{text}");
}

#[test]
fn gruvbox_baby_is_the_default_with_orange_black_search_matches() {
    assert_eq!(theme::DEFAULT, "gruvbox-baby");
    let baby = theme::Theme::default();
    assert_eq!(baby.name, "gruvbox-baby");
    assert_eq!(baby.search_bg, Color::Rgb(0xd6, 0x5d, 0x0e));
    assert_eq!(baby.search_fg, Some(Color::Rgb(0, 0, 0)));
    let comment = theme::capture("comment").unwrap();
    let style = baby.style(theme::Role {
        capture: Some(comment),
        ..Default::default()
    });
    assert!(!style.add_modifier.contains(Modifier::ITALIC));
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    assert_eq!(app.theme.name, "gruvbox-baby");
    app.key(KeyCode::Enter);
    screen(&mut app, 100, 24);
    let preview = app.preview.as_ref().unwrap();
    let row = preview
        .lines
        .iter()
        .position(|line| line.to_string().contains("answer"))
        .unwrap();
    let column = preview.lines[row].to_string().find("answer").unwrap();
    app.cursor = (row, column);
    app.key(KeyCode::Char('*'));
    assert!(app.preview.as_ref().unwrap().search.is_some());
    // Styles of every displayed cell inside a search match.
    let matched = |app: &App| -> Vec<ratatui::style::Style> {
        let preview = app.preview.as_ref().unwrap();
        let lines = preview.visible(0, 0, preview.lines.len(), 200);
        let mut styles = Vec::new();
        for m in &preview.search.as_ref().unwrap().matches {
            let mut column = 0;
            for span in &lines[m.row].spans {
                let end = column + span.width();
                if column < m.end && end > m.start {
                    styles.push(span.style);
                }
                column = end;
            }
        }
        styles
    };
    let styles = matched(&app);
    assert!(!styles.is_empty());
    for style in styles {
        assert_eq!(style.bg, Some(Color::Rgb(0xd6, 0x5d, 0x0e)));
        assert_eq!(style.fg, Some(Color::Rgb(0, 0, 0)));
    }
    // Other themes keep their own match foreground.
    run_command(&mut app, "colorscheme gruvbox-dark");
    let dark = theme::Theme::named("gruvbox-dark").unwrap();
    assert_eq!(dark.search_fg, None);
    assert!(theme::Theme::classic().search_fg.is_none());
    let styles = matched(&app);
    assert!(!styles.is_empty());
    assert!(styles.iter().any(|style| style.bg == Some(dark.search_bg)));
    assert!(
        styles
            .iter()
            .all(|style| style.fg != Some(Color::Rgb(0, 0, 0)))
    );
}

fn fallback_fg(view: &preview::Preview, token: &str) -> Option<Color> {
    let line = view
        .lines
        .iter()
        .find(|line| line.to_string().contains(token))
        .unwrap_or_else(|| panic!("{token}"));
    let start = line.to_string().find(token).unwrap();
    let mut offset = 0;
    line.spans
        .iter()
        .find_map(|span| {
            let contains = offset <= start && start < offset + span.content.len();
            offset += span.content.len();
            contains.then_some(span.style.fg)
        })
        .unwrap()
}

fn fallback_source(name: &str, text: &str, theme: &theme::Theme) -> preview::Preview {
    let mut catalog = source::Catalog::default();
    catalog.insert(
        source::ComparedSide::New,
        name.as_bytes().to_vec(),
        text.as_bytes().to_vec(),
    );
    preview::Preview::source(&catalog.sources[0], theme)
}

#[test]
fn syntect_scope_mapping_prefers_innermost_scope_and_longest_prefix() {
    use two_face::re_exports::syntect::parsing::Scope;
    let scopes = |names: &[&str]| -> Vec<Scope> {
        names.iter().map(|name| Scope::new(name).unwrap()).collect()
    };
    let capture = |name| theme::capture(name);
    for (stack, expected) in [
        (
            &["source.js", "comment.line.double-slash.js"][..],
            capture("comment"),
        ),
        (&["source.js", "storage.type.js"], capture("type")),
        (
            &["source.js", "storage.type.function.js"],
            capture("keyword"),
        ),
        (&["source.js", "storage.type.class.js"], capture("keyword")),
        (&["source.js", "storage.modifier.js"], capture("keyword")),
        (&["source.js", "storage.js"], capture("keyword")),
        (&["source.js", "keyword.operator.js"], capture("operator")),
        (&["source.js", "keyword.control.js"], capture("keyword")),
        (
            &["source.js", "constant.language.boolean.true.js"],
            capture("boolean"),
        ),
        (
            &["source.js", "constant.language.null.js"],
            capture("constant"),
        ),
        (
            &["source.js", "constant.numeric.integer.js"],
            capture("number"),
        ),
        (
            &["source.js", "variable.other.property.js"],
            capture("property"),
        ),
        (&["source.js", "variable.function.js"], capture("function")),
        (
            &["source.js", "variable.other.readwrite.js"],
            capture("variable"),
        ),
        (
            &["text.html.markdown", "markup.heading.1.markdown"],
            capture("keyword"),
        ),
        (
            &["text.html.markdown", "markup.underline.link.markdown"],
            capture("string.special"),
        ),
        (
            &["text.html.markdown", "markup.raw.inline.markdown"],
            capture("string"),
        ),
        // Innermost matching scope wins over an outer, longer prefix match.
        (
            &[
                "source.js",
                "constant.numeric.integer.js",
                "punctuation.separator.js",
            ],
            capture("punctuation"),
        ),
        // Within one scope the longest prefix wins: string delimiters keep the
        // string color, other punctuation inside a string does not.
        (
            &[
                "source.js",
                "string.quoted.double.js",
                "punctuation.definition.string.begin.js",
            ],
            capture("string"),
        ),
        (
            &[
                "source.js",
                "string.quoted.double.js",
                "punctuation.separator.js",
            ],
            capture("punctuation"),
        ),
        // Unmatched inner scopes fall through to the next matching outer scope.
        (
            &["source.js", "comment.block.js", "meta.unknown.js"],
            capture("comment"),
        ),
        (&["source.js", "meta.block.js"], None),
        (&[], None),
    ] {
        assert_eq!(syntect::capture(&scopes(stack)), expected, "{stack:?}");
    }
}

#[test]
fn syntect_fallback_colors_js_diff_source_view_and_theme_switch() {
    let directory = fixture();
    let old = "// note\nfunction hello() { return \"hi\"; }\n";
    let new = "// note\nfunction hello() { return \"hi\"; } // added\nconst ok = true;\n";
    std::fs::write(directory.path().join("app.js"), old).unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "js"]);
    std::fs::write(directory.path().join("app.js"), new).unwrap();
    let captured = capture(directory.path(), &["HEAD"]);
    assert_eq!(captured.items.len(), 1);
    let classic = theme::Theme::classic();
    let diff = preview::Preview::item(directory.path(), &captured.items[0], &classic);
    assert_eq!(diff.reason, None);
    for (token, color) in [
        ("function", Color::Magenta),
        ("\"hi\"", Color::Green),
        ("// note", Color::DarkGray),
        ("// added", Color::DarkGray),
        ("hello", Color::Blue),
        ("true", Color::Magenta),
    ] {
        assert_eq!(fallback_fg(&diff, token), Some(color), "{token}");
    }
    let added = diff
        .lines
        .iter()
        .find(|line| line.to_string().contains("// added"))
        .unwrap();
    assert!(added.spans.iter().all(|span| span.style.bg.is_some()));
    let source = fallback_source("app.js", new, &classic);
    for (token, color) in [
        ("function", Color::Magenta),
        ("\"hi\"", Color::Green),
        ("// note", Color::DarkGray),
    ] {
        assert_eq!(fallback_fg(&source, token), Some(color), "{token}");
    }
    for view in [&source, &diff] {
        for (row, cells) in view.cells.iter().enumerate() {
            assert_eq!(cells.len(), view.lines[row].width());
        }
    }

    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    assert_eq!(app.theme.name, theme::GRUVBOX_BABY);
    app.key(KeyCode::Enter);
    screen(&mut app, 100, 24);
    let view = app.preview.as_ref().unwrap();
    assert_eq!(
        fallback_fg(view, "function"),
        Some(Color::Rgb(0xfb, 0x49, 0x34))
    );
    assert_eq!(
        fallback_fg(view, "\"hi\""),
        Some(Color::Rgb(0x98, 0x97, 0x1a))
    );
    assert_eq!(
        fallback_fg(view, "// note"),
        Some(Color::Rgb(0x66, 0x5c, 0x54))
    );
    run_command(&mut app, "colorscheme review-classic");
    let view = app.preview.as_ref().unwrap();
    assert_eq!(fallback_fg(view, "function"), Some(Color::Magenta));
    assert_eq!(fallback_fg(view, "\"hi\""), Some(Color::Green));
    assert_eq!(fallback_fg(view, "// note"), Some(Color::DarkGray));
    assert_eq!(view.lines, diff.lines);
}

#[test]
fn syntect_fallback_detects_names_shebangs_and_rejects_plain_text() {
    let classic = theme::Theme::classic();
    let makefile = fallback_source("Makefile", "# build\nall:\n\tcc -o app app.c\n", &classic);
    assert_eq!(fallback_fg(&makefile, "# build"), Some(Color::DarkGray));
    let dockerfile = fallback_source("Dockerfile", "FROM alpine:3\nRUN true\n", &classic);
    assert_eq!(fallback_fg(&dockerfile, "FROM"), Some(Color::Magenta));
    let ruby = fallback_source(
        "tool",
        "#!/usr/bin/env ruby\ndef greet\n  puts \"hi\"\nend\n",
        &classic,
    );
    assert_eq!(fallback_fg(&ruby, "def"), Some(Color::Magenta));
    assert_eq!(fallback_fg(&ruby, "\"hi\""), Some(Color::Green));
    for name in ["notes.txt", "x.zzz", "tool"] {
        let view = fallback_source(name, "plain words \"here\" # not code\n", &classic);
        for token in ["plain", "\"here\"", "# not code"] {
            assert_eq!(
                fallback_fg(&view, token),
                classic.default_fg,
                "{name} {token}"
            );
        }
    }
}

#[test]
fn syntect_fallback_diff_reasons_for_plain_text_and_oversized_sources() {
    let directory = fixture();
    let big = format!("// big\n{}", "let value = 1;\n".repeat(20_000));
    assert!(big.len() > syntect::LIMIT && big.len() < 1024 * 1024);
    std::fs::write(directory.path().join("notes.txt"), "old\n").unwrap();
    std::fs::write(directory.path().join("x.zzz"), "old\n").unwrap();
    std::fs::write(directory.path().join("big.js"), &big).unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "plain"]);
    std::fs::write(directory.path().join("notes.txt"), "new\n").unwrap();
    std::fs::write(directory.path().join("x.zzz"), "new\n").unwrap();
    std::fs::write(
        directory.path().join("big.js"),
        big.replacen("big", "huge", 1),
    )
    .unwrap();
    let captured = capture(directory.path(), &["HEAD"]);
    for (name, reason) in [
        (
            "notes.txt",
            "syntax highlighting unavailable for this file type",
        ),
        (
            "x.zzz",
            "syntax highlighting unavailable for this file type",
        ),
        ("big.js", "source exceeds highlighting limit"),
    ] {
        let item = captured
            .items
            .iter()
            .find(|item| item.new_path == name.as_bytes())
            .unwrap();
        let diff = preview::Preview::item(directory.path(), item, &theme::Theme::classic());
        assert!(
            diff.reason
                .as_deref()
                .is_some_and(|text| text.contains(reason)),
            "{name}: {:?}",
            diff.reason
        );
    }
}

#[test]
fn syntect_fallback_colors_name_and_shebang_diffs_including_added_files() {
    let directory = fixture();
    std::fs::write(
        directory.path().join("Makefile"),
        "# build\nall:\n\tcc -o app app.c\n",
    )
    .unwrap();
    git_run(directory.path(), &["add", "."]);
    git_run(directory.path(), &["commit", "-qm", "makefile"]);
    std::fs::write(
        directory.path().join("Makefile"),
        "# build\nall:\n\tcc -O2 -o app app.c\n",
    )
    .unwrap();
    std::fs::write(
        directory.path().join("tool"),
        "#!/usr/bin/env ruby\ndef greet\n  puts \"hi\"\nend\n",
    )
    .unwrap();
    std::fs::write(directory.path().join("notes.txt"), "plain words\n").unwrap();
    git_run(directory.path(), &["add", "-N", "tool", "notes.txt"]);
    let captured = capture(directory.path(), &["HEAD"]);
    let classic = theme::Theme::classic();
    let item = |name: &str| {
        captured
            .items
            .iter()
            .find(|item| item.new_path == name.as_bytes())
            .unwrap_or_else(|| panic!("{name}"))
    };
    let makefile = preview::Preview::item(directory.path(), item("Makefile"), &classic);
    assert_eq!(makefile.reason, None);
    assert_eq!(fallback_fg(&makefile, "# build"), Some(Color::DarkGray));
    let tool = preview::Preview::item(directory.path(), item("tool"), &classic);
    assert_eq!(tool.reason, None);
    assert_eq!(fallback_fg(&tool, "def"), Some(Color::Magenta));
    assert_eq!(fallback_fg(&tool, "\"hi\""), Some(Color::Green));
    let notes = preview::Preview::item(directory.path(), item("notes.txt"), &classic);
    assert!(
        notes
            .reason
            .as_deref()
            .is_some_and(|text| text.contains("syntax highlighting unavailable for this file type")),
        "{:?}",
        notes.reason
    );
}

mod switch_file_diffs {
    use super::*;

    /// A repository with three changed files: one.rs, three.rs, two.rs.
    fn three_files() -> tempfile::TempDir {
        let directory = fixture();
        std::fs::write(directory.path().join("three.rs"), "fn three() {}\n").unwrap();
        git_run(directory.path(), &["add", "."]);
        git_run(directory.path(), &["commit", "-qm", "three"]);
        change(directory.path());
        std::fs::write(directory.path().join("three.rs"), "fn three() { 3 }\n").unwrap();
        directory
    }

    fn shown(app: &App) -> String {
        app.preview
            .as_ref()
            .expect("preview")
            .lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn assert_diff_of(app: &App, index: usize) {
        assert_eq!(app.selected, index);
        let path = String::from_utf8_lossy(&app.capture.items[index].new_path).into_owned();
        assert!(shown(app).contains(&path), "{}", shown(app));
        for (other, item) in app.capture.items.iter().enumerate() {
            if other != index {
                let path = String::from_utf8_lossy(&item.new_path).into_owned();
                assert!(!shown(app).contains(&path));
            }
        }
    }

    #[test]
    fn next_previous_and_wrap_in_both_panes() {
        let directory = three_files();
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        assert_eq!(app.capture.items.len(), 3);
        app.key(KeyCode::Enter);
        assert_diff_of(&app, 0);
        app.status = "stale".into();
        app.key(KeyCode::Char('j'));
        app.key(KeyCode::Char('l'));
        app.key_event(ctrl('j'));
        assert_diff_of(&app, 1);
        assert_eq!(app.focus, Focus::Preview);
        assert_eq!(app.status, "");
        assert_eq!((app.cursor, app.top, app.left), ((0, 0), 0, 0));
        app.key_event(ctrl('k'));
        assert_diff_of(&app, 0);
        app.key_event(ctrl('k'));
        assert_diff_of(&app, 2);
        assert_eq!(app.status, "Wrapped to last file");
        app.key_event(ctrl('j'));
        assert_diff_of(&app, 0);
        assert_eq!(app.status, "Wrapped to first file");
        // Counts are ignored.
        app.key(KeyCode::Char('2'));
        app.key_event(ctrl('j'));
        assert_diff_of(&app, 1);
        assert_eq!(app.status, "");
        // From the Files pane focus stays there.
        app.key(KeyCode::Tab);
        assert_eq!(app.focus, Focus::Files);
        app.key_event(ctrl('j'));
        assert_diff_of(&app, 2);
        assert_eq!(app.focus, Focus::Files);
        app.key_event(ctrl('j'));
        assert_diff_of(&app, 0);
        assert_eq!(app.focus, Focus::Files);
        assert_eq!(app.status, "Wrapped to first file");
    }

    #[test]
    fn file_list_keeps_the_new_selection_visible() {
        let directory = three_files();
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        screen(&mut app, 80, 5);
        let height = app.file_area.height as usize;
        assert!(height > 0 && height < 3, "{height} {:?}", app.file_area);
        app.key(KeyCode::Enter);
        app.key(KeyCode::Tab);
        app.key_event(ctrl('k'));
        assert_eq!(app.selected, 2);
        assert!(app.file_offset <= 2 && 2 < app.file_offset + height);
        app.key_event(ctrl('j'));
        assert_eq!(app.selected, 0);
        assert_eq!(app.file_offset, 0);
    }

    #[test]
    fn inactive_without_a_file_diff() {
        let directory = three_files();
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        app.key_event(ctrl('j'));
        assert_eq!(app.selected, 0);
        assert!(app.preview.is_none());
        assert_eq!(
            app.status,
            "Next/previous file works while a file diff is open"
        );
        // A source view opened from symbol search is not a file diff.
        app.key(KeyCode::Enter);
        app.key(KeyCode::Char('/'));
        for character in "two".chars() {
            app.key(KeyCode::Char(character));
        }
        // Ctrl-J inside the query input keeps its current (ignored) handling.
        app.key_event(ctrl('j'));
        assert_eq!(app.navigation.as_ref().unwrap().query, "two");
        app.key(KeyCode::Enter);
        await_navigation(&mut app);
        app.status.clear();
        app.key_event(ctrl('k'));
        assert_eq!(app.selected, 0);
        assert!(app.navigation.is_some());
        assert_eq!(
            app.status,
            "Next/previous file works while a file diff is open"
        );
        app.key(KeyCode::Enter);
        assert!(app.navigation.is_none());
        let source = shown(&app);
        app.key_event(ctrl('j'));
        assert_eq!(app.selected, 0);
        assert_eq!(shown(&app), source);
        assert_eq!(
            app.status,
            "Next/previous file works while a file diff is open"
        );
    }

    #[test]
    fn switching_drops_the_previous_file_history() {
        let directory = three_files();
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        app.key(KeyCode::Enter);
        let first = shown(&app);
        app.key(KeyCode::Char('/'));
        for character in "two".chars() {
            app.key(KeyCode::Char(character));
        }
        app.key(KeyCode::Enter);
        await_navigation(&mut app);
        app.key(KeyCode::Enter);
        assert!(app.navigation.is_none());
        // Enter in the source view reopens the diff of the first file with
        // the navigation history kept.
        app.key(KeyCode::Enter);
        assert_diff_of(&app, 0);
        assert_eq!(shown(&app), first);
        app.key_event(ctrl('j'));
        assert_diff_of(&app, 1);
        let second = shown(&app);
        app.key_event(ctrl('o'));
        assert_eq!(app.status, "No earlier navigation target");
        assert_eq!(app.selected, 1);
        assert_eq!(shown(&app), second);
        assert!(app.navigation.is_none());
    }

    #[test]
    fn documentation_and_definition_views_are_not_file_diffs() {
        for (text, query, key, documentation) in [
            (
                include_str!("../tests/fixtures/navigation/documentation.rs"),
                "documented",
                'K',
                true,
            ),
            (
                "struct CapturedType;\nfn captured_function() {}\nfn caller() { captured_function(); }\n",
                "captured_function",
                'P',
                false,
            ),
        ] {
            let directory = three_files();
            std::fs::write(directory.path().join("one.rs"), text).unwrap();
            let state = tempfile::tempdir().unwrap();
            let mut app = application(directory.path(), state.path());
            assert_eq!(app.capture.items.len(), 3);
            app.key(KeyCode::Char('/'));
            for character in query.chars() {
                app.key(KeyCode::Char(character));
            }
            app.key(KeyCode::Enter);
            await_navigation(&mut app);
            app.key(KeyCode::Char(key));
            await_navigation(&mut app);
            assert!(app.navigation.is_none(), "{key}: {}", app.status);
            assert_eq!(app.documentation, documentation, "{key}");
            assert_eq!(app.definition_preview, !documentation, "{key}");
            let view = shown(&app);
            for key_event in [ctrl('j'), ctrl('k')] {
                app.key_event(key_event);
                assert_eq!(
                    app.status,
                    "Next/previous file works while a file diff is open"
                );
                assert_eq!(app.selected, 0);
                assert_eq!(shown(&app), view);
                assert_eq!(app.documentation, documentation);
                assert_eq!(app.definition_preview, !documentation);
            }
        }
    }

    #[test]
    fn single_file_and_original_output_mode() {
        let directory = fixture();
        std::fs::write(
            directory.path().join("one.rs"),
            "fn one() {\n\tlet answer = 100;\n}\n",
        )
        .unwrap();
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        assert_eq!(app.capture.items.len(), 1);
        app.key(KeyCode::Enter);
        let before = shown(&app);
        app.key_event(ctrl('j'));
        assert_eq!(app.status, "Only one file");
        assert_eq!((app.selected, shown(&app)), (0, before.clone()));
        app.key_event(ctrl('k'));
        assert_eq!(app.status, "Only one file");
        assert_eq!((app.selected, shown(&app)), (0, before));

        change(directory.path());
        let mut app = App::new(
            capture(directory.path(), &["--stat", "HEAD"]),
            state::Store::at(state.path(), directory.path(), &[]),
        );
        let before = shown(&app);
        app.key_event(ctrl('j'));
        assert_eq!(
            app.status,
            "File actions are unavailable in original-output mode"
        );
        assert_eq!(shown(&app), before);
    }

    #[test]
    fn marks_and_state_are_untouched() {
        let directory = three_files();
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        app.key(KeyCode::Char('m'));
        assert!(app.marks[0]);
        let bytes = std::fs::read(&app.store.path).unwrap();
        app.key(KeyCode::Enter);
        for _ in 0..4 {
            app.key_event(ctrl('j'));
        }
        for _ in 0..4 {
            app.key_event(ctrl('k'));
        }
        assert_eq!(app.marks, vec![true, false, false]);
        assert_eq!(std::fs::read(&app.store.path).unwrap(), bytes);
    }

    #[test]
    fn comment_editor_keeps_ctrl_j_as_newline() {
        let directory = three_files();
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        app.key(KeyCode::Enter);
        for _ in 0..5 {
            app.key(KeyCode::Char('j'));
        }
        app.key(KeyCode::Char('c'));
        assert!(app.editor.is_some(), "{}", app.status);
        app.key(KeyCode::Char('a'));
        app.key_event(ctrl('j'));
        app.key(KeyCode::Char('b'));
        assert_eq!(app.editor.as_ref().unwrap().text(), "a\nb");
        assert_diff_of(&app, 0);
        app.key(KeyCode::Esc);
        assert!(app.editor.is_none());
        assert_diff_of(&app, 0);
    }

    #[test]
    fn emacs_keymap_rebinding_and_help() {
        let directory = three_files();
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        app.bindings = config::Config::parse("[features]\nkeymap = 'emacs'\n")
            .unwrap()
            .bindings(None)
            .unwrap();
        app.key(KeyCode::Enter);
        app.key_event(ctrl('j'));
        assert_diff_of(&app, 1);
        app.key_event(ctrl('k'));
        app.key_event(ctrl('k'));
        assert_diff_of(&app, 2);
        assert_eq!(app.status, "Wrapped to last file");
        for keymap in ["vim", "emacs"] {
            let help = config::Config::parse(&format!("[features]\nkeymap = '{keymap}'\n"))
                .unwrap()
                .bindings(None)
                .unwrap()
                .help();
            assert!(help.contains("  next_file_diff: ctrl-j\n"), "{help}");
            assert!(help.contains("  previous_file_diff: ctrl-k\n"), "{help}");
        }
        assert!(!app.bindings.legend().contains("ctrl-j"));
        app.bindings = config::Config::parse(
            "[keybindings]\nnext_file_diff = \"alt-j\"\nprevious_file_diff = \"\"\n",
        )
        .unwrap()
        .bindings(None)
        .unwrap();
        let help = app.bindings.help();
        assert!(help.contains("  next_file_diff: alt-j\n"), "{help}");
        assert!(
            help.contains("  previous_file_diff: (disabled)\n"),
            "{help}"
        );
        app.key_event(ctrl('j'));
        app.key_event(ctrl('k'));
        assert_diff_of(&app, 2);
        app.key_event(alt('j'));
        assert_diff_of(&app, 0);
        assert_eq!(app.status, "Wrapped to first file");
    }
}

fn telemetry_app(directory: &Path, state: &Path) -> App {
    std::fs::write(directory.join("one.rs"), "fn first() {} fn second() {}\n").unwrap();
    let mut app = application(directory, state);
    app.key(KeyCode::Char('/'));
    for ch in "first".chars() {
        app.key(KeyCode::Char(ch));
    }
    app.key(KeyCode::Enter);
    await_navigation(&mut app);
    app.key(KeyCode::Enter);
    app
}

fn telemetry_events(name: &str) -> Vec<serde_json::Value> {
    telemetry::take_captured()
        .into_iter()
        .filter(|event| event["event"] == name)
        .collect()
}

#[test]
fn telemetry_records_only_definitions_and_references_navigation() {
    let directory = fixture();
    let state_dir = tempfile::tempdir().unwrap();
    let mut app = telemetry_app(directory.path(), state_dir.path());
    assert!(telemetry_events("navigation").is_empty(), "search recorded");
    app.viewport = (1, 1);
    for _ in 0..14 {
        app.key(KeyCode::Right);
    }
    for (keys, operation) in [("gd", "definitions"), ("gr", "references")] {
        for ch in keys.chars() {
            app.key(KeyCode::Char(ch));
        }
        await_navigation(&mut app);
        let candidates = app.navigation.as_ref().unwrap().candidates.len();
        let events = telemetry_events("navigation");
        assert_eq!(events.len(), 1, "{keys}: {events:?}");
        let event = &events[0];
        assert_eq!(event["v"], 1);
        assert_eq!(event["operation"], operation);
        assert_eq!(event["candidates"], candidates);
        assert_eq!(event["language"], "rust");
        assert!(event["latency_ms"].as_f64().unwrap() >= 0.0);
        assert!(event["query_ms"].as_f64().unwrap() >= 0.0);
        assert!(event["waited_for_index"].is_boolean());
        let text = event.to_string();
        for private in ["one.rs", "second", "first", "HEAD"] {
            assert!(!text.contains(private), "{private} leaked: {text}");
        }
        assert!(
            !text.contains(directory.path().to_str().unwrap()),
            "path leaked"
        );
    }
    for key in ['K', 'P'] {
        let mut app = telemetry_app(directory.path(), state_dir.path());
        app.key(KeyCode::Char(key));
        await_navigation(&mut app);
        for _ in 0..100 {
            app.poll_analysis();
            std::thread::yield_now();
        }
        assert!(telemetry_events("navigation").is_empty(), "{key} recorded");
    }
}

#[test]
fn telemetry_ignores_superseded_navigation_results() {
    let directory = fixture();
    let state_dir = tempfile::tempdir().unwrap();
    let mut app = telemetry_app(directory.path(), state_dir.path());
    telemetry::take_captured();
    app.key(KeyCode::Char('g'));
    app.key(KeyCode::Char('d'));
    // Abandon the request before its results arrive.
    app.key(KeyCode::Esc);
    app.key(KeyCode::Esc);
    for _ in 0..1000 {
        app.poll_analysis();
        std::thread::yield_now();
    }
    let events = telemetry_events("navigation");
    assert!(events.is_empty(), "stale results recorded: {events:?}");
}

#[test]
fn telemetry_startup_and_analysis_fields_hold_sizes_only() {
    let directory = fixture();
    change(directory.path());
    let captured = capture(directory.path(), &["HEAD"]);
    let startup = telemetry::Startup {
        config: std::time::Duration::from_micros(1500),
        capture: std::time::Duration::from_millis(3),
        app: std::time::Duration::from_millis(2),
        keymap: "vim",
    };
    let fields = telemetry::startup_fields(
        &startup,
        std::time::Duration::from_millis(1),
        std::time::Duration::from_millis(9),
        &captured,
    );
    assert_eq!(fields["config_ms"], 1.5);
    assert_eq!(fields["files"], 2);
    assert_eq!(fields["added_lines"], 2);
    assert_eq!(fields["removed_lines"], 2);
    assert_eq!(fields["patch_bytes"], captured.original.len());
    assert_eq!(fields["sources"], captured.catalog.sources.len());
    assert_eq!(fields["structured"], true);
    assert_eq!(fields["keymap"], "vim");
    for key in [
        "capture_ms",
        "git_diff_ms",
        "app_ms",
        "terminal_ms",
        "first_frame_ms",
    ] {
        assert!(fields[key].as_f64().unwrap() >= 0.0, "{key}");
    }
    let analysis = telemetry::analysis_fields(
        &captured.catalog,
        std::time::Duration::from_millis(4),
        7,
        false,
    );
    assert_eq!(analysis["tags"], 7);
    assert_eq!(analysis["panicked"], false);
    assert_eq!(
        analysis["languages"]["rust"],
        captured.catalog.sources.len()
    );
    let text = format!(
        "{}{}",
        telemetry::record_value("abc", "startup", fields),
        analysis
    );
    for private in [
        "one.rs",
        "two.rs",
        "HEAD",
        directory.path().to_str().unwrap(),
    ] {
        assert!(!text.contains(private), "{private} leaked: {text}");
    }
    let original = capture(directory.path(), &["HEAD", "--stat"]);
    let fields = telemetry::startup_fields(
        &startup,
        std::time::Duration::ZERO,
        std::time::Duration::ZERO,
        &original,
    );
    assert_eq!(fields["structured"], false);
    assert!(!fields.to_string().contains("--stat"));
}

#[test]
fn telemetry_sink_appends_rotates_and_ignores_failures() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("review/telemetry.jsonl");
    let sink = telemetry::Sink::at(path.clone());
    sink.append("analysis", serde_json::json!({"tags": 1}))
        .unwrap();
    sink.append("analysis", serde_json::json!({"tags": 2}))
        .unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[1]["tags"], 2);
    assert_eq!(lines[0]["event"], "analysis");
    assert_eq!(lines[0]["session"], lines[1]["session"]);
    assert!(lines[0]["ts"].as_u64().unwrap() > 0);
    // Rotation: a full log moves to `.1`, replacing an older one.
    let rotated = directory.path().join("review/telemetry.jsonl.1");
    std::fs::write(&rotated, "old").unwrap();
    std::fs::write(&path, vec![b'x'; telemetry::MAX_FILE_BYTES as usize]).unwrap();
    sink.append("analysis", serde_json::json!({"tags": 3}))
        .unwrap();
    assert_eq!(
        std::fs::metadata(&rotated).unwrap().len(),
        telemetry::MAX_FILE_BYTES
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 1);
    // Unwritable locations fail without panicking.
    let blocker = directory.path().join("file");
    std::fs::write(&blocker, "").unwrap();
    assert!(
        telemetry::Sink::at(blocker.join("review/telemetry.jsonl"))
            .append("analysis", serde_json::json!({}))
            .is_err()
    );
    // Relative or missing state homes resolve to no sink at all.
    assert_eq!(
        telemetry::Sink::path_from(Some("relative".into()), Some("/home/x".into())),
        None
    );
    assert_eq!(telemetry::Sink::path_from(None, None), None);
    assert_eq!(
        telemetry::Sink::path_from(Some("".into()), Some("/home/x".into())),
        Some("/home/x/.local/state/review/telemetry.jsonl".into())
    );
    assert_eq!(
        telemetry::Sink::path_from(Some("/state".into()), Some("/home/x".into())),
        Some("/state/review/telemetry.jsonl".into())
    );
    // Without `init` nothing is installed; `init(false)` installs nothing.
    telemetry::init(false);
    assert!(!telemetry::enabled());
}

#[test]
fn telemetry_config_switch() {
    assert!(config::Config::parse("").unwrap().telemetry.enabled);
    assert!(
        !config::Config::parse("[telemetry]\nenabled = false\n")
            .unwrap()
            .telemetry
            .enabled
    );
    assert!(config::Config::parse("[telemetry]\nunknown = 1\n").is_err());
}

#[test]
fn telemetry_side_switch_does_not_record_navigation() {
    let directory = fixture();
    let state_dir = tempfile::tempdir().unwrap();
    let mut app = telemetry_app(directory.path(), state_dir.path());
    telemetry::take_captured();
    app.key(KeyCode::Char('g'));
    app.key(KeyCode::Char('d'));
    await_navigation(&mut app);
    for side in [']', '['] {
        app.key(KeyCode::Char(side));
        await_navigation(&mut app);
        for _ in 0..100 {
            app.poll_analysis();
            std::thread::yield_now();
        }
    }
    let events = telemetry_events("navigation");
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["operation"], "definitions");
}

mod keyboard_layouts {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use keybindings::{Action, Bindings, Keymap, Resolution};

    fn bindings(toml: &str) -> Bindings {
        config::Config::parse(toml).unwrap().bindings(None).unwrap()
    }

    fn press(bindings: &mut Bindings, c: char, modifiers: KeyModifiers) -> Resolution {
        bindings.resolve(KeyEvent::new(KeyCode::Char(c), modifiers), false)
    }

    /// Resolve every character of `keys`; the last one must produce `expected`.
    fn sequence(bindings: &mut Bindings, keys: &str) -> Option<Action> {
        let mut last = None;
        for c in keys.chars() {
            last = match press(bindings, c, KeyModifiers::NONE) {
                Resolution::Action(action) => Some(action),
                _ => None,
            };
        }
        last
    }

    #[test]
    fn russian_table_is_vim_russian_jcukenwin() {
        let expected: Vec<(char, char)> = [
            ("йцукенгшщзхъ", "qwertyuiop[]"),
            ("фывапролджэ", "asdfghjkl;'"),
            ("ячсмитьбю", "zxcvbnm,."),
            ("ё", "`"),
            ("ЙЦУКЕНГШЩЗХЪ", "QWERTYUIOP{}"),
            ("ФЫВАПРОЛДЖЭ", "ASDFGHJKL:\""),
            ("ЯЧСМИТЬБЮ", "ZXCVBNM<>"),
            ("Ё", "~"),
            ("№", "#"),
        ]
        .iter()
        .flat_map(|(from, to)| {
            assert_eq!(from.chars().count(), to.chars().count());
            from.chars().zip(to.chars())
        })
        .collect();
        assert_eq!(layouts::pairs("ru").unwrap(), expected);
        for name in layouts::NAMES {
            let pairs = layouts::pairs(name).unwrap();
            assert!(pairs.iter().all(|(from, _)| !from.is_ascii()), "{name}");
            assert!(pairs.iter().all(|(_, to)| to.is_ascii()), "{name}");
            let mut sources: Vec<_> = pairs.iter().map(|(from, _)| *from).collect();
            sources.sort_unstable();
            sources.dedup();
            assert_eq!(sources.len(), pairs.len(), "{name} maps a character twice");
        }
        let ukrainian = layouts::pairs("uk").unwrap();
        for pair in [
            ('і', 's'),
            ('ї', ']'),
            ('є', '\''),
            ('ґ', '\\'),
            ('Ї', '}'),
            ('\u{2bc}', '~'),
        ] {
            assert!(ukrainian.contains(&pair), "{pair:?}");
        }
        let belarusian = layouts::pairs("be").unwrap();
        for pair in [('ў', 'o'), ('і', 'b'), ('ы', 's'), ('Ў', 'O')] {
            assert!(belarusian.contains(&pair), "{pair:?}");
        }
        assert!(layouts::pairs("xx").is_none());
    }

    #[test]
    fn non_ascii_keys_resolve_as_us_keys() {
        let mut vim = bindings("");
        assert!(matches!(
            press(&mut vim, 'о', KeyModifiers::NONE),
            Resolution::Action(Action::CursorDown)
        ));
        assert_eq!(sequence(&mut vim, "пв"), Some(Action::GoToDefinition));
        assert_eq!(sequence(&mut vim, "пк"), Some(Action::GoToReferences));
        assert_eq!(sequence(&mut vim, "пп"), Some(Action::FirstLine));
        assert_eq!(sequence(&mut vim, "бй"), Some(Action::Quit));
        assert_eq!(sequence(&mut vim, "ь"), Some(Action::ToggleMark));
        // Mixed raw and translated keys inside one sequence.
        assert_eq!(sequence(&mut vim, "gв"), Some(Action::GoToDefinition));
        assert_eq!(sequence(&mut vim, ",й"), Some(Action::Quit));
        // Shifted Cyrillic acts as the shifted US key.
        assert!(matches!(
            press(&mut vim, 'Ж', KeyModifiers::SHIFT),
            Resolution::Action(Action::CommandLine)
        ));
        assert!(matches!(
            press(&mut vim, 'Л', KeyModifiers::SHIFT),
            Resolution::Action(Action::ShowDocumentation)
        ));
        // Modifiers are kept.
        assert!(matches!(
            press(&mut vim, 'о', KeyModifiers::CONTROL),
            Resolution::Action(Action::NextFileDiff)
        ));
        assert!(matches!(
            press(&mut vim, 'з', KeyModifiers::ALT),
            Resolution::Action(Action::CommandPalette)
        ));
        // Ukrainian і is s (unbound); ї is ].
        assert!(matches!(
            press(&mut vim, 'і', KeyModifiers::NONE),
            Resolution::Unbound
        ));
        assert!(!vim.pending());
        assert_eq!(sequence(&mut vim, "ї"), Some(Action::NewSide));
        // Unmapped non-ASCII keys stay unbound and clear the sequence.
        assert!(matches!(
            press(&mut vim, 'g', KeyModifiers::NONE),
            Resolution::Pending
        ));
        assert!(matches!(
            press(&mut vim, 'ä', KeyModifiers::NONE),
            Resolution::Unbound
        ));
        assert!(!vim.pending());
        // Typed text never translates: the quit path only accepts the raw keys.
        assert!(matches!(
            vim.resolve_quit(KeyEvent::new(KeyCode::Char('б'), KeyModifiers::NONE)),
            Resolution::Unbound
        ));
        assert!(!vim.pending());

        let mut emacs = bindings("[features]\nkeymap = 'emacs'");
        assert!(matches!(
            press(&mut emacs, 'ч', KeyModifiers::ALT),
            Resolution::Action(Action::CommandLine)
        ));
        assert!(matches!(
            press(&mut emacs, 'т', KeyModifiers::CONTROL),
            Resolution::Action(Action::CursorDown)
        ));
        // The default Bindings carry the default tables too.
        assert_eq!(
            sequence(&mut Bindings::default(), "о"),
            Some(Action::CursorDown)
        );
        assert_eq!(Keymap::Vim, Bindings::default().keymap);
    }

    #[test]
    fn layout_configuration_selects_overrides_and_validates() {
        // A binding on the non-ASCII key itself beats translation.
        let mut direct = bindings("[keybindings]\ntoggle_mark = 'о'");
        assert_eq!(sequence(&mut direct, "о"), Some(Action::ToggleMark));
        assert_eq!(sequence(&mut direct, "j"), Some(Action::CursorDown));
        assert!(direct.help().contains("toggle_mark: о"));
        // Disabled built-ins.
        let mut disabled = bindings("[keyboard]\nlayouts = []");
        assert_eq!(sequence(&mut disabled, "о"), None);
        assert_eq!(sequence(&mut disabled, "бй"), None);
        // Custom pairs and lists.
        let mut custom = bindings("[keyboard]\nlayouts = []\nlangmap = 'оj'");
        assert_eq!(sequence(&mut custom, "о"), Some(Action::CursorDown));
        let mut lists = bindings("[keyboard]\nlayouts = []\nlangmap = 'ОЛ;jk'");
        assert_eq!(sequence(&mut lists, "О"), Some(Action::CursorDown));
        assert_eq!(sequence(&mut lists, "Л"), Some(Action::CursorUp));
        // Escaped separators are literal targets; custom pairs beat built-ins.
        let mut escaped = bindings("[keyboard]\nlangmap = 'ж\\,,оk'");
        assert_eq!(sequence(&mut escaped, "жй"), Some(Action::Quit));
        assert_eq!(sequence(&mut escaped, "о"), Some(Action::CursorUp));
        // Among built-ins the first listed layout wins.
        let mut belarusian_first = bindings("[keyboard]\nlayouts = ['be', 'uk']");
        assert_eq!(
            sequence(&mut belarusian_first, "і"),
            Some(Action::WordBackward)
        );
        let mut ukrainian_first = bindings("[keyboard]\nlayouts = ['uk', 'be']");
        assert_eq!(sequence(&mut ukrainian_first, "і"), None);
        assert_eq!(sequence(&mut ukrainian_first, "ў"), None);

        let error = |toml: &str| {
            format!(
                "{:#}",
                config::Config::parse(toml)
                    .and_then(|c| c.bindings(None))
                    .err()
                    .unwrap_or_else(|| panic!("accepted {toml}"))
            )
        };
        let unknown = error("[keyboard]\nlayouts = ['ru', 'xx']");
        assert!(
            unknown.contains("\"xx\"") && unknown.contains("ru, uk, be"),
            "{unknown}"
        );
        for langmap in ["оjk", "о", "ОЛ;j", "ОЛ;jk;x", ";", "оj,", "aб", "о\\"] {
            let message = error(&format!("[keyboard]\nlangmap = '{langmap}'"));
            assert!(
                message.contains("[keyboard] langmap"),
                "{langmap}: {message}"
            );
        }
        error("[keyboard]\nunknown = true");
        error("[keyboard]\nlayouts = 'ru'");
    }

    #[test]
    fn application_acts_on_cyrillic_shortcuts_and_keeps_text_verbatim() {
        let directory = fixture();
        change(directory.path());
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        assert_eq!(app.selected, 0);
        app.key(KeyCode::Char('о'));
        assert_eq!(app.selected, 1);
        app.key(KeyCode::Char('л'));
        assert_eq!(app.selected, 0);
        // A count followed by a translated motion.
        app.key(KeyCode::Char('3'));
        app.key(KeyCode::Char('о'));
        assert_eq!(app.selected, 1);
        app.key(KeyCode::Char('ь'));
        assert_eq!(app.marks, [false, true]);
        assert_eq!(
            application(directory.path(), state.path()).marks,
            [false, true]
        );
        app.key(KeyCode::Char('ь'));
        assert_eq!(app.marks, [false, false]);
        let store = std::fs::read(&app.store.path).unwrap();

        // Command line: Cyrillic is text, and бй does not quit.
        app.key(KeyCode::Char('Ж'));
        assert!(app.command.is_some());
        for c in "привет бй".chars() {
            assert!(!app.key(KeyCode::Char(c)));
        }
        assert_eq!(app.command.as_ref().unwrap().text, "привет бй");
        app.key(KeyCode::Esc);
        // Palette filter.
        app.key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT));
        assert!(app.palette.is_some());
        for c in "привет бй".chars() {
            assert!(!app.key(KeyCode::Char(c)));
        }
        assert_eq!(app.palette.as_ref().unwrap().query, "привет бй");
        app.key(KeyCode::Esc);
        // Symbol query.
        app.key(KeyCode::Char('/'));
        for c in "привет бй".chars() {
            assert!(!app.key(KeyCode::Char(c)));
        }
        assert_eq!(app.navigation.as_ref().unwrap().query, "привет бй");
        app.key(KeyCode::Esc);
        assert!(app.navigation.is_none());
        assert_eq!(app.selected, 1);
        assert_eq!(app.marks, [false, false]);
        assert_eq!(std::fs::read(&app.store.path).unwrap(), store);

        // Comment editor in the diff opened through a translated key.
        app.key(KeyCode::Char('0'));
        app.key(KeyCode::Char('л'));
        app.key(KeyCode::Enter);
        assert!(app.preview.is_some());
        app.key(KeyCode::Char('о'));
        assert_eq!(app.cursor.0, 1);
        app.key(KeyCode::Char('с'));
        assert!(app.editor.is_some(), "{}", app.status);
        for c in "привет бй".chars() {
            assert!(!app.key(KeyCode::Char(c)));
        }
        assert_eq!(app.editor.as_ref().unwrap().text(), "привет бй");
        app.key(KeyCode::Esc);
        assert!(app.editor.is_none());
        assert_eq!(app.marks, [false, false]);
        // The leader quit sequence quits from Cyrillic.
        assert!(!app.key(KeyCode::Char('б')));
        assert!(app.key(KeyCode::Char('й')));
    }
}

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
    assert!(captured.unavailable.is_some());
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
    assert!(
        git::capture(directory.path(), &args)
            .unwrap_err()
            .to_string()
            .contains("Git comparison failed")
    );
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

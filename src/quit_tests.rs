use super::*;

#[test]
fn q_closes_each_view_before_exiting_in_both_styles() {
    let directory = fixture();
    change(directory.path());
    for style in ["vim", "emacs"] {
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        app.bindings = config::Config::parse(&format!("[features]\nkeymap='{style}'"))
            .unwrap()
            .bindings(None)
            .unwrap();
        app.key(KeyCode::Enter);
        assert!(!app.key(KeyCode::Char('q')));
        assert!(app.preview.is_none());
        assert_eq!(app.focus, Focus::Files);
        assert!(app.key(KeyCode::Char('q')));
        app.key(KeyCode::Enter);
        app.key(KeyCode::Char('/'));
        app.key(KeyCode::Enter);
        await_navigation(&mut app);
        assert!(app.navigation.is_some());
        assert!(!app.key(KeyCode::Char('q')));
        assert!(app.navigation.is_none() && app.preview.is_none());
        assert!(app.key(KeyCode::Char('q')));
        app.capture.unavailable = Some("original output".into());
        app.preview = Some(preview::Preview::original(b"original output"));
        assert!(!app.key(KeyCode::Char('q')));
        assert!(app.preview.is_none());
        assert!(app.key(KeyCode::Char('q')));
        assert!(app.marks.iter().all(|mark| !mark));
        assert!(state.path().read_dir().unwrap().next().is_none());
    }
}

#[test]
fn leader_quit_exits_from_files_preview_candidates_and_query() {
    let directory = fixture();
    change(directory.path());
    for context in 0..4 {
        let state = tempfile::tempdir().unwrap();
        let mut app = application(directory.path(), state.path());
        if context >= 1 {
            app.key(KeyCode::Enter);
        }
        if context >= 2 {
            app.key(KeyCode::Char('/'));
        }
        if context == 3 {
            app.key(KeyCode::Enter);
            await_navigation(&mut app);
        }
        let had_preview = app.preview.is_some();
        assert!(!app.key(KeyCode::Char(',')));
        assert!(app.key(KeyCode::Char('q')));
        assert_eq!(app.preview.is_some(), had_preview);
        assert!(app.marks.iter().all(|mark| !mark));
    }
}

#[test]
fn global_quit_prefix_preserves_query_text_and_obeys_remapping() {
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.key(KeyCode::Char('/'));
    for c in "a,bq".chars() {
        assert!(!app.key(KeyCode::Char(c)));
    }
    assert_eq!(app.navigation.as_ref().unwrap().query, "a,bq");
    app.key(KeyCode::Char(','));
    app.key(KeyCode::Backspace);
    assert_eq!(app.navigation.as_ref().unwrap().query, "a,bq");
    app.key(KeyCode::Esc);
    app.bindings = config::Config::parse("[keybindings]\nquit='zz'\nclose_or_quit='x'")
        .unwrap()
        .bindings(None)
        .unwrap();
    assert!(!app.key(KeyCode::Char('q')));
    app.key(KeyCode::Char('/'));
    assert!(!app.key(KeyCode::Char('z')));
    assert!(app.key(KeyCode::Char('z')));
    app.key(KeyCode::Esc);
    app.bindings = config::Config::parse("[keybindings]\nquit=''")
        .unwrap()
        .bindings(None)
        .unwrap();
    app.key(KeyCode::Char('/'));
    assert!(!app.key(KeyCode::Char(',')));
    assert!(!app.key(KeyCode::Char('q')));
    assert_eq!(app.navigation.as_ref().unwrap().query, ",q");
}

#[test]
fn documentation_disabled_binding_and_literal_query_leave_global_quit_available() {
    let directory = fixture();
    change(directory.path());
    let state = tempfile::tempdir().unwrap();
    let mut app = application(directory.path(), state.path());
    app.bindings = config::Config::parse("[keybindings]\nshow_documentation=''")
        .unwrap()
        .bindings(None)
        .unwrap();
    app.key(KeyCode::Char('K'));
    assert!(app.navigation.is_none());
    app.key(KeyCode::Char('/'));
    app.key(KeyCode::Char('K'));
    assert_eq!(app.navigation.as_ref().unwrap().query, "K");
    app.key(KeyCode::Char(','));
    assert!(app.key(KeyCode::Char('q')));
    assert!(app.marks.iter().all(|marked| !marked));
}

use super::{
    library::{Column, GAP, table_columns},
    sessions::Pane,
    settings::theme_option,
    tabs::{drop_index, session_tab},
    *,
};
use egui::{Rect, Vec2};
fn test_app(ctx: &egui::Context) -> App {
    let connections = ["Alpha", "Bravo", "Charlie"]
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let mut c = Connection::new();
            c.id = i as u64 + 1;
            c.name = (*name).into();
            c.host = format!("{}.example", name.to_lowercase());
            c
        })
        .collect();
    // Tests that save write here, never into the working directory.
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let path = std::env::temp_dir().join(format!(
        "relay-test-{}-{}.json",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    App::with(
        ctx.clone(),
        path,
        Ok(connections),
        None,
        None,
        None,
        state::State::default(),
    )
}

fn library_frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) {
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1200., 800.),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.workspace(ui));
        },
    );
}

fn press(key: egui::Key) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }
}

#[test]
fn search_enter_connects_the_top_match() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    app.search_focus = true;
    library_frame(&mut app, &ctx, vec![]);
    library_frame(&mut app, &ctx, vec![egui::Event::Text("ch".into())]);
    library_frame(&mut app, &ctx, vec![]);
    assert_eq!(app.query, "ch");
    assert_eq!(app.selected, Some(3));
    library_frame(&mut app, &ctx, vec![press(egui::Key::Enter)]);
    assert_eq!(app.selected, Some(3));
    assert_eq!(
        app.connect_request.map(|c| c.name).as_deref(),
        Some("Charlie"),
        "Enter did not ask to connect"
    );
}

#[test]
fn new_connection_starts_in_the_open_group() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    app.new_connection();
    assert!(matches!(&app.modal, Modal::Editor(c) if c.group == "Personal"));
    app.group = Some("Servers".into());
    app.new_connection();
    assert!(matches!(&app.modal, Modal::Editor(c) if c.group == "Servers"));
}

#[test]
fn enter_quick_connects_when_nothing_matches() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    app.search_focus = true;
    library_frame(&mut app, &ctx, vec![]);
    library_frame(
        &mut app,
        &ctx,
        vec![egui::Event::Text("deploy@10.0.0.9".into())],
    );
    library_frame(&mut app, &ctx, vec![]);
    assert!(app.filtered().is_empty());
    library_frame(&mut app, &ctx, vec![press(egui::Key::Enter)]);
    let request = app
        .connect_request
        .expect("Enter did not ask to quick connect");
    assert_eq!(
        (
            request.protocol,
            request.username.as_str(),
            request.host.as_str()
        ),
        (Protocol::Ssh, "deploy", "10.0.0.9")
    );
}

#[test]
fn arrows_move_selection_and_escape_clears_search() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    app.search_focus = true;
    library_frame(&mut app, &ctx, vec![]);
    library_frame(&mut app, &ctx, vec![egui::Event::Text("a".into())]);
    library_frame(&mut app, &ctx, vec![]);
    // "a" matches all three names (and hosts); the first is selected.
    assert_eq!(app.selected, Some(1));
    for expected in [2, 3, 3] {
        library_frame(&mut app, &ctx, vec![press(egui::Key::ArrowDown)]);
        assert_eq!(app.selected, Some(expected));
        assert!(ctx.memory(|m| m.has_focus(egui::Id::new("library-search"))));
    }
    library_frame(&mut app, &ctx, vec![press(egui::Key::ArrowUp)]);
    assert_eq!(app.selected, Some(2));
    library_frame(&mut app, &ctx, vec![press(egui::Key::Escape)]);
    assert!(app.query.is_empty());
    // With nothing focused, arrows and Enter still drive the list.
    library_frame(&mut app, &ctx, vec![press(egui::Key::ArrowUp)]);
    assert_eq!(app.selected, Some(1));
    assert!(app.connect_request.is_none());
    library_frame(&mut app, &ctx, vec![press(egui::Key::Enter)]);
    assert_eq!(
        app.connect_request.map(|c| c.id),
        Some(1),
        "Enter did not ask to connect"
    );
}

#[test]
fn connection_rows_never_show_a_text_cursor() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    apply_style(&ctx);
    library_frame(&mut app, &ctx, vec![]);
    // Sweep the pointer over the header and rows, including names, hosts and groups.
    for y in (100..300).step_by(8) {
        for x in (20..1180).step_by(20) {
            let pos = egui::pos2(x as f32, y as f32);
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1200., 800.),
                    )),
                    events: vec![egui::Event::PointerMoved(pos)],
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| app.workspace(ui));
                },
            );
            // The search field is the one place where typing is expected.
            let search = ctx
                .read_response(egui::Id::new("library-search"))
                .is_some_and(|r| r.rect.expand(8.).contains(pos));
            assert!(
                output.platform_output.cursor_icon != egui::CursorIcon::Text || search,
                "text cursor at {pos:?}"
            );
        }
    }
}

#[test]
fn tabs_from_the_last_run_come_back_waiting() {
    let ctx = egui::Context::default();
    let mut state = state::State::default();
    let mut desk = Connection::new();
    desk.name = "Desk".into();
    desk.host = "desk.example".into();
    let mut quick = Connection::quick("deploy@10.0.0.9").unwrap();
    quick.id = 0;
    state.open = vec![desk, quick];
    state.active_tab = Some(1);
    let mut app = App::with(
        ctx.clone(),
        PathBuf::from("unused.json"),
        Ok(vec![]),
        None,
        None,
        None,
        state,
    );
    app.restore_tabs();
    assert_eq!(app.sessions.len(), 2);
    assert_eq!(app.active, Some(1));
    assert!(
        app.sessions
            .iter()
            .all(|s| s.tab_state() == session::TabState::Waiting)
    );
    assert_eq!(app.sessions[1].connection.username, "deploy");
    // Out-of-range selections are dropped rather than trusted.
    app.state.active_tab = Some(9);
    app.sessions.clear();
    app.restore_tabs();
    assert_eq!(app.active, None);
}

#[test]
fn split_view_pairs_two_tabs_and_follows_moves() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    for c in app.connections.clone() {
        app.sessions
            .push(session::Session::restored(c, false, ctx.clone()));
    }
    app.active = Some(0);
    app.toggle_split();
    assert_eq!(app.split, Some(1), "the neighbouring tab opens beside");
    app.toggle_split();
    assert_eq!(app.split, None);
    app.split_with(0);
    assert_eq!(app.split, None, "a tab cannot sit beside itself");
    app.split_with(2);
    assert_eq!(app.split, Some(2));
    app.move_session(2, 0);
    assert_eq!((app.active, app.split), (Some(1), Some(0)));
    app.active = Some(2);
    app.toggle_split();
    assert_eq!(app.split, None, "toggling closes an open split");
    app.toggle_split();
    assert_eq!(
        app.split,
        Some(1),
        "the last tab pairs with the one before it"
    );
}

#[test]
fn moving_tabs_keeps_every_index_consistent() {
    let items = ['a', 'b', 'c', 'd'];
    for from in 0..items.len() {
        for to in 0..items.len() {
            let mut after = items.to_vec();
            let item = after.remove(from);
            after.insert(to, item);
            for (index, item) in items.iter().enumerate() {
                assert_eq!(after[moved(index, from, to)], *item, "{from} -> {to}");
            }
        }
    }
    let tab = |x: f32| Rect::from_min_size(egui::pos2(x, 0.), Vec2::new(100., 36.));
    let tabs = [tab(0.), tab(110.), tab(220.)];
    assert_eq!(drop_index(&tabs, 0, 10.), 0);
    assert_eq!(drop_index(&tabs, 0, 200.), 1);
    assert_eq!(drop_index(&tabs, 0, 400.), 2);
    assert_eq!(drop_index(&tabs, 2, 0.), 0);
}

#[test]
fn recent_filter_lists_opened_profiles_newest_first() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let location = app.location.clone();
    for id in [1, 3, 99] {
        app.state.note_recent(&location, id);
    }
    app.filter = Filter::Recent;
    let ids: Vec<_> = app.filtered().iter().map(|c| c.id).collect();
    assert_eq!(ids, [3, 1], "newest first, deleted profiles skipped");
}

#[test]
fn narrow_tables_drop_columns_and_fit() {
    let kinds = |width| -> Vec<_> { table_columns(width).into_iter().map(|c| c.0).collect() };
    assert!(kinds(900.).contains(&Column::Group));
    assert!(!kinds(700.).contains(&Column::Group) && kinds(700.).contains(&Column::Protocol));
    assert!(!kinds(500.).contains(&Column::Protocol));
    for width in [500., 700., 900., 1400.] {
        let columns = table_columns(width);
        let used: f32 = columns.iter().map(|c| c.1).sum::<f32>() + GAP * (columns.len() - 1) as f32;
        assert!((used - width).abs() < 0.5, "{width}: {used}");
    }
}

#[test]
fn tab_close_is_inside_tab_and_does_not_select_it() {
    for close_target in [false, true] {
        let ctx = egui::Context::default();
        let frame = |events| {
            let mut result = None;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(500., 100.))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        ui.horizontal(|ui| {
                            let mut first = Connection::new();
                            first.name = "Design workstation".into();
                            let mut second = Connection::new();
                            second.name = "Build server".into();
                            let session =
                                session_tab(ui, &first, false, session::TabState::Open, true);
                            assert!(session.response.rect.contains_rect(session.inner.1.rect));
                            assert!(!session.inner.0.rect.intersects(session.inner.1.rect));
                            let next =
                                session_tab(ui, &second, true, session::TabState::Waiting, false);
                            assert_eq!(
                                session.response.rect.center().y,
                                next.response.rect.center().y
                            );
                            assert!(session.response.rect.right() < next.response.rect.left());
                            result = Some((
                                session.inner.0.clicked(),
                                session.inner.1.clicked(),
                                if close_target {
                                    session.inner.1.rect.center()
                                } else {
                                    session.inner.0.rect.center()
                                },
                            ));
                        });
                    });
                },
            );
            result.unwrap()
        };
        let (_, _, pos) = frame(vec![]);
        frame(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
        let (selected, closed, _) = frame(vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert_eq!(selected, !close_target);
        assert_eq!(closed, close_target);
    }
}

#[test]
fn navigation_and_theme_clicks_include_label_and_both_edges() {
    for (theme_row, x) in [
        (false, 18.),
        (false, 85.),
        (false, 242.),
        (true, 18.),
        (true, 85.),
        (true, 242.),
    ] {
        let ctx = egui::Context::default();
        let frame = |events| {
            let mut clicked = false;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(260., 80.))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        clicked = if theme_row {
                            theme_option(ui, theme::Theme::Midnight, false).clicked()
                        } else {
                            nav_row(ui, Icon::Folder, "Infrastructure", 10, false)
                        };
                    });
                },
            );
            (clicked, output.platform_output.cursor_icon)
        };
        frame(vec![]);
        let pos = egui::pos2(x, 26.);
        let (_, cursor) = frame(vec![egui::Event::PointerMoved(pos)]);
        assert_ne!(cursor, egui::CursorIcon::Text, "text cursor at x={x}");
        frame(vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }]);
        let (clicked, _) = frame(vec![egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }]);
        assert!(clicked, "navigation row ignored click at x={x}");
    }
}

/// An app with one file browser tab showing `/srv`.
fn files_app(ctx: &egui::Context) -> App {
    let mut app = test_app(ctx);
    let entry = |name: &str, dir: bool| crate::sftp::Entry {
        name: name.into(),
        dir,
        link: false,
        special: false,
        size: 10,
        modified: Some(1_700_000_000),
        mode: Some(if dir { 0o040_755 } else { 0o100_644 }),
    };
    let browser = crate::sftp::Browser::demo(
        "/srv",
        vec![
            entry("b.txt", false),
            entry("app", true),
            entry("c.txt", false),
        ],
    );
    let mut c = app.connections[0].clone();
    c.protocol = Protocol::Ssh;
    app.sessions
        .push(session::Session::demo_files(c, browser, ctx.clone()));
    app.active = Some(0);
    app
}

fn files_frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) {
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1200., 800.),
            )),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.files_pane(ui, 0, Pane::Main));
        },
    );
}

fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers,
    }
}

fn files(app: &App) -> &crate::sftp::Browser {
    app.sessions[0].files.as_ref().unwrap()
}

#[test]
fn file_browser_keys_select_ranges_and_ask_before_deleting() {
    let ctx = egui::Context::default();
    let mut app = files_app(&ctx);
    files_frame(&mut app, &ctx, vec![]);
    assert!(files(&app).view.focused, "a new browser takes the keyboard");
    let none = egui::Modifiers::NONE;
    files_frame(&mut app, &ctx, vec![key(egui::Key::ArrowDown, none)]);
    assert_eq!(
        files(&app).view.cursor.as_deref(),
        Some("app"),
        "folders come first"
    );
    files_frame(&mut app, &ctx, vec![key(egui::Key::ArrowDown, none)]);
    files_frame(
        &mut app,
        &ctx,
        vec![key(egui::Key::ArrowDown, egui::Modifiers::SHIFT)],
    );
    let selected: Vec<_> = files(&app).view.selected.iter().cloned().collect();
    assert_eq!(selected, ["b.txt", "c.txt"]);
    files_frame(&mut app, &ctx, vec![key(egui::Key::Delete, none)]);
    assert_eq!(
        files(&app).view.confirm,
        Some(crate::sftp::Confirm::Delete(vec![
            "b.txt".into(),
            "c.txt".into()
        ]))
    );
    files_frame(&mut app, &ctx, vec![key(egui::Key::Escape, none)]);
    files_frame(&mut app, &ctx, vec![]);
    assert_eq!(files(&app).view.confirm, None, "Escape keeps the files");
    files_frame(&mut app, &ctx, vec![key(egui::Key::Home, none)]);
    files_frame(&mut app, &ctx, vec![key(egui::Key::Enter, none)]);
    assert!(files(&app).loading, "Enter opens the folder");
    assert!(!files(&app).ended());
}

#[test]
fn f2_renames_in_place_and_keeps_the_extension() {
    let ctx = egui::Context::default();
    let mut app = files_app(&ctx);
    files_frame(&mut app, &ctx, vec![]);
    let none = egui::Modifiers::NONE;
    files_frame(&mut app, &ctx, vec![key(egui::Key::End, none)]);
    files_frame(&mut app, &ctx, vec![key(egui::Key::F2, none)]);
    files_frame(&mut app, &ctx, vec![]);
    files_frame(&mut app, &ctx, vec![egui::Event::Text("notes".into())]);
    files_frame(&mut app, &ctx, vec![key(egui::Key::Enter, none)]);
    let browser = files(&app);
    assert_eq!(browser.view.rename, None);
    assert_eq!(browser.error, None);
    assert_eq!(browser.busy, 1, "the rename was sent");
    let selected: Vec<_> = browser.view.selected.iter().cloned().collect();
    assert_eq!(selected, ["notes.txt"]);
}

#[test]
fn file_tabs_are_remembered_as_browsers() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let mut c = app.connections[0].clone();
    c.protocol = Protocol::Ssh;
    app.sessions
        .push(session::Session::restored(c.clone(), false, ctx.clone()));
    app.sessions
        .push(session::Session::restored(c.clone(), true, ctx.clone()));
    app.remember_tabs();
    assert_eq!(app.state.file_tabs, [1]);
    let mut restored = App::with(
        ctx.clone(),
        PathBuf::from("unused.json"),
        Ok(vec![]),
        None,
        None,
        None,
        app.state.clone(),
    );
    restored.restore_tabs();
    let kinds: Vec<_> = restored.sessions.iter().map(|s| s.browsing).collect();
    assert_eq!(kinds, [false, true]);

    // A terminal's Files button pairs it with the open browser instead of opening another.
    app.active = Some(0);
    app.open_companion(0);
    assert_eq!(
        (app.active, app.split, app.sessions.len()),
        (Some(0), Some(1), 2)
    );
    app.split = None;
    app.open_companion(1);
    assert_eq!((app.active, app.split), (Some(1), Some(0)));
}

#[test]
fn breadcrumbs_read_like_the_path() {
    let trail = |path: &str, home: &str| {
        let crumbs = crate::sftp::crumbs(path, home);
        (0..crumbs.len())
            .map(|i| {
                let slash = if super::files::separated(&crumbs, i) {
                    "/"
                } else {
                    ""
                };
                format!("{slash}{}", crumbs[i].0)
            })
            .collect::<String>()
    };
    assert_eq!(trail("/var", "/home/deploy"), "/var");
    assert_eq!(trail("/var/log/nginx", "/home/deploy"), "/var/log/nginx");
    assert_eq!(trail("/", "/home/deploy"), "/");
    assert_eq!(trail("/home/deploy/app/src", "/home/deploy"), "~/app/src");
    assert_eq!(trail("/home/deploy", "/home/deploy"), "~");
}

#[test]
fn ended_and_unconnected_tabs_close_without_asking() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    for c in app.connections.clone() {
        app.sessions
            .push(session::Session::restored(c, false, ctx.clone()));
    }
    app.active = Some(2);
    // Restored tabs that never connected close at once, keeping the selection.
    app.request_close(0);
    assert!(matches!(app.modal, Modal::None));
    assert_eq!((app.sessions.len(), app.active), (2, Some(1)));
    // A connected session asks first.
    let mut c = app.connections[0].clone();
    c.protocol = Protocol::Ssh;
    let browser = crate::sftp::Browser::demo("/srv", vec![]);
    app.sessions
        .push(session::Session::demo_files(c, browser, ctx.clone()));
    app.request_close(2);
    assert!(matches!(app.modal, Modal::Close(2)));
    assert_eq!(app.sessions.len(), 3);
    // Once its connection ends, it closes without asking.
    app.modal = Modal::None;
    app.sessions[2]
        .files
        .as_mut()
        .unwrap()
        .disconnect("The connection ended.");
    app.sessions[2].poll(None);
    app.request_close(2);
    assert!(matches!(app.modal, Modal::None));
    assert_eq!(app.sessions.len(), 2);
}

#[test]
fn turning_off_reopening_forgets_and_skips_tabs() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    for c in app.connections.clone() {
        app.sessions
            .push(session::Session::restored(c, false, ctx.clone()));
    }
    app.active = Some(1);
    app.remember_tabs();
    assert_eq!(app.state.open.len(), 3);
    app.state.forget_tabs = true;
    app.remember_tabs();
    assert!(app.state.open.is_empty() && app.state.active_tab.is_none());
    // Even a state file that still lists tabs opens none.
    let mut state = app.state.clone();
    state.open = app.connections.clone();
    let mut next = App::with(
        ctx.clone(),
        PathBuf::from("unused.json"),
        Ok(vec![]),
        None,
        None,
        None,
        state,
    );
    next.restore_tabs();
    assert!(next.sessions.is_empty());
}

#[test]
fn deleting_the_last_search_match_clears_the_search() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    app.query = "charlie".into();
    app.selected = Some(3);
    assert!(app.delete_connection(3));
    assert!(app.query.is_empty(), "the search would offer quick connect");
    assert_eq!(app.selected, None);
    // Other matches keep the search.
    let mut app = test_app(&ctx);
    app.query = "a".into();
    assert!(app.delete_connection(1));
    assert_eq!(app.query, "a");
}

#[test]
fn middle_clicking_a_tab_closes_it() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let c = app.connections[0].clone();
    app.sessions
        .push(session::Session::restored(c, false, ctx.clone()));
    let frame = |app: &mut App, events| {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1200., 800.),
                )),
                events,
                ..Default::default()
            },
            |ctx| app.tabs(ctx),
        );
    };
    frame(&mut app, vec![]);
    // Walk along the tab bar; past the library tab, the session tab closes.
    for x in (20..400).step_by(10) {
        let pos = egui::pos2(x as f32, 20.);
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Middle,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&mut app, vec![egui::Event::PointerMoved(pos)]);
        frame(&mut app, vec![button(true)]);
        frame(&mut app, vec![button(false)]);
        if app.sessions.is_empty() {
            return;
        }
    }
    panic!("no middle click closed the tab");
}

#[test]
fn the_version_names_the_release() {
    let version = super::settings::version();
    assert!(version.starts_with(&format!("Relay {}", env!("CARGO_PKG_VERSION"))));
}

#[test]
fn shared_library_syncs_silently() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let path = std::env::temp_dir().join(format!("relay-sync-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let location = database::Location::Sqlite(path.clone());
    let save = |revision, connections| {
        database::start(location.clone(), revision, Some(connections), ctx.clone())
            .recv()
            .unwrap()
            .unwrap()
    };
    let mine = save(0, app.connections.clone());
    app.location = location.clone();
    app.revision = mine.revision;
    // A teammate adds a connection.
    let mut theirs = app.connections.clone();
    let mut added = Connection::new();
    added.id = 9;
    added.name = "Delta".into();
    added.host = "delta.example".into();
    theirs.push(added);
    save(mine.revision, theirs);

    let sync = |app: &mut App| {
        app.synced = Instant::now() - Duration::from_secs(61);
        app.sync_library(&ctx);
        assert!(app.sync.is_some(), "no background reload started");
        while app.sync.is_some() {
            std::thread::sleep(Duration::from_millis(10));
            app.sync_library(&ctx);
        }
    };
    // Unsaved changes after a rejected save are not replaced.
    app.recovery = Some(app.connections.clone());
    app.synced = Instant::now() - Duration::from_secs(61);
    app.sync_library(&ctx);
    assert!(app.sync.is_none());
    app.recovery = None;

    sync(&mut app);
    assert_eq!(app.connections.len(), 4);
    assert!(app.pending.is_none());
    assert!(app.notice.is_none() && app.error.is_none());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn changes_that_repeat_an_id_are_not_saved() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let mut next = app.connections.clone();
    next[2].id = 1;
    assert!(!app.persist(next));
    assert!(app.pending.is_none() && app.recovery.is_none());
    assert!(app.error.is_some());
}

#[test]
fn search_clear_button_empties_the_search() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    library_frame(&mut app, &ctx, vec![]);
    let clear_id = egui::Id::new("library-search").with("clear");
    assert!(
        ctx.read_response(clear_id).is_none(),
        "clear shown while empty"
    );
    app.query = "ch".into();
    library_frame(&mut app, &ctx, vec![]);
    let at = ctx
        .read_response(clear_id)
        .expect("no clear button")
        .rect
        .center();
    let click = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    library_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(at)]);
    library_frame(&mut app, &ctx, vec![click(true)]);
    library_frame(&mut app, &ctx, vec![click(false)]);
    library_frame(&mut app, &ctx, vec![]);
    assert!(app.query.is_empty());
    assert_eq!(app.filtered().len(), 3);
    assert!(ctx.memory(|m| m.has_focus(egui::Id::new("library-search"))));
}

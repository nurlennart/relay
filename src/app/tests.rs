use super::{
    library::{Column, GAP, table_columns},
    sessions::Pane,
    settings::theme_option,
    tabs::{Kind, drop_index, session_tab},
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
fn shortcut_help_respects_session_focus_and_open_editors() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let frame = |app: &mut App, key, session_focused| {
        let mut key_remaining = false;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1200., 800.),
                )),
                events: vec![press(key)],
                ..Default::default()
            },
            |ctx| {
                app.keyboard_shortcuts(ctx, session_focused);
                key_remaining = ctx.input(|i| i.key_pressed(key));
            },
        );
        key_remaining
    };
    assert!(!frame(&mut app, egui::Key::F1, false));
    assert!(matches!(app.modal, Modal::Shortcuts));
    frame(&mut app, egui::Key::Escape, false);
    assert!(matches!(app.modal, Modal::None));

    assert!(
        frame(&mut app, egui::Key::F1, true),
        "F1 belongs to the session"
    );
    assert!(matches!(app.modal, Modal::None));
    app.modal = Modal::Editor(Connection::new());
    frame(&mut app, egui::Key::F1, false);
    assert!(
        matches!(app.modal, Modal::Editor(_)),
        "keep the editor draft"
    );

    app.modal = Modal::Settings;
    frame(&mut app, egui::Key::F1, true);
    assert!(matches!(app.modal, Modal::Shortcuts));
    app.modal = Modal::None;
    app.quitting = true;
    frame(&mut app, egui::Key::F1, false);
    assert!(
        matches!(app.modal, Modal::None),
        "keep the quit confirmation"
    );
}

#[test]
fn enter_in_notes_adds_a_line_and_notes_are_searchable() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
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
            |ctx| app.editor(ctx),
        );
    };
    app.modal = Modal::Editor(app.connections[0].clone());
    frame(&mut app, vec![]);
    ctx.memory_mut(|m| m.request_focus(egui::Id::new(editor::NOTES_ID)));
    frame(&mut app, vec![egui::Event::Text("Rack 4".into())]);
    frame(&mut app, vec![press(egui::Key::Enter)]);
    frame(&mut app, vec![egui::Event::Text("UPS".into())]);
    assert!(
        matches!(&app.modal, Modal::Editor(c) if c.notes == "Rack 4\nUPS"),
        "Enter in the notes saved or lost the line break"
    );
    ctx.memory_mut(|m| m.surrender_focus(egui::Id::new(editor::NOTES_ID)));
    frame(&mut app, vec![press(egui::Key::Enter)]);
    assert!(matches!(app.modal, Modal::None));
    while app.pending.is_some() {
        std::thread::sleep(std::time::Duration::from_millis(10));
        app.poll_storage();
    }
    assert_eq!(app.connections[0].notes, "Rack 4\nUPS");
    app.query = "ups".into();
    assert_eq!(app.filtered(), vec![0]);
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

/// A frame with quick open in front of the library, as Relay draws them.
fn quick_frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>) {
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
            app.quick_open(ctx);
            egui::CentralPanel::default().show(ctx, |ui| app.workspace(ui));
        },
    );
}

#[test]
fn quick_open_lists_recent_connections_first_then_by_name() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let names = |app: &App, query: &str| -> Vec<String> {
        (app.quick_matches(query).into_iter())
            .map(|i| app.connections[i].name.clone())
            .collect()
    };
    assert_eq!(names(&app, ""), ["Alpha", "Bravo", "Charlie"]);
    app.state.note_recent(&app.location, 3);
    assert_eq!(names(&app, ""), ["Charlie", "Alpha", "Bravo"]);
    assert_eq!(names(&app, " BRAV "), ["Bravo"]);
    assert!(names(&app, "nothing").is_empty());
}

#[test]
fn quick_open_connects_with_enter_and_keeps_it_from_the_library() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    // The library behind it has Alpha selected, which its own Enter would connect.
    app.selected = Some(1);
    app.open_quick();
    quick_frame(&mut app, &ctx, vec![]);
    quick_frame(&mut app, &ctx, vec![egui::Event::Text("bravo".into())]);
    quick_frame(&mut app, &ctx, vec![press(egui::Key::Enter)]);
    assert_eq!(app.connect_request.take().unwrap().name, "Bravo");
    assert!(app.quick_open.is_none(), "choosing closes quick open");
    // Alt+Enter connects to what was typed even when the library has a match.
    app.open_quick();
    quick_frame(&mut app, &ctx, vec![]);
    quick_frame(&mut app, &ctx, vec![egui::Event::Text("alpha".into())]);
    let alt_enter = egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::ALT,
    };
    quick_frame(&mut app, &ctx, vec![alt_enter]);
    let typed = app.connect_request.take().unwrap();
    assert_eq!((typed.id, typed.host.as_str()), (0, "alpha"));
    // Esc closes it without connecting.
    app.open_quick();
    quick_frame(&mut app, &ctx, vec![]);
    quick_frame(&mut app, &ctx, vec![press(egui::Key::Escape)]);
    assert!(app.quick_open.is_none() && app.connect_request.is_none());
}

#[test]
fn quick_open_acts_on_text_typed_or_pasted_in_the_same_frame_as_enter() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    app.open_quick();
    quick_frame(&mut app, &ctx, vec![]);
    // The list before the text starts with Alpha; Enter must act on what the text leaves.
    quick_frame(
        &mut app,
        &ctx,
        vec![egui::Event::Text("bravo".into()), press(egui::Key::Enter)],
    );
    assert_eq!(app.connect_request.take().unwrap().name, "Bravo");
    app.open_quick();
    quick_frame(&mut app, &ctx, vec![]);
    quick_frame(
        &mut app,
        &ctx,
        vec![
            egui::Event::Paste("deploy@10.0.0.9".into()),
            press(egui::Key::Enter),
        ],
    );
    let typed = app.connect_request.take().unwrap();
    assert_eq!(
        (typed.username.as_str(), typed.host.as_str()),
        ("deploy", "10.0.0.9")
    );
}

#[test]
fn quick_open_keeps_the_arrow_keys_choice_under_a_resting_pointer() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    app.open_quick();
    quick_frame(&mut app, &ctx, vec![]);
    // Point at the second row, then leave the pointer there.
    let second = (0..800)
        .map(|y| egui::pos2(600., y as f32))
        .find(|&pos| {
            quick_frame(&mut app, &ctx, vec![egui::Event::PointerMoved(pos)]);
            app.quick_open.as_ref().unwrap().selected == 1
        })
        .expect("the second row");
    quick_frame(&mut app, &ctx, vec![]);
    quick_frame(&mut app, &ctx, vec![press(egui::Key::ArrowDown)]);
    quick_frame(&mut app, &ctx, vec![]);
    assert_eq!(app.quick_open.as_ref().unwrap().selected, 2);
    // Moving it again chooses the row under it.
    quick_frame(
        &mut app,
        &ctx,
        vec![egui::Event::PointerMoved(second + Vec2::new(5., 0.))],
    );
    assert_eq!(app.quick_open.as_ref().unwrap().selected, 1);
}

#[test]
fn snippets_send_what_text_typed_in_the_same_frame_as_enter_selects() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    for (name, command) in [("Alpha", "echo-alpha"), ("Beta", "echo-beta")] {
        app.state.snippets.push(state::Snippet {
            name: name.into(),
            command: command.into(),
            run: true,
        });
    }
    let mut ssh = app.connections[0].clone();
    ssh.protocol = Protocol::Ssh;
    // cat shows what the snippet types.
    app.sessions
        .push(session::Session::scripted(ssh, "cat", ctx.clone()));
    app.active = Some(0);
    app.palette = Some(Palette::default());
    let frame = |app: &mut App, events| {
        let _ = ctx.run(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ctx| app.snippet_palette(ctx),
        );
    };
    frame(&mut app, vec![]);
    frame(
        &mut app,
        vec![egui::Event::Text("beta".into()), press(egui::Key::Enter)],
    );
    assert!(app.palette.is_none(), "Enter sent a snippet");
    let terminal = app.sessions[0].terminal.as_ref().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !terminal.screen_text().contains("echo-beta") {
        assert!(
            !terminal.screen_text().contains("echo-alpha"),
            "sent the snippet chosen before the text"
        );
        assert!(Instant::now() < deadline, "nothing arrived");
        std::thread::sleep(Duration::from_millis(20));
    }
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
fn tabs_take_saved_settings_only_from_their_own_library() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let team = state::Library::Sqlite(PathBuf::from("team.sqlite"));
    // A tab from a shared library whose ID 2 is Bravo's here, and one of Alpha's from before
    // its host changed.
    let mut elsewhere = Connection::new();
    elsewhere.id = 2;
    elsewhere.host = "elsewhere.example".into();
    let mut alpha = app.connections[0].clone();
    alpha.host = "old.example".into();
    app.state.open = vec![elsewhere, alpha];
    // Tabs saved before libraries were recorded came from the library then open.
    app.state.tab_libraries = vec![team.clone()];
    app.restore_tabs();
    assert_eq!(app.current_settings(0).host, "elsewhere.example");
    assert_eq!(app.current_settings(1).host, "alpha.example");

    // Once Alpha is deleted, a new connection that reuses its ID is not its tab's.
    app.connections.remove(0);
    app.forget_removed();
    assert_eq!(app.sessions[1].connection.id, 0);
    assert_eq!(
        app.sessions[0].connection.id, 2,
        "other libraries are left alone"
    );
    assert_eq!(app.state.tab_libraries, [team, state::Library::Personal]);
}

#[test]
fn reconnecting_never_takes_settings_from_another_database_of_the_same_kind() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let database = |name: &str| {
        database::Location::Mssql(database::Server {
            connection: format!("Server=sql01;Database={name}"),
            tls: true,
        })
    };
    // A tab for connection 1 of database A, opened while A was the library.
    let mut from_a = app.connections[0].clone();
    from_a.host = "a.example".into();
    app.location = database("A");
    app.state.open = vec![from_a.clone(), from_a];
    app.state.tab_libraries = vec![
        state::Library::of(&app.location),
        // One saved before Relay recorded which server a tab came from.
        state::Library::Mssql(None),
    ];
    app.restore_tabs();
    // Database B has a different connection 1.
    app.location = database("B");
    app.connections[0].host = "b.example".into();
    assert_eq!(app.current_settings(0).host, "a.example");
    assert_eq!(app.current_settings(1).host, "a.example");
    app.forget_removed();
    assert_eq!(app.sessions[0].connection.id, 1, "B's IDs are not A's");
    // Back in A, the tab follows A's saved settings again.
    app.location = database("A");
    app.connections[0].host = "a2.example".into();
    assert_eq!(app.current_settings(0).host, "a2.example");
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
fn split_divider_drags_and_returns_to_the_middle_on_double_click() {
    let ctx = egui::Context::default();
    let mut fraction = 0.3;
    let mut time = 0.;
    let mut frame = |fraction: &mut f32, events: Vec<egui::Event>| {
        time += 0.05;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1000., 400.),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default()
                    .frame(egui::Frame::NONE)
                    .show(ctx, |ui| split_divider(ui, ui.max_rect(), fraction));
            },
        );
    };
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    frame(&mut fraction, vec![]);
    // Dragging moves it, but never past a quarter of the width.
    let on = egui::pos2(300., 200.);
    frame(
        &mut fraction,
        vec![egui::Event::PointerMoved(on), button(on, true)],
    );
    for x in [250., 150., 100.] {
        frame(
            &mut fraction,
            vec![egui::Event::PointerMoved(egui::pos2(x, 200.))],
        );
    }
    frame(&mut fraction, vec![button(egui::pos2(100., 200.), false)]);
    assert_eq!(fraction, 0.25);
    // A double-click returns it to the middle.
    let on = egui::pos2(250., 200.);
    frame(&mut fraction, vec![egui::Event::PointerMoved(on)]);
    for pressed in [true, false, true, false] {
        frame(&mut fraction, vec![button(on, pressed)]);
    }
    assert_eq!(fraction, SPLIT_MIDDLE);
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
    let ids: Vec<_> = app
        .filtered()
        .iter()
        .map(|&i| app.connections[i].id)
        .collect();
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
                            let session = session_tab(
                                ui,
                                &first,
                                Kind::Session,
                                session::TabState::Open,
                                true,
                            );
                            assert!(session.response.rect.contains_rect(session.inner.1.rect));
                            assert!(!session.inner.0.rect.intersects(session.inner.1.rect));
                            let next = session_tab(
                                ui,
                                &second,
                                Kind::Files,
                                session::TabState::Waiting,
                                false,
                            );
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

#[test]
fn settings_tabs_switch_on_click_including_the_gap_after_a_name() {
    let ctx = egui::Context::default();
    let mut tab = settings::SettingsTab::Appearance;
    let mut frame = |events| {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(500., 80.))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| settings::tab_bar(ui, &mut tab));
            },
        );
        tab
    };
    let rect = |tab| {
        ctx.read_response(egui::Id::new(("settings-tab", tab)))
            .unwrap()
            .rect
    };
    frame(vec![]);
    for (target, pos) in [
        (
            settings::SettingsTab::Library,
            rect(settings::SettingsTab::Library).center(),
        ),
        (
            settings::SettingsTab::Terminal,
            rect(settings::SettingsTab::Terminal).right_center() - Vec2::new(2., 0.),
        ),
    ] {
        for pressed in [true, false] {
            frame(vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ]);
        }
        assert_eq!(frame(vec![]), target);
    }
}

#[test]
fn library_rows_never_draw_over_the_table_header() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let order: Vec<usize> = (0..app.connections.len()).collect();
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(900., 600.))),
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.connection_table(ui, &order));
        },
    );
    let line = theme::palette(&ctx).line;
    let header = output
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::Shape::LineSegment { points, stroke }
                if stroke.color == line && points[0].y == points[1].y =>
            {
                Some(points[0].y)
            }
            _ => None,
        })
        .expect("the header's line");
    // Everything below the header is clipped at its line, not a few points above it.
    for clipped in &output.shapes {
        let clip = clipped.clip_rect;
        if clip.min.y > 0. {
            assert!(
                clip.min.y >= header,
                "clipped at {} above {header}",
                clip.min.y
            );
        }
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
fn admin_sessions_stay_with_their_tab() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let mut c = app.connections[0].clone();
    c.protocol = Protocol::Rdp;
    app.request_connect_admin(c.clone());
    assert!(app.connect_admin);
    // A plain Connect afterwards is not an admin session.
    app.request_connect(c.clone());
    assert!(!app.connect_admin);

    let mut admin = session::Session::restored(c.clone(), false, ctx.clone());
    admin.admin = true;
    assert_eq!(admin.title(), format!("{} (admin)", c.name));
    app.sessions
        .push(session::Session::restored(c.clone(), false, ctx.clone()));
    app.sessions.push(admin);
    app.remember_tabs();
    assert_eq!(app.state.admin_tabs, [1]);
    // The saved connection is unchanged, so editing it from the tab cannot store admin.
    assert_eq!(app.state.open[1], c);
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
    let kinds: Vec<_> = restored.sessions.iter().map(|s| s.admin).collect();
    assert_eq!(kinds, [false, true]);
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
    assert!(app.delete_connections(&[3]));
    assert!(app.query.is_empty(), "the search would offer quick connect");
    assert_eq!(app.selected, None);
    // Other matches keep the search.
    let mut app = test_app(&ctx);
    app.query = "a".into();
    assert!(app.delete_connections(&[1]));
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

#[test]
fn number_shortcuts_switch_tabs_from_the_library() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    for c in app.connections.clone() {
        app.sessions
            .push(session::Session::restored(c, false, ctx.clone()));
    }
    let frame = |app: &mut App, key| {
        let _ = ctx.run(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND,
                }],
                modifiers: egui::Modifiers::COMMAND,
                ..Default::default()
            },
            |ctx| {
                app.workspace_shortcuts(ctx);
            },
        );
    };
    frame(&mut app, egui::Key::Num2);
    assert_eq!(app.active, Some(0));
    frame(&mut app, egui::Key::Num1);
    assert_eq!(app.active, None, "Ctrl+1 is the library");
    frame(&mut app, egui::Key::Num3);
    assert_eq!(app.active, Some(1));
    frame(&mut app, egui::Key::Num9);
    assert_eq!(app.active, Some(2), "Ctrl+9 is the last tab");
}

#[test]
fn connected_tabs_offer_reconnect_from_their_menu() {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut app = test_app(&ctx);
    let mut c = app.connections[0].clone();
    c.protocol = Protocol::Ssh;
    let browser = crate::sftp::Browser::demo("/srv", vec![]);
    app.sessions
        .push(session::Session::demo_files(c, browser, ctx.clone()));
    let c = app.connections[1].clone();
    app.sessions
        .push(session::Session::restored(c, false, ctx.clone()));
    let frame = |app: &mut App, events| {
        let output = ctx.run(
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
        let update = output.platform_output.accesskit_update.unwrap();
        (update.nodes.iter()).any(|(_, node)| node.label() == Some("Reconnect"))
    };
    let tab_center = |app: &mut App, name: &str| {
        let output = ctx.run(Default::default(), |ctx| app.tabs(ctx));
        let update = output.platform_output.accesskit_update.unwrap();
        let (_, node) = (update.nodes.iter())
            .find(|(_, node)| node.label() == Some(name))
            .unwrap();
        let b = node.bounds().unwrap();
        egui::pos2(((b.x0 + b.x1) / 2.) as f32, ((b.y0 + b.y1) / 2.) as f32)
    };
    // A right click opens the tab's menu, which offers reconnecting only after connecting.
    let offered = |app: &mut App, name: &str| {
        let pos = tab_center(app, name);
        let button = |pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(app, vec![egui::Event::PointerMoved(pos)]);
        frame(app, vec![button(true)]);
        frame(app, vec![button(false)]);
        let shown = frame(app, vec![]);
        frame(app, vec![press(egui::Key::Escape)]);
        shown
    };
    assert!(offered(&mut app, "Alpha"));
    assert!(!offered(&mut app, "Bravo"));
}

#[test]
fn saving_an_edit_keeps_changes_made_meanwhile() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let save = |app: &mut App| {
        let _ = ctx.run(
            egui::RawInput {
                events: vec![press(egui::Key::Enter)],
                ..Default::default()
            },
            |ctx| app.editor(ctx),
        );
        while app.pending.is_some() {
            std::thread::sleep(std::time::Duration::from_millis(10));
            app.poll_storage();
        }
    };
    fn draft(app: &mut App) -> &mut Connection {
        match &mut app.modal {
            Modal::Editor(c) => c,
            _ => panic!("the editor closed"),
        }
    }
    // A teammate changes the host while the notes are edited: both changes are kept.
    app.edit(app.connections[0].clone());
    draft(&mut app).notes = "Rack 4".into();
    app.connections[0].host = "moved.example".into();
    save(&mut app);
    assert!(matches!(app.modal, Modal::None));
    assert_eq!(
        (
            app.connections[0].host.as_str(),
            app.connections[0].notes.as_str()
        ),
        ("moved.example", "Rack 4")
    );
    // Both change the host: the editor says so, and saving again keeps this edit.
    app.edit(app.connections[0].clone());
    draft(&mut app).host = "mine.example".into();
    app.connections[0].host = "theirs.example".into();
    app.connections[0].port = 3390;
    save(&mut app);
    assert!(app.editor_error.contains("changed the host"));
    assert_eq!(app.connections[0].host, "theirs.example");
    assert_eq!(
        draft(&mut app).port,
        3390,
        "the editor shows their other changes"
    );
    save(&mut app);
    assert!(matches!(app.modal, Modal::None));
    assert_eq!(
        (app.connections[0].host.as_str(), app.connections[0].port),
        ("mine.example", 3390)
    );
    // A teammate deletes it: saving again adds it back as a new connection.
    app.edit(app.connections[1].clone());
    draft(&mut app).notes = "Keep".into();
    app.connections.remove(1);
    save(&mut app);
    assert!(app.editor_error.contains("deleted"));
    assert_eq!(app.connections.len(), 2);
    save(&mut app);
    assert!(matches!(app.modal, Modal::None));
    assert!(
        app.connections
            .iter()
            .any(|c| c.name == "Bravo" && c.notes == "Keep")
    );
}

/// Waits for the library operation started last, as a save, to finish.
fn finish_saving(app: &mut App) {
    while app.pending.is_some() {
        std::thread::sleep(std::time::Duration::from_millis(10));
        app.poll_storage();
    }
}

#[test]
fn several_connections_are_chosen_and_changed_together() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let shown = [1, 2, 3];
    let ctrl = egui::Modifiers::COMMAND;
    let marked = |app: &App| app.marked.iter().copied().collect::<Vec<_>>();
    // Ctrl+click adds the row the keyboard was on and the clicked one, and takes one out.
    app.click_row(1, egui::Modifiers::NONE, &shown);
    app.click_row(3, ctrl, &shown);
    assert_eq!(marked(&app), [1, 3]);
    app.click_row(3, ctrl, &shown);
    assert_eq!(marked(&app), [1]);
    // Shift+click chooses the range from the selected row; a plain click starts over.
    app.click_row(1, egui::Modifiers::NONE, &shown);
    app.click_row(3, egui::Modifiers::SHIFT, &shown);
    assert_eq!(marked(&app), [1, 2, 3]);
    assert_eq!(app.selected, Some(1), "the range starts where it was");
    app.click_row(2, egui::Modifiers::NONE, &shown);
    assert!(app.marked.is_empty());

    // Ctrl+A chooses everything shown, a search narrows it, and Esc clears it.
    library_frame(&mut app, &ctx, vec![key(egui::Key::A, ctrl)]);
    assert_eq!(marked(&app), [1, 2, 3]);
    app.query = "ha".into();
    library_frame(&mut app, &ctx, vec![]);
    assert_eq!(marked(&app), [1, 3], "Bravo is hidden");
    library_frame(&mut app, &ctx, vec![press(egui::Key::Escape)]);
    assert!(app.marked.is_empty());
    app.query.clear();

    // Favorites, groups and deletion apply to all of them in one save.
    app.marked = [1, 3].into();
    app.favorite_marked(&shown);
    finish_saving(&mut app);
    let favorites: Vec<_> = app.connections.iter().map(|c| c.favorite).collect();
    assert_eq!(favorites, [true, false, true]);
    app.favorite_marked(&shown);
    finish_saving(&mut app);
    assert!(
        app.connections.iter().all(|c| !c.favorite),
        "all were favorites"
    );
    app.modal = Modal::MoveToGroup(vec![1, 3], "Datacenter".into());
    let _ = ctx.run(
        egui::RawInput {
            events: vec![press(egui::Key::Enter)],
            ..Default::default()
        },
        |ctx| app.confirmations(ctx),
    );
    finish_saving(&mut app);
    assert!(matches!(app.modal, Modal::None));
    let groups: Vec<_> = app.connections.iter().map(|c| c.group.as_str()).collect();
    assert_eq!(groups, ["Datacenter", "Personal", "Datacenter"]);
    assert!(app.delete_connections(&[1, 3]));
    finish_saving(&mut app);
    let names: Vec<_> = app.connections.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Bravo"]);
    assert!(app.marked.is_empty());

    // Enter with several chosen opens a tab for each.
    let mut app = test_app(&ctx);
    app.marked = [2, 3].into();
    app.connect_marked(&shown);
    let opening: Vec<_> = app.connect_many.iter().map(|c| c.id).collect();
    assert_eq!(opening, [2, 3]);
}

#[test]
fn local_shells_are_remembered_as_local_shells() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let mut c = app.connections[0].clone();
    c.protocol = Protocol::Ssh;
    app.sessions
        .push(session::Session::restored(c, false, ctx.clone()));
    app.sessions
        .push(session::Session::restored_local(ctx.clone()));
    app.remember_tabs();
    assert_eq!(app.state.local_tabs, [1]);
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
    let local: Vec<_> = restored.sessions.iter().map(|s| s.local).collect();
    assert_eq!(local, [false, true]);
    assert_eq!(
        restored.sessions[1].tab_state(),
        session::TabState::Waiting,
        "it starts when first shown, like other tabs"
    );
}

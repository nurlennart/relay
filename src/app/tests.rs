use super::{
    library::{Column, GAP, table_columns},
    settings::theme_option,
    tabs::{Kind, Look, Pair, Tab, drop_index, session_tab},
    *,
};
use egui::{Rect, Vec2};
/// An app started from `state`, with the tabs it saved reopened.
fn reopened(ctx: &egui::Context, state: state::State) -> App {
    let mut app = App::with(
        ctx.clone(),
        PathBuf::from("unused.json"),
        Ok(database::Snapshot {
            connections: vec![],
            revision: 0,
        }),
        None,
        None,
        None,
        state,
    );
    app.restore_tabs();
    app
}

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
    let path = std::env::temp_dir().join(model::temp_name("relay-test.json"));
    App::with(
        ctx.clone(),
        path,
        Ok(database::Snapshot {
            connections,
            revision: 0,
        }),
        None,
        None,
        None,
        state::State::default(),
    )
}

#[test]
fn large_import_preview_work_is_bounded() {
    let ctx = egui::Context::default();
    apply_style(&ctx);
    let mut app = test_app(&ctx);
    let path = std::env::temp_dir().join(model::temp_name("relay-import.csv"));
    let mut csv = String::from("Name,Host,Protocol\n");
    for i in 0..10_000 {
        csv.push_str(&format!("Machine {i},node-{i}.example.com,SSH\n"));
    }
    std::fs::write(&path, csv).unwrap();
    let started = Instant::now();
    let read_path = path.clone();
    app.start_import(move |existing| csv_io::read(&read_path, existing));
    let scheduled = started.elapsed();
    let deadline = Instant::now() + Duration::from_secs(10);
    while matches!(app.modal, Modal::Importing(_)) {
        assert!(Instant::now() < deadline, "import did not finish");
        app.poll_import();
        std::thread::yield_now();
    }
    let read = started.elapsed();
    std::fs::remove_file(&path).unwrap();
    assert!(matches!(&app.modal, Modal::Import(import) if import.connections.len() == 10_000));
    let started = Instant::now();
    let output = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1280., 800.),
            )),
            ..Default::default()
        },
        |ctx| app.library_dialogs(ctx),
    );
    eprintln!(
        "10,000 connections: UI scheduling {scheduled:?}; background read/parse {read:?}; preview frame {:?}; {} shapes",
        started.elapsed(),
        output.shapes.len()
    );
    assert!(output.shapes.len() < 500, "preview drew off-screen rows");
    assert_eq!(app.connections.len(), 3, "review never commits the import");
}

#[test]
fn import_review_aligns_columns_and_keeps_errors_and_actions_visible() {
    for theme in theme::Theme::ALL {
        for (viewport, zoom) in [
            (Vec2::new(1280., 800.), 1.),
            (Vec2::new(720., 560.), 1.),
            (Vec2::new(1200., 900.), 2.),
        ] {
            for count in [1, 2] {
                let ctx = egui::Context::default();
                ctx.enable_accesskit();
                apply_style(&ctx);
                theme.apply(&ctx);
                ctx.options_mut(|options| options.zoom_factor = zoom);
                let mut app = test_app(&ctx);
                let mut connections = app.connections[..count].to_vec();
                connections[0].name = "A very long connection name ".repeat(12);
                connections[0].group = "Office".into();
                if count == 2 {
                    connections[1].group = "Infrastructure".into();
                }
                app.modal = Modal::Import(csv_io::Import {
                    connections,
                    skipped: vec![],
                    duplicates: 1,
                    filename: format!("{}.csv", "long-file-name-".repeat(30)),
                });
                let error = "The library changed. Refresh before importing again.";
                app.error = Some(error.into());
                let screen = Rect::from_min_size(egui::Pos2::ZERO, viewport / zoom);
                let mut output = egui::FullOutput::default();
                for _ in 0..3 {
                    output = ctx.run(
                        egui::RawInput {
                            screen_rect: Some(screen),
                            ..Default::default()
                        },
                        |ctx| app.library_dialogs(ctx),
                    );
                }
                let modal = ctx
                    .memory(|m| m.area_rect(egui::Id::new("import")))
                    .unwrap();
                assert!(
                    screen.contains_rect(modal),
                    "{theme:?} at {screen:?}: {modal:?}"
                );
                let action = if count == 1 {
                    "Import 1 connection"
                } else {
                    "Import 2 connections"
                };
                for label in ["Review import", "1 duplicate", error, action, "Cancel"] {
                    assert!(
                        output.shapes.iter().any(|shape| matches!(&shape.shape,
                            egui::Shape::Text(text) if text.galley.text() == label
                            && shape.clip_rect.contains_rect(text.visual_bounding_rect())
                            && screen.contains_rect(text.visual_bounding_rect())
                        )),
                        "{theme:?} at {screen:?}: {label} clipped or missing"
                    );
                }
                if count == 2 {
                    let left = |label| {
                        output
                            .shapes
                            .iter()
                            .find_map(|shape| match &shape.shape {
                                egui::Shape::Text(text) if text.galley.text() == label => {
                                    Some(text.pos.x)
                                }
                                _ => None,
                            })
                            .unwrap()
                    };
                    assert!((left("Office") - left("Infrastructure")).abs() < 1.);
                }
                assert!(app.pending.is_none());
            }
        }
    }
}

#[test]
fn a_slow_import_renders_and_cancel_discards_its_late_result() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (finished_tx, finished_rx) = std::sync::mpsc::channel();
    app.start_import(move |existing| {
        entered_tx.send(std::thread::current().id()).unwrap();
        release_rx.recv().unwrap();
        let result = csv_io::read(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(csv_io::FIXTURE),
            existing,
        );
        finished_tx.send(()).unwrap();
        result
    });
    assert_ne!(
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        std::thread::current().id()
    );
    app.poll_import();
    assert!(matches!(app.modal, Modal::Importing(_)));
    let output = ctx.run(egui::RawInput::default(), |ctx| app.library_dialogs(ctx));
    assert!(
        !output.shapes.is_empty(),
        "UI paints while the read is blocked"
    );
    let _ = ctx.run(
        egui::RawInput {
            events: vec![press(egui::Key::Escape)],
            ..Default::default()
        },
        |ctx| app.library_dialogs(ctx),
    );
    assert!(matches!(app.modal, Modal::None));
    app.start_import(|_| Err("new import error".into()));
    release_tx.send(()).unwrap();
    finished_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while matches!(app.modal, Modal::Importing(_)) {
        assert!(Instant::now() < deadline);
        app.poll_import();
        std::thread::yield_now();
    }
    assert!(matches!(app.modal, Modal::None));
    assert_eq!(app.error.as_deref(), Some("new import error"));
    assert_eq!(app.connections.len(), 3);
    assert!(app.pending.is_none(), "no import was saved");
}

#[test]
fn import_errors_and_worker_panics_leave_the_library_unchanged() {
    for panics in [false, true] {
        let ctx = egui::Context::default();
        let mut app = test_app(&ctx);
        app.start_import(move |_| {
            if panics {
                panic!("import reader failed");
            }
            Err("Cannot read the selected file".into())
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while matches!(app.modal, Modal::Importing(_)) {
            assert!(Instant::now() < deadline);
            app.poll_import();
            std::thread::yield_now();
        }
        assert!(matches!(app.modal, Modal::None));
        assert!(app.error.as_deref().unwrap().contains(if panics {
            "worker stopped"
        } else {
            "Cannot read"
        }));
        assert_eq!(app.connections.len(), 3);
        assert!(app.pending.is_none() && app.recovery.is_none());
    }
}

#[test]
fn an_existing_personal_library_can_save_after_startup() {
    let ctx = egui::Context::default();
    let original = test_app(&ctx);
    let path = original.path.clone();
    model::save(&path, &original.connections).unwrap();
    let mut app = App::with(
        ctx,
        path.clone(),
        database::load_personal(&path),
        None,
        None,
        None,
        state::State::default(),
    );
    let mut next = app.connections.clone();
    next[0].notes = "Saved after restarting".into();
    assert!(app.persist(next));
    finish_saving(&mut app);
    assert!(app.error.is_none(), "{:?}", app.error);
    assert_eq!(
        model::load(&path).unwrap()[0].notes,
        "Saved after restarting"
    );
    assert!(!app.offers_refresh(), "nothing else writes it so far");

    // Another Relay window saves first: this one's save is refused and offers Refresh,
    // which loads that save and keeps the refused changes for export.
    let mut theirs = model::load(&path).unwrap();
    theirs[0].notes = "Saved by another window".into();
    model::save(&path, &theirs).unwrap();
    let mut mine = app.connections.clone();
    mine[1].notes = "Mine".into();
    assert!(app.persist(mine));
    finish_saving(&mut app);
    assert!(
        app.error
            .as_deref()
            .unwrap()
            .contains("changed this library")
    );
    assert!(app.offers_refresh());
    app.refresh();
    finish_saving(&mut app);
    assert_eq!(app.connections[0].notes, "Saved by another window");
    assert!(app.error.is_none(), "{:?}", app.error);
    assert_eq!(app.recovery.as_ref().unwrap()[1].notes, "Mine");
    // Saving again from the refreshed library goes through.
    let mut again = app.connections.clone();
    again[1].notes = "Mine, again".into();
    assert!(app.persist(again));
    finish_saving(&mut app);
    assert!(app.error.is_none(), "{:?}", app.error);
    assert!(!app.offers_refresh());
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_file_name(format!(
        ".{}.lock",
        path.file_name().unwrap().to_string_lossy()
    )));
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
    // The saved notes are searched, and an unchanged library is not searched again.
    app.query = "ups".into();
    assert_eq!(*app.filtered(), [0]);
    assert!(std::rc::Rc::ptr_eq(&app.filtered(), &app.filtered()));
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
    // Or with the protocol shown, on its usual port.
    app.group = None;
    app.filter = Filter::Vnc;
    app.new_connection();
    assert!(
        matches!(&app.modal, Modal::Editor(c) if c.protocol == Protocol::Vnc && c.port == 5900)
    );
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
    app.activate_session(0);
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
    let mut app = reopened(&ctx, state);
    assert_eq!(app.sessions.len(), 2);
    assert_eq!(app.active(), Some(1));
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
    assert_eq!(app.active(), None);
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
    assert!(app.saved_connection(0).is_none());
    assert_eq!(app.saved_connection(1).unwrap().host, "alpha.example");

    // Once Alpha is deleted, a new connection that reuses its ID is not its tab's.
    app.connections.remove(0);
    app.forget_removed();
    assert_eq!(app.sessions[1].connection.id, 0);
    assert!(app.saved_connection(1).is_none());
    assert_eq!(
        app.sessions[0].connection.id, 2,
        "other libraries are left alone"
    );
    assert_eq!(app.state.tab_libraries, [team, state::Library::Personal]);
}

fn session_pane_frame(
    app: &mut App,
    ctx: &egui::Context,
    pane: Pane,
    size: Vec2,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                app.session_pane(ui, 0, pane);
            });
        },
    )
}

#[test]
fn session_panes_keep_remote_identity_visible_and_loading_guidance_relevant() {
    for protocol in Protocol::ALL {
        for pane in [Pane::Main, Pane::Left, Pane::Beside, Pane::Window] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            let mut app = test_app(&ctx);
            let mut c = app.connections[0].clone();
            c.protocol = protocol;
            c.port = protocol.port();
            c.username = "deploy".into();
            if protocol == Protocol::Vnc {
                c.host = format!("{}.example", "long-host-".repeat(20));
            }
            let identity = format!("deploy@{}:{}", c.host, protocol.port());
            let mut session = session::Session::restored(c, false, ctx.clone());
            session.started = Instant::now() - Duration::from_secs(21);
            app.sessions.push(session);
            for width in [320., 1100.] {
                let output =
                    session_pane_frame(&mut app, &ctx, pane, Vec2::new(width, 600.), vec![]);
                let update = output.platform_output.accesskit_update.unwrap();
                let shown = |text: &str| update.nodes.iter().any(|(_, n)| n.value() == Some(text));
                if pane == Pane::Main {
                    assert!(!shown(&identity), "main pane added a separate identity row");
                } else {
                    let (_, identity_node) = update
                        .nodes
                        .iter()
                        .find(|(_, n)| {
                            n.role() == egui::accesskit::Role::Label && n.value() == Some(&identity)
                        })
                        .unwrap();
                    let bounds = identity_node.bounds().unwrap();
                    assert!(
                        bounds.x0 >= 0. && bounds.x1 <= f64::from(width),
                        "remote identity overflowed"
                    );
                }
                assert!(shown(
                    "Still connecting. Check that the host is reachable, or close this tab to cancel."
                ));
                assert!(!shown("Waiting for the Windows RDP control to initialize."));
                match protocol {
                    Protocol::Rdp => assert!(shown(rdp::CONNECTING_HELP)),
                    Protocol::Vnc => {
                        assert!(shown(
                            "Enter a password in this tab if the server asks for one."
                        ));
                        assert!(!shown(rdp::CONNECTING_HELP));
                    }
                    Protocol::Ssh => assert!(shown(
                        "Starting SSH with your configured client, keys and agent."
                    )),
                }
            }
        }
    }
}

#[test]
fn single_session_identity_uses_the_existing_tab_bar() {
    for width in [400., 760., 1100.] {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = test_app(&ctx);
        app.connections[0].username = "deploy".into();
        app.sessions.push(session::Session::restored(
            app.connections[0].clone(),
            false,
            ctx.clone(),
        ));
        app.sync_tabs();
        app.select_tab(1);
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(width, 600.),
                )),
                ..Default::default()
            },
            |ctx| app.tabs(ctx),
        );
        let update = output.platform_output.accesskit_update.unwrap();
        let identity = update.nodes.iter().find(|(_, n)| {
            n.role() == egui::accesskit::Role::Label
                && n.value() == Some("deploy@alpha.example:3389")
        });
        if width >= 760. {
            let bounds = identity.unwrap().1.bounds().unwrap();
            assert!(bounds.y0 >= 0. && bounds.y1 <= f64::from(BAR_HEIGHT));
            assert!(bounds.x0 >= 0. && bounds.x1 <= f64::from(width));
        } else {
            assert!(
                identity.is_none(),
                "narrow tab bar should preserve tab space"
            );
        }
    }
}

#[test]
fn connection_recovery_edits_current_settings_only_in_the_sessions_own_library() {
    for mode in ["desktop", "files-restored", "files-ended"] {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = test_app(&ctx);
        let mut c = app.connections[0].clone();
        c.protocol = if mode == "desktop" {
            Protocol::Rdp
        } else {
            Protocol::Ssh
        };
        let mut session = session::Session::restored(c, mode != "desktop", ctx.clone());
        session.error = Some("The server refused the connection. ".repeat(100));
        if mode == "files-ended" {
            let mut browser = crate::sftp::Browser::demo("/srv", vec![]);
            browser.status = crate::sftp::Status::Ended("The connection was closed.".into());
            session.files = Some(browser);
        }
        app.sessions.push(session);
        app.connections[0].host = "updated.example".into();
        let size = Vec2::new(400., 300.);
        let _ = session_pane_frame(&mut app, &ctx, Pane::Main, size, vec![]);
        let output = session_pane_frame(&mut app, &ctx, Pane::Main, size, vec![]);
        let update = output.platform_output.accesskit_update.unwrap();
        let mut edit_at = egui::Pos2::ZERO;
        let mut reconnect_left = 0.;
        for label in ["Reconnect", "Edit connection"] {
            let (_, node) = update
                .nodes
                .iter()
                .find(|(_, n)| n.label() == Some(label))
                .unwrap();
            let bounds = node.bounds().unwrap();
            assert!(
                bounds.x0 >= 0. && bounds.y0 >= 0. && bounds.x1 <= 400. && bounds.y1 <= 300.,
                "{mode}: {label} outside pane: {bounds:?}"
            );
            if label == "Edit connection" {
                assert!(
                    (reconnect_left + bounds.x1 - f64::from(size.x)).abs() <= 1.,
                    "{mode}: recovery buttons should be centered as a group"
                );
                edit_at = egui::pos2(
                    ((bounds.x0 + bounds.x1) / 2.) as f32,
                    ((bounds.y0 + bounds.y1) / 2.) as f32,
                );
            } else {
                reconnect_left = bounds.x0;
            }
        }
        let _ = session_pane_frame(
            &mut app,
            &ctx,
            Pane::Main,
            size,
            vec![egui::Event::PointerMoved(edit_at)],
        );
        for pressed in [true, false] {
            let _ = session_pane_frame(
                &mut app,
                &ctx,
                Pane::Main,
                size,
                vec![egui::Event::PointerButton {
                    pos: edit_at,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
        assert!(
            matches!(&app.modal, Modal::Editor(c) if c.host == "updated.example"),
            "{mode}: wrong connection edited"
        );
        app.modal = Modal::None;
        app.sessions[0].library = state::Library::Sqlite(PathBuf::from("other.sqlite"));
        let output = session_pane_frame(&mut app, &ctx, Pane::Main, size, vec![]);
        let update = output.platform_output.accesskit_update.unwrap();
        assert!(
            !update
                .nodes
                .iter()
                .any(|(_, n)| n.label() == Some("Edit connection"))
        );
        assert_eq!(app.current_settings(0).host, "alpha.example");
    }
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
fn closing_the_tab_in_front_shows_another_tab_rather_than_the_library() {
    let ctx = egui::Context::default();
    let (mut app, _) = app_with_tabs(&ctx);
    let name = |app: &App| {
        app.active()
            .map(|i| app.sessions[i].connection.name.clone())
    };
    // From Alpha, a new connection opens beside it; closing that returns to Alpha.
    app.activate_session(0);
    let mut delta = app.connections[1].clone();
    delta.name = "Delta".into();
    app.add_session(session::Session::restored(delta, false, ctx.clone()));
    app.close_tab(3);
    assert_eq!(
        name(&app).as_deref(),
        Some("Alpha"),
        "back to where it was opened"
    );
    // With the tab it came from gone, the left neighbour.
    app.activate_session(2);
    app.close_tab(0);
    app.close_tab(1);
    assert_eq!(name(&app).as_deref(), Some("Bravo"), "the tab to the left");
    // In a pair, the other side takes the front, as a tab of its own.
    let mut charlie = app.connections[2].clone();
    charlie.name = "Charlie".into();
    app.add_session(session::Session::restored(charlie, false, ctx.clone()));
    app.open_beside(0);
    app.close_tab(1);
    assert_eq!(name(&app).as_deref(), Some("Bravo"));
    assert_eq!(app.tabs, [Tab::Single(app.sessions[0].key)]);
    // The first tab closing shows the one now first, and the last the library.
    let mut echo = app.connections[0].clone();
    echo.name = "Echo".into();
    app.add_session(session::Session::restored(echo, false, ctx.clone()));
    app.activate_session(0);
    app.close_tab(0);
    assert_eq!(name(&app).as_deref(), Some("Echo"));
    app.close_tab(0);
    assert_eq!(app.active(), None, "no tabs left");
}

/// An app with a tab for each test connection, Alpha, Bravo and Charlie, and their keys.
fn app_with_tabs(ctx: &egui::Context) -> (App, [u64; 3]) {
    let mut app = test_app(ctx);
    for c in app.connections.clone() {
        app.sessions
            .push(session::Session::restored(c, false, ctx.clone()));
    }
    app.sync_tabs();
    let keys = [0, 1, 2].map(|i| app.sessions[i].key);
    (app, keys)
}

fn pair(left: u64, right: u64, focus: u64) -> Tab {
    Tab::Pair(Pair {
        left,
        right,
        fraction: SPLIT_MIDDLE,
        focus,
    })
}

#[test]
fn split_view_joins_two_tabs_into_one() {
    let ctx = egui::Context::default();
    let (mut app, [a, b, c]) = app_with_tabs(&ctx);
    app.activate_session(0);
    app.toggle_split();
    assert_eq!(
        app.tabs,
        [pair(a, b, a), Tab::Single(c)],
        "the next tab joins"
    );
    app.toggle_split();
    assert_eq!(app.tabs, [Tab::Single(a), Tab::Single(b), Tab::Single(c)]);
    app.open_beside(0);
    assert_eq!(app.tabs.len(), 3, "a tab cannot sit beside itself");
    // The pair takes the place of the tab in front, which stays in front.
    app.activate_session(2);
    app.open_beside(0);
    assert_eq!(app.tabs, [Tab::Single(b), pair(c, a, c)]);
    assert_eq!(app.active(), Some(2));
    // A session already in a pair joins no other.
    app.activate_session(1);
    app.open_beside(0);
    assert_eq!(app.tabs, [Tab::Single(b), pair(c, a, c)]);
    // The last tab joins the one before it, on its left.
    let (mut app, [a, b, c]) = app_with_tabs(&ctx);
    app.activate_session(2);
    app.toggle_split();
    assert_eq!(app.tabs, [Tab::Single(a), pair(b, c, c)]);
    // Alone, there is nothing to split with.
    let (mut app, [a, ..]) = app_with_tabs(&ctx);
    app.close_tab(2);
    app.close_tab(1);
    app.activate_session(0);
    app.toggle_split();
    assert_eq!(app.tabs, [Tab::Single(a)]);
    assert!(app.notice.is_some());
}

#[test]
fn pairs_swap_unsplit_and_move_as_one() {
    let ctx = egui::Context::default();
    let (mut app, [a, b, c]) = app_with_tabs(&ctx);
    app.activate_session(1);
    app.open_beside(2);
    app.tabs[1] = Tab::Pair(Pair {
        fraction: 0.3,
        ..match app.tabs[1] {
            Tab::Pair(p) => p,
            Tab::Single(_) => unreachable!(),
        }
    });
    app.swap_sides(1);
    assert_eq!(
        app.tabs[1],
        Tab::Pair(Pair {
            left: c,
            right: b,
            fraction: 0.7,
            focus: b,
        }),
        "each side keeps its width and the keyboard stays"
    );
    app.move_tab(1, 0);
    assert_eq!(app.tabs[1], Tab::Single(a));
    app.unsplit(0);
    assert_eq!(app.tabs, [Tab::Single(c), Tab::Single(b), Tab::Single(a)]);
    assert_eq!(app.active(), Some(1));
}

#[test]
fn a_pair_counts_as_one_tab_and_returns_to_its_last_side() {
    let ctx = egui::Context::default();
    let (mut app, [a, b, c]) = app_with_tabs(&ctx);
    app.activate_session(0);
    app.open_beside(1);
    // The right side takes the keyboard, then another tab is shown.
    app.activate_session(1);
    app.select_tab(2);
    assert_eq!(app.active(), Some(2), "Charlie is the second tab");
    assert_eq!(app.tabs, [pair(a, b, b), Tab::Single(c)]);
    app.select_tab(1);
    assert_eq!(app.active(), Some(1), "back to the side used last");
    app.select_tab(3);
    assert_eq!(app.active(), None, "past the last tab");
    app.select_tab(0);
    assert_eq!(app.active(), None, "the library");
}

#[test]
fn closing_or_detaching_a_side_leaves_the_other_as_a_tab() {
    let ctx = egui::Context::default();
    let (mut app, [a, b, c]) = app_with_tabs(&ctx);
    app.activate_session(1);
    app.open_beside(2);
    app.detach(1);
    assert_eq!(app.tabs, [Tab::Single(a), Tab::Single(b), Tab::Single(c)]);
    assert_eq!(app.active(), Some(2), "the other side stays in front");
    app.attach(b);
    app.activate_session(0);
    app.open_beside(1);
    app.close_tab(0);
    assert_eq!(app.tabs, [Tab::Single(b), Tab::Single(c)]);
    assert_eq!(app.active(), Some(0));
    // Whatever the change to the sessions, every one has exactly one tab.
    app.sessions.push(session::Session::restored(
        app.connections[0].clone(),
        false,
        ctx.clone(),
    ));
    app.sync_tabs();
    let mut keys: Vec<u64> = app.tabs.iter().flat_map(Tab::keys).collect();
    keys.sort();
    let mut sessions: Vec<u64> = app.sessions.iter().map(|s| s.key).collect();
    sessions.sort();
    assert_eq!(keys, sessions);
}

#[test]
fn pairs_come_back_with_their_order_width_and_side() {
    let ctx = egui::Context::default();
    let (mut app, [_, b, c]) = app_with_tabs(&ctx);
    app.activate_session(2);
    app.open_beside(1);
    app.swap_sides(1);
    if let Tab::Pair(pair) = &mut app.tabs[1] {
        pair.fraction = 0.4;
    }
    assert_eq!(
        app.tabs[1],
        Tab::Pair(Pair {
            left: b,
            right: c,
            fraction: 0.4,
            focus: c
        })
    );
    app.record_tabs();
    let names: Vec<_> = app.state.open.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Alpha", "Bravo", "Charlie"]);
    assert_eq!(
        app.state.pairs,
        [state::SavedPair {
            left: 1,
            right: 2,
            fraction: 0.4,
            right_focused: true,
        }]
    );
    let restored = reopened(&ctx, app.state.clone());
    let keys: Vec<u64> = restored.sessions.iter().map(|s| s.key).collect();
    assert_eq!(
        restored.tabs,
        [
            Tab::Single(keys[0]),
            Tab::Pair(Pair {
                left: keys[1],
                right: keys[2],
                fraction: 0.4,
                focus: keys[2],
            })
        ]
    );
    assert_eq!(restored.active(), Some(2));
    // Pairs that do not fit the saved tabs are dropped, and state from before pairs has none.
    let mut broken = app.state.clone();
    broken.pairs = vec![
        state::SavedPair {
            left: 0,
            right: 7,
            ..Default::default()
        },
        state::SavedPair {
            left: 1,
            right: 1,
            ..Default::default()
        },
    ];
    assert!(
        reopened(&ctx, broken)
            .tabs
            .iter()
            .all(|t| matches!(t, Tab::Single(_)))
    );
    let old: state::State = serde_json::from_str(r#"{"open": [], "active_tab": null}"#).unwrap();
    assert!(old.pairs.is_empty());
    let saved: state::SavedPair = serde_json::from_str(r#"{"left": 0, "right": 1}"#).unwrap();
    assert_eq!(saved.fraction, 0.5);
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
fn dragged_tabs_drop_between_the_others() {
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
fn recent_cleanup_saves_only_changes_in_the_loaded_library() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let path = std::env::temp_dir().join(model::temp_name("relay-recent-state.json"));
    app.state_path = Some(path.clone());
    let team = database::Location::Sqlite(PathBuf::from("team.sqlite"));
    for id in [1, 99, 3] {
        app.state.note_recent(&app.location, id);
    }
    app.state.note_recent(&team, 99);
    app.forget_removed();
    assert_eq!(app.state.recent(&app.location), [3, 1]);
    assert_eq!(app.state.recent(&team), [99]);
    assert_eq!(state::load(&path), app.state);

    std::fs::remove_file(&path).unwrap();
    app.forget_removed();
    assert!(!path.exists(), "unchanged history is not saved again");

    app.set_connections(vec![]);
    app.forget_removed();
    assert!(app.state.recent(&app.location).is_empty());
    assert_eq!(state::load(&path), app.state);
    std::fs::remove_file(&path).unwrap();
}

#[test]
fn failed_initial_load_preserves_recent_history() {
    let ctx = egui::Context::default();
    let original = test_app(&ctx);
    let mut saved = state::State::default();
    saved.note_recent(&original.location, 99);
    let mut app = App::with(
        ctx,
        original.path,
        Err("Cannot read the library".into()),
        None,
        None,
        None,
        saved.clone(),
    );
    app.forget_removed();
    assert_eq!(app.state, saved);
}

#[test]
fn recent_history_is_pruned_only_after_successful_library_operations() {
    let ctx = egui::Context::default();
    for operation in ["save", "refresh", "sync"] {
        for success in [false, true] {
            let mut app = test_app(&ctx);
            let path = std::env::temp_dir().join(model::temp_name("relay-recent-state.json"));
            app.state_path = Some(path.clone());
            app.location = database::Location::Sqlite(PathBuf::from("team.sqlite"));
            for id in [1, 2, 3] {
                app.state.note_recent(&app.location, id);
            }
            app.state.save(&path).unwrap();
            let result = if success {
                Ok(database::Snapshot {
                    connections: app
                        .connections
                        .iter()
                        .filter(|c| c.id != 2)
                        .cloned()
                        .collect(),
                    revision: app.revision + 1,
                })
            } else {
                Err("The library operation failed".into())
            };
            if operation == "sync" {
                let (tx, rx) = std::sync::mpsc::channel();
                tx.send(result.map(Some)).unwrap();
                app.sync = Some(rx);
                app.sync_library(&ctx);
                if success {
                    assert!(app.pending_sync.is_some());
                    app.apply_library_sync();
                }
            } else {
                let (tx, rx) = std::sync::mpsc::channel();
                tx.send(result).unwrap();
                app.pending = Some(rx);
                app.pending_save = operation == "save";
                app.poll_storage();
            }
            let expected: &[u64] = if success { &[3, 1] } else { &[3, 2, 1] };
            assert_eq!(
                app.state.recent(&app.location),
                expected,
                "{operation}: {success}"
            );
            assert_eq!(state::load(&path), app.state, "{operation}: {success}");
            std::fs::remove_file(&path).unwrap();
        }
    }
}

#[test]
fn narrow_tables_drop_columns_and_fit() {
    let kinds = |width| -> Vec<_> {
        table_columns(width, None)
            .into_iter()
            .map(|c| c.0)
            .collect()
    };
    assert!(kinds(900.).contains(&Column::Group));
    assert!(!kinds(700.).contains(&Column::Group) && kinds(700.).contains(&Column::Protocol));
    assert!(!kinds(500.).contains(&Column::Protocol));
    for width in [500., 700., 900., 1400.] {
        let columns = table_columns(width, None);
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
                                Look::Tab { selected: true },
                            );
                            assert!(session.response.rect.contains_rect(session.inner.1.rect));
                            assert!(!session.inner.0.rect.intersects(session.inner.1.rect));
                            let next = session_tab(
                                ui,
                                &second,
                                Kind::Files,
                                session::TabState::Waiting,
                                Look::Tab { selected: false },
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
fn inactive_tab_close_is_visible_and_outlined_on_keyboard_focus() {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut close_id = egui::Id::NULL;
    for step in 0..3 {
        if step > 0 {
            ctx.memory_mut(|m| m.request_focus(close_id));
        }
        let mut closed = false;
        let output = ctx.run(
            egui::RawInput {
                events: if step == 2 {
                    vec![press(egui::Key::Enter)]
                } else {
                    vec![]
                },
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let tab = session_tab(
                        ui,
                        &Connection::new(),
                        Kind::Session,
                        session::TabState::Open,
                        Look::Tab { selected: false },
                    );
                    close_id = tab.inner.1.id;
                    closed = tab.inner.1.clicked();
                });
            },
        );
        if step == 1 {
            assert!(output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.rect.size() == Vec2::splat(20.) && r.stroke.color == theme::palette(&ctx).accent && r.stroke.width > 0.)));
        }
        if step == 2 {
            assert!(closed, "focused close activates with Enter");
        }
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
    app.activate_session(0);
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
            egui::CentralPanel::default().show(ctx, |ui| app.files_pane(ui, 0));
        },
    );
}

#[test]
fn transfer_header_keeps_summary_clear_of_actions_in_compact_panes() {
    for theme in theme::Theme::ALL {
        for width in [300., 360., 640.] {
            let ctx = egui::Context::default();
            ctx.enable_accesskit();
            theme.apply(&ctx);
            apply_style(&ctx);
            let mut app = files_app(&ctx);
            let browser = app.sessions[0].files.as_mut().unwrap();
            browser.demo_transfer("archive.tar.gz", crate::sftp::Direction::Download, 10, 100);
            browser.demo_transfer("done.txt", crate::sftp::Direction::Upload, 100, 100);
            for _ in 0..2 {
                let _ =
                    session_pane_frame(&mut app, &ctx, Pane::Main, Vec2::new(width, 600.), vec![]);
            }
            let output =
                session_pane_frame(&mut app, &ctx, Pane::Main, Vec2::new(width, 600.), vec![]);
            let update = output.platform_output.accesskit_update.unwrap();
            let button = |label| {
                update
                    .nodes
                    .iter()
                    .find(|(_, n)| n.label() == Some(label))
                    .unwrap()
                    .1
                    .bounds()
                    .unwrap()
            };
            let cancel = button("Cancel all");
            let clear = button("Clear finished");
            assert!(cancel.x0 >= 0. && cancel.x1 <= clear.x0 && clear.x1 <= f64::from(width));
            assert_eq!(cancel.y0, clear.y0, "transfer actions added another row");
            for text in ["TRANSFERS", "1 in progress"] {
                let (_, node) = update
                    .nodes
                    .iter()
                    .find(|(_, n)| {
                        n.role() == egui::accesskit::Role::Label && n.value() == Some(text)
                    })
                    .unwrap();
                let bounds = node.bounds().unwrap();
                assert!(
                    bounds.x0 >= 0. && bounds.x1 <= cancel.x0,
                    "{theme:?}, {width}: {text} overlapped transfer actions"
                );
                assert!(
                    (bounds.y0 - cancel.y0).abs() < 16.,
                    "transfer title added another row"
                );
            }
        }
    }
}

#[test]
fn favorite_controls_name_the_machine_and_show_keyboard_focus() {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    apply_style(&ctx);
    let mut app = test_app(&ctx);
    for step in 0..24 {
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1200., 800.),
                )),
                events: if step == 0 {
                    vec![]
                } else {
                    vec![press(egui::Key::Tab)]
                },
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.workspace(ui));
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        for name in ["Alpha", "Bravo", "Charlie"] {
            assert!(
                update
                    .nodes
                    .iter()
                    .any(|(_, n)| n.label() == Some(&format!("Favorite {name}")))
            );
        }
        let (id, _) = update
            .nodes
            .iter()
            .find(|(_, n)| n.label() == Some("Favorite Alpha"))
            .unwrap();
        if update.focus == *id {
            assert!(output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.rect.size() == Vec2::splat(24.) && r.stroke.color == theme::palette(&ctx).accent && r.stroke.width > 0.)));
            return;
        }
    }
    panic!("favorite was unreachable by Tab");
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
fn renaming_a_name_without_an_extension_selects_all_of_it() {
    let ctx = egui::Context::default();
    let mut app = files_app(&ctx);
    let mut c = app.connections[0].clone();
    c.protocol = Protocol::Ssh;
    let entry = crate::sftp::Entry {
        name: "日本語".into(),
        dir: false,
        link: false,
        special: false,
        size: 10,
        modified: Some(1_700_000_000),
        mode: Some(0o100_644),
    };
    let browser = crate::sftp::Browser::demo("/srv", vec![entry]);
    app.sessions[0] = session::Session::demo_files(c, browser, ctx.clone());
    let none = egui::Modifiers::NONE;
    files_frame(&mut app, &ctx, vec![]);
    files_frame(&mut app, &ctx, vec![key(egui::Key::End, none)]);
    files_frame(&mut app, &ctx, vec![key(egui::Key::F2, none)]);
    files_frame(&mut app, &ctx, vec![]);
    files_frame(&mut app, &ctx, vec![egui::Event::Text("x".into())]);
    files_frame(&mut app, &ctx, vec![key(egui::Key::Enter, none)]);
    let selected: Vec<_> = files(&app).view.selected.iter().cloned().collect();
    assert_eq!(selected, ["x"]);
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
}

#[test]
fn every_kind_of_tab_comes_back_as_itself() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let mut ssh = app.connections[0].clone();
    ssh.protocol = Protocol::Ssh;
    let mut rdp = app.connections[1].clone();
    rdp.protocol = Protocol::Rdp;
    let mut admin = session::Session::restored(rdp.clone(), false, ctx.clone());
    admin.admin = true;
    app.sessions.extend([
        session::Session::restored(ssh.clone(), false, ctx.clone()),
        session::Session::restored(ssh, true, ctx.clone()),
        admin,
        session::Session::restored_local(ctx.clone()),
    ]);
    app.remember_tabs();
    let state = &app.state;
    assert_eq!(
        (&state.file_tabs, &state.admin_tabs, &state.local_tabs),
        (&vec![1], &vec![2], &vec![3])
    );
    // The saved connection is unchanged, so editing it from the tab cannot store admin.
    assert_eq!(state.open[2], rdp);
    let restored = reopened(&ctx, app.state.clone());
    let kinds: Vec<_> = (restored.sessions.iter())
        .map(|s| (s.browsing, s.admin, s.local))
        .collect();
    assert_eq!(
        kinds,
        [
            (false, false, false),
            (true, false, false),
            (false, true, false),
            (false, false, true)
        ]
    );
    // Each starts when first shown.
    assert!((restored.sessions.iter()).all(|s| s.tab_state() == session::TabState::Waiting));
}

#[test]
fn the_files_button_pairs_a_terminal_with_its_open_browser() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let mut c = app.connections[0].clone();
    c.protocol = Protocol::Ssh;
    app.sessions
        .push(session::Session::restored(c.clone(), false, ctx.clone()));
    app.sessions
        .push(session::Session::restored(c.clone(), true, ctx.clone()));
    // It pairs with the open browser instead of opening another.
    app.activate_session(0);
    app.open_companion(0);
    let keys = (app.sessions[0].key, app.sessions[1].key);
    assert_eq!(app.sessions.len(), 2);
    assert_eq!(app.tabs, [pair(keys.0, keys.1, keys.0)]);
    // From the pair, the button moves to the other side.
    app.open_companion(0);
    assert_eq!((app.active(), app.tabs.len()), (Some(1), 1));
    app.unsplit(0);
    app.open_companion(1);
    assert_eq!(app.active(), Some(1));
    assert_eq!(app.tabs, [pair(keys.1, keys.0, keys.1)]);
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
    let (mut app, _) = app_with_tabs(&ctx);
    app.activate_session(2);
    // Restored tabs that never connected close at once, keeping the selection.
    app.request_close(0);
    assert!(matches!(app.modal, Modal::None));
    assert_eq!((app.sessions.len(), app.active()), (2, Some(1)));
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
    let (mut app, _) = app_with_tabs(&ctx);
    app.activate_session(1);
    app.remember_tabs();
    assert_eq!(app.state.open.len(), 3);
    app.state.forget_tabs = true;
    app.remember_tabs();
    assert!(app.state.open.is_empty() && app.state.active_tab.is_none());
    // Even a state file that still lists tabs opens none.
    let mut state = app.state.clone();
    state.open = app.connections.clone();
    let next = reopened(&ctx, state);
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
    app.sync_tabs();
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
fn the_settings_commit_opens_its_github_page() {
    let Some(commit) = option_env!("RELAY_COMMIT") else {
        return;
    };
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    app.modal = Modal::Settings;
    let mut frame = |events| {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1200., 800.),
                )),
                events,
                ..Default::default()
            },
            |ctx| app.settings(ctx),
        )
    };
    for _ in 0..3 {
        frame(vec![]);
    }
    let output = frame(vec![]);
    let pos = output
        .shapes
        .iter()
        .find_map(|clipped| match &clipped.shape {
            egui::Shape::Text(text) if text.galley.text() == commit => {
                Some(text.visual_bounding_rect().center())
            }
            _ => None,
        })
        .expect("the commit link is visible in Settings");
    frame(vec![egui::Event::PointerMoved(pos)]);
    let mut output = output;
    for pressed in [true, false] {
        output = frame(vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            },
        ]);
    }
    let expected = format!("https://github.com/nurlennart/relay/commit/{commit}");
    assert!(
        output.platform_output.commands.iter().any(|command| {
            matches!(command, egui::OutputCommand::OpenUrl(url) if url.url == expected)
        }),
        "click at {pos:?}: {:?}",
        output.platform_output.commands
    );
}

#[test]
fn shared_library_sync_waits_for_review() {
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
    let theirs = save(mine.revision, theirs);

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
    assert!(app.pending_sync.is_some());
    assert_eq!(app.connections.len(), 3);
    assert!(
        app.pending_sync
            .as_ref()
            .unwrap()
            .connections
            .iter()
            .any(|c| c.name == "Delta" && c.host == "delta.example")
    );
    app.apply_library_sync();
    assert_eq!(app.connections.len(), 4);
    assert!(app.pending_sync.is_none());
    assert!(app.pending.is_none());
    assert!(app.error.is_none());

    // Retarget an existing host; declining suppresses that revision.
    let mut poisoned = app.connections.clone();
    poisoned[0].host = "evil.example".into();
    let poisoned = save(theirs.revision, poisoned);
    sync(&mut app);
    assert!(app.pending_sync.is_some());
    assert_eq!(app.connections[0].host, "alpha.example");
    app.dismiss_library_sync();
    assert_eq!(app.ignored_sync_revision, Some(poisoned.revision));
    assert_eq!(app.revision, theirs.revision);
    assert_eq!(app.connections[0].host, "alpha.example");
    sync(&mut app);
    assert!(
        app.pending_sync.is_none(),
        "dismissed revision must not reappear"
    );

    // Saving from the retained list must not overwrite the teammate's newer revision.
    let mut edited = app.connections.clone();
    edited[1].notes = "local edit".into();
    assert!(app.persist(edited));
    while app.pending.is_some() {
        std::thread::sleep(Duration::from_millis(10));
        app.poll_storage();
    }
    assert!(
        app.error
            .as_deref()
            .is_some_and(|error| error.contains("since your last refresh"))
    );
    let stored = database::start(location.clone(), 0, None, ctx.clone())
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
        .unwrap();
    assert_eq!(stored.revision, poisoned.revision);
    assert_eq!(stored.connections, poisoned.connections);
    app.recovery = None;

    // Notes-only edits have no phishing surface and apply without a prompt.
    let mut notes = app.connections.clone();
    notes[1].notes = "rack 2".into();
    save(poisoned.revision, notes);
    sync(&mut app);
    assert!(app.pending_sync.is_none());
    assert_eq!(app.connections[1].notes, "rack 2");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn an_overdue_reload_waits_without_redrawing() {
    let ctx = egui::Context::default();
    let delays = std::sync::Arc::new(std::sync::Mutex::new(vec![]));
    ctx.set_request_repaint_callback({
        let delays = delays.clone();
        move |info| delays.lock().unwrap().push(info.delay)
    });
    let mut app = test_app(&ctx);
    app.location = database::Location::Sqlite(std::env::temp_dir().join("relay-unused.sqlite"));
    // The next reload is scheduled for when it is due.
    app.synced = Instant::now();
    app.sync_library(&ctx);
    assert!(
        delays
            .lock()
            .unwrap()
            .iter()
            .all(|d| *d > Duration::from_secs(50))
    );
    assert!(!delays.lock().unwrap().is_empty());
    // A save or a reload that outlasts the interval wakes the window when it ends instead.
    let overdue = |app: &mut App| {
        delays.lock().unwrap().clear();
        app.synced = Instant::now() - Duration::from_secs(61);
        app.sync_library(&ctx);
        assert!(delays.lock().unwrap().is_empty());
    };
    let (_saving, rx) = std::sync::mpsc::channel();
    app.pending = Some(rx);
    overdue(&mut app);
    app.pending = None;
    let (_reloading, rx) = std::sync::mpsc::channel();
    app.sync = Some(rx);
    overdue(&mut app);
    assert!(app.sync.is_some());
}

#[test]
fn the_window_follows_a_full_screen_desktop() {
    let step = |state, wants, window| full_window_step(state, wants, window);
    // Asked for: the window goes full screen, and is told once.
    let (asked, command) = step(None, true, false);
    assert_eq!(command, Some(true));
    let (waiting, command) = step(asked, true, false);
    assert_eq!((waiting, command), (asked, None), "waits for the window");
    let (shown, command) = step(waiting, true, true);
    assert!(shown.is_some_and(|s| s.confirmed) && command.is_none());
    // Leaving restores the window.
    assert_eq!(step(shown, false, true), (None, Some(false)));
    // The window left on its own, such as with macOS's green button.
    assert_eq!(step(shown, true, false), (None, None));
    // A window that was full screen before stays that way.
    let (already, command) = step(None, true, true);
    assert!(command.is_none());
    assert_eq!(step(already, false, true), (None, None));
    assert_eq!(step(None, false, false), (None, None));
}

#[test]
fn full_screen_never_becomes_the_remembered_window_size() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let rect = |w, h| {
        Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(w, h),
        ))
    };
    app.remember_window(rect(1000., 700.), Some(false), false, 1.);
    assert_eq!(app.state.window, Some([1000., 700.]));
    // In full screen, the monitor's size is not the window's.
    app.remember_window(rect(2560., 1440.), Some(false), true, 1.);
    // Leaving, the window reports its old size for a moment before returning to its own.
    app.remember_window(rect(2560., 1440.), Some(false), false, 1.);
    assert_eq!(app.state.window, Some([1000., 700.]));
    assert!(!app.state.maximized);
    // Once settled, its size is followed again.
    app.full_screen_seen = Some(Instant::now() - FULL_SCREEN_SETTLE);
    app.remember_window(rect(1100., 720.), Some(false), false, 1.);
    assert_eq!(app.state.window, Some([1100., 720.]));
}

#[test]
fn local_shells_offer_snippets_but_not_files() {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut app = test_app(&ctx);
    app.sessions
        .push(session::Session::local(ctx.clone()).unwrap());
    app.sync_tabs();
    app.select_tab(1);
    let output = ctx.run(egui::RawInput::default(), |ctx| app.tabs(ctx));
    let update = output.platform_output.accesskit_update.unwrap();
    let shown = |label| (update.nodes.iter()).any(|(_, node)| node.label() == Some(label));
    assert!(shown("Snippets"));
    assert!(!shown("Files"));
}

#[test]
fn navigation_exposes_names_selection_and_keyboard_activation() {
    for compact in [false, true] {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut id = egui::Id::NULL;
        let mut activated = false;
        let mut frame = |events| {
            ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        id = ui.next_auto_id();
                        activated = if compact {
                            sidebar::rail_row(ui, Icon::Star, "Favorites (2)", true)
                        } else {
                            sidebar::nav_row(ui, Icon::Star, "Favorites", 2, true)
                        };
                    });
                },
            )
        };
        let output = frame(vec![]);
        let update = output.platform_output.accesskit_update.unwrap();
        let label = if compact {
            "Favorites (2)"
        } else {
            "Favorites"
        };
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, n)| n.label() == Some(label))
            .unwrap();
        assert_eq!(node.toggled(), Some(egui::accesskit::Toggled::True));
        drop(frame);
        ctx.memory_mut(|m| m.request_focus(id));
        let _ = ctx.run(
            egui::RawInput {
                events: vec![press(egui::Key::Enter)],
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    activated = if compact {
                        sidebar::rail_row(ui, Icon::Star, "Favorites (2)", true)
                    } else {
                        sidebar::nav_row(ui, Icon::Star, "Favorites", 2, true)
                    };
                });
            },
        );
        assert!(activated, "navigation did not activate with Enter");
    }
}

#[cfg(feature = "ui-check")]
#[test]
fn captures_start_without_profile_credentials_or_pending_connections() {
    let app = ui_check::new(egui::Context::default(), None);
    assert!(app.state_path.is_none());
    assert!(app.path.as_os_str().is_empty());
    assert!(app.pending.is_none() && app.pending_location.is_none() && app.sync.is_none());
    assert!(!app.db_mssql.remember && !app.db_mysql.remember);
    assert!(!app.server_remembered);
    assert!(app.state.open.is_empty());
    assert!(app.error.is_none() && app.notice.is_none());
    assert!(!app.storage_ok, "fixtures cannot save a real library");
    assert!(
        app.connections
            .iter()
            .any(|c| c.host == "build.example.com")
    );
}

#[test]
fn palettes_expose_named_choices_and_keyboard_selection() {
    for snippets in [false, true] {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = test_app(&ctx);
        if snippets {
            let connection = app.connections[0].clone();
            app.sessions
                .push(Session::scripted(connection, "exit 0", ctx.clone()));
            app.activate_session(0);
            app.state.snippets = ["Alpha", "Bravo"]
                .map(|name| state::Snippet {
                    name: name.into(),
                    command: format!("echo {name}"),
                    run: false,
                })
                .to_vec();
            app.palette = Some(Palette::default());
        } else {
            app.open_quick();
        }
        let mut first_result = None;
        for step in 0..6 {
            let output = ctx.run(
                egui::RawInput {
                    events: match step {
                        2 | 4 => vec![press(egui::Key::ArrowDown)],
                        3 => vec![egui::Event::AccessKitActionRequest(
                            egui::accesskit::ActionRequest {
                                action: egui::accesskit::Action::Focus,
                                target: first_result.unwrap(),
                                data: None,
                            },
                        )],
                        5 => vec![press(egui::Key::ArrowUp)],
                        _ => vec![],
                    },
                    ..Default::default()
                },
                |ctx| {
                    if snippets {
                        app.snippet_palette(ctx);
                    } else {
                        app.quick_open(ctx);
                    }
                },
            );
            if step == 0 {
                continue;
            }
            let update = output.platform_output.accesskit_update.unwrap();
            let name = if step == 2 || step == 4 {
                "Bravo"
            } else {
                "Alpha"
            };
            let (selected, node) = update
                .nodes
                .iter()
                .find(|(_, node)| {
                    node.toggled() == Some(egui::accesskit::Toggled::True)
                        && node.label().is_some_and(|label| label.starts_with(name))
                })
                .expect("named, selected result");
            if step == 1 {
                first_result = Some(*selected);
            }
            assert!(node.label().unwrap().contains(if snippets {
                "types: echo"
            } else {
                ".example"
            }));
            assert!(
                update
                    .nodes
                    .iter()
                    .any(|(_, node)| node.active_descendant() == Some(*selected))
            );
        }
    }
}

#[test]
fn file_sort_headers_expose_names_direction_and_keyboard_activation() {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut app = files_app(&ctx);
    let mut sort_id = None;
    for step in 0..4 {
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1200., 800.),
                )),
                events: match step {
                    1 => vec![egui::Event::AccessKitActionRequest(
                        egui::accesskit::ActionRequest {
                            action: egui::accesskit::Action::Focus,
                            target: sort_id.unwrap(),
                            data: None,
                        },
                    )],
                    2 => vec![press(egui::Key::Enter)],
                    _ => vec![],
                },
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.files_pane(ui, 0));
            },
        );
        let update = output.platform_output.accesskit_update.unwrap();
        let (id, node) = update
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Sort by NAME"))
            .unwrap();
        sort_id = Some(*id);
        if step == 0 {
            assert_eq!(
                node.sort_direction(),
                Some(egui::accesskit::SortDirection::Ascending)
            );
        }
        if step == 1 {
            assert_eq!(update.focus, *id);
            assert!(output.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.stroke.color == theme::palette(&ctx).accent && r.stroke.width > 0.)));
        }
        if step == 3 {
            assert_eq!(
                node.sort_direction(),
                Some(egui::accesskit::SortDirection::Descending)
            );
        }
    }
}

#[test]
fn connection_and_file_rows_expose_identity_and_multiple_selection() {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut app = test_app(&ctx);
    app.marked = [1, 3].into_iter().collect();
    let output = ctx.run(egui::RawInput::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| app.workspace(ui));
    });
    let update = output.platform_output.accesskit_update.unwrap();
    for (name, selected) in [("Alpha", true), ("Bravo", false), ("Charlie", true)] {
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, n)| {
                n.label()
                    .is_some_and(|label| label.starts_with(&format!("{name}, ")))
            })
            .unwrap();
        assert!(node.label().unwrap().contains(".example"));
        assert_eq!(
            node.toggled(),
            Some(if selected {
                egui::accesskit::Toggled::True
            } else {
                egui::accesskit::Toggled::False
            })
        );
    }
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut app = files_app(&ctx);
    let browser = app.sessions[0].files.as_mut().unwrap();
    browser.view.selected = ["app".into(), "c.txt".into()].into_iter().collect();
    let output = ctx.run(Default::default(), |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| app.files_pane(ui, 0));
    });
    let update = output.platform_output.accesskit_update.unwrap();
    for (label, selected) in [
        ("app, Folder", true),
        ("b.txt, File", false),
        ("c.txt, File", true),
    ] {
        let (_, node) = update
            .nodes
            .iter()
            .find(|(_, n)| n.label() == Some(label))
            .unwrap();
        assert_eq!(
            node.toggled(),
            Some(if selected {
                egui::accesskit::Toggled::True
            } else {
                egui::accesskit::Toggled::False
            })
        );
    }
}

#[test]
fn connection_editor_inputs_are_associated_with_their_labels() {
    for protocol in Protocol::ALL {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut app = test_app(&ctx);
        let mut c = app.connections[0].clone();
        c.protocol = protocol;
        c.ssh.jump = "bastion.example".into();
        c.rdp.gateway = "gateway.example".into();
        app.edit(c);
        for _ in 0..2 {
            let _ = connection_editor_frame(&mut app, &ctx, Vec2::new(1200., 1600.), vec![]);
        }
        let output = connection_editor_frame(&mut app, &ctx, Vec2::new(1200., 1600.), vec![]);
        let update = output.platform_output.accesskit_update.unwrap();
        let mut labels = vec![
            "Connection name",
            "Host / IP address",
            "Port",
            "Username (optional)",
            "Group",
            "Notes (optional)",
        ];
        match protocol {
            Protocol::Rdp => labels.extend([
                "RD Gateway (optional)",
                "Remote audio",
                "System key combinations",
            ]),
            Protocol::Ssh => labels.extend([
                "Jump host (optional)",
                "Identity file (optional)",
                "Port forwards (optional)",
            ]),
            Protocol::Vnc => {}
        }
        for label in labels {
            let (label_id, _) = update
                .nodes
                .iter()
                .find(|(_, n)| n.role() == egui::accesskit::Role::Label && n.value() == Some(label))
                .unwrap_or_else(|| panic!("missing label {label}"));
            assert!(
                update
                    .nodes
                    .iter()
                    .any(|(_, n)| n.labelled_by().contains(label_id)),
                "unassociated label {label} for {protocol:?}"
            );
        }
        let (_, choose) = update
            .nodes
            .iter()
            .find(|(_, n)| n.label() == Some("Choose an existing group"))
            .unwrap();
        assert_eq!(choose.role(), egui::accesskit::Role::Button);
    }
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
    let (mut app, _) = app_with_tabs(&ctx);
    app.sync_tabs();
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
    assert_eq!(app.active(), Some(0));
    frame(&mut app, egui::Key::Num1);
    assert_eq!(app.active(), None, "Ctrl+1 is the library");
    frame(&mut app, egui::Key::Num3);
    assert_eq!(app.active(), Some(1));
    frame(&mut app, egui::Key::Num9);
    assert_eq!(app.active(), Some(2), "Ctrl+9 is the last tab");
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
    app.sync_tabs();
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
fn tabs_beyond_the_window_width_stay_reachable() {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut app = test_app(&ctx);
    for i in 0..12 {
        let mut c = Connection::new();
        c.name = format!("Server number {i}");
        c.host = format!("s{i}.example");
        app.sessions
            .push(session::Session::restored(c, false, ctx.clone()));
    }
    app.sync_tabs();
    let width = 600.;
    let mut time = 0.;
    // Where each labelled button in the bar lies, left to right edges.
    let mut frame = |app: &mut App, events| {
        time += 1. / 60.;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(width, 800.),
                )),
                time: Some(time),
                predicted_dt: 1. / 60.,
                events,
                ..Default::default()
            },
            |ctx| app.tabs(ctx),
        );
        let update = output.platform_output.accesskit_update.unwrap();
        move |label: &str| {
            let (_, node) = (update.nodes.iter())
                .find(|(_, node)| node.label() == Some(label))
                .unwrap_or_else(|| panic!("no {label} in the tab bar"));
            let b = node.bounds().unwrap();
            (b.x0 as f32, b.x1 as f32)
        }
    };
    let visible = |(left, right): (f32, f32)| left >= 0. && right <= width;
    // Choosing the last tab, as Ctrl+9 does, scrolls it into view.
    app.select_tab(11);
    for _ in 0..30 {
        let _ = frame(&mut app, vec![]);
    }
    let bar = frame(&mut app, vec![]);
    assert!(
        visible(bar("Server number 11")),
        "{:?}",
        bar("Server number 11")
    );
    assert!(!visible(bar("Server number 0")));
    // The new connection and local shell buttons stay in reach.
    let local = format!("Local shell on this computer ({})", tabs::LOCAL_SHELL_KEYS);
    assert!(visible(bar("Open a connection (Ctrl+T)")));
    assert!(visible(bar(&local)));
    // The wheel scrolls the tabs back.
    let over = egui::pos2(200., 20.);
    let _ = frame(&mut app, vec![egui::Event::PointerMoved(over)]);
    let wheel = egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        delta: Vec2::new(0., 5000.),
        modifiers: egui::Modifiers::NONE,
    };
    let _ = frame(&mut app, vec![wheel]);
    for _ in 0..60 {
        let _ = frame(&mut app, vec![]);
    }
    let bar = frame(&mut app, vec![]);
    assert!(
        visible(bar("Server number 0")),
        "{:?}",
        bar("Server number 0")
    );
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
    // A selection that is no longer shown is not chosen along with the clicked row.
    app.click_row(3, ctrl, &[1, 3]);
    assert_eq!(marked(&app), [3]);
    app.marked.clear();

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
    // One confirmation serves one connection or several; Escape cancels it.
    let confirm = |app: &mut App, events| {
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| app.confirmations(ctx));
    };
    for ids in [vec![1], vec![1, 3]] {
        app.modal = Modal::Delete(ids);
        confirm(&mut app, vec![]);
        assert!(matches!(app.modal, Modal::Delete(_)));
        confirm(&mut app, vec![press(egui::Key::Escape)]);
        assert!(matches!(app.modal, Modal::None));
    }
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
fn a_chosen_side_keeps_the_keyboard_and_a_click_moves_it() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let mut ssh = app.connections[0].clone();
    ssh.protocol = Protocol::Ssh;
    for _ in 0..2 {
        app.sessions
            .push(session::Session::scripted(ssh.clone(), "cat", ctx.clone()));
    }
    app.activate_session(0);
    app.sync_tabs();
    app.open_beside(1);
    let mut time = 0.;
    let mut frame = |app: &mut App, events: Vec<egui::Event>| {
        time += 0.05;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1200., 800.),
                )),
                time: Some(time),
                events,
                ..Default::default()
            },
            |ctx| {
                app.follow_focus();
                egui::CentralPanel::default().show(ctx, |ui| {
                    let (place, pair) = app.active_pair().unwrap();
                    let sides = [pair.left, pair.right].map(|k| app.session_index(k).unwrap());
                    app.pair_panes(ui, place, pair.fraction, sides, &mut vec![]);
                });
            },
        );
    };
    let settle = |frame: &mut dyn FnMut(&mut App, Vec<egui::Event>), app: &mut App| {
        for _ in 0..3 {
            frame(app, vec![]);
        }
    };
    let keyboard =
        |app: &App| -> Vec<bool> { app.sessions.iter().map(|s| s.keyboard_focused()).collect() };
    settle(&mut frame, &mut app);
    assert_eq!(keyboard(&app), [true, false]);
    // Choosing a side, as its tab does, is not undone by the side that still had the keyboard.
    for side in [1, 0, 1] {
        app.activate_session(side);
        settle(&mut frame, &mut app);
        assert_eq!(app.active(), Some(side));
        assert_eq!(keyboard(&app), [side == 0, side == 1]);
    }
    // A click into the other side's terminal brings it in front, and the pair remembers it.
    let on_left = egui::pos2(300., 400.);
    let button = |pressed| egui::Event::PointerButton {
        pos: on_left,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    frame(
        &mut app,
        vec![egui::Event::PointerMoved(on_left), button(true)],
    );
    frame(&mut app, vec![button(false)]);
    settle(&mut frame, &mut app);
    assert_eq!(app.active(), Some(0));
    assert_eq!(keyboard(&app), [true, false]);
    let left = app.sessions[0].key;
    assert!(matches!(app.tabs[0], Tab::Pair(pair) if pair.focus == left));
}

#[test]
fn a_library_worker_that_stops_mid_save_returns_the_edit() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let mut edited = app.connections[0].clone();
    edited.name = "Alpha prime".into();
    let (stopped, rx) = std::sync::mpsc::channel();
    drop(stopped);
    app.pending = Some(rx);
    app.pending_save = true;
    app.pending_editor = Some(edited.clone());
    app.poll_storage();
    assert!(matches!(&app.modal, Modal::Editor(c) if *c == edited));
    assert!(!app.pending_save && app.pending.is_none());
    assert!(app.error.as_deref().is_some_and(|e| e.contains("stopped")));
}

#[test]
fn native_desktops_take_physical_pixels_with_overlays_cut_out() {
    let rect = Rect::from_min_size(egui::pos2(10., 20.), Vec2::new(100., 50.));
    let menu = Rect::from_min_size(egui::pos2(100., 30.), Vec2::new(40., 10.));
    let elsewhere = Rect::from_min_size(egui::pos2(300., 300.), Vec2::new(5., 5.));
    let (area, holes) = native_area(rect, 2., &[menu, elsewhere]);
    assert_eq!(area, [20, 40, 200, 100]);
    // Only the part of the menu over the desktop, from the desktop's corner.
    assert_eq!(holes, [[180, 20, 20, 20]]);
}

/// Settings must keep controls inside the modal even when text and zoom reduce its width.
#[test]
fn settings_fit_small_viewports_across_workspace_themes() {
    use settings::SettingsTab;
    for theme in theme::Theme::ALL {
        for (viewport, zoom) in [
            (Vec2::new(1200., 800.), 1.),
            (Vec2::new(640., 480.), 1.),
            (Vec2::new(480., 360.), 1.),
            (Vec2::new(900., 700.), 1.5),
            (Vec2::new(1200., 900.), 2.),
            (Vec2::new(800., 600.), 2.),
        ] {
            let size = viewport / zoom;
            let ctx = egui::Context::default();
            apply_style(&ctx);
            theme.apply(&ctx);
            ctx.options_mut(|options| options.zoom_factor = zoom);
            let mut app = test_app(&ctx);
            app.state_path = None;
            app.modal = Modal::Settings;
            app.location = database::Location::Sqlite(PathBuf::from(format!(
                "//fileserver/{}/connections.sqlite",
                "long-team-directory/".repeat(12)
            )));
            app.state.editor = "/a/very/long/editor/path/".repeat(16);
            app.state.snippets.push(state::Snippet {
                name: "A long snippet name ".repeat(20),
                command: "systemctl status nginx ".repeat(30),
                run: true,
            });
            for tab in [
                SettingsTab::Appearance,
                SettingsTab::Terminal,
                SettingsTab::Library,
                SettingsTab::Snippets,
            ] {
                app.settings_tab = tab;
                let mut header_text = vec![];
                for _ in 0..3 {
                    let output = ctx.run(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
                            ..Default::default()
                        },
                        |ctx| app.settings(ctx),
                    );
                    header_text = output
                        .shapes
                        .iter()
                        .filter_map(|clipped| {
                            if let egui::Shape::Text(text) = &clipped.shape {
                                Some((text.galley.text().to_owned(), text.visual_bounding_rect()))
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>();
                }
                let title = header_text
                    .iter()
                    .find(|(text, _)| text == "Settings")
                    .unwrap()
                    .1;
                for (text, rect) in &header_text {
                    if text != "Settings" {
                        assert!(
                            !title.intersects(*rect),
                            "{theme:?} {tab:?} at {size:?}: title overlaps {text:?}"
                        );
                    }
                }
                let rect = ctx
                    .memory(|memory| memory.area_rect(egui::Id::new("settings")))
                    .unwrap();
                assert!(
                    rect.width() <= size.x && rect.height() <= size.y,
                    "{theme:?} {tab:?} at {size:?}: modal {rect:?}"
                );
            }
        }
    }
}

#[test]
fn settings_scroll_to_the_last_section() {
    for (tab, label) in [
        (settings::SettingsTab::Appearance, "Reopen tabs at startup"),
        (settings::SettingsTab::Terminal, "External editor"),
    ] {
        let ctx = egui::Context::default();
        apply_style(&ctx);
        let mut app = test_app(&ctx);
        app.state_path = None;
        app.modal = Modal::Settings;
        app.settings_tab = tab;
        let mut last_visible = false;
        for frame in 0..12 {
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(640., 480.))),
                    time: Some(frame as f64 * 0.1),
                    events: if frame >= 3 {
                        vec![
                            egui::Event::PointerMoved(egui::pos2(320., 240.)),
                            egui::Event::MouseWheel {
                                unit: egui::MouseWheelUnit::Point,
                                delta: Vec2::new(0., -1000.),
                                modifiers: egui::Modifiers::NONE,
                            },
                        ]
                    } else {
                        vec![]
                    },
                    ..Default::default()
                },
                |ctx| app.settings(ctx),
            );
            last_visible |= output.shapes.iter().any(|clipped| {
                matches!(&clipped.shape, egui::Shape::Text(text)
                if text.galley.text() == label
                && clipped.clip_rect.contains_rect(text.visual_bounding_rect()))
            });
        }
        assert!(last_visible, "{label} in {tab:?} is reachable by scrolling");
    }
}

#[test]
fn library_filters_combine_with_search_and_cached_counts() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    app.connections[0].group = "Production".into();
    app.connections[1].group = "Production".into();
    app.connections[0].favorite = true;
    app.state.note_recent(&app.location, 1);
    app.state.note_recent(&app.location, 3);
    assert_eq!(
        app.groups().iter().find(|g| g.0 == "Production").unwrap().1,
        2
    );
    assert_eq!(app.counts().favorites, 1);
    assert_eq!(app.counts().recent, 2);
    app.group = Some("Production".into());
    assert_eq!(&*app.filtered(), &[0, 1]);
    app.query = "bravo".into();
    assert_eq!(&*app.filtered(), &[1]);
    app.filter = Filter::Favorites;
    assert!(app.filtered().is_empty());
    app.query = "alpha".into();
    assert_eq!(&*app.filtered(), &[0]);
    app.filter = Filter::Recent;
    assert_eq!(&*app.filtered(), &[0]);
    app.group = None;
    app.query.clear();
    assert_eq!(&*app.filtered(), &[2, 0]);
    app.filter = Filter::All;
    assert_eq!(&*app.filtered(), &[0, 1, 2]);
}

#[test]
fn dragging_and_resetting_library_columns_save_their_widths_for_restart() {
    let ctx = egui::Context::default();
    let mut app = test_app(&ctx);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join(model::temp_name("column-state.json"));
    app.state_path = Some(path.clone());
    let frame = |app: &mut App, events| {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    Vec2::new(1000., 600.),
                )),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.connection_table(ui, &[0, 1, 2]));
            },
        )
    };
    let output = frame(&mut app, vec![]);
    let handle = output
        .shapes
        .iter()
        .find_map(|s| match &s.shape {
            egui::Shape::LineSegment { points, .. } if points[0].x == points[1].x => {
                Some(points[0].lerp(points[1], 0.5))
            }
            _ => None,
        })
        .expect("a column resize handle");
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        pressed,
        button: egui::PointerButton::Primary,
        modifiers: egui::Modifiers::NONE,
    };
    frame(&mut app, vec![egui::Event::PointerMoved(handle)]);
    frame(&mut app, vec![button(handle, true)]);
    let dragged = handle + Vec2::new(40., 0.);
    frame(&mut app, vec![egui::Event::PointerMoved(dragged)]);
    frame(&mut app, vec![button(dragged, false)]);
    let saved = state::load(&path);
    assert!(
        saved.library_columns.is_some(),
        "dragging saves column preferences"
    );
    assert_eq!(saved.library_columns, app.state.library_columns);
    let width = output
        .shapes
        .iter()
        .find_map(|s| match &s.shape {
            egui::Shape::LineSegment { points, .. } if points[0].y == points[1].y => {
                Some(points[1].x - points[0].x)
            }
            _ => None,
        })
        .unwrap();
    let defaults = table_columns(width, None);
    let restored = table_columns(width, saved.library_columns);
    let name = |columns: &[(Column, f32)]| columns.iter().find(|c| c.0 == Column::Name).unwrap().1;
    assert!(
        (name(&restored) - name(&defaults) - 40.).abs() < 0.1,
        "{} -> {} at {width}",
        name(&defaults),
        name(&restored)
    );
    let output = frame(&mut app, vec![]);
    let handle = output
        .shapes
        .iter()
        .find_map(|s| match &s.shape {
            egui::Shape::LineSegment { points, .. } if points[0].x == points[1].x => {
                Some(points[0].lerp(points[1], 0.5))
            }
            _ => None,
        })
        .unwrap();
    frame(&mut app, vec![egui::Event::PointerMoved(handle)]);
    for _ in 0..2 {
        frame(&mut app, vec![button(handle, true)]);
        frame(&mut app, vec![button(handle, false)]);
    }
    let saved = state::load(&path);
    assert_eq!(saved.library_columns, app.state.library_columns);
    let reset = table_columns(width, saved.library_columns);
    for ((_, expected), (_, actual)) in defaults.iter().zip(&reset) {
        assert!(
            (expected - actual).abs() < 0.01,
            "double-click restores default sizing"
        );
    }
    std::fs::remove_file(path).unwrap();
}

fn connection_editor_frame(
    app: &mut App,
    ctx: &egui::Context,
    size: Vec2,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)),
            time: Some(ctx.input(|input| input.time) + 0.1),
            events,
            ..Default::default()
        },
        |ctx| app.editor(ctx),
    )
}

#[test]
fn connection_editor_keeps_header_and_actions_visible_across_themes_and_sizes() {
    for theme in theme::Theme::ALL {
        for size in [
            Vec2::new(1200., 800.),
            Vec2::new(480., 360.),
            Vec2::new(400., 300.),
        ] {
            for protocol in Protocol::ALL {
                let ctx = egui::Context::default();
                apply_style(&ctx);
                theme.apply(&ctx);
                let mut app = test_app(&ctx);
                let mut c = app.connections[0].clone();
                c.protocol = protocol;
                c.port = protocol.port();
                c.rdp.gateway = "gateway.example.com".into();
                c.rdp.display = model::Display::Fixed {
                    width: 1920,
                    height: 1080,
                };
                c.ssh.forwards = "L8080:localhost:80".into();
                c.vnc.view_only = true;
                app.edit(c);
                let last = match protocol {
                    Protocol::Rdp => "Fixed resolution",
                    Protocol::Ssh => "Port forwards (optional)",
                    Protocol::Vnc => "View only",
                };
                let mut last_visible = false;
                // Sweep the form: backend-specific notices can follow its last control.
                for frame in 0..48 {
                    if frame == 44 {
                        app.editor_error = "Enter a hostname, IPv4 address, or bare IPv6 address (without a port).".into();
                    }
                    if frame == 45 {
                        app.editor_error = "Someone else changed this connection while you edited. Save again to replace their values with yours, or Cancel to keep theirs.".into();
                    }
                    let output = connection_editor_frame(
                        &mut app,
                        &ctx,
                        size,
                        if frame >= 3 {
                            vec![
                                egui::Event::PointerMoved(egui::pos2(size.x / 2., size.y / 2.)),
                                egui::Event::MouseWheel {
                                    unit: egui::MouseWheelUnit::Point,
                                    delta: Vec2::new(0., -32.),
                                    modifiers: egui::Modifiers::NONE,
                                },
                            ]
                        } else {
                            vec![]
                        },
                    );
                    if frame < 2 {
                        continue;
                    }
                    if frame < 44 {
                        last_visible |= output.shapes.iter().any(|shape| {
                            matches!(&shape.shape,
                            egui::Shape::Text(text) if text.galley.text() == last
                                && shape.clip_rect.contains_rect(text.visual_bounding_rect()))
                        });
                    }
                    let screen = Rect::from_min_size(egui::Pos2::ZERO, size);
                    for label in [
                        "Edit connection",
                        "RDP",
                        "SSH",
                        "VNC",
                        "Save changes",
                        "Cancel",
                    ] {
                        assert!(
                            output.shapes.iter().any(|shape| matches!(&shape.shape,
                            egui::Shape::Text(text) if text.galley.text() == label
                                && screen.contains_rect(text.visual_bounding_rect())
                                && shape.clip_rect.contains_rect(text.visual_bounding_rect()))),
                            "{theme:?} {protocol:?} {size:?}: {label} clipped"
                        );
                    }
                    let rect = ctx
                        .memory(|m| m.area_rect(egui::Id::new("edit-connection")))
                        .unwrap();
                    assert!(
                        rect.width() <= size.x && rect.height() <= size.y,
                        "{theme:?} {protocol:?} {size:?}: modal {rect:?}"
                    );
                }
                assert!(
                    last_visible,
                    "{theme:?} {protocol:?} {size:?}: last setting unreachable"
                );
            }
        }
    }
}

#[test]
fn connection_editor_preserves_protocol_options_ports_and_cancelled_changes() {
    let ctx = egui::Context::default();
    apply_style(&ctx);
    ctx.style_mut(|style| style.animation_time = 0.);
    let mut app = test_app(&ctx);
    let original = app.connections.clone();
    let mut c = Connection::new();
    c.name = "Draft".into();
    c.host = "draft.example".into();
    c.rdp.gateway = "gateway.example".into();
    c.ssh.jump = "jump.example".into();
    c.vnc.view_only = true;
    let draft = c.clone();
    app.edit(c);
    let frame =
        |app: &mut App, events| connection_editor_frame(app, &ctx, Vec2::new(1200., 900.), events);
    for protocol in [Protocol::Ssh, Protocol::Vnc, Protocol::Rdp] {
        let mut output = frame(&mut app, vec![]);
        for _ in 0..8 {
            output = frame(&mut app, vec![]);
        }
        let pos = output
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.text() == protocol.label() => {
                    Some(text.visual_bounding_rect().center())
                }
                _ => None,
            })
            .unwrap();
        frame(&mut app, vec![egui::Event::PointerMoved(pos)]);
        for pressed in [true, false] {
            frame(
                &mut app,
                vec![egui::Event::PointerButton {
                    pos,
                    pressed,
                    button: egui::PointerButton::Primary,
                    modifiers: egui::Modifiers::NONE,
                }],
            );
        }
        let Modal::Editor(c) = &app.modal else {
            panic!("draft closed");
        };
        assert_eq!(c.protocol, protocol);
        assert_eq!(c.port, protocol.port());
        assert_eq!(
            (&c.rdp, &c.ssh, &c.vnc),
            (&draft.rdp, &draft.ssh, &draft.vnc)
        );
    }
    ctx.memory_mut(|m| m.request_focus(egui::Id::new("connection-port")));
    frame(
        &mut app,
        vec![
            key(egui::Key::A, egui::Modifiers::COMMAND),
            egui::Event::Text("99999".into()),
        ],
    );
    frame(&mut app, vec![press(egui::Key::Enter)]);
    assert_eq!(app.editor_port.as_deref(), Some("99999"));
    assert!(app.editor_error.contains("Port must"));
    assert!(matches!(&app.modal, Modal::Editor(c) if c.port == 0));
    assert_eq!(app.connections, original);
    frame(&mut app, vec![press(egui::Key::Escape)]);
    assert!(matches!(app.modal, Modal::None));
    assert_eq!(app.connections, original);
    app.edit(Connection::new());
    assert!(app.editor_port.is_none());
}

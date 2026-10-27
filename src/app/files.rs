//! The SFTP file browser in a tab: sign-in prompts, the path bar, the folder listing, and
//! the transfer queue.
use super::*;
use crate::askpass;
use crate::sftp::{
    self, Browser, Confirm, Direction, Entry, Failure, Sort, Status, Transfer, TransferState,
};
use egui::{Align, Layout, Margin, Rect, Sense, UiBuilder};

/// The height of a row in the listing.
const FILE_ROW: f32 = 30.;

/// The height of fields and buttons in the bars above the listing.
const BAR_CONTROL: f32 = 28.;

/// The height of a row in the transfer queue: name and status, progress, destination.
const TRANSFER_ROW: f32 = 46.;

/// Rows of the transfer queue shown before it scrolls, when the tab has room for them.
const TRANSFER_ROWS: usize = 4;

/// Items dragged in a listing, which can be dropped on a folder to move them there.
#[derive(Clone)]
struct Dragged {
    /// The session they come from; they can only move within the same server.
    key: u64,
    names: Vec<String>,
}

/// What the browser asks of the app, which owns file pickers and saved state.
enum Action {
    Download(Vec<String>),
    PickUpload { folders: bool },
    Reconnect,
    Copy(String),
}

/// Where the listing's columns sit for the browser's width.
struct Columns {
    name: f32,
    size: f32,
    modified: f32,
    permissions: f32,
}

impl Columns {
    fn for_width(width: f32) -> Self {
        let size = 84.;
        // Narrow panes, such as split view, drop permissions and then dates.
        let modified = if width >= 460. { 128. } else { 0. };
        let permissions = if width >= 640. { 96. } else { 0. };
        Self {
            name: (width - size - modified - permissions - 3. * COLUMN_GAP).max(80.),
            size,
            modified,
            permissions,
        }
    }
}

const COLUMN_GAP: f32 = 12.;

impl App {
    /// Shows session `index`'s file browser below its header.
    pub(super) fn files_pane(&mut self, ui: &mut egui::Ui, index: usize, pane: Pane) {
        let covered = self.covered();
        let main_browses = self
            .active
            .and_then(|a| self.sessions.get(a))
            .is_some_and(|s| s.browsing);
        // Files dropped without a pointer position go to the main pane, or to the pane beside
        // it when the main one is not a browser.
        let fallback = pane != Pane::Beside || !main_browses;
        let session = &mut self.sessions[index];
        let key = session.key;
        let mut actions = vec![];
        match session.files.as_mut() {
            Some(files) => browser(ui, files, key, covered, fallback, &mut actions),
            None => {
                // A restored tab not connected yet, or a restart that failed.
                let error = session
                    .error
                    .clone()
                    .unwrap_or_else(|| "Not connected.".into());
                ui.add_space(40.);
                ui.label(RichText::new(error).size(15.));
                ui.add_space(12.);
                if ui.button("Reconnect").clicked() {
                    actions.push(Action::Reconnect);
                }
            }
        }
        for action in actions {
            match action {
                Action::Reconnect => self.reconnect(index),
                Action::Copy(text) => {
                    ui.ctx().copy_text(text.clone());
                    self.notify(format!("Copied {text}"));
                }
                Action::PickUpload { folders } => {
                    let local = file_dialog::pick_uploads(self.parent, folders);
                    if let Some(files) = &mut self.sessions[index].files {
                        let folder = files.path.clone();
                        files.offer_upload(local, &folder);
                    }
                }
                Action::Download(names) => self.download(index, names),
            }
        }
    }

    /// Asks where to save `names` from session `index`'s current folder, then queues them.
    fn download(&mut self, index: usize, names: Vec<String>) {
        let Some(files) = &self.sessions[index].files else {
            return;
        };
        let start = self
            .state
            .download_folder
            .clone()
            .filter(|p| p.is_dir())
            .or_else(platform::downloads_dir);
        let single_file = match names.as_slice() {
            [name] => files.entries.iter().any(|e| &e.name == name && !e.dir),
            _ => false,
        };
        let folder = if single_file {
            let Some(name) = sftp::local_name(&names[0]) else {
                self.error = Some(format!("“{}” cannot be saved on this computer.", names[0]));
                return;
            };
            let Some(path) =
                file_dialog::save_as(self.parent, &name.to_string_lossy(), start.as_deref())
            else {
                return;
            };
            let folder = path.parent().map(PathBuf::from);
            if let Some(files) = &mut self.sessions[index].files {
                files.download_as(&names[0], path);
            }
            folder
        } else {
            let Some(folder) = file_dialog::pick_folder(self.parent, start.as_deref()) else {
                return;
            };
            if let Some(files) = &mut self.sessions[index].files {
                files.download(&names, &folder);
            }
            Some(folder)
        };
        if folder.is_some() {
            self.state.download_folder = folder;
            self.save_state();
        }
    }
}

fn browser(
    ui: &mut egui::Ui,
    files: &mut Browser,
    key: u64,
    covered: bool,
    fallback: bool,
    actions: &mut Vec<Action>,
) {
    let area = ui.available_rect_before_wrap();
    // The browser takes the keyboard when clicked and lets go when something else is.
    if let Some(pos) = ui.input(|i| {
        i.pointer
            .any_pressed()
            .then(|| i.pointer.interact_pos())
            .flatten()
    }) {
        files.view.focused = area.contains(pos);
    }
    if std::mem::take(&mut files.view.focus_requested) {
        files.view.focused = true;
    }
    match files.status.clone() {
        Status::Connecting => return connecting(ui, files),
        Status::Ended(reason) => {
            ui.add_space(40.);
            ui.label(RichText::new(reason).size(15.));
            ui.add_space(12.);
            if ui.button("Reconnect").clicked() {
                actions.push(Action::Reconnect);
            }
            if !files.transfers.is_empty() {
                ui.add_space(20.);
                let height = ui.available_height() - 60.;
                transfers(ui, files, height);
            }
            return;
        }
        Status::Ready => {}
    }
    for index in 0..files.prompts.len().min(1) {
        prompt_card(ui, files, index);
        ui.add_space(8.);
    }
    // The listing is filtered and sorted once a frame. A change to how it is shown takes
    // effect on the next, which it asks for.
    let shown = |f: &Browser| {
        let v = &f.view;
        (v.sort, v.descending, v.filter.clone(), v.show_hidden)
    };
    let before = shown(files);
    let order = files.order();
    let names: Vec<String> = order
        .iter()
        .map(|&i| files.entries[i].name.clone())
        .collect();
    if !covered && files.view.focused && ui.memory(|m| m.focused().is_none()) {
        keys(ui, files, &names, actions);
    }
    toolbar(ui, files, key, actions);
    ui.add_space(8.);
    banners(ui, files);
    // The status line and transfer queue sit at the bottom, at the height they took last
    // frame; the listing fills the rest.
    let full = ui.available_rect_before_wrap();
    let measured = ui.id().with(("files-bottom", key));
    let bottom = ui.data(|d| d.get_temp::<f32>(measured)).unwrap_or(30.);
    let split = (full.bottom() - bottom).max(full.top() + FILE_ROW * 4.);
    let listing = Rect::from_min_max(full.min, egui::pos2(full.right(), split - 6.));
    let mut top = ui.new_child(
        UiBuilder::new()
            .max_rect(listing)
            .layout(Layout::top_down(Align::LEFT)),
    );
    let target = table(
        &mut top,
        files,
        key,
        &order,
        &names,
        listing.height(),
        actions,
    );
    // Not bounded below, so it takes the height it wants and the next frame makes room.
    let mut below = ui.new_child(
        UiBuilder::new()
            .max_rect(Rect::from_min_max(
                egui::pos2(full.left(), split),
                egui::pos2(full.right(), split + full.height()),
            ))
            .layout(Layout::top_down(Align::LEFT)),
    );
    status_bar(&mut below, files, &names);
    if !files.transfers.is_empty() {
        below.add_space(2.);
        // At most a third of the tab, so a long queue scrolls instead of crowding out the
        // listing.
        let height = (full.height() / 3.).max(TRANSFER_ROW * 1.5);
        transfers(&mut below, files, height);
    }
    let used = below.min_rect().height();
    if (used - bottom).abs() > 0.5 {
        ui.data_mut(|d| d.insert_temp(measured, used));
        ui.ctx().request_repaint();
    }
    ui.allocate_rect(full, Sense::hover());
    os_drops(ui, files, area, fallback, target);
    if shown(files) != before {
        ui.ctx().request_repaint();
    }
}

/// Spinner, prompts, and a way out while ssh connects.
fn connecting(ui: &mut egui::Ui, files: &mut Browser) {
    let colors = theme::palette(ui.ctx());
    ui.add_space((ui.available_height() * 0.12).clamp(16., 90.));
    ui.vertical_centered(|ui| {
        if files.prompts.is_empty() {
            ui.spinner();
            ui.add_space(4.);
            ui.label(RichText::new("Connecting…").size(15.).strong());
            ui.label(
                RichText::new("Opening SFTP with your SSH settings, keys and agent.")
                    .color(colors.muted),
            );
            let patience = Duration::from_secs(15);
            match patience.checked_sub(files.started.elapsed()) {
                // Nothing else wakes the window when the note becomes due.
                Some(left) => ui.ctx().request_repaint_after(left),
                None => {
                    ui.label(
                        RichText::new("Still waiting for the server or a jump host.")
                            .size(12.)
                            .color(colors.muted),
                    );
                }
            }
            ui.add_space(12.);
            if ui.button("Cancel").clicked() {
                files.disconnect("Connecting was cancelled.");
            }
        } else {
            // The prompt has its own way out.
            prompt_card(ui, files, 0);
        }
    });
}

/// Asks ssh's question `index`: a password, a host key to trust, or a confirmation.
fn prompt_card(ui: &mut egui::Ui, files: &mut Browser, index: usize) {
    let colors = theme::palette(ui.ctx());
    let width = ui.available_width().min(480.);
    let mut answer = None;
    // Declining a password would only make ssh ask again, so it stops signing in.
    let mut cancel = false;
    let prompt = &mut files.prompts[index];
    let id = prompt.id;
    ui.allocate_ui_with_layout(Vec2::new(width, 0.), Layout::top_down(Align::LEFT), |ui| {
        egui::Frame::new()
            .fill(colors.panel)
            .stroke(Stroke::new(1_f32, colors.line))
            .corner_radius(RADIUS_LARGE)
            .inner_margin(16.)
            .show(ui, |ui| {
                ui.set_width(width - 32.);
                let title = match prompt.kind {
                    askpass::Kind::Secret => "Sign in",
                    askpass::Kind::YesNo => "Verify the server",
                    askpass::Kind::Confirm => "Confirm",
                    askpass::Kind::Notice => "Action needed",
                };
                ui.label(RichText::new(title).size(TITLE_SIZE).strong());
                ui.add_space(2.);
                ui.label(&prompt.text);
                ui.add_space(6.);
                match prompt.kind {
                    askpass::Kind::Secret => {
                        let field = ui.add_sized(
                            [ui.available_width(), CONTROL_HEIGHT],
                            input(&mut prompt.answer, "")
                                .password(!prompt.reveal)
                                .id(egui::Id::new(("askpass", id))),
                        );
                        if !field.has_focus() && !field.lost_focus() {
                            field.request_focus();
                        }
                        let enter =
                            field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        ui.horizontal(|ui| {
                            if accent_button(ui, "Continue", 96.).clicked() || enter {
                                answer = Some(Some(std::mem::take(&mut prompt.answer)));
                            }
                            if ui.button("Cancel").clicked() {
                                cancel = true;
                            }
                            ui.checkbox(&mut prompt.reveal, "Show");
                        });
                    }
                    askpass::Kind::YesNo => {
                        ui.label(
                            RichText::new(
                                "Compare the fingerprint with one you trust, such as from \
                                     the server's administrator. Trusting adds it to known_hosts.",
                            )
                            .size(12.)
                            .color(colors.muted),
                        );
                        ui.add_space(4.);
                        ui.horizontal(|ui| {
                            if accent_button(ui, "Trust and connect", 140.).clicked() {
                                answer = Some(Some("yes".into()));
                            }
                            if ui.button("Cancel").clicked() {
                                answer = Some(Some("no".into()));
                            }
                        });
                    }
                    askpass::Kind::Confirm => {
                        ui.horizontal(|ui| {
                            if accent_button(ui, "Allow", 80.).clicked() {
                                answer = Some(Some(String::new()));
                            }
                            if ui.button("Deny").clicked() {
                                answer = Some(None);
                            }
                        });
                    }
                    askpass::Kind::Notice => {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(RichText::new("Waiting…").color(colors.muted));
                        });
                    }
                }
            });
    });
    if let Some(answer) = answer {
        files.answer(id, answer);
    }
    if cancel {
        files.disconnect("Sign-in was cancelled.");
    }
}

fn toolbar(ui: &mut egui::Ui, files: &mut Browser, key: u64, actions: &mut Vec<Action>) {
    let compact = ui.available_width() < 720.;
    let selection = files.view.selected.iter().cloned().collect::<Vec<_>>();
    ui.horizontal(|ui| {
        ui.set_min_height(CONTROL_HEIGHT);
        ui.spacing_mut().item_spacing.x = 6.;
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let download = ui.add_enabled_ui(!selection.is_empty(), |ui| {
                if compact {
                    icon_button(ui, Icon::Download, 28., "Download selected · Enter")
                } else {
                    outline_button(ui, "Download", 0.).on_hover_text("Download selected · Enter")
                }
            });
            if download.inner.clicked() {
                actions.push(Action::Download(selection.clone()));
            }
            let upload = if compact {
                icon_button(ui, Icon::Upload, 28., "Upload · Ctrl+U")
            } else {
                outline_button(ui, "Upload", 0.).on_hover_text("Upload · Ctrl+U · or drop files")
            };
            egui::Popup::menu(&upload).show(|ui| {
                if ui.button("Files…").clicked() {
                    actions.push(Action::PickUpload { folders: false });
                }
                if ui.button("Folders…").clicked() {
                    actions.push(Action::PickUpload { folders: true });
                }
            });
            let new_folder = if compact {
                icon_button(ui, Icon::Plus, 28., "New folder · Ctrl+Shift+N")
            } else {
                outline_button(ui, "New folder", 0.).on_hover_text("Ctrl+Shift+N")
            };
            if new_folder.clicked() {
                start_new_folder(files);
            }
            let filter_id = egui::Id::new(("files-filter", key));
            // The other fields that open with the keyboard take it themselves.
            let others = files.view.path_edit.is_some()
                || files.view.rename.is_some()
                || files.view.new_folder.is_some();
            let focus = !others && files.view.focus_field;
            // A narrow pane leaves the path bar room; the filter appears with Ctrl+F.
            let shown = ui.available_width() >= 460.
                || focus
                || !files.view.filter.is_empty()
                || ui.memory(|m| m.has_focus(filter_id));
            if shown {
                let width = (ui.available_width() * 0.25).clamp(110., 200.);
                let filter =
                    search_field(ui, &mut files.view.filter, "Filter", "", filter_id, width)
                        .on_hover_text("Ctrl+F · Esc clears");
                if focus {
                    files.view.focus_field = false;
                    filter.request_focus();
                }
                if filter.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    files.view.filter.clear();
                    filter.surrender_focus();
                }
            }
            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                let back = !files.view.back.is_empty();
                if ui
                    .add_enabled_ui(back, |ui| icon_button(ui, Icon::Back, 28., "Back · Alt+←"))
                    .inner
                    .clicked()
                {
                    files.back();
                }
                let up = sftp::parent(&files.path).is_some();
                if ui
                    .add_enabled_ui(up, |ui| {
                        icon_button(ui, Icon::Up, 28., "Enclosing folder · Backspace")
                    })
                    .inner
                    .clicked()
                {
                    files.up();
                }
                if icon_button(ui, Icon::Refresh, 28., "Refresh · F5").clicked() {
                    files.refresh();
                }
                path_bar(ui, files, key);
            });
        });
    });
}

fn start_new_folder(files: &mut Browser) {
    files.view.new_folder = Some(String::new());
    files.view.rename = None;
    files.view.focus_field = true;
}

/// The current folder as breadcrumbs, which become a text field when clicked.
fn path_bar(ui: &mut egui::Ui, files: &mut Browser, key: u64) {
    let colors = theme::palette(ui.ctx());
    let width = ui.available_width().max(80.);
    let (rect, background) =
        ui.allocate_exact_size(Vec2::new(width, CONTROL_HEIGHT), Sense::click());
    let id = egui::Id::new(("files-path", key));
    if let Some(text) = &mut files.view.path_edit {
        let field = ui.put(rect, input(text, "Folder path, ~ for home").id(id));
        if std::mem::take(&mut files.view.focus_field) {
            field.request_focus();
        }
        if field.lost_focus() {
            let target = text.clone();
            files.view.path_edit = None;
            if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                files.navigate(&target);
            }
        }
        return;
    }
    ui.painter().rect(
        rect,
        RADIUS,
        colors.surface,
        Stroke::new(1_f32, colors.line),
        egui::StrokeKind::Inside,
    );
    if background
        .on_hover_text("Click to type a path · Ctrl+L")
        .on_hover_cursor(egui::CursorIcon::Text)
        .clicked()
    {
        files.view.path_edit = Some(files.path.clone());
        files.view.focus_field = true;
    }
    let inner = rect.shrink2(Vec2::new(6., 3.));
    let mut crumbs_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::left_to_right(Align::Center)),
    );
    crumbs_ui.set_clip_rect(inner);
    egui::ScrollArea::horizontal()
        .id_salt(("files-crumbs", key))
        .stick_to_right(true)
        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
        .show(&mut crumbs_ui, |ui| {
            ui.spacing_mut().item_spacing.x = 0.;
            ui.spacing_mut().button_padding = Vec2::new(5., 2.);
            let crumbs = sftp::crumbs(&files.path, &files.home);
            let last = crumbs.len() - 1;
            let separated: Vec<_> = (0..crumbs.len()).map(|i| separated(&crumbs, i)).collect();
            for (i, (name, path)) in crumbs.into_iter().enumerate() {
                if separated[i] {
                    ui.label(RichText::new("/").size(12.).color(colors.muted));
                }
                let text = RichText::new(&name).size(12.5).color(if i == last {
                    colors.text
                } else {
                    colors.muted
                });
                let crumb = ui
                    .add(egui::Button::new(text).frame(false))
                    .on_hover_text(&path);
                if crumb.clicked() && i != last {
                    files.navigate(&path);
                }
                // Items dragged onto a breadcrumb move into that folder.
                if i != last {
                    if crumb
                        .dnd_hover_payload::<Dragged>()
                        .is_some_and(|d| d.key == key)
                    {
                        ui.painter().rect_stroke(
                            crumb.rect,
                            RADIUS_SMALL,
                            Stroke::new(1_f32, colors.accent),
                            egui::StrokeKind::Inside,
                        );
                    }
                    if let Some(dragged) = crumb.dnd_release_payload::<Dragged>()
                        && dragged.key == key
                    {
                        files.move_into(&dragged.names, &path);
                    }
                }
            }
        });
}

/// Whether breadcrumb `i` follows a `/`: every one after the first, except right after the
/// root, which is a slash itself.
pub(super) fn separated(crumbs: &[(String, String)], i: usize) -> bool {
    i > 0 && crumbs[i - 1].0 != "/"
}

/// Errors, confirmations, and the new-folder field, above the listing.
fn banners(ui: &mut egui::Ui, files: &mut Browser) {
    let colors = theme::palette(ui.ctx());
    if let Some(error) = files.error.clone() {
        egui::Frame::new()
            .fill(colors.error_bg)
            .inner_margin(Margin::symmetric(12, 6))
            .corner_radius(RADIUS)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                    if ui.small_button("Dismiss").clicked() {
                        files.error = None;
                    }
                });
            });
        ui.add_space(8.);
    }
    let bar = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
        egui::Frame::new()
            .fill(colors.surface)
            .stroke(Stroke::new(1_f32, colors.line))
            .inner_margin(Margin::symmetric(12, 6))
            .corner_radius(RADIUS)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    // A row as tall as its field centers labels, field and buttons on one line.
                    ui.set_min_height(BAR_CONTROL);
                    add(ui)
                });
            });
        ui.add_space(8.);
    };
    let escape = ui.input(|i| i.key_pressed(egui::Key::Escape));
    if let Some(name) = &mut files.view.new_folder {
        let mut done = None;
        bar(ui, &mut |ui| {
            ui.label("New folder");
            let field = ui.add_sized([220., BAR_CONTROL], input(name, "Name"));
            if std::mem::take(&mut files.view.focus_field) {
                field.request_focus();
            }
            let enter = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Create").clicked() || enter {
                done = Some(true);
            }
            if ui.button("Cancel").clicked() || (field.lost_focus() && escape) {
                done = Some(false);
            }
        });
        match done {
            Some(true) => {
                let name = files.view.new_folder.take().unwrap_or_default();
                if let Err(e) = files.create_folder(&name) {
                    files.error = Some(e);
                    files.view.new_folder = Some(name);
                    files.view.focus_field = true;
                } else {
                    files.view.focused = true;
                }
            }
            Some(false) => {
                files.view.new_folder = None;
                files.view.focused = true;
            }
            None => {}
        }
    }
    match files.view.confirm.clone() {
        Some(Confirm::Delete(names)) => {
            let (mut delete, mut cancel) = (false, escape);
            bar(ui, &mut |ui| {
                let what = match names.as_slice() {
                    [name] => format!("Delete “{name}”?"),
                    names => format!("Delete {} items?", names.len()),
                };
                ui.label(RichText::new(what).strong());
                let explain = egui::Label::new(
                    RichText::new("Folders go with everything in them. This cannot be undone.")
                        .color(colors.muted),
                )
                .truncate();
                // Leave room for the buttons after the explanation.
                ui.scope(|ui| {
                    ui.set_max_width((ui.available_width() - 160.).max(0.));
                    ui.add(explain);
                });
                delete = ui
                    .add(
                        egui::Button::new(
                            RichText::new("Delete").color(ui.visuals().error_fg_color),
                        )
                        .stroke(Stroke::new(1_f32, ui.visuals().error_fg_color)),
                    )
                    .clicked();
                cancel |= ui.button("Cancel").clicked();
            });
            if delete {
                files.view.confirm = None;
                files.delete(&names);
            } else if cancel {
                files.view.confirm = None;
            }
        }
        Some(Confirm::Replace {
            local,
            folder,
            existing,
        }) => {
            let mut choice = escape.then_some(0);
            bar(ui, &mut |ui| {
                let what = match existing.as_slice() {
                    [name] => format!("“{name}” already exists here."),
                    names => format!("{} items already exist here.", names.len()),
                };
                ui.label(RichText::new(what).strong())
                    .on_hover_text(existing.join("\n"));
                if ui.button("Replace").clicked() {
                    choice = Some(2);
                }
                if local.len() > existing.len() && ui.button("Skip existing").clicked() {
                    choice = Some(1);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(0);
                }
            });
            if let Some(choice) = choice {
                files.view.confirm = None;
                match choice {
                    2 => files.upload(local, &folder, true),
                    1 => {
                        let fresh = local
                            .into_iter()
                            .filter(|p| {
                                p.file_name().is_some_and(|n| {
                                    !existing.contains(&n.to_string_lossy().into_owned())
                                })
                            })
                            .collect();
                        files.upload(fresh, &folder, false);
                    }
                    _ => {}
                }
            }
        }
        None => {}
    }
}

/// The folder listing. Returns the folder row under the pointer, for files dropped on it.
fn table(
    ui: &mut egui::Ui,
    files: &mut Browser,
    key: u64,
    order: &[usize],
    names: &[String],
    height: f32,
    actions: &mut Vec<Action>,
) -> Option<String> {
    let colors = theme::palette(ui.ctx());
    let mut target = None;
    egui::Frame::new()
        .fill(colors.panel)
        .corner_radius(RADIUS_LARGE)
        .stroke(Stroke::new(1_f32, colors.line))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            let width = ui.available_width();
            let columns = Columns::for_width(width);
            ui.spacing_mut().item_spacing.y = 0.;
            header(ui, files, &columns, width);
            // The header, and the frame's margin and outline, come off the height.
            let rows = height - 32. - 6.;
            if order.is_empty() {
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(width, rows.max(FILE_ROW)), Sense::hover());
                let text = if files.loading {
                    "Loading…".to_string()
                } else if !files.view.filter.trim().is_empty() {
                    format!("Nothing here matches “{}”.", files.view.filter.trim())
                } else if files.entries.iter().any(Entry::hidden) {
                    "Only hidden items. Tick Show hidden to see them.".into()
                } else {
                    "This folder is empty. Drop files here to upload them.".into()
                };
                ui.painter().text(
                    rect.center_top() + Vec2::new(0., 40.),
                    egui::Align2::CENTER_CENTER,
                    text,
                    egui::FontId::proportional(13.),
                    colors.muted,
                );
                return;
            }
            let mut area = egui::ScrollArea::vertical()
                .id_salt(("files-list", key))
                .max_height(rows)
                .auto_shrink([false, false]);
            if std::mem::take(&mut files.view.scroll_to_cursor)
                && let Some(index) = files
                    .view
                    .cursor
                    .as_ref()
                    .and_then(|c| names.iter().position(|n| n == c))
            {
                let id = ui.make_persistent_id(("files-list", key)).with("scroll");
                let (offset, visible) = ui
                    .data(|d| d.get_temp::<(f32, f32)>(id))
                    .unwrap_or((0., rows));
                let top = index as f32 * FILE_ROW;
                if top < offset {
                    area = area.vertical_scroll_offset(top);
                } else if top + FILE_ROW > offset + visible {
                    area = area.vertical_scroll_offset(top + FILE_ROW - visible);
                }
            }
            let output = area.show_rows(ui, FILE_ROW, order.len(), |ui, range| {
                // Only the rows on screen are drawn, and copied to draw them.
                for index in range {
                    let entry = files.entries[order[index]].clone();
                    if let Some(folder) = row(ui, files, key, &entry, names, &columns, actions) {
                        target = Some(folder);
                    }
                }
            });
            let id = ui.make_persistent_id(("files-list", key)).with("scroll");
            ui.data_mut(|d| d.insert_temp(id, (output.state.offset.y, output.inner_rect.height())));
        });
    // Show what is being dragged beside the pointer.
    if let Some(dragged) = egui::DragAndDrop::payload::<Dragged>(ui.ctx())
        && dragged.key == key
        && let Some(pos) = ui.ctx().pointer_interact_pos()
    {
        let text = match dragged.names.as_slice() {
            [name] => format!("Move “{name}”"),
            names => format!("Move {} items", names.len()),
        };
        egui::Area::new(egui::Id::new(("files-drag", key)))
            .order(egui::Order::Tooltip)
            .fixed_pos(pos + Vec2::new(14., 10.))
            .interactable(false)
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| ui.label(text));
            });
    }
    target
}

/// Column titles; clicking one sorts by it, clicking again reverses.
fn header(ui: &mut egui::Ui, files: &mut Browser, columns: &Columns, width: f32) {
    let colors = theme::palette(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 32.), Sense::hover());
    let mut x = rect.min.x;
    let mut column = |ui: &mut egui::Ui, title: &str, w: f32, sort: Option<Sort>, right: bool| {
        if w <= 0. {
            return;
        }
        let cell = Rect::from_min_size(egui::pos2(x, rect.min.y), Vec2::new(w, rect.height()));
        x += w + COLUMN_GAP;
        let active = sort.is_some_and(|s| s == files.view.sort);
        let response = ui.interact(cell, ui.id().with(("files-sort", title)), Sense::click());
        let color = if active || response.hovered() {
            colors.text
        } else {
            colors.muted
        };
        let galley = egui::WidgetText::from(caption(title)).into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            w - 16.,
            egui::TextStyle::Small,
        );
        let text_x = if right {
            cell.right() - galley.size().x - if active { 14. } else { 0. }
        } else {
            cell.left()
        };
        let text_width = galley.size().x;
        ui.painter().galley(
            egui::pos2(text_x, cell.center().y - galley.size().y / 2.),
            galley,
            color,
        );
        if active {
            icons::paint_sized(
                ui.painter(),
                if files.view.descending {
                    Icon::ChevronDown
                } else {
                    Icon::ChevronUp
                },
                egui::pos2(text_x + text_width + 8., cell.center().y),
                10.,
                color,
            );
        }
        if let Some(sort) = sort
            && response
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .clicked()
        {
            if files.view.sort == sort {
                files.view.descending = !files.view.descending;
            } else {
                files.view.sort = sort;
                // Sizes and dates read best largest and newest first.
                files.view.descending = sort != Sort::Name;
            }
        }
    };
    column(ui, "NAME", columns.name, Some(Sort::Name), false);
    column(ui, "SIZE", columns.size, Some(Sort::Size), true);
    column(
        ui,
        "MODIFIED",
        columns.modified,
        Some(Sort::Modified),
        false,
    );
    column(ui, "PERMISSIONS", columns.permissions, None, false);
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1_f32, colors.line),
    );
}

/// One item. Returns its path if it is a folder under the pointer.
fn row(
    ui: &mut egui::Ui,
    files: &mut Browser,
    key: u64,
    entry: &Entry,
    names: &[String],
    columns: &Columns,
    actions: &mut Vec<Action>,
) -> Option<String> {
    let colors = theme::palette(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(
        Vec2::new(ui.available_width(), FILE_ROW),
        Sense::click_and_drag(),
    );
    let name = &entry.name;
    let selected = files.view.selected.contains(name);
    let cursor = files.view.cursor.as_ref() == Some(name);
    let hovered = ui.rect_contains_pointer(rect);
    let dropping = entry.dir
        && response
            .dnd_hover_payload::<Dragged>()
            .is_some_and(|d| d.key == key && !d.names.contains(name));
    let band = rect.shrink2(Vec2::new(0., 1.));
    if dropping {
        ui.painter().rect(
            band,
            RADIUS,
            colors.accent.gamma_multiply(0.15),
            Stroke::new(1_f32, colors.accent),
            egui::StrokeKind::Inside,
        );
    } else if selected || hovered {
        let fill = if selected {
            colors.selected
        } else {
            colors.hover
        };
        ui.painter().rect_filled(band, RADIUS, fill);
    }
    if cursor && files.view.focused && !selected {
        ui.painter().rect_stroke(
            band,
            RADIUS,
            Stroke::new(1_f32, colors.line),
            egui::StrokeKind::Inside,
        );
    }

    let path = sftp::join(&files.path, name);
    let (icon, color) = if entry.dir {
        (Icon::Folder, colors.accent)
    } else {
        (Icon::File, colors.muted)
    };
    let icon_center = egui::pos2(rect.left() + 12., rect.center().y);
    icons::paint_sized(ui.painter(), icon, icon_center, ICON_SIZE, color);
    if entry.link {
        // A small arrow marks links.
        let base = icon_center + Vec2::new(-6., 6.);
        ui.painter().line_segment(
            [base, base + Vec2::new(4., -4.)],
            Stroke::new(1.5_f32, colors.text),
        );
    }
    let mut x = rect.left() + 26.;
    let name_width = columns.name - 26.;
    let renaming = files
        .view
        .rename
        .as_ref()
        .is_some_and(|(from, _)| from == name);
    if renaming {
        let cell = Rect::from_min_size(
            egui::pos2(x, rect.top() + 2.),
            Vec2::new(name_width, FILE_ROW - 4.),
        );
        let (_, text) = files.view.rename.as_mut().unwrap();
        let field = ui.put(
            cell,
            egui::TextEdit::singleline(text)
                .id(egui::Id::new(("files-rename", key)))
                .margin(Margin::symmetric(6, 3))
                .vertical_align(Align::Center),
        );
        if std::mem::take(&mut files.view.focus_field) {
            field.request_focus();
            // Select the name without its extension, as file managers do.
            let stem = text
                .rfind('.')
                .filter(|i| *i > 0 && !entry.dir)
                .unwrap_or(text.chars().count());
            let stem = text[..stem].chars().count();
            if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), field.id) {
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(stem),
                    )));
                state.store(ui.ctx(), field.id);
            }
        }
        if field.lost_focus() {
            let (from, to) = files.view.rename.take().unwrap();
            files.view.focused = true;
            if ui.input(|i| i.key_pressed(egui::Key::Enter))
                && let Err(e) = files.rename(&from, &to)
            {
                files.error = Some(e);
            }
        }
    } else {
        let text = RichText::new(name).size(13.).color(if entry.hidden() {
            colors.muted
        } else {
            colors.text
        });
        cell_text(ui, rect, x, name_width, text, false);
    }
    x += name_width + COLUMN_GAP;
    let size = if entry.dir {
        String::new()
    } else {
        sftp::size(entry.size)
    };
    cell_text(
        ui,
        rect,
        x,
        columns.size,
        RichText::new(size).size(12.).color(colors.muted),
        true,
    );
    x += columns.size + COLUMN_GAP;
    if columns.modified > 0. {
        let date = entry.modified.map(sftp::modified).unwrap_or_default();
        cell_text(
            ui,
            rect,
            x,
            columns.modified,
            RichText::new(date).size(12.).color(colors.muted),
            false,
        );
        x += columns.modified + COLUMN_GAP;
    }
    if columns.permissions > 0. {
        cell_text(
            ui,
            rect,
            x,
            columns.permissions,
            RichText::new(entry.permissions())
                .monospace()
                .size(11.5)
                .color(colors.muted),
            false,
        );
    }

    if response.clicked() {
        files.view.focused = true;
        let modifiers = ui.input(|i| i.modifiers);
        click(&mut files.view, names, name, modifiers);
    }
    if response.secondary_clicked() && !selected {
        files.view.select_only(name);
    }
    if response.double_clicked() && !renaming {
        open(files, std::slice::from_ref(name), actions);
    }
    if response.dragged() && !renaming {
        let names = if selected {
            // Keep the listing's order.
            names
                .iter()
                .filter(|n| files.view.selected.contains(*n))
                .cloned()
                .collect()
        } else {
            vec![name.clone()]
        };
        response.dnd_set_drag_payload(Dragged { key, names });
    }
    if entry.dir
        && let Some(dragged) = response.dnd_release_payload::<Dragged>()
        && dragged.key == key
        && !dragged.names.contains(name)
    {
        files.move_into(&dragged.names, &path);
    }
    let tooltip = if entry.link {
        format!("{name}\nLink")
    } else if entry.special {
        format!("{name}\nSpecial file")
    } else {
        name.clone()
    };
    let response = response.on_hover_text_at_pointer(tooltip);
    response.context_menu(|ui| {
        let chosen: Vec<String> = if files.view.selected.contains(name) {
            names
                .iter()
                .filter(|n| files.view.selected.contains(*n))
                .cloned()
                .collect()
        } else {
            vec![name.clone()]
        };
        if entry.dir && chosen.len() == 1 && ui.button("Open").clicked() {
            files.navigate(&path);
            ui.close();
        }
        if ui.button("Download…").clicked() {
            actions.push(Action::Download(chosen.clone()));
            ui.close();
        }
        if chosen.len() == 1 && ui.button("Rename").clicked() {
            start_rename(files, name);
            ui.close();
        }
        if ui.button("Copy path").clicked() {
            actions.push(Action::Copy(path.clone()));
            ui.close();
        }
        ui.separator();
        if ui.button("Delete…").clicked() {
            files.view.confirm = Some(Confirm::Delete(chosen));
            ui.close();
        }
    });
    (entry.dir && hovered).then_some(path)
}

/// Draws `text` in one column of a row, shortened to fit.
fn cell_text(ui: &egui::Ui, rect: Rect, x: f32, width: f32, text: RichText, right: bool) {
    if width <= 0. {
        return;
    }
    let galley = egui::WidgetText::from(text).into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        width,
        egui::TextStyle::Body,
    );
    let left = if right {
        x + width - galley.size().x
    } else {
        x
    };
    ui.painter().galley(
        egui::pos2(left, rect.center().y - galley.size().y / 2.),
        galley,
        theme::palette(ui.ctx()).text,
    );
}

/// Selects after a click: alone, toggled with Ctrl or Cmd, or as a range with Shift.
fn click(view: &mut sftp::View, names: &[String], name: &str, modifiers: egui::Modifiers) {
    let position = |n: &str| names.iter().position(|v| v == n);
    if modifiers.shift
        && let (Some(from), Some(to)) = (view.anchor.as_deref().and_then(position), position(name))
    {
        if !modifiers.command {
            view.selected.clear();
        }
        let (low, high) = (from.min(to), from.max(to));
        view.selected.extend(names[low..=high].iter().cloned());
        view.cursor = Some(name.into());
    } else if modifiers.command {
        if !view.selected.remove(name) {
            view.selected.insert(name.into());
        }
        view.cursor = Some(name.into());
        view.anchor = Some(name.into());
    } else {
        view.select_only(name);
    }
}

/// Opens a single folder, or downloads what was chosen.
fn open(files: &mut Browser, chosen: &[String], actions: &mut Vec<Action>) {
    if let [name] = chosen
        && files.entries.iter().any(|e| &e.name == name && e.dir)
    {
        files.navigate(name);
    } else if !chosen.is_empty() {
        actions.push(Action::Download(chosen.to_vec()));
    }
}

fn start_rename(files: &mut Browser, name: &str) {
    files.view.rename = Some((name.into(), name.into()));
    files.view.new_folder = None;
    files.view.focus_field = true;
}

/// Keyboard navigation while the browser has the keyboard and no text field does.
fn keys(ui: &mut egui::Ui, files: &mut Browser, names: &[String], actions: &mut Vec<Action>) {
    use egui::{Key, Modifiers};
    let ctrl = Modifiers::COMMAND;
    let shift = Modifiers::SHIFT;
    let press = |modifiers, key| ui.input_mut(|i| i.consume_key(modifiers, key));
    // Modified keys are checked first: a plain key also matches with Shift or Alt held.
    if press(ctrl | shift, Key::N) {
        return start_new_folder(files);
    }
    if press(Modifiers::ALT, Key::ArrowUp) || press(Modifiers::NONE, Key::Backspace) {
        return files.up();
    }
    if press(Modifiers::ALT, Key::ArrowLeft) {
        return files.back();
    }
    if press(ctrl, Key::L) {
        files.view.path_edit = Some(files.path.clone());
        files.view.focus_field = true;
        return;
    }
    if press(ctrl, Key::F) {
        files.view.focus_field = true;
        return;
    }
    if press(ctrl, Key::U) {
        return actions.push(Action::PickUpload { folders: false });
    }
    if press(ctrl, Key::R) || press(Modifiers::NONE, Key::F5) {
        return files.refresh();
    }
    if press(ctrl, Key::A) {
        files.view.selected = names.iter().cloned().collect();
        return;
    }
    let chosen: Vec<String> = names
        .iter()
        .filter(|n| files.view.selected.contains(*n))
        .cloned()
        .collect();
    if press(Modifiers::NONE, Key::Delete) && !chosen.is_empty() {
        files.view.confirm = Some(Confirm::Delete(chosen));
        return;
    }
    if press(Modifiers::NONE, Key::F2)
        && let Some(name) = files.view.cursor.clone().filter(|c| chosen.contains(c))
    {
        return start_rename(files, &name);
    }
    if press(Modifiers::NONE, Key::Enter) {
        return open(files, &chosen, actions);
    }
    if press(Modifiers::NONE, Key::Escape) {
        files.view.confirm = None;
        files.view.selected.clear();
        return;
    }
    if names.is_empty() {
        return;
    }
    let current = files
        .view
        .cursor
        .as_ref()
        .and_then(|c| names.iter().position(|n| n == c));
    let last = names.len() - 1;
    let mut moved = None;
    for (key, step) in [
        (Key::ArrowDown, 1isize),
        (Key::ArrowUp, -1),
        (Key::PageDown, 10),
        (Key::PageUp, -10),
        (Key::Home, -isize::MAX),
        (Key::End, isize::MAX),
    ] {
        let extend = press(shift, key);
        if extend || press(Modifiers::NONE, key) {
            // Without a cursor, moves start just outside the list in their direction.
            let from = current.map_or(if step > 0 { -1 } else { names.len() as isize }, |i| {
                i as isize
            });
            let next = from.saturating_add(step).clamp(0, last as isize) as usize;
            moved = Some((next, extend));
            break;
        }
    }
    if let Some((next, extend)) = moved {
        let name = names[next].clone();
        if extend {
            click(&mut files.view, names, &name, Modifiers::SHIFT);
        } else {
            files.view.select_only(&name);
        }
        files.view.scroll_to_cursor = true;
    }
}

/// The item count and selection under the listing.
fn status_bar(ui: &mut egui::Ui, files: &mut Browser, names: &[String]) {
    let colors = theme::palette(ui.ctx());
    ui.horizontal(|ui| {
        ui.set_min_height(28.);
        let selected: Vec<&Entry> = files
            .entries
            .iter()
            .filter(|e| files.view.selected.contains(&e.name))
            .collect();
        let mut text = match names.len() {
            1 => "1 item".to_string(),
            n => format!("{n} items"),
        };
        if !selected.is_empty() {
            let bytes: u64 = selected.iter().filter(|e| !e.dir).map(|e| e.size).sum();
            text.push_str(&format!(" · {} selected", selected.len()));
            if bytes > 0 {
                text.push_str(&format!(" ({})", sftp::size(bytes)));
            }
        }
        ui.label(RichText::new(text).size(12.).color(colors.muted));
        if let Some((notice, _)) = &files.notice {
            let (dot, _) = ui.allocate_exact_size(Vec2::splat(8.), Sense::hover());
            ui.painter().circle_filled(dot.center(), 3., colors.accent);
            ui.label(RichText::new(notice).size(12.));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.checkbox(
                &mut files.view.show_hidden,
                RichText::new("Show hidden").size(12.),
            );
            if files.loading || files.busy > 0 {
                ui.add(egui::Spinner::new().size(14.));
            }
        });
    });
}

/// The queue of uploads and downloads, newest last, scrolling past `max_height` of rows.
fn transfers(ui: &mut egui::Ui, files: &mut Browser, max_height: f32) {
    let colors = theme::palette(ui.ctx());
    let active = files.active_transfers();
    let failed = files
        .transfers
        .iter()
        .filter(|t| {
            matches!(
                t.state,
                TransferState::Stopped(Failure::Error(_) | Failure::Exists(_))
            )
        })
        .count();
    let mut actions = vec![];
    egui::Frame::new()
        .fill(colors.panel)
        .corner_radius(RADIUS_LARGE)
        .stroke(Stroke::new(1_f32, colors.line))
        .inner_margin(Margin::symmetric(10, 4))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.set_min_height(28.);
                ui.label(caption("TRANSFERS").color(colors.muted));
                let mut summary = vec![];
                if active > 0 {
                    summary.push(format!("{active} in progress"));
                }
                if failed > 0 {
                    summary.push(format!("{failed} need attention"));
                }
                ui.label(
                    RichText::new(summary.join(" · "))
                        .size(12.)
                        .color(colors.muted),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if active < files.transfers.len() && ui.small_button("Clear finished").clicked()
                    {
                        files.clear_finished();
                    }
                    if active > 0 && ui.small_button("Cancel all").clicked() {
                        files.cancel_all();
                    }
                });
            });
            egui::ScrollArea::vertical()
                .id_salt("files-transfers")
                .max_height((TRANSFER_ROW * TRANSFER_ROWS as f32).min(max_height))
                .auto_shrink([false, true])
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.;
                    for transfer in &files.transfers {
                        transfer_row(ui, transfer, &mut actions);
                    }
                });
        });
    for (id, action) in actions {
        match action {
            TransferAction::Cancel => files.cancel(id),
            TransferAction::Retry => files.retry(id, false),
            TransferAction::Replace => files.retry(id, true),
        }
    }
}

enum TransferAction {
    Cancel,
    Retry,
    Replace,
}

/// One transfer on two lines: the name with its status, a progress bar beneath while it
/// runs, and where it goes. The full paths show on hover.
fn transfer_row(ui: &mut egui::Ui, t: &Transfer, actions: &mut Vec<(u64, TransferAction)>) {
    let colors = theme::palette(ui.ctx());
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(Vec2::new(width, TRANSFER_ROW), Sense::hover());
    let (icon, verb) = match t.direction {
        Direction::Upload => (Icon::Upload, "Upload to"),
        Direction::Download => (Icon::Download, "Download to"),
    };
    let tint = match &t.state {
        TransferState::Done => colors.accent,
        TransferState::Stopped(Failure::Error(_)) => ui.visuals().error_fg_color,
        TransferState::Stopped(Failure::Exists(_)) => ui.visuals().warn_fg_color,
        _ => colors.muted,
    };
    icons::paint_sized(
        ui.painter(),
        icon,
        egui::pos2(rect.left() + 8., rect.center().y),
        ICON_SIZE,
        tint,
    );
    // The button sits at the right, centered on the row; a finished transfer keeps an
    // empty slot the size of the cancel button, so the statuses line up.
    let mut buttons = ui.new_child(
        UiBuilder::new()
            .max_rect(Rect::from_min_max(
                egui::pos2(rect.right() - 120., rect.top()),
                rect.max,
            ))
            .layout(Layout::right_to_left(Align::Center)),
    );
    match &t.state {
        TransferState::Queued | TransferState::Running => {
            if icon_button(&mut buttons, Icon::Close, 22., "Cancel").clicked() {
                actions.push((t.id, TransferAction::Cancel));
            }
        }
        TransferState::Stopped(Failure::Exists(_)) => {
            if buttons.small_button("Replace").clicked() {
                actions.push((t.id, TransferAction::Replace));
            }
        }
        TransferState::Stopped(_) => {
            if buttons.small_button("Retry").clicked() {
                actions.push((t.id, TransferAction::Retry));
            }
        }
        TransferState::Done => {
            buttons.allocate_exact_size(Vec2::splat(22.), Sense::hover());
        }
    }
    let right = buttons.min_rect().left() - 10.;
    let column = Rect::from_min_max(
        egui::pos2(rect.left() + 24., rect.top()),
        egui::pos2(right, rect.bottom()),
    );
    if column.width() <= 0. {
        return;
    }
    let line = |center: f32, height: f32| {
        Rect::from_center_size(
            egui::pos2(column.center().x, rect.top() + center),
            Vec2::new(column.width(), height),
        )
    };
    let (first, bar, second) = (line(13., 18.), line(26., 4.), line(37., 14.));
    // The status keeps its full width up to most of the line; the name takes the rest.
    let (status, color) = transfer_status(ui, t);
    let status_galley = egui::WidgetText::from(RichText::new(&status).size(12.).color(color))
        .into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            column.width() * 0.6,
            egui::TextStyle::Body,
        );
    let status_width = status_galley.size().x;
    ui.painter().galley(
        egui::pos2(
            first.right() - status_width,
            first.center().y - status_galley.size().y / 2.,
        ),
        status_galley,
        colors.text,
    );
    cell_text(
        ui,
        first,
        first.left(),
        first.width() - status_width - 12.,
        RichText::new(&t.name).size(13.),
        false,
    );
    if let (TransferState::Running | TransferState::Queued, Some(fraction)) =
        (&t.state, t.fraction())
    {
        // In a child, so the row's layout keeps its place below the whole row.
        ui.new_child(UiBuilder::new().max_rect(bar)).add(
            egui::ProgressBar::new(fraction)
                .desired_height(bar.height())
                .fill(colors.accent)
                .corner_radius(2),
        );
    }
    let small = |text: String| RichText::new(text).size(11.5).color(colors.muted);
    let destination = fit_path(ui, verb, &t.destination, second.width(), small);
    cell_text(
        ui,
        second,
        second.left(),
        second.width(),
        destination,
        false,
    );
    let (from, to) = t.paths();
    response.on_hover_text(format!("{status}\n\nFrom  {from}\nTo  {to}"));
}

/// "`verb` `path`" styled by `style`, with the start of the path dropped as needed to fit
/// `width`: the folders nearest the item say the most about where it goes.
fn fit_path(
    ui: &egui::Ui,
    verb: &str,
    path: &str,
    width: f32,
    style: impl Fn(String) -> RichText,
) -> RichText {
    let fits = |text: &RichText| {
        egui::WidgetText::from(text.clone())
            .into_galley(
                ui,
                Some(egui::TextWrapMode::Extend),
                f32::INFINITY,
                egui::TextStyle::Body,
            )
            .size()
            .x
            <= width
    };
    let mut text = style(format!("{verb} {path}"));
    for (index, _) in path.char_indices().skip(1) {
        if fits(&text) {
            break;
        }
        text = style(format!("{verb} …{}", &path[index..]));
    }
    text
}

/// What a transfer is doing, in words, and the color to show it in.
fn transfer_status(ui: &egui::Ui, t: &Transfer) -> (String, Color32) {
    let colors = theme::palette(ui.ctx());
    let mut text = match &t.state {
        TransferState::Queued => "Waiting".to_string(),
        TransferState::Running => {
            let mut text = match t.total {
                Some(total) => format!("{} of {}", sftp::size(t.done), sftp::size(total)),
                None => "Counting…".into(),
            };
            if t.files > 1 {
                text.push_str(&format!(" · {} of {} files", t.files_done, t.files));
            }
            if let Some(rate) = t.rate() {
                text.push_str(&format!(" · {}/s", sftp::size(rate as u64)));
            }
            text
        }
        TransferState::Done => match t.files {
            0 | 1 => format!("Done · {}", sftp::size(t.done)),
            n => format!("Done · {n} files, {}", sftp::size(t.done)),
        },
        TransferState::Stopped(Failure::Cancelled) => "Cancelled".into(),
        TransferState::Stopped(Failure::Exists(message) | Failure::Error(message)) => {
            message.clone()
        }
    };
    if t.skipped > 0 {
        text.push_str(&format!(" · {} skipped", t.skipped));
    }
    let color = match &t.state {
        TransferState::Stopped(Failure::Error(_)) => ui.visuals().error_fg_color,
        TransferState::Stopped(Failure::Exists(_)) => ui.visuals().warn_fg_color,
        _ => colors.muted,
    };
    (text, color)
}

/// Uploads files dropped from the system's file manager: into the folder row under the
/// pointer, or the current folder. Shows where they will go while they hover.
fn os_drops(
    ui: &mut egui::Ui,
    files: &mut Browser,
    area: Rect,
    fallback: bool,
    target: Option<String>,
) {
    let colors = theme::palette(ui.ctx());
    let (hovering, dropped, pointer) = ui.input(|i| {
        (
            !i.raw.hovered_files.is_empty(),
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect::<Vec<_>>(),
            i.pointer.latest_pos(),
        )
    });
    let mine = match pointer {
        Some(pos) => area.contains(pos),
        None => fallback,
    };
    if !mine {
        return;
    }
    let folder = target
        .filter(|_| pointer.is_some())
        .unwrap_or(files.path.clone());
    if hovering {
        let painter = ui.ctx().layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("files-drop"),
        ));
        painter.rect(
            area,
            RADIUS_LARGE,
            colors.bg.gamma_multiply(0.85),
            Stroke::new(2_f32, colors.accent),
            egui::StrokeKind::Inside,
        );
        painter.text(
            area.center(),
            egui::Align2::CENTER_CENTER,
            format!("Drop to upload to {folder}"),
            egui::FontId::proportional(15.),
            colors.text,
        );
    }
    if !dropped.is_empty() {
        files.offer_upload(dropped, &folder);
    }
}

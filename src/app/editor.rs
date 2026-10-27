//! The dialog that creates and edits a connection.
use super::*;

pub(super) const NOTES_ID: &str = "connection-notes";

impl App {
    pub(super) fn editor(&mut self, ctx: &egui::Context) {
        let colors = theme::palette(ctx);
        let Modal::Editor(c) = &self.modal else {
            return;
        };
        let mut c = c.clone();
        let mut save = false;
        let mut cancel = false;
        let rdp = c.protocol == Protocol::Rdp;
        let groups: std::collections::BTreeSet<_> =
            self.connections.iter().map(|c| c.group.clone()).collect();
        let fields_height = (ctx.content_rect().height() - 300.).max(200.);
        let parent = self.parent;
        let mut notes_focused = false;
        dialog("edit-connection", ctx).show(ctx, |ui| {
            ui.set_width(440.);
            let title = if c.id == 0 {
                "New connection"
            } else {
                "Edit connection"
            };
            ui.label(RichText::new(title).size(TITLE_SIZE).strong());
            ui.add_space(8.);
            egui::ScrollArea::vertical()
                .id_salt("editor-fields")
                .max_height(fields_height)
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let half = (ui.available_width() - ui.spacing().item_spacing.x) / 2.;
                        for protocol in [Protocol::Rdp, Protocol::Ssh] {
                            let button = egui::Button::new(protocol.label())
                                .selected(c.protocol == protocol);
                            if ui.add_sized([half, CONTROL_HEIGHT], button).clicked() {
                                if c.port == c.protocol.port() {
                                    c.port = protocol.port();
                                }
                                c.protocol = protocol;
                            }
                        }
                    });
                    ui.add_space(10.);
                    field(
                        ui,
                        "Connection name",
                        &mut c.name,
                        "e.g. Office workstation",
                    );
                    ui.horizontal_top(|ui| {
                        let port_width = 96.;
                        let host_width =
                            ui.available_width() - port_width - ui.spacing().item_spacing.x;
                        labeled(ui, "Host / IP address", |ui| {
                            ui.add_sized(
                                [host_width, CONTROL_HEIGHT],
                                input(&mut c.host, "e.g. 192.168.1.20"),
                            );
                        });
                        labeled(ui, "Port", |ui| {
                            // Digits only; an empty field reads as 0, which validation rejects.
                            let mut port = if c.port == 0 {
                                String::new()
                            } else {
                                c.port.to_string()
                            };
                            let response =
                                ui.add_sized([port_width, CONTROL_HEIGHT], input(&mut port, ""));
                            if response.changed() {
                                let digits: String =
                                    port.chars().filter(char::is_ascii_digit).collect();
                                c.port = digits.parse().unwrap_or(if digits.is_empty() {
                                    0
                                } else {
                                    c.port
                                });
                            }
                        });
                    });
                    let user_hint = if rdp { "DOMAIN\\username" } else { "username" };
                    field(ui, "Username (optional)", &mut c.username, user_hint);
                    labeled(ui, "Group", |ui| {
                        ui.horizontal(|ui| {
                            let button = CONTROL_HEIGHT;
                            ui.add_sized(
                                [
                                    ui.available_width() - button - ui.spacing().item_spacing.x,
                                    CONTROL_HEIGHT,
                                ],
                                input(&mut c.group, "Personal"),
                            );
                            let choose = ui
                                .add_enabled(
                                    !groups.is_empty(),
                                    egui::Button::new("").min_size(Vec2::splat(button)),
                                )
                                .on_hover_text("Choose an existing group");
                            icons::paint_sized(
                                ui.painter(),
                                Icon::ChevronDown,
                                choose.rect.center(),
                                14.,
                                if choose.enabled() {
                                    colors.text
                                } else {
                                    colors.muted
                                },
                            );
                            egui::Popup::menu(&choose).show(|ui| {
                                for group in &groups {
                                    if ui.button(group).clicked() {
                                        c.group = group.clone();
                                    }
                                }
                            });
                        });
                    });
                    labeled(ui, "Notes (optional)", |ui| {
                        let notes = egui::TextEdit::multiline(&mut c.notes)
                            .id(NOTES_ID.into())
                            .hint_text(
                                RichText::new("Searchable, e.g. who to ask before rebooting")
                                    .weak(),
                            )
                            .margin(egui::Margin::symmetric(10, 6))
                            .desired_rows(3)
                            .desired_width(f32::INFINITY);
                        notes_focused = ui.add(notes).has_focus();
                    });
                    ui.add_space(2.);
                    ui.checkbox(&mut c.favorite, "Add to favorites");
                    // The optional settings start a section of their own.
                    ui.add_space(10.);
                    ui.separator();
                    ui.add_space(2.);
                    if c.protocol == Protocol::Rdp {
                        rdp_options(ui, &mut c.rdp);
                    } else {
                        ssh_options(ui, &mut c.ssh, parent);
                    }
                });
            ui.add_space(8.);
            let help = if rdp {
                rdp::SIGN_IN_HELP
            } else {
                "Uses your OpenSSH client, including your SSH config and agent.\n\
                 Host verification and password prompts appear in the terminal."
            };
            ui.label(RichText::new(help).size(12.).color(colors.muted));
            if !self.editor_error.is_empty() {
                ui.colored_label(ui.visuals().error_fg_color, &self.editor_error);
            }
            ui.add_space(12.);
            ui.horizontal(|ui| {
                save = accent_button(ui, "Save connection", 140.).clicked();
                cancel = outline_button(ui, "Cancel", 80.).clicked();
            });
            ui.label(
                RichText::new("Enter saves · Esc cancels")
                    .size(11.)
                    .color(colors.muted),
            );
        });
        let (enter, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::Escape),
            )
        });
        if cancel || escape {
            self.modal = Modal::None;
            return;
        }
        // Enter starts a new line in the notes rather than saving.
        save |= enter && !notes_focused;
        if save && let Err(e) = c.validate() {
            self.editor_error = e;
            save = false;
        }
        self.modal = Modal::Editor(c.clone());
        if save {
            let mut next = self.connections.clone();
            if c.id == 0 {
                c.id = next
                    .iter()
                    .map(|c| c.id)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1);
                next.push(c);
            } else if let Some(existing) = next.iter_mut().find(|v| v.id == c.id) {
                let base = self.editor_base.as_ref().unwrap_or(existing);
                let (merged, conflicts) = model::merge(base, &c, existing);
                if !conflicts.is_empty() {
                    // The editor takes the others' other changes; saving again keeps the rest.
                    self.editor_error = format!(
                        "Someone else changed the {} while you edited. Save again to replace \
                         their values with yours, or Cancel to keep theirs.",
                        list(&conflicts)
                    );
                    self.editor_base = Some(existing.clone());
                    self.modal = Modal::Editor(merged);
                    return;
                }
                *existing = merged;
            } else {
                // Saving again adds it as a new connection.
                c.id = 0;
                self.editor_error =
                    "Someone else deleted this connection while you edited. Save again to add it \
                     back."
                        .into();
                self.editor_base = None;
                self.modal = Modal::Editor(c);
                return;
            }
            if self.persist(next) {
                self.modal = Modal::None;
            }
        }
    }
}

pub(super) fn ssh_options(
    ui: &mut egui::Ui,
    options: &mut model::SshOptions,
    parent: Option<platform::Window>,
) {
    egui::CollapsingHeader::new("SSH options")
        .id_salt("ssh-options")
        .default_open(*options != model::SshOptions::default())
        .show(ui, |ui| {
            field(
                ui,
                "Jump host (optional)",
                &mut options.jump,
                "user@bastion.example.com:22",
            );
            labeled(ui, "Identity file (optional)", |ui| {
                ui.horizontal(|ui| {
                    let browse = 80.;
                    ui.add_sized(
                        [
                            ui.available_width() - browse - ui.spacing().item_spacing.x,
                            CONTROL_HEIGHT,
                        ],
                        input(
                            &mut options.identity,
                            "Default: from your SSH config or agent",
                        ),
                    );
                    let button =
                        egui::Button::new("Browse…").min_size(Vec2::new(browse, CONTROL_HEIGHT));
                    if ui.add(button).clicked()
                        && let Some(path) =
                            file_dialog::choose(parent, file_dialog::Kind::Key).first()
                    {
                        options.identity = path.display().to_string();
                    }
                });
            });
            field(
                ui,
                "Port forwards (optional)",
                &mut options.forwards,
                "L8080:localhost:80, R9000:localhost:9000, D1080",
            );
            ui.label(
                RichText::new(
                    "L forwards a local port to the remote side, R the reverse, and D opens a \
                     SOCKS proxy.",
                )
                .size(11.)
                .color(theme::palette(ui.ctx()).muted),
            );
        });
}

pub(super) fn rdp_options(ui: &mut egui::Ui, options: &mut model::RdpOptions) {
    use model::{Audio, Display, Keys};
    egui::CollapsingHeader::new("Remote desktop options")
        .id_salt("rdp-options")
        .default_open(*options != model::RdpOptions::default())
        .show(ui, |ui| {
            field(
                ui,
                "RD Gateway (optional)",
                &mut options.gateway,
                "gateway.example.com",
            );
            ui.add_space(6.);
            // Every option has its label above it, as the fields do.
            labeled(ui, "Use on the remote computer", |ui| {
                egui::Grid::new("rdp-resources")
                    .num_columns(3)
                    .spacing([24., 8.])
                    .show(ui, |ui| {
                        ui.checkbox(&mut options.clipboard, "Clipboard");
                        ui.checkbox(&mut options.drives, "Local drives");
                        ui.checkbox(&mut options.printers, "Printers");
                        ui.end_row();
                        ui.checkbox(&mut options.smart_cards, "Smart cards");
                        ui.checkbox(&mut options.microphone, "Microphone");
                        ui.end_row();
                    });
            });
            ui.add_space(6.);
            let width = ui.available_width();
            // Drop-downs as tall as the text fields above them.
            ui.spacing_mut().interact_size.y = CONTROL_HEIGHT;
            labeled(ui, "Remote audio", |ui| {
                egui::ComboBox::from_id_salt("rdp-audio")
                    .width(width)
                    .selected_text(options.audio.label())
                    .show_ui(ui, |ui| {
                        for audio in Audio::ALL {
                            ui.selectable_value(&mut options.audio, audio, audio.label());
                        }
                    });
            });
            ui.add_space(6.);
            labeled(ui, "System key combinations, such as Alt+Tab", |ui| {
                egui::ComboBox::from_id_salt("rdp-keys")
                    .width(width)
                    .selected_text(options.keys.label())
                    .show_ui(ui, |ui| {
                        for keys in Keys::ALL {
                            ui.selectable_value(&mut options.keys, keys, keys.label());
                        }
                    });
            });
            ui.add_space(6.);
            labeled(ui, "Resolution", |ui| {
                ui.horizontal(|ui| {
                    ui.radio_value(&mut options.display, Display::Fit, "Fit to tab");
                    let fixed = matches!(options.display, Display::Fixed { .. });
                    if ui.radio(fixed, "Fixed").clicked() && !fixed {
                        options.display = Display::Fixed {
                            width: 1920,
                            height: 1080,
                        };
                    }
                    if let Display::Fixed { width, height } = &mut options.display {
                        ui.add(egui::DragValue::new(width).range(200..=8192));
                        ui.label("×");
                        ui.add(egui::DragValue::new(height).range(200..=8192));
                    }
                });
            });
        });
    // Shown outside the collapsed options too, since the defaults can be affected.
    let missing = rdp::unsupported(options);
    if !missing.is_empty() {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            format!(
                "Not supported on this platform, so not used: {}.",
                missing.join("; ")
            ),
        );
    }
}

/// `items` as an English list: "a", "a and b", "a, b and c".
fn list(items: &[&str]) -> String {
    match items {
        [] => String::new(),
        [one] => (*one).into(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

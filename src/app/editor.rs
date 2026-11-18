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
        let focused = ctx.memory(|memory| memory.focused());
        let enter_saves = !egui::Popup::is_any_open(ctx)
            && focused.is_none_or(|id| egui::TextEdit::load_state(ctx, id).is_some());
        let mut save = false;
        let mut cancel = false;
        let groups: std::collections::BTreeSet<_> =
            self.connections.iter().map(|c| c.group.clone()).collect();
        let parent = self.parent;
        let mut notes_focused = false;
        dialog("edit-connection", ctx).show(ctx, |ui| {
            ui.set_width(dialog_width(ctx, 440.));
            ui.spacing_mut().item_spacing.y = 6.;
            let title = if c.id == 0 {
                "New connection"
            } else {
                "Edit connection"
            };
            ui.label(RichText::new(title).size(TITLE_SIZE).strong());
            ui.horizontal(|ui| {
                let count = Protocol::ALL.len() as f32;
                let width =
                    (ui.available_width() - ui.spacing().item_spacing.x * (count - 1.)) / count;
                for protocol in Protocol::ALL {
                    let button =
                        egui::Button::new(protocol.label()).selected(c.protocol == protocol);
                    if ui.add_sized([width, CONTROL_HEIGHT], button).clicked() {
                        switch_protocol(&mut c, protocol, self.editor_port.is_some());
                    }
                }
            });
            let footer_error = validation_field(&self.editor_error)
                .is_none_or(|field| !validation_visible(c.protocol, field));
            let compact_error = ctx.content_rect().height() < 480.;
            let error_height = if footer_error && !self.editor_error.is_empty() && compact_error {
                ui.text_style_height(&egui::TextStyle::Body) + 6.
            } else if footer_error && !self.editor_error.is_empty() {
                ui.fonts_mut(|fonts| {
                    fonts.layout(
                        self.editor_error.clone(),
                        egui::FontId::proportional(13.),
                        ui.visuals().error_fg_color,
                        ui.available_width(),
                    )
                })
                .size()
                .y + 6.
            } else {
                0.
            };
            let fields_height = (ctx.content_rect().height() - 230. - error_height).max(24.);
            egui::ScrollArea::vertical()
                .id_salt("editor-fields")
                .max_height(fields_height)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    field(
                        ui,
                        "Connection name",
                        &mut c.name,
                        "e.g. Office workstation",
                    );
                    validation(ui, &self.editor_error, "name");
                    ui.horizontal_top(|ui| {
                        let port_width = 96.;
                        let host_width =
                            ui.available_width() - port_width - ui.spacing().item_spacing.x;
                        labeled(ui, "Host / IP address", |ui, label| {
                            ui.set_width(host_width);
                            ui.add_sized(
                                [host_width, CONTROL_HEIGHT],
                                input(&mut c.host, "e.g. 192.168.1.20"),
                            )
                            .labelled_by(label);
                            validation(ui, &self.editor_error, "host");
                        });
                        labeled(ui, "Port", |ui, label| {
                            ui.set_width(port_width);
                            let mut port = self
                                .editor_port
                                .clone()
                                .unwrap_or_else(|| c.port.to_string());
                            let response = ui
                                .add_sized(
                                    [port_width, CONTROL_HEIGHT],
                                    input(&mut port, "1–65535")
                                        .id(egui::Id::new("connection-port")),
                                )
                                .labelled_by(label);
                            if response.changed() {
                                port.retain(|c| c.is_ascii_digit());
                                c.port = port.parse().unwrap_or(0);
                                self.editor_port = Some(port);
                            }
                            validation(ui, &self.editor_error, "port");
                        });
                    });
                    let user_hint = match c.protocol {
                        Protocol::Rdp => "DOMAIN\\username",
                        Protocol::Ssh => "username",
                        Protocol::Vnc => "for a Mac, an account on it",
                    };
                    field(ui, "Username (optional)", &mut c.username, user_hint);
                    validation(ui, &self.editor_error, "username");
                    labeled(ui, "Group", |ui, label| {
                        group_input(ui, label, &mut c.group, &groups)
                    });
                    labeled(ui, "Notes (optional)", |ui, label| {
                        let notes = egui::TextEdit::multiline(&mut c.notes)
                            .id(NOTES_ID.into())
                            .hint_text(
                                RichText::new("Searchable, e.g. who to ask before rebooting")
                                    .weak(),
                            )
                            .margin(egui::Margin::symmetric(10, 6))
                            .desired_rows(2)
                            .desired_width(f32::INFINITY);
                        notes_focused = ui.add(notes).labelled_by(label).has_focus();
                    });
                    ui.add_space(2.);
                    option_checkbox(ui, &mut c.favorite, "Add to favorites");
                    // The optional settings start a section of their own.
                    ui.add_space(4.);
                    ui.separator();
                    ui.add_space(2.);
                    match c.protocol {
                        Protocol::Rdp => rdp_options(ui, &mut c.rdp, &self.editor_error),
                        Protocol::Ssh => ssh_options(
                            ui,
                            &mut c.ssh,
                            parent,
                            &self.editor_error,
                            &mut self.editor_forwards_touched,
                        ),
                        Protocol::Vnc => vnc_options(ui, &mut c.vnc),
                    }
                });
            ui.add_space(8.);
            let help = match c.protocol {
                Protocol::Rdp => rdp::SIGN_IN_HELP,
                Protocol::Ssh => {
                    "Uses your OpenSSH client, including your SSH config and agent.\n\
                     Host verification and password prompts appear in the terminal; the file \
                     browser can remember a login password in your system credential store."
                }
                Protocol::Vnc => vnc::SIGN_IN_HELP,
            };
            if c.protocol == Protocol::Vnc {
                ui.label(
                    RichText::new("VNC is unencrypted. Use a trusted network or SSH tunnel.")
                        .size(12.)
                        .color(ui.visuals().warn_fg_color),
                )
                .on_hover_text(help);
            } else {
                ui.label(
                    RichText::new("Authentication is handled when connecting.")
                        .size(12.)
                        .color(colors.muted),
                )
                .on_hover_text(help);
            }
            if !self.editor_error.is_empty() && footer_error {
                let label = egui::Label::new(
                    RichText::new(&self.editor_error).color(ui.visuals().error_fg_color),
                );
                ui.add(if compact_error {
                    label.truncate()
                } else {
                    label.wrap()
                })
                .on_hover_text(&self.editor_error);
            }
            ui.horizontal(|ui| {
                save = accent_button(
                    ui,
                    if c.id == 0 {
                        "Create connection"
                    } else {
                        "Save changes"
                    },
                    140.,
                )
                .clicked();
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
        save |= enter && enter_saves && !notes_focused;
        if validation_field(&self.editor_error).is_some() {
            self.editor_error = c.clone().validate().err().unwrap_or_default();
        }
        if save && let Err(e) = c.validate() {
            self.editor_error = e;
            ctx.data_mut(|data| data.insert_temp(egui::Id::new("editor-reveal-error"), true));
            save = false;
        }
        self.modal = Modal::Editor(c.clone());
        if save {
            let mut next = self.connections.clone();
            // New ids are max+1, so a deleted highest id can come back; clear any orphan.
            let mut clear_orphan_id = None;
            if c.id == 0 {
                c.id = next
                    .iter()
                    .map(|c| c.id)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1);
                clear_orphan_id = Some(c.id);
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
                if let Some(id) = clear_orphan_id
                    && let Err(e) = secret::forget_connection_password(id)
                {
                    self.error = Some(format!(
                        "Saved the connection, but could not clear a leftover password: {e}"
                    ));
                }
                self.modal = Modal::None;
            }
        }
    }
}

pub(super) fn ssh_options(
    ui: &mut egui::Ui,
    options: &mut model::SshOptions,
    parent: Option<platform::Window>,
    error: &str,
    forwards_touched: &mut bool,
) {
    egui::CollapsingHeader::new("SSH options")
        .id_salt("ssh-options")
        .open(matches!(validation_field(error), Some("jump" | "identity" | "forwards")).then_some(true))
        .default_open(*options != model::SshOptions::default())
        .show(ui, |ui| {
            field(
                ui,
                "Jump host (optional)",
                &mut options.jump,
                "user@bastion.example.com:22",
            );
            validation(ui, error, "jump");
            labeled(ui, "Identity file (optional)", |ui, label| {
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
                    ).labelled_by(label);
                    let button =
                        egui::Button::new("Browse…").min_size(Vec2::new(browse, CONTROL_HEIGHT));
                    if ui.add(button).clicked()
                        && let Some(path) = file_dialog::pick_key(parent, &options.identity)
                    {
                        options.identity = path.display().to_string();
                    }
                });
            });
            validation(ui, error, "identity");
            labeled(ui, "Port forwards (optional)", |ui, label| {
                let response = ui.add_sized([ui.available_width(), CONTROL_HEIGHT],
                    input(&mut options.forwards, "L8080:localhost:80, R9000:localhost:9000, D1080"))
                    .labelled_by(label)
                    .on_hover_text("Comma-separated forwards: L8080:localhost:80 (local), R9000:localhost:9000 (remote), D1080 (SOCKS proxy).");
                *forwards_touched |= response.changed();
                if (*forwards_touched || validation_field(error) == Some("forwards"))
                    && let Err(error) = options.forward_args() {
                    validation(ui, &error, "forwards");
                }
            });
        });
}

pub(super) fn vnc_options(ui: &mut egui::Ui, options: &mut model::VncOptions) {
    egui::CollapsingHeader::new("VNC options")
        .id_salt("vnc-options")
        .default_open(*options != model::VncOptions::default())
        .show(ui, |ui| {
            option_checkbox(ui, &mut options.clipboard, "Clipboard synchronization")
                .on_hover_text("Share copied text with the remote computer");
            option_checkbox(ui, &mut options.view_only, "View only")
                .on_hover_text("Shows the remote screen without sending keys or the pointer");
        });
}

pub(super) fn rdp_options(ui: &mut egui::Ui, options: &mut model::RdpOptions, error: &str) {
    use model::{Audio, Display, Keys};
    egui::CollapsingHeader::new("Remote desktop options")
        .id_salt("rdp-options")
        .open(matches!(validation_field(error), Some("gateway" | "resolution")).then_some(true))
        .default_open(*options != model::RdpOptions::default())
        .show(ui, |ui| {
            section_label(ui, "GATEWAY");
            field(
                ui,
                "RD Gateway (optional)",
                &mut options.gateway,
                "gateway.example.com",
            );
            ui.add_space(6.);
            validation(ui, error, "gateway");
            section_label(ui, "DEVICE REDIRECTION");
            let columns = if ui.available_width() < 360. { 2 } else { 3 };
            egui::Grid::new("rdp-resources")
                .num_columns(columns)
                .spacing([16., 4.])
                .show(ui, |ui| {
                    for (i, (value, label)) in [
                        (&mut options.clipboard, "Clipboard"),
                        (&mut options.drives, "Local drives"),
                        (&mut options.printers, "Printers"),
                        (&mut options.smart_cards, "Smart cards"),
                        (&mut options.microphone, "Microphone"),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        option_checkbox(ui, value, label);
                        if (i + 1) % columns == 0 {
                            ui.end_row();
                        }
                    }
                });
            ui.add_space(6.);
            section_label(ui, "DISPLAY AND AUDIO");
            let width = ui.available_width();
            // Drop-downs as tall as the text fields above them.
            ui.spacing_mut().interact_size.y = CONTROL_HEIGHT;
            labeled(ui, "Remote audio", |ui, label| {
                egui::ComboBox::from_id_salt("rdp-audio")
                    .width(width)
                    .selected_text(options.audio.label())
                    .show_ui(ui, |ui| {
                        for audio in Audio::ALL {
                            ui.selectable_value(&mut options.audio, audio, audio.label());
                        }
                    })
                    .response
                    .labelled_by(label);
            });
            labeled(ui, "System key combinations", |ui, label| {
                egui::ComboBox::from_id_salt("rdp-keys")
                    .width(width)
                    .selected_text(options.keys.label())
                    .show_ui(ui, |ui| {
                        for keys in Keys::ALL {
                            ui.selectable_value(&mut options.keys, keys, keys.label());
                        }
                    })
                    .response
                    .labelled_by(label);
            });
            labeled(ui, "Resolution", |ui, label| {
                ui.horizontal_wrapped(|ui| {
                    ui.radio_value(&mut options.display, Display::Fit, "Fit to tab");
                    let fixed = matches!(options.display, Display::Fixed { .. });
                    if ui.radio(fixed, "Fixed resolution").clicked() && !fixed {
                        options.display = Display::Fixed {
                            width: 1920,
                            height: 1080,
                        };
                    }
                    if let Display::Fixed { width, height } = &mut options.display {
                        ui.add(egui::DragValue::new(width).range(200..=8192))
                            .labelled_by(label);
                        ui.label("×");
                        ui.add(egui::DragValue::new(height).range(200..=8192))
                            .labelled_by(label);
                    }
                });
            });
            validation(ui, error, "resolution");
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

/// Native checkbox behavior and focus handling, with square corners distinct from radios.
fn option_checkbox(ui: &mut egui::Ui, checked: &mut bool, text: &str) -> egui::Response {
    ui.scope(|ui| {
        let widgets = &mut ui.visuals_mut().widgets;
        for widget in [
            &mut widgets.inactive,
            &mut widgets.hovered,
            &mut widgets.active,
            &mut widgets.noninteractive,
            &mut widgets.open,
        ] {
            widget.corner_radius = egui::CornerRadius::same(2);
        }
        ui.checkbox(checked, text)
    })
    .inner
}

fn switch_protocol(c: &mut Connection, protocol: Protocol, port_edited: bool) {
    if c.id == 0 && !port_edited && c.port == c.protocol.port() {
        c.port = protocol.port();
    }
    c.protocol = protocol;
}

/// Route the existing model's validation messages without duplicating its validation rules.
fn validation_field(error: &str) -> Option<&'static str> {
    [
        ("Give this connection", "name"),
        ("Names cannot", "name"),
        ("Enter a hostname", "host"),
        ("Use the separate Port", "host"),
        ("The username", "username"),
        ("Port must", "port"),
        ("Enter the RD Gateway", "gateway"),
        ("Enter jump hosts", "jump"),
        ("Enter the identity file", "identity"),
        ("Port forward", "forwards"),
        ("A fixed resolution", "resolution"),
    ]
    .into_iter()
    .find_map(|(prefix, field)| error.starts_with(prefix).then_some(field))
}

fn validation_visible(protocol: Protocol, field: &str) -> bool {
    match field {
        "gateway" | "resolution" => protocol == Protocol::Rdp,
        "jump" | "identity" | "forwards" => protocol == Protocol::Ssh,
        _ => true,
    }
}

fn validation(ui: &mut egui::Ui, error: &str, field: &str) {
    if validation_field(error) == Some(field) {
        let text = if field == "port" {
            "Use 1–65535."
        } else {
            error
        };
        let response = ui
            .add(
                egui::Label::new(
                    RichText::new(text)
                        .size(11.)
                        .color(ui.visuals().error_fg_color),
                )
                .wrap(),
            )
            .on_hover_text(error);
        if ui
            .data_mut(|data| data.remove_temp::<bool>(egui::Id::new("editor-reveal-error")))
            .unwrap_or(false)
        {
            response.scroll_to_me(Some(egui::Align::Center));
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_protocol_changes_only_untouched_new_default_ports() {
        for (id, port, edited, expected) in [
            (0, 3389, false, 22),
            (0, 3389, true, 3389),
            (0, 2222, false, 2222),
            (1, 3389, false, 3389),
        ] {
            let mut c = Connection::new();
            c.id = id;
            c.port = port;
            switch_protocol(&mut c, Protocol::Ssh, edited);
            assert_eq!(c.port, expected);
        }
    }
}

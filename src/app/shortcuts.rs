//! The keyboard reference, available from Settings or F1 while Relay has the keyboard.
use super::*;

impl App {
    pub(super) fn keyboard_shortcuts(&mut self, ctx: &egui::Context, session_focused: bool) {
        if !self.quitting
            && (matches!(self.modal, Modal::Settings)
                || (!session_focused && matches!(self.modal, Modal::None)))
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::F1))
        {
            self.modal = Modal::Shortcuts;
        }
        if !matches!(self.modal, Modal::Shortcuts) {
            return;
        }
        let colors = theme::palette(ctx);
        let modifier = if cfg!(target_os = "macos") {
            "Cmd"
        } else {
            "Ctrl"
        };
        let mut close = false;
        dialog("keyboard-shortcuts", ctx).show(ctx, |ui| {
            ui.set_width(dialog_width(ctx, 620.));
            ui.horizontal(|ui| {
                ui.label(RichText::new("Keyboard shortcuts").size(TITLE_SIZE).strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    close = icon_button(ui, Icon::Close, 26., "Close (Esc)").clicked();
                });
            });
            ui.label(RichText::new("Shortcuts apply to the area with keyboard focus.").size(12.).color(colors.muted));
            let groups: &[(&str, &[(&str, &str)])] = &[
                ("WORKSPACE & TABS", &[
                    ("F1", "Open keyboard shortcuts"),
                    ("Mod+K", "Open library search"),
                    ("Mod+T", "Open a connection: search or type a host"),
                    ("Enter in quick open", "Connect; Shift+Enter browses SSH files"),
                    ("Alt+Enter in quick open", "Connect to the typed address"),
                    ("Mod+N", "New connection"),
                    ("Mod+B", "Toggle sidebar"),
                    ("Mod+W", "Close current session tab"),
                    ("Mod+1", "Open library tab"),
                    ("Mod+2–8", "Open session tabs 1–7"),
                    ("Mod+9", "Open last tab"),
                    ("Mod+Tab / Mod+Shift+Tab", "Next / previous tab"),
                    ("Mod+Alt+PageDown / PageUp", "Next / previous tab"),
                    ("Mod+Shift+PageUp / PageDown", "Move tab left / right"),
                    ("Mod+\\", "Split with the neighbouring tab, or unsplit"),
                    ("Mod+Shift+T", "Open a local shell"),
                ]),
                ("CONNECTION LIBRARY & DIALOGS", &[
                    ("↑ / ↓", "Select connection"),
                    ("Enter", "Connect; quick-connect when no results match"),
                    ("Shift+Enter", "Browse selected SSH connection's files"),
                    ("Alt+Enter", "Quick-connect to the typed address"),
                    ("Mod+click / Shift+click", "Choose several connections, or a range"),
                    ("Mod+A", "Choose every connection shown"),
                    ("Enter with several chosen", "Connect to each in its own tab"),
                    ("Enter in connection editor", "Save connection"),
                    ("Esc", "Clear search or close dialog"),
                ]),
                ("FILE BROWSER", &[
                    ("Mod+L", "Edit remote path"),
                    ("Mod+F", "Filter folder"),
                    ("F5 / Mod+R", "Refresh folder"),
                    ("Backspace / Alt+↑", "Go to parent folder"),
                    ("Alt+←", "Go back"),
                    ("↑ / ↓", "Move selection"),
                    ("Home / End", "First / last item"),
                    ("PageUp / PageDown", "Move selection by 10 items"),
                    ("Shift+navigation", "Extend selection"),
                    ("Mod+A", "Select all listed items"),
                    ("Enter", "Open folder or download selected files"),
                    ("Mod+U", "Choose files to upload"),
                    ("Mod+E", "Edit selected file in the external editor"),
                    ("Mod+Shift+N", "New folder"),
                    ("F2", "Rename selected item"),
                    ("Delete", "Delete selected items, with confirmation"),
                    ("Esc", "Cancel editing or clear selection"),
                ]),
                ("SSH TERMINAL & SNIPPETS", &[
                    ("Ctrl+C", "Interrupt remote command"),
                    (if cfg!(target_os = "macos") { "Cmd+C" } else { "Ctrl+Shift+C" }, "Copy selection, or screen if nothing is selected"),
                    ("Mod+V / Shift+Insert", "Paste"),
                    (if cfg!(target_os = "macos") { "Cmd+F" } else { "Ctrl+Shift+F" }, "Search output, including scrollback"),
                    ("Enter / Shift+Enter in search", "Older / newer match"),
                    ("Mod+click", "Open web address"),
                    ("Shift+drag", "Select text in programs that use the mouse"),
                    ("Mod+Plus / Mod+Minus", "Larger / smaller terminal text"),
                    ("Mod+0", "Reset terminal text size"),
                    ("Mod+Shift+P", "Open snippets"),
                    ("↑ / ↓ in snippets", "Select snippet"),
                    ("Enter in snippets", "Send selected snippet"),
                    ("Esc in snippets", "Close snippets"),
                ]),
                #[cfg(not(windows))]
                ("REMOTE DESKTOP", &[
                    ("Ctrl+Alt+PageUp / PageDown", "Previous / next tab"),
                    ("Ctrl+Alt+Enter", "Toggle remote desktop full screen"),
                ]),
                #[cfg(windows)]
                ("WINDOWS REMOTE DESKTOP", &[
                    ("Ctrl+Alt+Home", "Return keyboard focus to Relay"),
                    ("Ctrl+Alt+PageUp / PageDown", "Previous / next tab"),
                    ("Ctrl+Alt+Enter", "Toggle VNC full screen"),
                    ("Ctrl+Alt+Break", "Toggle remote desktop full screen"),
                ]),
            ];
            // Keys use the description size and one column width for every section, so each key
            // sits on its description's line and descriptions line up down the whole list.
            let mono = egui::FontId::monospace(egui::TextStyle::Body.resolve(ui.style()).size);
            let longest = groups
                .iter()
                .flat_map(|(_, rows)| rows.iter())
                .map(|(keys, _)| keys.replace("Mod", modifier).chars().count())
                .max()
                .unwrap_or(0);
            let key_width = ui.fonts_mut(|f| f.glyph_width(&mono, 'M')) * longest as f32;
            egui::ScrollArea::vertical()
                .id_salt("keyboard-shortcuts-list")
                .min_scrolled_height((ctx.content_rect().height() - 240.).max(160.))
                .max_height((ctx.content_rect().height() - 240.).max(160.))
                .show(ui, |ui| {
                    // Rows are as tall as their text, not a control.
                    ui.spacing_mut().interact_size.y = 0.;
                    for (section, rows) in groups {
                        ui.add_space(12.);
                        section_label(ui, section);
                        for (keys, action) in *rows {
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = 20.;
                                ui.allocate_ui_with_layout(
                                    Vec2::new(key_width, 0.),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        ui.set_width(key_width);
                                        ui.label(RichText::new(keys.replace("Mod", modifier)).font(mono.clone()));
                                    },
                                );
                                ui.add(egui::Label::new(*action).wrap());
                            });
                        }
                    }
                    ui.add_space(12.);
                    ui.label(RichText::new("Focused terminals and remote desktops receive most keys. Tab switching stays available. Windows RDP tab shortcuts require Relay in the foreground and do not apply in full screen or when Windows keys go to the remote computer.").size(12.).color(colors.muted));
                });
        });
        if close || ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            self.modal = Modal::None;
        }
    }
}

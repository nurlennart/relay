//! What typing sends: egui's key, text and clipboard events as the bytes a terminal program
//! reads.
use eframe::egui;

/// What the program asked of the terminal that changes the bytes keys send.
#[derive(Clone, Copy, Default)]
pub struct Mode {
    pub application_cursor: bool,
    pub bracketed_paste: bool,
}

/// Converts one frame of egui input into terminal bytes, and reports whether to copy the screen.
pub fn translate(
    events: &[egui::Event],
    modifiers: egui::Modifiers,
    mode: Mode,
) -> (Vec<u8>, bool) {
    let mut input = vec![];
    let mut copy = false;
    // egui-winit reports Ctrl+C/X (Cmd+C/X on macOS) and Shift+Delete only as Copy/Cut, and
    // sends the text of an Alt combination after its key event. On macOS Ctrl+C stays a key.
    let mut meta = false;
    for event in events {
        match event {
            egui::Event::Text(text) => {
                if std::mem::take(&mut meta) {
                    input.push(0x1b);
                }
                input.extend(text.as_bytes());
            }
            egui::Event::Paste(text) => {
                if mode.bracketed_paste {
                    input.extend(b"\x1b[200~");
                }
                input.extend(text.replace("\r\n", "\n").replace('\n', "\r").as_bytes());
                if mode.bracketed_paste {
                    input.extend(b"\x1b[201~");
                }
            }
            egui::Event::Copy if modifiers.shift || modifiers.mac_cmd => copy = true,
            egui::Event::Copy => input.push(0x03),
            egui::Event::Cut if modifiers.mac_cmd => copy = true,
            egui::Event::Cut if modifiers.ctrl => input.push(0x18),
            egui::Event::Cut => input.extend(b"\x1b[3;2~"),
            egui::Event::Key {
                key,
                pressed: true,
                modifiers,
                ..
            } => {
                let bytes = key_bytes(*key, *modifiers, mode.application_cursor);
                if modifiers.alt && !modifiers.ctrl {
                    if bytes.is_empty() {
                        meta = true;
                    } else {
                        input.push(0x1b);
                        input.extend(bytes);
                    }
                } else {
                    input.extend(bytes);
                }
            }
            _ => {}
        }
    }
    (input, copy)
}

/// The bytes a snippet sends: line breaks become Enter, multi-line text is pasted as one block
/// when the shell supports bracketed paste, and `run` presses Enter at the end.
pub fn snippet_bytes(text: &str, run: bool, bracketed: bool) -> Vec<u8> {
    let text = text.trim_end_matches(['\r', '\n']).replace("\r\n", "\n");
    let body = text.replace('\n', "\r");
    let mut bytes = vec![];
    if bracketed && text.contains('\n') {
        bytes.extend(b"\x1b[200~");
        bytes.extend(body.as_bytes());
        bytes.extend(b"\x1b[201~");
    } else {
        bytes.extend(body.as_bytes());
    }
    if run {
        bytes.push(b'\r');
    }
    bytes
}

pub fn key_bytes(key: egui::Key, modifiers: egui::Modifiers, application_cursor: bool) -> Vec<u8> {
    use egui::Key::*;
    if modifiers.ctrl && !modifiers.alt {
        let name = key.name();
        if name.len() == 1 && name.as_bytes()[0].is_ascii_alphabetic() {
            return vec![name.as_bytes()[0].to_ascii_uppercase() - b'@'];
        }
        if key == Space {
            return vec![0];
        }
    }
    let bytes: &[u8] = match key {
        Enter => b"\r",
        Backspace => b"\x7f",
        Tab if modifiers.shift => b"\x1b[Z",
        Tab => b"\t",
        Escape => b"\x1b",
        ArrowUp if application_cursor => b"\x1bOA",
        ArrowDown if application_cursor => b"\x1bOB",
        ArrowRight if application_cursor => b"\x1bOC",
        ArrowLeft if application_cursor => b"\x1bOD",
        ArrowUp => b"\x1b[A",
        ArrowDown => b"\x1b[B",
        ArrowRight => b"\x1b[C",
        ArrowLeft => b"\x1b[D",
        Home => b"\x1b[H",
        End => b"\x1b[F",
        Delete => b"\x1b[3~",
        Insert => b"\x1b[2~",
        PageUp => b"\x1b[5~",
        PageDown => b"\x1b[6~",
        F1 => b"\x1bOP",
        F2 => b"\x1bOQ",
        F3 => b"\x1bOR",
        F4 => b"\x1bOS",
        F5 => b"\x1b[15~",
        F6 => b"\x1b[17~",
        F7 => b"\x1b[18~",
        F8 => b"\x1b[19~",
        F9 => b"\x1b[20~",
        F10 => b"\x1b[21~",
        F11 => b"\x1b[23~",
        F12 => b"\x1b[24~",
        _ => b"",
    };
    bytes.to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_clipboard_events_become_terminal_bytes() {
        assert_eq!(
            key_bytes(egui::Key::C, egui::Modifiers::CTRL, false),
            vec![3]
        );
        assert_eq!(
            key_bytes(egui::Key::ArrowUp, egui::Modifiers::NONE, true),
            b"\x1bOA"
        );
        let key = |key, modifiers| egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        };
        let mode = Mode::default();
        assert_eq!(
            translate(&[egui::Event::Copy], egui::Modifiers::CTRL, mode),
            (vec![3], false)
        );
        assert_eq!(
            translate(
                &[egui::Event::Copy],
                egui::Modifiers::CTRL | egui::Modifiers::SHIFT,
                mode
            ),
            (vec![], true)
        );
        assert_eq!(
            translate(&[egui::Event::Cut], egui::Modifiers::CTRL, mode).0,
            vec![0x18]
        );
        assert_eq!(
            translate(
                &[
                    key(egui::Key::B, egui::Modifiers::ALT),
                    egui::Event::Text("b".into())
                ],
                egui::Modifiers::ALT,
                mode
            )
            .0,
            b"\x1bb"
        );
        assert_eq!(
            translate(
                &[egui::Event::Paste("a\r\nb".into())],
                egui::Modifiers::NONE,
                Mode {
                    bracketed_paste: true,
                    ..mode
                }
            )
            .0,
            b"\x1b[200~a\rb\x1b[201~"
        );
    }
    #[test]
    fn snippets_type_lines_and_optionally_run() {
        assert_eq!(snippet_bytes("df -h", true, false), b"df -h\r");
        assert_eq!(snippet_bytes("df -h\n", false, true), b"df -h");
        assert_eq!(
            snippet_bytes("cd /srv\r\nls", true, true),
            b"\x1b[200~cd /srv\rls\x1b[201~\r"
        );
        assert_eq!(snippet_bytes("a\nb", false, false), b"a\rb");
    }
}

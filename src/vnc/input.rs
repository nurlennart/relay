//! Turns egui keyboard, mouse and wheel events into VNC key and pointer events. VNC names keys
//! by the X11 keysym of what they mean rather than where they sit, so typed text is sent as
//! the characters it produced, and other keys by their names.
use crate::frame::View;
use eframe::egui::{self, Key};
use std::collections::HashMap;

const SHIFT: u32 = 0xffe1;
const CONTROL: u32 = 0xffe3;
const ALT: u32 = 0xffe9;
const SUPER: u32 = 0xffeb;

/// Points of smooth (touchpad) scrolling that make up one wheel click.
const POINTS_PER_CLICK: f32 = 50.;

/// What the server is told.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input {
    Key { down: bool, keysym: u32 },
    Pointer { buttons: u8, at: [u16; 2] },
}

/// Input state carried between frames.
#[derive(Default)]
pub struct Translator {
    /// Modifier keysyms the server has been told are held.
    modifiers: Vec<u32>,
    /// Keysyms pressed by a key that has not been released yet.
    held: HashMap<Key, u32>,
    /// Pointer buttons held, as the protocol's bit mask, and where the pointer was last.
    buttons: u8,
    at: [u16; 2],
    /// Smooth scrolling not yet worth a wheel click, in clicks.
    wheel: egui::Vec2,
    /// The modifiers of the latest key event, which text events do not carry.
    latest: egui::Modifiers,
    /// The latest key press went out as a shortcut, such as Alt+F, so the text it also typed
    /// is not sent again.
    shortcut_sent: bool,
    /// The server is a Mac, so Cmd is its Command key rather than Ctrl.
    pub mac: bool,
}

impl Translator {
    /// Converts one frame of egui `events`. Keys count only with `keyboard` focus, and the
    /// pointer only while `pointer` is over the desktop or dragging. `modifiers` is the state
    /// at the end of the frame, which catches a modifier pressed alone.
    pub fn translate(
        &mut self,
        events: &[egui::Event],
        modifiers: egui::Modifiers,
        view: &View,
        keyboard: bool,
        pointer: bool,
    ) -> Vec<Input> {
        let mut out = vec![];
        for event in events {
            match event {
                egui::Event::Key {
                    key,
                    pressed,
                    modifiers,
                    ..
                } if keyboard => {
                    self.latest = *modifiers;
                    if *pressed {
                        self.shortcut_sent = false;
                        // Typed characters arrive as text; shortcuts such as Ctrl+C do not.
                        let keysym = match named(*key) {
                            Some(keysym) => Some(keysym),
                            None if self.shortcut(*modifiers) => character(*key),
                            None => None,
                        };
                        if let Some(keysym) = keysym {
                            self.sync(*modifiers, &mut out);
                            self.held.insert(*key, keysym);
                            out.push(Input::Key { down: true, keysym });
                            self.shortcut_sent = named(*key).is_none();
                        }
                    } else if let Some(keysym) = self.held.remove(key) {
                        out.push(Input::Key {
                            down: false,
                            keysym,
                        });
                    }
                }
                // Text composed with an input method arrives as a commit instead.
                egui::Event::Text(text) | egui::Event::Ime(egui::ImeEvent::Commit(text))
                    if keyboard =>
                {
                    if std::mem::take(&mut self.shortcut_sent) {
                        continue;
                    }
                    // Shift is part of the character; other modifiers, such as Option or AltGr
                    // used to type it, would turn it into a shortcut.
                    self.sync(
                        egui::Modifiers {
                            shift: self.latest.shift,
                            ..Default::default()
                        },
                        &mut out,
                    );
                    for c in text.chars().filter(|c| !c.is_control()) {
                        let keysym = keysym(c);
                        out.push(Input::Key { down: true, keysym });
                        out.push(Input::Key {
                            down: false,
                            keysym,
                        });
                    }
                }
                // egui-winit reports Ctrl+C/X/V (Cmd on macOS) only as these events, so press
                // the key itself; its release still arrives as a key event.
                egui::Event::Copy | egui::Event::Cut | egui::Event::Paste(_) if keyboard => {
                    let key = match event {
                        egui::Event::Copy => Key::C,
                        egui::Event::Cut => Key::X,
                        _ => Key::V,
                    };
                    let mut held = modifiers;
                    if !held.ctrl && !held.mac_cmd {
                        held.ctrl = true;
                    }
                    self.sync(held, &mut out);
                    let keysym = character(key).expect("a letter");
                    self.held.insert(key, keysym);
                    out.push(Input::Key { down: true, keysym });
                }
                egui::Event::PointerMoved(pos) if pointer => {
                    self.at = view.remote(*pos);
                    out.push(self.pointer());
                }
                egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                } => {
                    let Some(bit) = button_bit(*button) else {
                        continue;
                    };
                    if *pressed && pointer {
                        // A Shift-click selects a range only if Shift is down first.
                        self.sync(*modifiers, &mut out);
                        self.at = view.remote(*pos);
                        self.buttons |= bit;
                        out.push(self.pointer());
                    } else if !*pressed && self.buttons & bit != 0 {
                        // Releases always go through, so a drag past the edge still ends.
                        self.sync(*modifiers, &mut out);
                        self.at = view.remote(*pos);
                        self.buttons &= !bit;
                        out.push(self.pointer());
                    }
                }
                egui::Event::MouseWheel {
                    unit,
                    delta,
                    modifiers,
                    ..
                } if pointer => {
                    self.sync(*modifiers, &mut out);
                    let per_unit = match unit {
                        egui::MouseWheelUnit::Point => 1. / POINTS_PER_CLICK,
                        egui::MouseWheelUnit::Line => 1.,
                        egui::MouseWheelUnit::Page => 3.,
                    };
                    self.wheel += *delta * per_unit;
                    out.extend(self.wheel_clicks());
                }
                _ => {}
            }
        }
        // Without the keyboard, modifiers are held only for the pointer events that had them.
        self.sync(
            if keyboard {
                modifiers
            } else {
                egui::Modifiers::NONE
            },
            &mut out,
        );
        // Text follows its key in the same frame; a later one was typed separately.
        self.shortcut_sent = false;
        out
    }

    /// Releases every key and button the server thinks is held, as when focus moves away.
    pub fn release_all(&mut self) -> Vec<Input> {
        let mut out: Vec<Input> = (self.held.drain().map(|(_, keysym)| keysym))
            .chain(self.modifiers.drain(..))
            .map(|keysym| Input::Key {
                down: false,
                keysym,
            })
            .collect();
        if self.buttons != 0 {
            self.buttons = 0;
            out.push(self.pointer());
        }
        self.wheel = egui::Vec2::ZERO;
        out
    }

    /// Whether `modifiers` make a key a shortcut rather than typing. Option on a Mac types
    /// characters, so only Ctrl and Cmd count there.
    fn shortcut(&self, modifiers: egui::Modifiers) -> bool {
        modifiers.ctrl || modifiers.mac_cmd || (modifiers.alt && !cfg!(target_os = "macos"))
    }

    /// The modifier keysyms `modifiers` hold. Cmd is the Mac's Command key on a Mac server
    /// and Ctrl elsewhere, so Cmd+C copies remotely as it does locally.
    fn wanted(&self, modifiers: egui::Modifiers) -> Vec<u32> {
        let mut keys = vec![];
        if modifiers.shift {
            keys.push(SHIFT);
        }
        if modifiers.ctrl || (modifiers.mac_cmd && !self.mac) {
            keys.push(CONTROL);
        }
        if modifiers.alt {
            keys.push(ALT);
        }
        if modifiers.mac_cmd && self.mac {
            keys.push(SUPER);
        }
        keys
    }

    /// Presses or releases modifiers until the server matches `now`.
    fn sync(&mut self, now: egui::Modifiers, out: &mut Vec<Input>) {
        let wanted = self.wanted(now);
        self.modifiers.retain(|keysym| {
            let keep = wanted.contains(keysym);
            if !keep {
                out.push(Input::Key {
                    down: false,
                    keysym: *keysym,
                });
            }
            keep
        });
        for keysym in wanted {
            if !self.modifiers.contains(&keysym) {
                self.modifiers.push(keysym);
                out.push(Input::Key { down: true, keysym });
            }
        }
    }

    fn pointer(&self) -> Input {
        Input::Pointer {
            buttons: self.buttons,
            at: self.at,
        }
    }

    /// Whole wheel clicks from accumulated scrolling, each a press and release of the wheel's
    /// button, keeping the remainder.
    fn wheel_clicks(&mut self) -> Vec<Input> {
        let mut out = vec![];
        // egui's delta moves content: positive y scrolls up, positive x scrolls left.
        for (amount, towards, away) in [(&mut self.wheel.y, 8, 16), (&mut self.wheel.x, 32, 64)] {
            while amount.abs() >= 1. {
                let bit = if *amount > 0. { towards } else { away };
                *amount -= amount.signum();
                for buttons in [self.buttons | bit, self.buttons] {
                    out.push(Input::Pointer {
                        buttons,
                        at: self.at,
                    });
                }
            }
        }
        out
    }
}

fn button_bit(button: egui::PointerButton) -> Option<u8> {
    match button {
        egui::PointerButton::Primary => Some(1),
        egui::PointerButton::Middle => Some(2),
        egui::PointerButton::Secondary => Some(4),
        egui::PointerButton::Extra1 | egui::PointerButton::Extra2 => None,
    }
}

/// The keysym of character `c`: Latin-1 characters are their own, others are Unicode keysyms.
pub fn keysym(c: char) -> u32 {
    match u32::from(c) {
        code @ (0x20..=0x7e | 0xa0..=0xff) => code,
        code => 0x0100_0000 + code,
    }
}

/// The keysym of a key that types a character, unshifted, for shortcuts such as Ctrl+C.
fn character(key: Key) -> Option<u32> {
    let name = key.symbol_or_name();
    let mut chars = name.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if !c.is_control() => Some(keysym(c.to_ascii_lowercase())),
        _ => None,
    }
}

/// The keysym of a key that does not type text.
fn named(key: Key) -> Option<u32> {
    use Key::*;
    Some(match key {
        Escape => 0xff1b,
        Tab => 0xff09,
        Backspace => 0xff08,
        Enter => 0xff0d,
        Insert => 0xff63,
        Delete => 0xffff,
        Home => 0xff50,
        End => 0xff57,
        PageUp => 0xff55,
        PageDown => 0xff56,
        ArrowLeft => 0xff51,
        ArrowUp => 0xff52,
        ArrowRight => 0xff53,
        ArrowDown => 0xff54,
        F1 => 0xffbe,
        F2 => 0xffbf,
        F3 => 0xffc0,
        F4 => 0xffc1,
        F5 => 0xffc2,
        F6 => 0xffc3,
        F7 => 0xffc4,
        F8 => 0xffc5,
        F9 => 0xffc6,
        F10 => 0xffc7,
        F11 => 0xffc8,
        F12 => 0xffc9,
        F13 => 0xffca,
        F14 => 0xffcb,
        F15 => 0xffcc,
        F16 => 0xffcd,
        F17 => 0xffce,
        F18 => 0xffcf,
        F19 => 0xffd0,
        F20 => 0xffd1,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Modifiers, Rect, pos2, vec2};

    fn view() -> View {
        View {
            rect: Rect::from_min_size(pos2(100., 50.), vec2(400., 300.)),
            size: [800, 600],
        }
    }

    fn key(key: Key, pressed: bool, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers,
        }
    }

    fn down(keysym: u32) -> Input {
        Input::Key { down: true, keysym }
    }

    fn up(keysym: u32) -> Input {
        Input::Key {
            down: false,
            keysym,
        }
    }

    fn typed(t: &mut Translator, events: &[egui::Event], modifiers: Modifiers) -> Vec<Input> {
        t.translate(events, modifiers, &view(), true, true)
    }

    #[test]
    fn text_is_sent_as_the_characters_typed() {
        let mut t = Translator::default();
        // The key event of a typing key is left to the text it produces.
        let events = [
            key(Key::A, true, Modifiers::SHIFT),
            egui::Event::Text("Aé€".into()),
            key(Key::A, false, Modifiers::SHIFT),
        ];
        let out = typed(&mut t, &events, Modifiers::NONE);
        assert_eq!(
            out,
            [
                down(SHIFT),
                down(0x41),
                up(0x41),
                down(0xe9),
                up(0xe9),
                down(0x0100_20ac),
                up(0x0100_20ac),
                up(SHIFT)
            ]
        );
    }

    #[test]
    fn named_keys_and_shortcuts_press_and_release() {
        let mut t = Translator::default();
        let out = typed(
            &mut t,
            &[
                key(Key::Enter, true, Modifiers::NONE),
                key(Key::Enter, false, Modifiers::NONE),
            ],
            Modifiers::NONE,
        );
        assert_eq!(out, [down(0xff0d), up(0xff0d)]);
        let out = typed(
            &mut t,
            &[
                key(Key::S, true, Modifiers::CTRL),
                key(Key::S, false, Modifiers::CTRL),
            ],
            Modifiers::CTRL,
        );
        assert_eq!(out, [down(CONTROL), down(b's'.into()), up(b's'.into())]);
        // Typing text lets go of Ctrl, so AltGr or Option combinations type the character.
        let out = typed(&mut t, &[egui::Event::Text("@".into())], Modifiers::NONE);
        assert_eq!(out, [up(CONTROL), down(b'@'.into()), up(b'@'.into())]);
        // A modifier held alone shows up only in the frame's state.
        assert_eq!(typed(&mut t, &[], Modifiers::ALT), [down(ALT)]);
        assert_eq!(t.release_all(), [up(ALT)]);
    }

    #[test]
    fn cmd_is_command_on_a_mac_server_and_ctrl_elsewhere() {
        let cmd = Modifiers {
            mac_cmd: true,
            command: true,
            ..Modifiers::NONE
        };
        let mut t = Translator::default();
        let out = typed(&mut t, &[egui::Event::Copy], cmd);
        assert_eq!(out, [down(CONTROL), down(b'c'.into())]);
        let mut t = Translator {
            mac: true,
            ..Default::default()
        };
        let out = typed(&mut t, &[egui::Event::Paste("x".into())], cmd);
        assert_eq!(out, [down(SUPER), down(b'v'.into())]);
        // The key's release arrives as a key event.
        let out = typed(&mut t, &[key(Key::V, false, cmd)], cmd);
        assert_eq!(out, [up(b'v'.into())]);
    }

    #[test]
    fn a_shortcut_is_not_typed_again_and_input_methods_type() {
        let mut t = Translator::default();
        let events = [
            key(Key::A, true, Modifiers::ALT),
            egui::Event::Text("a".into()),
            key(Key::A, false, Modifiers::ALT),
        ];
        let out = typed(&mut t, &events, Modifiers::NONE);
        if cfg!(target_os = "macos") {
            // Option types characters on a Mac.
            assert_eq!(out, [down(b'a'.into()), up(b'a'.into())]);
        } else {
            assert_eq!(
                out,
                [down(ALT), down(b'a'.into()), up(b'a'.into()), up(ALT)]
            );
        }
        let commit = egui::Event::Ime(egui::ImeEvent::Commit("日".into()));
        let out = typed(&mut t, &[commit], Modifiers::NONE);
        assert_eq!(out, [down(0x0100_65e5), up(0x0100_65e5)]);
    }

    #[test]
    fn modified_clicks_and_wheels_press_their_modifiers_first() {
        let mut t = Translator::default();
        let click = egui::Event::PointerButton {
            pos: pos2(100., 50.),
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::SHIFT,
        };
        // Without the keyboard, Shift is let go again at the end of the frame.
        let out = t.translate(&[click], Modifiers::SHIFT, &view(), false, true);
        let at = [0, 0];
        assert_eq!(
            out,
            [down(SHIFT), Input::Pointer { buttons: 1, at }, up(SHIFT)]
        );
        let wheel = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: vec2(0., 1.),
            modifiers: Modifiers::CTRL,
        };
        let out = typed(&mut t, &[wheel], Modifiers::CTRL);
        assert_eq!(
            out,
            [
                down(CONTROL),
                Input::Pointer { buttons: 1 | 8, at },
                Input::Pointer { buttons: 1, at }
            ]
        );
    }

    #[test]
    fn keys_need_focus_and_the_pointer_needs_hover() {
        let mut t = Translator::default();
        let events = [
            key(Key::Enter, true, Modifiers::NONE),
            egui::Event::Text("a".into()),
            egui::Event::PointerMoved(pos2(300., 200.)),
        ];
        assert!(
            t.translate(&events, Modifiers::NONE, &view(), false, false)
                .is_empty()
        );
        let out = t.translate(&events[2..], Modifiers::NONE, &view(), false, true);
        assert_eq!(
            out,
            [Input::Pointer {
                buttons: 0,
                at: [400, 300]
            }]
        );
    }

    #[test]
    fn buttons_drags_and_wheel_clicks() {
        let mut t = Translator::default();
        let button = |pressed, pos| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let out = t.translate(
            &[button(true, pos2(100., 50.))],
            Modifiers::NONE,
            &view(),
            false,
            true,
        );
        assert_eq!(
            out,
            [Input::Pointer {
                buttons: 4,
                at: [0, 0]
            }]
        );
        // Released outside the desktop, the drag still ends there, at its edge.
        let out = t.translate(
            &[button(false, pos2(0., 0.))],
            Modifiers::NONE,
            &view(),
            false,
            false,
        );
        assert_eq!(
            out,
            [Input::Pointer {
                buttons: 0,
                at: [0, 0]
            }]
        );
        let wheel = |unit, delta| egui::Event::MouseWheel {
            unit,
            delta,
            modifiers: Modifiers::NONE,
        };
        let out = t.translate(
            &[wheel(egui::MouseWheelUnit::Line, vec2(0., -1.))],
            Modifiers::NONE,
            &view(),
            false,
            true,
        );
        assert_eq!(
            out,
            [
                Input::Pointer {
                    buttons: 16,
                    at: [0, 0]
                },
                Input::Pointer {
                    buttons: 0,
                    at: [0, 0]
                }
            ]
        );
        // Touchpad scrolling waits until it adds up to a click.
        let small = [wheel(egui::MouseWheelUnit::Point, vec2(30., 0.))];
        assert!(
            t.translate(&small, Modifiers::NONE, &view(), false, true)
                .is_empty()
        );
        let out = t.translate(&small, Modifiers::NONE, &view(), false, true);
        assert_eq!(out.len(), 2);
        assert!(matches!(out[0], Input::Pointer { buttons: 32, .. }));
    }

    #[test]
    fn keysyms_for_characters_and_keys() {
        assert_eq!(keysym('a'), 0x61);
        assert_eq!(keysym('ß'), 0xdf);
        assert_eq!(keysym('ж'), 0x0100_0436);
        assert_eq!(character(Key::Q), Some(0x71));
        assert_eq!(character(Key::Num1), Some(0x31));
        assert_eq!(character(Key::Slash), Some(0x2f));
        assert_eq!(character(Key::Enter), None);
        assert_eq!(named(Key::F12), Some(0xffc9));
        assert_eq!(named(Key::A), None);
    }
}

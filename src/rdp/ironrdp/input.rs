//! Turns egui keyboard, mouse, and wheel events into RDP input. Keys are sent as scancodes of
//! their physical position, so the remote keyboard layout decides which character they type.
use crate::frame::View;
use eframe::egui::{self, Key, Pos2};
use ironrdp_input::{MouseButton, MousePosition, Operation, Scancode, WheelRotations};

const LEFT_SHIFT: u16 = 0x2A;
const LEFT_CTRL: u16 = 0x1D;
const LEFT_ALT: u16 = 0x38;

/// One wheel notch in RDP rotation units.
const NOTCH: f32 = 120.;
/// Points of smooth (touchpad) scrolling that make up one notch.
const POINTS_PER_NOTCH: f32 = 50.;

/// The remote pixel under `pos` as IronRDP takes it.
fn position(view: &View, pos: Pos2) -> MousePosition {
    let [x, y] = view.remote(pos);
    MousePosition { x, y }
}

/// Input state carried between frames.
#[derive(Default)]
pub struct Translator {
    /// The modifiers the remote computer has been told are held.
    modifiers: egui::Modifiers,
    /// Smooth scrolling not yet worth a wheel event, in rotation units.
    wheel: egui::Vec2,
}

impl Translator {
    /// Converts one frame of egui `events` into input operations. Keys count only with
    /// `keyboard` focus, and the pointer only while `pointer` is over the desktop or dragging.
    /// `modifiers` is the state at the end of the frame, which catches a modifier pressed alone.
    pub fn translate(
        &mut self,
        events: &[egui::Event],
        modifiers: egui::Modifiers,
        view: &View,
        keyboard: bool,
        pointer: bool,
    ) -> Vec<Operation> {
        let mut ops = vec![];
        for event in events {
            match event {
                egui::Event::Key {
                    key,
                    physical_key,
                    pressed,
                    modifiers,
                    ..
                } if keyboard => {
                    self.sync(*modifiers, &mut ops);
                    if let Some(code) = scancode(physical_key.unwrap_or(*key)) {
                        ops.push(if *pressed {
                            Operation::KeyPressed(Scancode::from_u16(code))
                        } else {
                            Operation::KeyReleased(Scancode::from_u16(code))
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
                    let mut held = self.modifiers;
                    held.ctrl = true;
                    self.sync(held, &mut ops);
                    if let Some(code) = scancode(key) {
                        ops.push(Operation::KeyPressed(Scancode::from_u16(code)));
                    }
                }
                egui::Event::PointerMoved(pos) if pointer => {
                    ops.push(Operation::MouseMove(position(view, *pos)));
                }
                egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                } => {
                    let button = mouse_button(*button);
                    if *pressed && pointer {
                        self.sync(*modifiers, &mut ops);
                        ops.push(Operation::MouseMove(position(view, *pos)));
                        ops.push(Operation::MouseButtonPressed(button));
                    } else if !*pressed {
                        // Releases always go through; unpressed buttons are ignored remotely.
                        ops.push(Operation::MouseMove(position(view, *pos)));
                        ops.push(Operation::MouseButtonReleased(button));
                    }
                }
                egui::Event::MouseWheel {
                    unit,
                    delta,
                    modifiers,
                    ..
                } if pointer => {
                    self.sync(*modifiers, &mut ops);
                    let per_unit = match unit {
                        egui::MouseWheelUnit::Point => NOTCH / POINTS_PER_NOTCH,
                        egui::MouseWheelUnit::Line => NOTCH,
                        egui::MouseWheelUnit::Page => NOTCH * 3.,
                    };
                    // egui's delta moves content; a positive RDP wheel rotation scrolls up, and
                    // a positive horizontal one scrolls right.
                    self.wheel += egui::vec2(-delta.x, delta.y) * per_unit;
                    ops.extend(self.wheel_steps());
                }
                _ => {}
            }
        }
        if keyboard {
            self.sync(modifiers, &mut ops);
        }
        ops
    }

    /// Forgets held modifiers after the remote side released everything.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Presses or releases modifiers until the remote computer matches `now`. Cmd on macOS
    /// acts as Ctrl, so Cmd+C copies remotely as it does locally.
    fn sync(&mut self, now: egui::Modifiers, ops: &mut Vec<Operation>) {
        let ctrl = |m: egui::Modifiers| m.ctrl || m.mac_cmd;
        for (was, is, code) in [
            (self.modifiers.shift, now.shift, LEFT_SHIFT),
            (ctrl(self.modifiers), ctrl(now), LEFT_CTRL),
            (self.modifiers.alt, now.alt, LEFT_ALT),
        ] {
            match (was, is) {
                (false, true) => ops.push(Operation::KeyPressed(Scancode::from_u16(code))),
                (true, false) => ops.push(Operation::KeyReleased(Scancode::from_u16(code))),
                _ => {}
            }
        }
        self.modifiers = now;
    }

    /// Whole wheel steps from accumulated scrolling, keeping the remainder.
    fn wheel_steps(&mut self) -> Vec<Operation> {
        let mut ops = vec![];
        for (vertical, amount) in [(true, &mut self.wheel.y), (false, &mut self.wheel.x)] {
            // Each event carries at most one notch, well inside the protocol's 9-bit range.
            while amount.abs() >= NOTCH {
                let step = NOTCH.copysign(*amount);
                *amount -= step;
                ops.push(Operation::WheelRotations(WheelRotations {
                    is_vertical: vertical,
                    rotation_units: step as i16,
                }));
            }
            // Small touchpad movements still scroll, a little at a time.
            if amount.abs() >= NOTCH / 4. {
                let step = amount.trunc();
                *amount -= step;
                ops.push(Operation::WheelRotations(WheelRotations {
                    is_vertical: vertical,
                    rotation_units: step as i16,
                }));
            }
        }
        ops
    }
}

fn mouse_button(button: egui::PointerButton) -> MouseButton {
    match button {
        egui::PointerButton::Primary => MouseButton::Left,
        egui::PointerButton::Secondary => MouseButton::Right,
        egui::PointerButton::Middle => MouseButton::Middle,
        egui::PointerButton::Extra1 => MouseButton::X1,
        egui::PointerButton::Extra2 => MouseButton::X2,
    }
}

/// The PC/AT (set 1) scancode of a key on a US keyboard; extended keys carry 0xE0 in the high
/// byte. egui names keys by their US position when it reports a physical key.
pub fn scancode(key: Key) -> Option<u16> {
    use Key::*;
    Some(match key {
        Escape => 0x01,
        Num1 | Exclamationmark => 0x02,
        Num2 => 0x03,
        Num3 => 0x04,
        Num4 => 0x05,
        Num5 => 0x06,
        Num6 => 0x07,
        Num7 => 0x08,
        Num8 => 0x09,
        Num9 => 0x0A,
        Num0 => 0x0B,
        Minus => 0x0C,
        Equals | Plus => 0x0D,
        Backspace => 0x0E,
        Tab => 0x0F,
        Q => 0x10,
        W => 0x11,
        E => 0x12,
        R => 0x13,
        T => 0x14,
        Y => 0x15,
        U => 0x16,
        I => 0x17,
        O => 0x18,
        P => 0x19,
        OpenBracket | OpenCurlyBracket => 0x1A,
        CloseBracket | CloseCurlyBracket => 0x1B,
        Enter => 0x1C,
        A => 0x1E,
        S => 0x1F,
        D => 0x20,
        F => 0x21,
        G => 0x22,
        H => 0x23,
        J => 0x24,
        K => 0x25,
        L => 0x26,
        Semicolon | Colon => 0x27,
        Quote => 0x28,
        Backtick => 0x29,
        Backslash | Pipe => 0x2B,
        Z => 0x2C,
        X => 0x2D,
        C => 0x2E,
        V => 0x2F,
        B => 0x30,
        N => 0x31,
        M => 0x32,
        Comma => 0x33,
        Period => 0x34,
        Slash | Questionmark => 0x35,
        Space => 0x39,
        F1 => 0x3B,
        F2 => 0x3C,
        F3 => 0x3D,
        F4 => 0x3E,
        F5 => 0x3F,
        F6 => 0x40,
        F7 => 0x41,
        F8 => 0x42,
        F9 => 0x43,
        F10 => 0x44,
        F11 => 0x57,
        F12 => 0x58,
        F13 => 0x64,
        F14 => 0x65,
        F15 => 0x66,
        F16 => 0x67,
        F17 => 0x68,
        F18 => 0x69,
        F19 => 0x6A,
        F20 => 0x6B,
        F21 => 0x6C,
        F22 => 0x6D,
        F23 => 0x6E,
        F24 => 0x76,
        Home => 0xE047,
        ArrowUp => 0xE048,
        PageUp => 0xE049,
        ArrowLeft => 0xE04B,
        ArrowRight => 0xE04D,
        End => 0xE04F,
        ArrowDown => 0xE050,
        PageDown => 0xE051,
        Insert => 0xE052,
        Delete => 0xE053,
        BrowserBack => 0xE06A,
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

    fn key(key: Key, physical: Option<Key>, pressed: bool, modifiers: Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: physical,
            pressed,
            repeat: false,
            modifiers,
        }
    }

    fn pressed(code: u16) -> Operation {
        Operation::KeyPressed(Scancode::from_u16(code))
    }

    fn released(code: u16) -> Operation {
        Operation::KeyReleased(Scancode::from_u16(code))
    }

    /// Operations are compared through their debug form, as ironrdp-input does not derive
    /// `PartialEq` for them.
    fn same(actual: Vec<Operation>, expected: Vec<Operation>) {
        assert_eq!(format!("{actual:?}"), format!("{expected:?}"));
    }

    #[test]
    fn keys_use_physical_positions_and_extended_codes() {
        assert_eq!(scancode(Key::A), Some(0x1E));
        assert_eq!(scancode(Key::Enter), Some(0x1C));
        assert_eq!(scancode(Key::F12), Some(0x58));
        assert_eq!(scancode(Key::Delete), Some(0xE053));
        assert_eq!(scancode(Key::Copy), None);
        let extended = Scancode::from_u16(scancode(Key::ArrowLeft).unwrap_or_default());
        assert_eq!(extended.as_u8(), (true, 0x4B));
        // On a French layout the key labeled A sits where US keyboards have Q.
        let mut t = Translator::default();
        let ops = t.translate(
            &[key(Key::A, Some(Key::Q), true, Modifiers::NONE)],
            Modifiers::NONE,
            &view(),
            true,
            true,
        );
        same(ops, vec![pressed(0x10)]);
    }

    #[test]
    fn modifiers_are_pressed_before_keys_and_released_after() {
        let mut t = Translator::default();
        let ops = t.translate(
            &[
                key(Key::T, Some(Key::T), true, Modifiers::SHIFT),
                key(Key::T, Some(Key::T), false, Modifiers::SHIFT),
            ],
            Modifiers::NONE,
            &view(),
            true,
            true,
        );
        same(
            ops,
            vec![
                pressed(LEFT_SHIFT),
                pressed(0x14),
                released(0x14),
                released(LEFT_SHIFT),
            ],
        );
        // A modifier held alone shows up only in the frame's state.
        let ops = t.translate(&[], Modifiers::ALT, &view(), true, true);
        same(ops, vec![pressed(LEFT_ALT)]);
    }

    #[test]
    fn clipboard_shortcuts_become_ctrl_keys() {
        let mut t = Translator::default();
        let ops = t.translate(
            &[egui::Event::Paste("text".into())],
            Modifiers::CTRL,
            &view(),
            true,
            true,
        );
        same(ops, vec![pressed(LEFT_CTRL), pressed(0x2F)]);
        let mac = Modifiers {
            mac_cmd: true,
            command: true,
            ..Modifiers::NONE
        };
        let mut t = Translator::default();
        let ops = t.translate(&[egui::Event::Copy], mac, &view(), true, true);
        same(ops, vec![pressed(LEFT_CTRL), pressed(0x2E)]);
    }

    #[test]
    fn keys_need_focus_and_the_pointer_needs_hover() {
        let mut t = Translator::default();
        let events = [
            key(Key::A, Some(Key::A), true, Modifiers::NONE),
            egui::Event::PointerMoved(pos2(300., 200.)),
            egui::Event::PointerButton {
                pos: pos2(300., 200.),
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Modifiers::NONE,
            },
        ];
        assert!(
            t.translate(&events, Modifiers::NONE, &view(), false, false)
                .is_empty()
        );
        // A release outside the desktop still ends a drag.
        let release = egui::Event::PointerButton {
            pos: pos2(0., 0.),
            button: egui::PointerButton::Secondary,
            pressed: false,
            modifiers: Modifiers::NONE,
        };
        let ops = t.translate(&[release], Modifiers::NONE, &view(), false, false);
        same(
            ops,
            vec![
                Operation::MouseMove(MousePosition { x: 0, y: 0 }),
                Operation::MouseButtonReleased(MouseButton::Right),
            ],
        );
    }

    #[test]
    fn wheel_notches_and_touchpad_scrolling() {
        let wheel = |unit, delta| egui::Event::MouseWheel {
            unit,
            delta,
            modifiers: Modifiers::NONE,
        };
        let rotations = |ops: Vec<Operation>| -> Vec<(bool, i16)> {
            ops.into_iter()
                .filter_map(|op| match op {
                    Operation::WheelRotations(w) => Some((w.is_vertical, w.rotation_units)),
                    _ => None,
                })
                .collect()
        };
        let mut t = Translator::default();
        // Two notches up, one at a time; then one to the right.
        let ops = t.translate(
            &[
                wheel(egui::MouseWheelUnit::Line, vec2(0., 2.)),
                wheel(egui::MouseWheelUnit::Line, vec2(-1., 0.)),
            ],
            Modifiers::NONE,
            &view(),
            false,
            true,
        );
        assert_eq!(rotations(ops), [(true, 120), (true, 120), (false, 120)]);
        // Touchpad scrolling accumulates until it is worth sending.
        let small = [wheel(egui::MouseWheelUnit::Point, vec2(0., -10.))];
        assert!(rotations(t.translate(&small, Modifiers::NONE, &view(), false, true)).is_empty());
        let ops = t.translate(&small, Modifiers::NONE, &view(), false, true);
        assert_eq!(rotations(ops), [(true, -48)]);
        assert!(
            t.translate(&small, Modifiers::NONE, &view(), false, false)
                .is_empty()
        );
    }
}

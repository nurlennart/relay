//! Mouse reports for programs that ask for them, such as htop, vim, mc and tmux.
use vt100::{MouseProtocolEncoding as Encoding, MouseProtocolMode as Mode};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Button {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

impl Button {
    pub fn from_egui(button: eframe::egui::PointerButton) -> Option<Self> {
        match button {
            eframe::egui::PointerButton::Primary => Some(Self::Left),
            eframe::egui::PointerButton::Middle => Some(Self::Middle),
            eframe::egui::PointerButton::Secondary => Some(Self::Right),
            _ => None,
        }
    }

    fn code(self) -> u32 {
        match self {
            Self::Left => 0,
            Self::Middle => 1,
            Self::Right => 2,
            Self::WheelUp => 64,
            Self::WheelDown => 65,
        }
    }

    fn wheel(self) -> bool {
        matches!(self, Self::WheelUp | Self::WheelDown)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Press(Button),
    Release(Button),
    /// The pointer moved to another cell, with a button held or none.
    Motion(Option<Button>),
}

/// The bytes that report `action` at the zero-based `cell`, or none when `mode` does not ask
/// for it or the cell is beyond what `encoding` can express.
pub fn report(
    mode: Mode,
    encoding: Encoding,
    action: Action,
    (row, col): (u16, u16),
    modifiers: eframe::egui::Modifiers,
) -> Vec<u8> {
    let wanted = match action {
        Action::Press(_) => mode != Mode::None,
        // Wheels have no release.
        Action::Release(button) => {
            !button.wheel()
                && matches!(
                    mode,
                    Mode::PressRelease | Mode::ButtonMotion | Mode::AnyMotion
                )
        }
        Action::Motion(Some(_)) => matches!(mode, Mode::ButtonMotion | Mode::AnyMotion),
        Action::Motion(None) => mode == Mode::AnyMotion,
    };
    if !wanted {
        return vec![];
    }
    let mut code = match action {
        Action::Press(button) => button.code(),
        // Only SGR says which button was released.
        Action::Release(button) if encoding == Encoding::Sgr => button.code(),
        Action::Release(_) => 3,
        Action::Motion(button) => 32 + button.map_or(3, Button::code),
    };
    // X10 mode reports no modifiers.
    if mode != Mode::Press {
        if modifiers.shift {
            code += 4;
        }
        if modifiers.alt {
            code += 8;
        }
        if modifiers.ctrl {
            code += 16;
        }
    }
    let (x, y) = (u32::from(col) + 1, u32::from(row) + 1);
    match encoding {
        Encoding::Sgr => {
            let end = if matches!(action, Action::Release(_)) {
                'm'
            } else {
                'M'
            };
            format!("\x1b[<{code};{x};{y}{end}").into_bytes()
        }
        // Each value is offset by 32 and sent as one byte, which limits it to 223.
        Encoding::Default => {
            let values = [code + 32, x + 32, y + 32];
            if values.iter().any(|v| *v > 255) {
                return vec![];
            }
            let mut bytes = b"\x1b[M".to_vec();
            bytes.extend(values.map(|v| v as u8));
            bytes
        }
        // As the default, with values encoded as UTF-8 characters, up to 2047.
        Encoding::Utf8 => {
            let values = [code + 32, x + 32, y + 32];
            let mut text = String::from("\x1b[M");
            for value in values {
                match char::from_u32(value).filter(|_| value < 2048) {
                    Some(c) => text.push(c),
                    None => return vec![],
                }
            }
            text.into_bytes()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Modifiers;

    #[test]
    fn reports_follow_the_mode_and_encoding() {
        let none = Modifiers::NONE;
        let sgr = |mode, action| report(mode, Encoding::Sgr, action, (4, 9), none);
        assert_eq!(
            sgr(Mode::PressRelease, Action::Press(Button::Left)),
            b"\x1b[<0;10;5M"
        );
        assert_eq!(
            sgr(Mode::PressRelease, Action::Release(Button::Right)),
            b"\x1b[<2;10;5m"
        );
        assert_eq!(
            sgr(Mode::PressRelease, Action::Press(Button::WheelUp)),
            b"\x1b[<64;10;5M"
        );
        assert!(sgr(Mode::PressRelease, Action::Release(Button::WheelUp)).is_empty());
        // Motion only where asked for: with a button held, or always.
        assert!(sgr(Mode::PressRelease, Action::Motion(Some(Button::Left))).is_empty());
        assert_eq!(
            sgr(Mode::ButtonMotion, Action::Motion(Some(Button::Left))),
            b"\x1b[<32;10;5M"
        );
        assert!(sgr(Mode::ButtonMotion, Action::Motion(None)).is_empty());
        assert_eq!(
            sgr(Mode::AnyMotion, Action::Motion(None)),
            b"\x1b[<35;10;5M"
        );
        // X10 reports presses alone; with no mode, nothing is reported.
        assert!(sgr(Mode::Press, Action::Release(Button::Left)).is_empty());
        assert!(sgr(Mode::None, Action::Press(Button::Left)).is_empty());
        let ctrl_shift = Modifiers {
            ctrl: true,
            shift: true,
            ..Modifiers::NONE
        };
        let modified = report(
            Mode::PressRelease,
            Encoding::Sgr,
            Action::Press(Button::Middle),
            (0, 0),
            ctrl_shift,
        );
        assert_eq!(modified, b"\x1b[<21;1;1M");
    }

    #[test]
    fn legacy_encodings_offset_values_and_have_limits() {
        let none = Modifiers::NONE;
        let press = Action::Press(Button::Left);
        let release = Action::Release(Button::Left);
        let default =
            |action, cell| report(Mode::PressRelease, Encoding::Default, action, cell, none);
        assert_eq!(default(press, (4, 9)), b"\x1b[M\x20\x2a\x25");
        assert_eq!(default(release, (4, 9)), b"\x1b[M\x23\x2a\x25");
        assert!(
            default(press, (0, 223)).is_empty(),
            "column 224 does not fit a byte"
        );
        let utf8 = report(Mode::PressRelease, Encoding::Utf8, press, (0, 299), none);
        assert_eq!(utf8, "\x1b[M\u{20}\u{14c}\u{21}".as_bytes());
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0

//! Key numbers as menu QuakeC sees them (FTE/DP `K_*` codes), and the mapping from winit keys.

use winit::event::MouseButton;
use winit::keyboard::KeyCode;

pub const TAB: f32 = 9.0;
pub const ENTER: f32 = 13.0;
pub const ESCAPE: f32 = 27.0;
pub const SPACE: f32 = 32.0;
pub const BACKSPACE: f32 = 127.0;
pub const UPARROW: f32 = 128.0;
pub const DOWNARROW: f32 = 129.0;
pub const LEFTARROW: f32 = 130.0;
pub const RIGHTARROW: f32 = 131.0;
pub const ALT: f32 = 132.0;
pub const CTRL: f32 = 133.0;
pub const SHIFT: f32 = 134.0;
pub const F1: f32 = 135.0;
pub const INS: f32 = 147.0;
pub const DEL: f32 = 148.0;
pub const PGDN: f32 = 149.0;
pub const PGUP: f32 = 150.0;
pub const HOME: f32 = 151.0;
pub const END: f32 = 152.0;
pub const PAUSE: f32 = 153.0;
pub const MOUSE1: f32 = 512.0;
pub const MOUSE2: f32 = 513.0;
pub const MOUSE3: f32 = 514.0;
pub const MWHEELUP: f32 = 515.0;
pub const MWHEELDOWN: f32 = 516.0;

/// The key number for a physical key, as on a US layout (Quake binds keys, not characters, and
/// applies shift itself). Right-hand modifiers report as the left-hand ones.
pub fn from_keycode(code: KeyCode) -> Option<f32> {
    let ascii = |c: u8| Some(f32::from(c));
    match code {
        KeyCode::Tab => Some(TAB),
        KeyCode::Enter | KeyCode::NumpadEnter => Some(ENTER),
        KeyCode::Escape => Some(ESCAPE),
        KeyCode::Space => Some(SPACE),
        KeyCode::Backspace => Some(BACKSPACE),
        KeyCode::ArrowUp => Some(UPARROW),
        KeyCode::ArrowDown => Some(DOWNARROW),
        KeyCode::ArrowLeft => Some(LEFTARROW),
        KeyCode::ArrowRight => Some(RIGHTARROW),
        KeyCode::AltLeft | KeyCode::AltRight => Some(ALT),
        KeyCode::ControlLeft | KeyCode::ControlRight => Some(CTRL),
        KeyCode::ShiftLeft | KeyCode::ShiftRight => Some(SHIFT),
        KeyCode::F1 => Some(F1),
        KeyCode::F2 => Some(F1 + 1.0),
        KeyCode::F3 => Some(F1 + 2.0),
        KeyCode::F4 => Some(F1 + 3.0),
        KeyCode::F5 => Some(F1 + 4.0),
        KeyCode::F6 => Some(F1 + 5.0),
        KeyCode::F7 => Some(F1 + 6.0),
        KeyCode::F8 => Some(F1 + 7.0),
        KeyCode::F9 => Some(F1 + 8.0),
        KeyCode::F10 => Some(F1 + 9.0),
        KeyCode::F11 => Some(F1 + 10.0),
        KeyCode::F12 => Some(F1 + 11.0),
        KeyCode::Insert => Some(INS),
        KeyCode::Delete => Some(DEL),
        KeyCode::PageDown => Some(PGDN),
        KeyCode::PageUp => Some(PGUP),
        KeyCode::Home => Some(HOME),
        KeyCode::End => Some(END),
        KeyCode::Pause => Some(PAUSE),
        KeyCode::Backquote => ascii(b'`'),
        KeyCode::Minus => ascii(b'-'),
        KeyCode::Equal => ascii(b'='),
        KeyCode::BracketLeft => ascii(b'['),
        KeyCode::BracketRight => ascii(b']'),
        KeyCode::Backslash => ascii(b'\\'),
        KeyCode::Semicolon => ascii(b';'),
        KeyCode::Quote => ascii(b'\''),
        KeyCode::Comma => ascii(b','),
        KeyCode::Period => ascii(b'.'),
        KeyCode::Slash => ascii(b'/'),
        KeyCode::Digit0 => ascii(b'0'),
        KeyCode::Digit1 => ascii(b'1'),
        KeyCode::Digit2 => ascii(b'2'),
        KeyCode::Digit3 => ascii(b'3'),
        KeyCode::Digit4 => ascii(b'4'),
        KeyCode::Digit5 => ascii(b'5'),
        KeyCode::Digit6 => ascii(b'6'),
        KeyCode::Digit7 => ascii(b'7'),
        KeyCode::Digit8 => ascii(b'8'),
        KeyCode::Digit9 => ascii(b'9'),
        KeyCode::KeyA => ascii(b'a'),
        KeyCode::KeyB => ascii(b'b'),
        KeyCode::KeyC => ascii(b'c'),
        KeyCode::KeyD => ascii(b'd'),
        KeyCode::KeyE => ascii(b'e'),
        KeyCode::KeyF => ascii(b'f'),
        KeyCode::KeyG => ascii(b'g'),
        KeyCode::KeyH => ascii(b'h'),
        KeyCode::KeyI => ascii(b'i'),
        KeyCode::KeyJ => ascii(b'j'),
        KeyCode::KeyK => ascii(b'k'),
        KeyCode::KeyL => ascii(b'l'),
        KeyCode::KeyM => ascii(b'm'),
        KeyCode::KeyN => ascii(b'n'),
        KeyCode::KeyO => ascii(b'o'),
        KeyCode::KeyP => ascii(b'p'),
        KeyCode::KeyQ => ascii(b'q'),
        KeyCode::KeyR => ascii(b'r'),
        KeyCode::KeyS => ascii(b's'),
        KeyCode::KeyT => ascii(b't'),
        KeyCode::KeyU => ascii(b'u'),
        KeyCode::KeyV => ascii(b'v'),
        KeyCode::KeyW => ascii(b'w'),
        KeyCode::KeyX => ascii(b'x'),
        KeyCode::KeyY => ascii(b'y'),
        KeyCode::KeyZ => ascii(b'z'),
        _ => None,
    }
}

pub fn from_mouse_button(button: MouseButton) -> Option<f32> {
    match button {
        MouseButton::Left => Some(MOUSE1),
        MouseButton::Right => Some(MOUSE2),
        MouseButton::Middle => Some(MOUSE3),
        _ => None,
    }
}

/// `stringtokeynum`: the number of a named key, or -1. Only names for keys this host sends;
/// there are no separate right-hand modifiers (`ralt`, `rctrl`, `rshift` → -1).
pub fn from_name(name: &[u8]) -> f32 {
    let name = name.to_ascii_lowercase();
    let named: &[(&[u8], f32)] = &[
        (b"tab", TAB),
        (b"enter", ENTER),
        (b"escape", ESCAPE),
        (b"space", SPACE),
        (b"backspace", BACKSPACE),
        (b"uparrow", UPARROW),
        (b"downarrow", DOWNARROW),
        (b"leftarrow", LEFTARROW),
        (b"rightarrow", RIGHTARROW),
        (b"alt", ALT),
        (b"lalt", ALT),
        (b"ctrl", CTRL),
        (b"lctrl", CTRL),
        (b"shift", SHIFT),
        (b"lshift", SHIFT),
        (b"ins", INS),
        (b"del", DEL),
        (b"pgdn", PGDN),
        (b"pgup", PGUP),
        (b"home", HOME),
        (b"end", END),
        (b"pause", PAUSE),
        (b"mouse1", MOUSE1),
        (b"mouse2", MOUSE2),
        (b"mouse3", MOUSE3),
        (b"mwheelup", MWHEELUP),
        (b"mwheeldown", MWHEELDOWN),
    ];
    if let Some(&(_, key)) = named.iter().find(|(n, _)| *n == name.as_slice()) {
        return key;
    }
    match name.as_slice() {
        [c] if c.is_ascii_graphic() => f32::from(*c),
        [b'f', digits @ ..] => {
            match std::str::from_utf8(digits).ok().and_then(|d| d.parse().ok()) {
                Some(n @ 1..=12u8) => F1 + f32::from(n - 1),
                _ => -1.0,
            }
        }
        _ => -1.0,
    }
}

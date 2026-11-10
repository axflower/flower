//! Translation from Windows virtual-key values to the platform-neutral API.

use super::{Key, KeyEvent, Modifiers};

pub const VK_BACK: u32 = 0x08;
pub const VK_RETURN: u32 = 0x0D;
pub const VK_ESCAPE: u32 = 0x1B;
pub const VK_SPACE: u32 = 0x20;
pub const VK_PRIOR: u32 = 0x21;
pub const VK_NEXT: u32 = 0x22;
pub const VK_LEFT: u32 = 0x25;
pub const VK_UP: u32 = 0x26;
pub const VK_RIGHT: u32 = 0x27;
pub const VK_DOWN: u32 = 0x28;
pub const VK_DELETE: u32 = 0x2E;

/// Maps the Windows virtual-key subset used by the MVP.
///
/// The function is available on every target so it can be unit tested without
/// linking the Windows crate. Unsupported keys return `None` and must be
/// passed through by the TSF layer.
pub fn map_windows_virtual_key(virtual_key: u32, modifiers: Modifiers) -> Option<KeyEvent> {
    let key = match virtual_key {
        0x30..=0x39 if modifiers.shift => {
            Key::Char(b")!@#$%^&*("[(virtual_key - 0x30) as usize] as char)
        }
        0x30..=0x39 => Key::Char((b'0' + (virtual_key - 0x30) as u8) as char),
        0x41..=0x5A => Key::Char((b'a' + (virtual_key - 0x41) as u8) as char),
        0xDE => Key::Char(if modifiers.shift { '"' } else { '\'' }),
        0xBA => Key::Char(if modifiers.shift { ':' } else { ';' }),
        0xBC => Key::Char(if modifiers.shift { '<' } else { ',' }),
        0xBE => Key::Char(if modifiers.shift { '>' } else { '.' }),
        0xBF => Key::Char(if modifiers.shift { '?' } else { '/' }),
        0xC0 => Key::Char(if modifiers.shift { '~' } else { '`' }),
        0xDB => Key::Char(if modifiers.shift { '{' } else { '[' }),
        0xDC => Key::Char(if modifiers.shift { '|' } else { '\\' }),
        0xDD => Key::Char(if modifiers.shift { '}' } else { ']' }),
        0xBD if modifiers.shift => Key::Char('_'),
        0xBB if modifiers.shift => Key::Char('+'),
        0xBB | 0x6B => Key::PageDown,            // = and numpad +
        0xBD if !modifiers.shift => Key::PageUp, // - but not _
        0x6D => Key::PageUp,                     // numpad -
        VK_BACK => Key::Backspace,
        VK_DELETE => Key::Delete,
        VK_RETURN => Key::Enter,
        VK_ESCAPE => Key::Escape,
        VK_SPACE => Key::Space,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        VK_LEFT => Key::Left,
        VK_UP => Key::Up,
        VK_RIGHT => Key::Right,
        VK_DOWN => Key::Down,
        _ => return None,
    };

    Some(KeyEvent::new(key, modifiers))
}

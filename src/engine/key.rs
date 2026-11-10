/// A platform-independent key understood by the input session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Key {
    Char(char),
    Backspace,
    Delete,
    Enter,
    Space,
    Escape,
    PageUp,
    PageDown,
    Left,
    Right,
    Up,
    Down,
    #[default]
    Other,
}

impl Key {
    /// Creates a character key. The engine normalizes ASCII letters itself.
    pub const fn character(value: char) -> Self {
        Self::Char(value)
    }

    /// Creates a key for the digit `1..=9`.
    pub const fn candidate_digit(value: u8) -> Option<Self> {
        if value >= 1 && value <= 9 {
            Some(Self::Char((b'0' + value) as char))
        } else {
            None
        }
    }
}

/// Modifier state attached to a key event.
///
/// Shift is allowed during composition because Pinyin letters are normalized
/// to lowercase. Command-style modifiers are passed through to the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub meta: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        shift: false,
        control: false,
        alt: false,
        meta: false,
    };

    pub const SHIFT: Self = Self {
        shift: true,
        ..Self::NONE
    };

    pub const CONTROL: Self = Self {
        control: true,
        ..Self::NONE
    };

    pub const ALT: Self = Self {
        alt: true,
        ..Self::NONE
    };

    pub const META: Self = Self {
        meta: true,
        ..Self::NONE
    };

    /// Returns true when the key belongs to a host or system shortcut.
    pub const fn is_command(self) -> bool {
        self.control || self.alt || self.meta
    }

    pub const fn with_shift(mut self) -> Self {
        self.shift = true;
        self
    }

    pub const fn with_control(mut self) -> Self {
        self.control = true;
        self
    }

    pub const fn with_alt(mut self) -> Self {
        self.alt = true;
        self
    }

    pub const fn with_meta(mut self) -> Self {
        self.meta = true;
        self
    }
}

/// A key plus its modifier state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct KeyEvent {
    pub key: Key,
    pub modifiers: Modifiers,
}

impl KeyEvent {
    pub const fn is_script_toggle(self) -> bool {
        matches!(self.key, Key::Char('f') | Key::Char('F'))
            && self.modifiers.control
            && self.modifiers.shift
            && !self.modifiers.alt
            && !self.modifiers.meta
    }

    pub const fn new(key: Key, modifiers: Modifiers) -> Self {
        Self { key, modifiers }
    }

    pub const fn plain(key: Key) -> Self {
        Self::new(key, Modifiers::NONE)
    }

    /// Command shortcuts remain host-owned except the exact script toggle.
    pub const fn is_passthrough(self) -> bool {
        self.modifiers.is_command() && !self.is_script_toggle()
    }
}

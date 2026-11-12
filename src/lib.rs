#![cfg_attr(not(target_os = "windows"), forbid(unsafe_code))]
//! Flower's platform-independent engine, settings, and platform boundary.
//!
//! Hosts normally use the public modules directly:
//! [`engine`], [`settings`], [`registration`], and [`platform`]. The most common engine and
//! settings items are also re-exported at the crate root for convenience.

pub mod ai;
pub mod chinese;
pub use chinese::ChineseScript;
pub mod engine;
pub mod platform;
pub mod registration;
pub mod settings;
pub mod user_dictionary;
pub mod writing;

pub use engine::{
    keymap, Candidate, CompositionUpdate, EngineError, EngineRouter, EngineSession, Key, KeyEvent,
    Lexicon, Modifiers, PinyinSession, DEFAULT_PAGE_SIZE,
};
pub use platform::{is_supported_platform, TEXT_SERVICE_DESCRIPTION};
pub use settings::{
    Settings, SettingsError, SettingsStore, Theme, DEFAULT_CANDIDATE_PAGE_SIZE,
    MAX_CANDIDATE_PAGE_SIZE, MIN_CANDIDATE_PAGE_SIZE,
};

#[cfg(target_os = "windows")]
pub use platform::{DllCanUnloadNow, DllGetClassObject, FLOWER_TEXT_SERVICE_CLSID};

//! Small, UI-independent settings model and JSON persistence.
//!
//! The native settings UI validates values with [`Settings::validate`] and uses
//! [`SettingsStore`] for persistence. TSF activation reads the same user store
//! once per activation, outside its live state borrow.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(target_os = "windows")]
use windows::core::PWSTR;

pub const DEFAULT_CANDIDATE_PAGE_SIZE: usize = 7;
pub const MIN_CANDIDATE_PAGE_SIZE: usize = 1;
pub const MAX_CANDIDATE_PAGE_SIZE: usize = 99;

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

/// Persisted settings shared by the TSF service and the native settings window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub chinese_script: crate::ChineseScript,
    pub candidate_page_size: usize,
    /// Zero is a legacy file whose old default page size has not been migrated.
    #[serde(default)]
    pub candidate_layout_version: u8,
    pub theme: Theme,
    pub ai_correction_enabled: bool,
    pub ai_undeclared_scope_compatibility: bool,
    pub online_polish: OnlinePolishSettings,
    /// Explicit opt-in for both online writing and selection polishing.
    #[serde(default)]
    pub online_ai_enabled: bool,
    pub writing_shortcuts: WritingShortcuts,
    pub writing_send_consent: Option<String>,
    pub model_api: crate::writing::config::ModelApiSettings,
}

/// A context-local key chord, never a registered global hotkey.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WritingShortcut {
    /// Zero denotes a modifier-only gesture.
    pub key: u16,
    #[serde(default = "legacy_shortcut_modifiers")]
    pub modifiers: u8,
}

pub const SHORTCUT_CTRL: u8 = 1;
pub const SHORTCUT_ALT: u8 = 2;
pub const SHORTCUT_SHIFT: u8 = 4;
pub const SHORTCUT_WIN: u8 = 8;
const fn legacy_shortcut_modifiers() -> u8 {
    SHORTCUT_CTRL | SHORTCUT_SHIFT
}

impl WritingShortcut {
    pub const fn new(key: u16, modifiers: u8) -> Self {
        Self { key, modifiers }
    }

    pub fn is_supported(self) -> bool {
        if self.modifiers == 0 || self.modifiers & !15 != 0 {
            return false;
        }
        if self.key == 0 {
            return self.modifiers.count_ones() >= 2;
        }
        if !(0x08..=0xfe).contains(&self.key)
            || matches!(self.key, 0x10..=0x12 | 0x5b..=0x5c | 0xa0..=0xa5)
        {
            return false;
        }
        !matches!(
            (self.key, self.modifiers),
            (0x46, 5) | (0x1b, 1) | (0x1b, 5) | (0x09, 2) | (0x73, 2) | (0x4c, 8) | (0x09, 8)
        ) && !(self.key == 0x2e && self.modifiers & 3 == 3)
    }

    pub fn matches(self, key: u16, modifiers: crate::engine::Modifiers) -> bool {
        self.is_supported()
            && self.key != 0
            && self.key == key
            && self.modifiers == Self::modifier_mask(modifiers)
    }

    pub fn modifier_mask(value: crate::engine::Modifiers) -> u8 {
        u8::from(value.control)
            | (u8::from(value.alt) << 1)
            | (u8::from(value.shift) << 2)
            | (u8::from(value.meta) << 3)
    }

    pub fn label(self) -> String {
        let mut parts = Vec::new();
        for (bit, name) in [(1, "Ctrl"), (2, "Alt"), (4, "Shift"), (8, "Win")] {
            if self.modifiers & bit != 0 {
                parts.push(name.to_owned());
            }
        }
        let key = match self.key {
            0 => String::new(),
            0x30..=0x39 | 0x41..=0x5a => char::from_u32(self.key as u32).unwrap().to_string(),
            0x70..=0x87 => format!("F{}", self.key - 0x70 + 1),
            0x20 => "Space".into(),
            0x0d => "Enter".into(),
            0x09 => "Tab".into(),
            0x08 => "Backspace".into(),
            0x1b => "Esc".into(),
            0x2e => "Delete".into(),
            0x25 => "Left".into(),
            0x26 => "Up".into(),
            0x27 => "Right".into(),
            0x28 => "Down".into(),
            0x24 => "Home".into(),
            0x23 => "End".into(),
            0x21 => "PageUp".into(),
            0x22 => "PageDown".into(),
            0xba => ";".into(),
            0xbb => "=".into(),
            0xbc => ",".into(),
            0xbd => "-".into(),
            0xbe => ".".into(),
            0xbf => "/".into(),
            0xc0 => "`".into(),
            0xdb => "[".into(),
            0xdc => "\\".into(),
            0xdd => "]".into(),
            0xde => "'".into(),
            _ => format!("VK 0x{:02X}", self.key),
        };
        if !key.is_empty() {
            parts.push(key);
        }
        parts.join("+")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct WritingShortcuts {
    pub draft: WritingShortcut,
    pub polish: WritingShortcut,
}

impl Default for WritingShortcuts {
    fn default() -> Self {
        Self {
            draft: WritingShortcut::new(0, SHORTCUT_CTRL | SHORTCUT_ALT),
            polish: WritingShortcut::new(0x20, SHORTCUT_CTRL),
        }
    }
}

impl<'de> Deserialize<'de> for WritingShortcuts {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize, Default)]
        #[serde(default)]
        struct Raw {
            draft: Option<RawShortcut>,
            polish: Option<RawShortcut>,
        }
        #[derive(Deserialize)]
        struct RawShortcut {
            key: u16,
            modifiers: Option<u8>,
        }
        impl RawShortcut {
            fn shortcut(self) -> WritingShortcut {
                WritingShortcut::new(
                    self.key,
                    self.modifiers.unwrap_or_else(legacy_shortcut_modifiers),
                )
            }
        }
        let raw = Raw::deserialize(deserializer)?;
        let legacy_pair = raw.draft.as_ref().is_some_and(|s| s.modifiers.is_none())
            && raw.polish.as_ref().is_some_and(|s| s.modifiers.is_none());
        let defaults = Self::default();
        let value = Self {
            draft: raw
                .draft
                .map(RawShortcut::shortcut)
                .unwrap_or(defaults.draft),
            polish: raw
                .polish
                .map(RawShortcut::shortcut)
                .unwrap_or(defaults.polish),
        };
        if legacy_pair
            && value.draft == WritingShortcut::new(0x31, 5)
            && value.polish == WritingShortcut::new(0x32, 5)
        {
            Ok(defaults)
        } else {
            Ok(value)
        }
    }
}

impl WritingShortcuts {
    pub fn is_valid(self) -> bool {
        self.draft.is_supported() && self.polish.is_supported() && self.draft != self.polish
    }
}

/// Enabled by default; external processing still requires an explicit shortcut and safe selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OnlinePolishSettings {
    pub enabled: bool,
    pub style: crate::writing::PolishStyle,
    /// None follows the activation's current script at the start of the task.
    pub script: Option<crate::ChineseScript>,
}

impl Default for OnlinePolishSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            style: crate::writing::PolishStyle::default(),
            script: None,
        }
    }
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            chinese_script: crate::ChineseScript::default(),
            candidate_page_size: DEFAULT_CANDIDATE_PAGE_SIZE,
            candidate_layout_version: 1,
            theme: Theme::System,
            ai_correction_enabled: true,
            ai_undeclared_scope_compatibility: true,
            online_polish: OnlinePolishSettings::default(),
            online_ai_enabled: false,
            writing_shortcuts: WritingShortcuts::default(),
            writing_send_consent: None,
            model_api: crate::writing::config::ModelApiSettings::default(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), SettingsError> {
        self.model_api
            .validate()
            .map_err(|_| SettingsError::InvalidModelApi)?;
        if !self.writing_shortcuts.is_valid() {
            return Err(SettingsError::InvalidWritingShortcuts);
        }
        if !(MIN_CANDIDATE_PAGE_SIZE..=MAX_CANDIDATE_PAGE_SIZE).contains(&self.candidate_page_size)
        {
            return Err(SettingsError::InvalidCandidatePageSize {
                actual: self.candidate_page_size,
                min: MIN_CANDIDATE_PAGE_SIZE,
                max: MAX_CANDIDATE_PAGE_SIZE,
            });
        }
        Ok(())
    }
}

#[cfg(any(target_os = "windows", test))]
pub(crate) fn parse_candidate_page_size_input(text: &str) -> Option<usize> {
    let parsed = text.trim().parse::<usize>().ok()?;
    (MIN_CANDIDATE_PAGE_SIZE..=MAX_CANDIDATE_PAGE_SIZE)
        .contains(&parsed)
        .then_some(parsed)
}

/// Candidate-window theme choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

/// JSON-backed settings store at a caller-selected path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsStore {
    path: PathBuf,
}

impl SettingsStore {
    /// Returns the per-user store on Windows at
    /// `%LOCALAPPDATA%\Flower\settings.json`.
    ///
    /// Other targets return [`SettingsError::UnsupportedPlatform`]. This
    /// function never falls back to the process working directory or the
    /// location of the running DLL/executable.
    pub fn for_current_user() -> Result<Self, SettingsError> {
        current_user_settings_path().map(Self::new)
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Loads settings, returning defaults when the file does not exist.
    pub fn load(&self) -> Result<Settings, SettingsError> {
        let json = match fs::read_to_string(&self.path) {
            Ok(json) => json,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Settings::default());
            }
            Err(error) => return Err(error.into()),
        };

        let mut settings: Settings = serde_json::from_str(&json)?;
        if settings.candidate_layout_version == 0 {
            if settings.candidate_page_size == 9 {
                settings.candidate_page_size = DEFAULT_CANDIDATE_PAGE_SIZE;
            }
            settings.candidate_layout_version = 1;
        }
        settings.validate()?;
        Ok(settings)
    }

    /// Validates and writes settings as pretty-printed UTF-8 JSON.
    pub fn save(&self, settings: &Settings) -> Result<(), SettingsError> {
        settings.validate()?;
        let json = serde_json::to_vec_pretty(settings)?;

        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        save_atomic(&self.path, &json)?;
        Ok(())
    }
}

fn save_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let (temporary_path, temporary_file) = create_sibling_temp_file(path)?;
    let write_result = write_and_replace(temporary_file, &temporary_path, path, contents);

    match write_result {
        Ok(()) => Ok(()),
        Err(error) => match fs::remove_file(&temporary_path) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(io::Error::new(
                error.kind(),
                format!(
                    "{error}; additionally failed to remove temporary settings file: \
                     {cleanup_error}"
                ),
            )),
        },
    }
}

fn write_and_replace(
    mut temporary_file: File,
    temporary_path: &Path,
    path: &Path,
    contents: &[u8],
) -> io::Result<()> {
    temporary_file.write_all(contents)?;
    temporary_file.flush()?;
    temporary_file.sync_all()?;
    drop(temporary_file);
    replace_file(temporary_path, path)
}

fn create_sibling_temp_file(path: &Path) -> io::Result<(PathBuf, File)> {
    let file_name = path.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "settings path must include a file name",
        )
    })?;
    let parent = path.parent().unwrap_or_else(|| Path::new(""));

    for _ in 0..128 {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let mut temporary_name = OsString::from(file_name);
        temporary_name.push(format!(".{}-{sequence}.tmp", std::process::id()));
        let temporary_path = parent.join(temporary_name);

        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
        {
            Ok(file) => return Ok((temporary_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a unique temporary settings file",
    ))
}

#[cfg(not(target_os = "windows"))]
fn replace_file(temporary_path: &Path, path: &Path) -> io::Result<()> {
    fs::rename(temporary_path, path)
}

#[cfg(target_os = "windows")]
fn replace_file(temporary_path: &Path, path: &Path) -> io::Result<()> {
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let temporary = wide_null(temporary_path);
    let destination = wide_null(path);

    // SAFETY: Both buffers are null-terminated UTF-16 paths that remain alive
    // for the call. The temporary file is in the destination directory, so the
    // native replacement stays on one volume.
    unsafe {
        MoveFileExW(
            PCWSTR::from_raw(temporary.as_ptr()),
            PCWSTR::from_raw(destination.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    }
    .map_err(|error| io::Error::other(format!("could not replace settings file: {error}")))
}

#[cfg(target_os = "windows")]
fn current_user_settings_path() -> Result<PathBuf, SettingsError> {
    use std::os::windows::ffi::OsStringExt;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::UI::Shell::{FOLDERID_LocalAppData, SHGetKnownFolderPath, KF_FLAG_DEFAULT};

    // SAFETY: The known-folder ID is a valid static GUID. A null token asks
    // Shell for the current user's folder, and the returned allocation is
    // owned by the RAII guard below.
    let allocated =
        unsafe { SHGetKnownFolderPath(&FOLDERID_LocalAppData, KF_FLAG_DEFAULT, HANDLE::default()) }
            .map_err(|error| io::Error::other(format!("could not locate LocalAppData: {error}")))?;
    let allocation = CoTaskMemory { pointer: allocated };

    // SAFETY: SHGetKnownFolderPath returns a null-terminated UTF-16 string
    // owned by `allocation`, which keeps it alive for this conversion.
    let local_app_data = OsString::from_wide(unsafe { allocation.pointer.as_wide() });
    let mut path = PathBuf::from(local_app_data);
    path.push("Flower");
    path.push("settings.json");
    Ok(path)
}

#[cfg(not(target_os = "windows"))]
fn current_user_settings_path() -> Result<PathBuf, SettingsError> {
    Err(SettingsError::UnsupportedPlatform)
}

#[cfg(target_os = "windows")]
struct CoTaskMemory {
    pointer: PWSTR,
}

#[cfg(target_os = "windows")]
impl Drop for CoTaskMemory {
    fn drop(&mut self) {
        let pointer = self.pointer.0;
        // SAFETY: This allocation came from SHGetKnownFolderPath and is freed
        // exactly once when the owning guard is dropped.
        unsafe {
            windows::Win32::System::Com::CoTaskMemFree(Some(pointer.cast()));
        }
    }
}

#[cfg(target_os = "windows")]
fn wide_null(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[derive(Debug, Error)]
pub enum SettingsError {
    #[error("model API settings are invalid")]
    InvalidModelApi,
    #[error("AI快捷键必须是两个不同的有效组合；Ctrl+Shift+F保留简繁切换，系统保留组合不能使用")]
    InvalidWritingShortcuts,
    #[error("candidate page size must be between {min} and {max}, got {actual}")]
    InvalidCandidatePageSize {
        actual: usize,
        min: usize,
        max: usize,
    },

    #[error("could not read or write settings: {0}")]
    Io(#[from] io::Error),

    #[error("settings JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),

    #[error("the per-user settings path is only available on Windows")]
    UnsupportedPlatform,
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    static NEXT_TEST_DIRECTORY: AtomicU64 = AtomicU64::new(1);

    fn temporary_store() -> SettingsStore {
        let nonce = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "flower-settings-test-{}-{timestamp}-{nonce}",
            std::process::id(),
        ));
        SettingsStore::new(directory.join("settings.json"))
    }

    #[test]
    fn defaults_match_the_mvp_contract() {
        let settings = Settings::default();
        assert_eq!(settings.candidate_page_size, 7);
        assert_eq!(settings.theme, Theme::System);
        assert!(settings.ai_correction_enabled);
        assert!(settings.ai_undeclared_scope_compatibility);
        assert_eq!(settings.online_polish, OnlinePolishSettings::default());
        assert!(settings.online_polish.enabled);
        assert!(!settings.online_ai_enabled);
        assert_eq!(settings.candidate_layout_version, 1);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn legacy_settings_use_new_default_but_preserve_explicit_opt_out() {
        let old: Settings =
            serde_json::from_str(r#"{"candidate_page_size":7,"theme":"dark"}"#).unwrap();
        assert!(old.ai_correction_enabled);
        assert!(old.ai_undeclared_scope_compatibility);
        assert!(old.online_polish.enabled);
        let opted_out = Settings {
            ai_correction_enabled: false,
            ai_undeclared_scope_compatibility: false,
            ..old
        };
        let restored: Settings =
            serde_json::from_str(&serde_json::to_string(&opted_out).unwrap()).unwrap();
        assert!(!restored.ai_correction_enabled);
        assert!(!restored.ai_undeclared_scope_compatibility);
        assert_eq!(restored.candidate_page_size, 7);
        assert_eq!(restored.theme, Theme::Dark);
    }

    #[test]
    fn old_default_page_size_migrates_once_without_rewriting_or_enabling_online() {
        let store = temporary_store();
        fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        for (json, expected) in [
            (
                r#"{"candidate_page_size":9,"online_polish":{"enabled":true}}"#,
                7,
            ),
            (r#"{"candidate_page_size":5}"#, 5),
            (
                r#"{"candidate_page_size":9,"candidate_layout_version":1}"#,
                9,
            ),
        ] {
            fs::write(store.path(), json).unwrap();
            let loaded = store.load().unwrap();
            assert_eq!(loaded.candidate_page_size, expected);
            assert_eq!(loaded.candidate_layout_version, 1);
            assert!(!loaded.online_ai_enabled);
            assert_eq!(fs::read_to_string(store.path()).unwrap(), json);
        }
        let mut opted_in = store.load().unwrap();
        opted_in.online_ai_enabled = true;
        opted_in.candidate_page_size = 9;
        store.save(&opted_in).unwrap();
        let reloaded = store.load().unwrap();
        assert!(reloaded.online_ai_enabled);
        assert_eq!(reloaded.candidate_page_size, 9);
        opted_in.online_ai_enabled = false;
        store.save(&opted_in).unwrap();
        assert!(!store.load().unwrap().online_ai_enabled);
        fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
    }

    #[test]
    fn candidate_page_size_input_requires_a_complete_in_range_integer() {
        assert_eq!(parse_candidate_page_size_input("1"), Some(1));
        assert_eq!(parse_candidate_page_size_input("99"), Some(99));
        assert_eq!(parse_candidate_page_size_input("0"), None);
        assert_eq!(parse_candidate_page_size_input("100"), None);
        assert_eq!(parse_candidate_page_size_input(""), None);
        assert_eq!(parse_candidate_page_size_input("5abc"), None);
    }

    #[test]
    fn online_polish_defaults_on_but_preserves_explicit_disable_and_valid_enums() {
        let missing: Settings = serde_json::from_str("{}").unwrap();
        let partial: Settings =
            serde_json::from_str(r#"{"online_polish":{"style":"concise"}}"#).unwrap();
        assert!(missing.online_polish.enabled);
        assert!(partial.online_polish.enabled);
        let disabled: Settings =
            serde_json::from_str(r#"{"online_polish":{"enabled":false}}"#).unwrap();
        assert!(!disabled.online_polish.enabled);
        assert_eq!(
            partial.online_polish.style,
            crate::writing::PolishStyle::Concise
        );
        assert_eq!(partial.online_polish.script, None);
        let configured = OnlinePolishSettings {
            enabled: true,
            style: crate::writing::PolishStyle::Formal,
            script: Some(crate::ChineseScript::Traditional),
        };
        let wire = serde_json::to_string(&configured).unwrap();
        assert_eq!(
            serde_json::from_str::<OnlinePolishSettings>(&wire).unwrap(),
            configured
        );
        assert!(
            serde_json::from_str::<Settings>(r#"{"online_polish":{"style":"unbounded"}}"#).is_err()
        );
        assert!(!wire.contains("key"));
    }

    #[test]
    fn writing_shortcuts_migrate_to_custom_chords_and_round_trip() {
        let old: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(old.writing_shortcuts, WritingShortcuts::default());
        assert_eq!(old.writing_shortcuts.draft.label(), "Ctrl+Alt");
        assert_eq!(old.writing_shortcuts.polish.label(), "Ctrl+Space");
        let legacy: Settings = serde_json::from_str(
            r#"{"writing_shortcuts":{"draft":{"key":49},"polish":{"key":50}}}"#,
        )
        .unwrap();
        assert_eq!(legacy.writing_shortcuts, WritingShortcuts::default());
        let explicit_old: Settings = serde_json::from_str(r#"{"writing_shortcuts":{"draft":{"key":49,"modifiers":5},"polish":{"key":50,"modifiers":5}}}"#).unwrap();
        assert_eq!(
            explicit_old.writing_shortcuts.draft,
            WritingShortcut::new(0x31, 5)
        );
        assert_eq!(
            explicit_old.writing_shortcuts.polish,
            WritingShortcut::new(0x32, 5)
        );
        let prior_custom: Settings = serde_json::from_str(
            r#"{"writing_shortcuts":{"draft":{"key":65},"polish":{"key":114}}}"#,
        )
        .unwrap();
        assert_eq!(prior_custom.writing_shortcuts.draft.label(), "Ctrl+Shift+A");
        let custom = Settings {
            writing_shortcuts: WritingShortcuts {
                draft: WritingShortcut::new(0x41, 3),
                polish: WritingShortcut::new(0x72, 9),
            },
            ..Settings::default()
        };
        custom.validate().unwrap();
        assert_eq!(
            serde_json::from_str::<Settings>(&serde_json::to_string(&custom).unwrap()).unwrap(),
            custom
        );
        assert_eq!(custom.writing_shortcuts.polish.label(), "Ctrl+Win+F3");
    }

    #[test]
    fn custom_shortcuts_accept_named_keys_and_reject_reserved_or_bare_keys() {
        for shortcut in [
            WritingShortcut::new(0, 3),
            WritingShortcut::new(0x20, 1),
            WritingShortcut::new(0xbe, 3),
            WritingShortcut::new(0x25, 5),
            WritingShortcut::new(0x87, 8),
            WritingShortcut::new(0, 6),
        ] {
            assert!(shortcut.is_supported(), "{shortcut:?}");
        }
        for shortcut in [
            WritingShortcut::new(0, 4),
            WritingShortcut::new(0x41, 0),
            WritingShortcut::new(0x41, 16),
            WritingShortcut::new(0x10, 3),
            WritingShortcut::new(0x46, 5),
            WritingShortcut::new(0x2e, 3),
            WritingShortcut::new(0x09, 2),
            WritingShortcut::new(0x4c, 8),
        ] {
            assert!(!shortcut.is_supported(), "{shortcut:?}");
        }
        let chord = WritingShortcut::new(0xbe, 3);
        assert!(chord.matches(0xbe, crate::engine::Modifiers::CONTROL.with_alt()));
        assert!(!chord.matches(
            0xbe,
            crate::engine::Modifiers::CONTROL.with_alt().with_shift()
        ));
        assert!(!WritingShortcuts::default()
            .draft
            .matches(0x12, crate::engine::Modifiers::CONTROL.with_alt()));
        assert_eq!(chord.label(), "Ctrl+Alt+.");
    }

    #[test]
    fn invalid_or_colliding_shortcuts_cannot_replace_existing_settings() {
        let store = temporary_store();
        store.save(&Settings::default()).unwrap();
        let before = fs::read(store.path()).unwrap();
        for pair in [
            WritingShortcuts {
                draft: WritingShortcut::new(0x31, 5),
                polish: WritingShortcut::new(0x31, 5),
            },
            WritingShortcuts {
                draft: WritingShortcut::new(0x46, 5),
                ..WritingShortcuts::default()
            },
            WritingShortcuts {
                polish: WritingShortcut::new(0x1b, 5),
                ..WritingShortcuts::default()
            },
        ] {
            let settings = Settings {
                writing_shortcuts: pair,
                ..Settings::default()
            };
            assert!(matches!(
                store.save(&settings),
                Err(SettingsError::InvalidWritingShortcuts)
            ));
            assert_eq!(fs::read(store.path()).unwrap(), before);
        }
        let _ = fs::remove_dir_all(store.path().parent().unwrap());
    }

    #[test]
    fn missing_file_loads_defaults() {
        let store = temporary_store();
        assert_eq!(store.load().expect("defaults"), Settings::default());
        assert!(!store.path().exists());
    }

    #[test]
    fn saving_replaces_an_existing_file_without_leaving_a_temporary_file() {
        let store = temporary_store();
        let settings = Settings {
            candidate_page_size: 7,
            candidate_layout_version: 1,
            online_ai_enabled: true,
            chinese_script: crate::ChineseScript::Traditional,
            theme: Theme::Dark,
            ai_correction_enabled: false,
            ai_undeclared_scope_compatibility: false,
            online_polish: OnlinePolishSettings {
                enabled: true,
                style: crate::writing::PolishStyle::Formal,
                script: Some(crate::ChineseScript::Traditional),
            },
            writing_shortcuts: WritingShortcuts::default(),
            writing_send_consent: None,
            model_api: crate::writing::config::ModelApiSettings::default(),
        };
        store.save(&settings).expect("save settings");
        assert_eq!(store.load().expect("load settings"), settings);

        let replacement = Settings {
            candidate_page_size: 19,
            candidate_layout_version: 1,
            online_ai_enabled: false,
            chinese_script: crate::ChineseScript::Simplified,
            theme: Theme::Light,
            ai_correction_enabled: true,
            ai_undeclared_scope_compatibility: true,
            online_polish: OnlinePolishSettings::default(),
            writing_shortcuts: WritingShortcuts::default(),
            writing_send_consent: None,
            model_api: crate::writing::config::ModelApiSettings::default(),
        };
        store.save(&replacement).expect("replace settings");
        assert_eq!(store.load().expect("load replacement"), replacement);

        let entries = fs::read_dir(store.path().parent().expect("parent"))
            .expect("read settings directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("settings directory entries");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].file_name().to_string_lossy(), "settings.json");
        let _ = fs::remove_dir_all(store.path().parent().expect("parent"));
    }

    #[test]
    fn invalid_page_size_is_rejected_without_overwriting_an_existing_file() {
        let store = temporary_store();
        let original = Settings {
            candidate_page_size: 9,
            candidate_layout_version: 1,
            online_ai_enabled: false,
            chinese_script: crate::ChineseScript::Simplified,
            theme: Theme::System,
            ai_correction_enabled: false,
            ai_undeclared_scope_compatibility: false,
            online_polish: OnlinePolishSettings::default(),
            writing_shortcuts: WritingShortcuts::default(),
            writing_send_consent: None,
            model_api: crate::writing::config::ModelApiSettings::default(),
        };
        store.save(&original).expect("save original settings");
        let original_json = fs::read(store.path()).expect("read original JSON");

        let settings = Settings {
            candidate_page_size: 0,
            candidate_layout_version: 1,
            online_ai_enabled: false,
            chinese_script: crate::ChineseScript::Simplified,
            theme: Theme::Dark,
            ai_correction_enabled: false,
            ai_undeclared_scope_compatibility: false,
            online_polish: OnlinePolishSettings::default(),
            writing_shortcuts: WritingShortcuts::default(),
            writing_send_consent: None,
            model_api: crate::writing::config::ModelApiSettings::default(),
        };
        assert!(matches!(
            settings.validate(),
            Err(SettingsError::InvalidCandidatePageSize { .. })
        ));
        assert!(matches!(
            store.save(&settings),
            Err(SettingsError::InvalidCandidatePageSize { .. })
        ));
        assert_eq!(
            fs::read(store.path()).expect("read unchanged original"),
            original_json
        );
        assert_eq!(store.load().expect("load original"), original);
        let _ = fs::remove_dir_all(store.path().parent().expect("parent"));
    }

    #[test]
    fn invalid_json_is_reported_without_modifying_the_file() {
        let store = temporary_store();
        let parent = store.path().parent().expect("parent");
        fs::create_dir_all(parent).expect("create settings directory");
        let original = br#"{"candidate_page_size": "not a number"}"#;
        fs::write(store.path(), original).expect("write invalid JSON");

        assert!(matches!(store.load(), Err(SettingsError::Json(_))));
        assert_eq!(fs::read(store.path()).expect("read invalid JSON"), original);
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn save_creates_missing_directories() {
        let store = temporary_store();
        let settings = Settings {
            candidate_page_size: 21,
            candidate_layout_version: 1,
            online_ai_enabled: false,
            chinese_script: crate::ChineseScript::Traditional,
            theme: Theme::Dark,
            ai_correction_enabled: false,
            ai_undeclared_scope_compatibility: false,
            online_polish: OnlinePolishSettings::default(),
            writing_shortcuts: WritingShortcuts::default(),
            writing_send_consent: None,
            model_api: crate::writing::config::ModelApiSettings::default(),
        };
        store.save(&settings).expect("create directory and save");
        assert_eq!(store.load().expect("load settings"), settings);
        let _ = fs::remove_dir_all(store.path().parent().expect("parent"));
    }

    #[test]
    fn an_invalid_parent_reports_an_io_error() {
        let store = temporary_store();
        let root = store.path().parent().expect("root");
        fs::create_dir_all(root).expect("create test root");
        let blocked_parent = root.join("blocked");
        fs::write(&blocked_parent, b"not a directory").expect("create blocking file");
        let blocked_store = SettingsStore::new(blocked_parent.join("settings.json"));

        assert!(matches!(
            blocked_store.save(&Settings::default()),
            Err(SettingsError::Io(_))
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn failed_replacement_preserves_target_and_cleans_the_temporary_file() {
        let store = temporary_store();
        let parent = store.path().parent().expect("parent");
        let target = store.path();
        fs::create_dir_all(target).expect("create target directory");
        let sentinel = target.join("sentinel.txt");
        fs::write(&sentinel, b"keep this content").expect("write sentinel");

        assert!(matches!(
            store.save(&Settings::default()),
            Err(SettingsError::Io(_))
        ));
        assert!(target.is_dir());
        assert_eq!(
            fs::read(&sentinel).expect("read preserved sentinel"),
            b"keep this content"
        );
        let temporary_files = fs::read_dir(parent)
            .expect("read settings parent")
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("settings.json.") && name.ends_with(".tmp")
            })
            .count();
        assert_eq!(temporary_files, 0);
        let _ = fs::remove_dir_all(parent);
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn current_user_store_is_explicitly_unsupported_off_windows() {
        assert!(matches!(
            SettingsStore::for_current_user(),
            Err(SettingsError::UnsupportedPlatform)
        ));
    }
}

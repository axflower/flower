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

pub const DEFAULT_CANDIDATE_PAGE_SIZE: usize = 9;
pub const MIN_CANDIDATE_PAGE_SIZE: usize = 1;
pub const MAX_CANDIDATE_PAGE_SIZE: usize = 99;

static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(1);

/// Persisted settings shared by the TSF service and the native settings window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub chinese_script: crate::ChineseScript,
    pub candidate_page_size: usize,
    pub theme: Theme,
    pub ai_correction_enabled: bool,
    pub ai_undeclared_scope_compatibility: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            chinese_script: crate::ChineseScript::default(),
            candidate_page_size: DEFAULT_CANDIDATE_PAGE_SIZE,
            theme: Theme::System,
            ai_correction_enabled: true,
            ai_undeclared_scope_compatibility: true,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), SettingsError> {
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

        let settings: Settings = serde_json::from_str(&json)?;
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
        assert_eq!(settings.candidate_page_size, 9);
        assert_eq!(settings.theme, Theme::System);
        assert!(settings.ai_correction_enabled);
        assert!(settings.ai_undeclared_scope_compatibility);
        assert!(settings.validate().is_ok());
    }

    #[test]
    fn legacy_settings_use_new_default_but_preserve_explicit_opt_out() {
        let old: Settings =
            serde_json::from_str(r#"{"candidate_page_size":7,"theme":"dark"}"#).unwrap();
        assert!(old.ai_correction_enabled);
        assert!(old.ai_undeclared_scope_compatibility);
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
    fn candidate_page_size_input_requires_a_complete_in_range_integer() {
        assert_eq!(parse_candidate_page_size_input("1"), Some(1));
        assert_eq!(parse_candidate_page_size_input("99"), Some(99));
        assert_eq!(parse_candidate_page_size_input("0"), None);
        assert_eq!(parse_candidate_page_size_input("100"), None);
        assert_eq!(parse_candidate_page_size_input(""), None);
        assert_eq!(parse_candidate_page_size_input("5abc"), None);
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
            chinese_script: crate::ChineseScript::Traditional,
            theme: Theme::Dark,
            ai_correction_enabled: false,
            ai_undeclared_scope_compatibility: false,
        };
        store.save(&settings).expect("save settings");
        assert_eq!(store.load().expect("load settings"), settings);

        let replacement = Settings {
            candidate_page_size: 19,
            chinese_script: crate::ChineseScript::Simplified,
            theme: Theme::Light,
            ai_correction_enabled: true,
            ai_undeclared_scope_compatibility: true,
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
            chinese_script: crate::ChineseScript::Simplified,
            theme: Theme::System,
            ai_correction_enabled: false,
            ai_undeclared_scope_compatibility: false,
        };
        store.save(&original).expect("save original settings");
        let original_json = fs::read(store.path()).expect("read original JSON");

        let settings = Settings {
            candidate_page_size: 0,
            chinese_script: crate::ChineseScript::Simplified,
            theme: Theme::Dark,
            ai_correction_enabled: false,
            ai_undeclared_scope_compatibility: false,
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
            chinese_script: crate::ChineseScript::Traditional,
            theme: Theme::Dark,
            ai_correction_enabled: false,
            ai_undeclared_scope_compatibility: false,
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

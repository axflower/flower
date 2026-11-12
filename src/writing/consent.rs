//! Provider/model-scoped, explicit opt-out from repeated send confirmation.
use crate::settings::SettingsStore;

pub const SCOPE: &str = "bigmodel/glm-4.7-flash/v1";

pub fn confirmation_required(store: Option<&SettingsStore>) -> bool {
    match super::config::load_active(store) {
        Ok(profile) => confirmation_required_for(store, &profile),
        Err(_) => true,
    }
}

pub fn confirmation_required_for(
    store: Option<&SettingsStore>,
    profile: &super::config::ApiProfile,
) -> bool {
    if !super::config::is_current(store, profile) {
        return true;
    }
    let scope = match profile.scope() {
        Ok(scope) => scope,
        Err(_) => return true,
    };
    store.and_then(|s| s.load().ok()).map_or(true, |s| {
        s.writing_send_consent.as_deref() != Some(scope.as_str())
    })
}

/// Merge only the preference into the latest configuration.
pub fn remember(store: &SettingsStore, skip: bool) -> Result<(), String> {
    if skip {
        let profile = super::config::load_active(Some(store)).map_err(|e| e.to_string())?;
        return remember_for(store, &profile, true);
    }
    let mut latest = store.load().map_err(|_| "读取设置失败，未记住发送偏好。")?;
    latest.writing_send_consent = None;
    store
        .save(&latest)
        .map_err(|_| "保存设置失败，未记住发送偏好。".into())
}

pub fn remember_for(
    store: &SettingsStore,
    profile: &super::config::ApiProfile,
    skip: bool,
) -> Result<(), String> {
    let scope = profile.scope().map_err(|e| e.to_string())?;
    let mut latest = store.load().map_err(|_| "读取设置失败，未记住发送偏好。")?;
    let current = latest
        .model_api
        .active()
        .and_then(|p| p.scope())
        .map_err(|e| e.to_string())?;
    if current != scope {
        return Err("模型连接已变化，请重新生成并确认。".into());
    }
    latest.writing_send_consent = skip.then_some(scope);
    store
        .save(&latest)
        .map_err(|_| "保存设置失败，未记住发送偏好。".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{Settings, WritingShortcut};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn store() -> SettingsStore {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::var_os("FLOWER_CONSENT_TEST_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("flower-send-consent-tests"))
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        SettingsStore::new(root.join("settings.json"))
    }

    #[test]
    fn default_scope_restore_and_latest_fields_are_independent() {
        let store = store();
        assert!(confirmation_required(Some(&store)));
        let mut settings = Settings::default();
        settings.writing_shortcuts.draft = WritingShortcut::new(0x41, 5);
        settings.online_polish.enabled = true;
        store.save(&settings).unwrap();
        remember(&store, true).unwrap();
        assert!(!confirmation_required(Some(&SettingsStore::new(
            store.path()
        ))));
        let loaded = store.load().unwrap();
        assert_eq!(loaded.writing_shortcuts, settings.writing_shortcuts);
        assert_eq!(loaded.online_polish, settings.online_polish);
        let changed_scope = Settings {
            writing_send_consent: Some("different-provider/model/v1".into()),
            ..loaded
        };
        store.save(&changed_scope).unwrap();
        assert!(confirmation_required(Some(&store)));
        remember(&store, true).unwrap();
        remember(&store, false).unwrap();
        assert!(confirmation_required(Some(&store)));
        let _ = std::fs::remove_dir_all(store.path().parent().unwrap());
    }

    #[test]
    fn unavailable_or_corrupt_store_fails_closed_and_is_not_overwritten() {
        assert!(confirmation_required(None));
        let store = store();
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        let bytes = b"not valid JSON";
        std::fs::write(store.path(), bytes).unwrap();
        assert!(confirmation_required(Some(&store)));
        assert!(remember(&store, true).is_err());
        assert_eq!(std::fs::read(store.path()).unwrap(), bytes);
        let _ = std::fs::remove_dir_all(store.path().parent().unwrap());
    }
}

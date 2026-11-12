//! Non-secret model connections. A validated snapshot fixes each request's destination.
use super::WritingError;
use crate::settings::SettingsStore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ApiProtocol {
    #[default]
    ChatCompletions,
    Responses,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiProfile {
    pub id: String,
    pub name: String,
    pub protocol: ApiProtocol,
    pub base_url: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiEndpoint {
    pub host: String,
    pub port: u16,
    pub path: String,
}

impl Default for ApiProfile {
    fn default() -> Self {
        Self {
            id: "legacy-bigmodel".into(),
            name: "智谱（原配置）".into(),
            protocol: ApiProtocol::ChatCompletions,
            base_url: "https://open.bigmodel.cn/api/paas/v4".into(),
            model: super::MODEL.into(),
        }
    }
}

fn digest(value: &impl Serialize) -> String {
    let bytes = serde_json::to_vec(value).expect("serializable connection identity");
    format!("{:x}", Sha256::digest(bytes))
}

impl ApiProfile {
    pub fn normalized(&self) -> Result<Self, WritingError> {
        if !valid_model_id(&self.model) {
            return Err(WritingError::InvalidConfiguration);
        }
        self.connection_normalized()
    }

    /// Validate a discovery target before a model has been selected.
    pub fn connection_normalized(&self) -> Result<Self, WritingError> {
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            || self.name.trim().is_empty()
            || self.name.chars().count() > 80
            || self.name.chars().any(char::is_control)
        {
            return Err(WritingError::InvalidConfiguration);
        }
        let endpoint = self.endpoint()?;
        let prefix = self
            .base_url
            .strip_prefix("https://")
            .ok_or(WritingError::InvalidConfiguration)?;
        let authority = prefix
            .split('/')
            .next()
            .ok_or(WritingError::InvalidConfiguration)?;
        let path = prefix[authority.len()..].trim_end_matches('/');
        let authority = if endpoint.port == 443 {
            endpoint.host
        } else {
            format!("{}:{}", endpoint.host, endpoint.port)
        };
        Ok(Self {
            name: self.name.trim().to_owned(),
            base_url: format!("https://{authority}{path}"),
            ..self.clone()
        })
    }

    pub fn endpoint(&self) -> Result<ApiEndpoint, WritingError> {
        self.endpoint_for(match self.protocol {
            ApiProtocol::ChatCompletions => "chat/completions",
            ApiProtocol::Responses => "responses",
        })
    }

    pub fn catalog_endpoint(&self) -> Result<ApiEndpoint, WritingError> {
        self.endpoint_for("models")
    }

    fn endpoint_for(&self, suffix: &str) -> Result<ApiEndpoint, WritingError> {
        let invalid = || WritingError::InvalidConfiguration;
        let url = &self.base_url;
        if url.len() > 1024
            || !url.bytes().all(|b| (0x21..=0x7e).contains(&b))
            || url.bytes().any(|b| b"@?#\\%".contains(&b))
        {
            return Err(invalid());
        }
        let rest = url.strip_prefix("https://").ok_or_else(invalid)?;
        let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
        let (host, port) = match authority.split_once(':') {
            Some((host, port)) => {
                if port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(invalid());
                }
                (host, port.parse::<u16>().map_err(|_| invalid())?)
            }
            None => (authority, 443),
        };
        if host.is_empty()
            || host.len() > 253
            || port == 0
            || host.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
            || path.split('/').any(|p| p == "." || p == "..")
            || !path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"/-._~".contains(&b))
        {
            return Err(invalid());
        }
        let path = path.trim_end_matches('/');
        if path == "chat/completions"
            || path.ends_with("/chat/completions")
            || path == "responses"
            || path.ends_with("/responses")
            || path == "models"
            || path.ends_with("/models")
        {
            return Err(invalid());
        }
        Ok(ApiEndpoint {
            host: host.to_ascii_lowercase(),
            port,
            path: if path.is_empty() {
                format!("/{suffix}")
            } else {
                format!("/{path}/{suffix}")
            },
        })
    }

    pub fn is_legacy(&self) -> bool {
        self.normalized().is_ok_and(|p| {
            p.id == "legacy-bigmodel"
                && p.protocol == ApiProtocol::ChatCompletions
                && p.base_url == "https://open.bigmodel.cn/api/paas/v4"
                && p.model == super::MODEL
        })
    }

    pub fn is_bigmodel(&self) -> bool {
        self.endpoint().is_ok_and(|e| {
            e.host == super::API_HOST
                && e.port == 443
                && self.protocol == ApiProtocol::ChatCompletions
                && e.path == super::API_PATH
        })
    }

    pub fn is_bigmodel_api(&self) -> bool {
        self.endpoint().is_ok_and(|e| {
            e.host == super::API_HOST
                && e.port == 443
                && matches!(
                    e.path.as_str(),
                    "/api/paas/v4/chat/completions" | "/api/v1/chat/completions"
                )
        })
    }

    pub fn is_coding_plan(&self) -> bool {
        self.endpoint().is_ok_and(|e| {
            e.host == super::API_HOST && e.port == 443 && e.path.starts_with("/api/v1/")
        })
    }

    pub fn is_deepseek(&self) -> bool {
        self.endpoint().is_ok_and(|e| {
            e.host == "api.deepseek.com"
                && e.port == 443
                && matches!(
                    e.path.as_str(),
                    "/chat/completions" | "/v1/chat/completions" | "/responses" | "/v1/responses"
                )
        })
    }

    pub fn scope(&self) -> Result<String, WritingError> {
        let p = self.normalized()?;
        if p.is_legacy() {
            return Ok(super::consent::SCOPE.into());
        }
        Ok(format!(
            "model-api/v1/{}",
            digest(&(&p.id, p.protocol, &p.base_url, &p.model))
        ))
    }

    pub fn credential_target(&self) -> Result<String, WritingError> {
        let p = self.normalized()?;
        if p.id == "legacy-bigmodel"
            && p.protocol == ApiProtocol::ChatCompletions
            && p.base_url == "https://open.bigmodel.cn/api/paas/v4"
        {
            return Ok("Flower/OnlineWriting/BigModel".into());
        }
        Ok(format!(
            "Flower/OnlineWriting/Api/{}",
            digest(&(&p.id, p.protocol, &p.base_url))
        ))
    }

    pub fn display_target(&self) -> Result<String, WritingError> {
        let p = self.normalized()?;
        let protocol = match p.protocol {
            ApiProtocol::ChatCompletions => "Chat Completions",
            ApiProtocol::Responses => "Responses",
        };
        Ok(format!(
            "{} · {} · {protocol}\n{}",
            p.name, p.model, p.base_url
        ))
    }
}

pub fn valid_model_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 160 && value.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelApiSettings {
    pub profiles: Vec<ApiProfile>,
    pub active_id: Option<String>,
}

impl Default for ModelApiSettings {
    fn default() -> Self {
        Self {
            profiles: vec![ApiProfile::default()],
            active_id: Some("legacy-bigmodel".into()),
        }
    }
}

impl ModelApiSettings {
    pub fn validate(&self) -> Result<(), WritingError> {
        if self.profiles.len() > 16 {
            return Err(WritingError::InvalidConfiguration);
        }
        let mut ids = std::collections::HashSet::new();
        for p in &self.profiles {
            p.normalized()?;
            if !ids.insert(&p.id) {
                return Err(WritingError::InvalidConfiguration);
            }
        }
        if self.active_id.as_ref().is_some_and(|id| !ids.contains(id)) {
            return Err(WritingError::InvalidConfiguration);
        }
        Ok(())
    }

    pub fn active(&self) -> Result<ApiProfile, WritingError> {
        self.validate()?;
        self.profiles
            .iter()
            .find(|p| Some(&p.id) == self.active_id.as_ref())
            .ok_or(WritingError::InvalidConfiguration)?
            .normalized()
    }
}

pub fn load_active(store: Option<&SettingsStore>) -> Result<ApiProfile, WritingError> {
    store
        .ok_or(WritingError::InvalidConfiguration)?
        .load()
        .map_err(|_| WritingError::InvalidConfiguration)?
        .model_api
        .active()
}

pub fn is_current(store: Option<&SettingsStore>, profile: &ApiProfile) -> bool {
    load_active(store)
        .and_then(|p| p.scope())
        .ok()
        .zip(profile.scope().ok())
        .is_some_and(|(a, b)| a == b)
}

/// Explicit user opt-in, independent from saving a model or a polish preference.
pub fn online_enabled(store: Option<&SettingsStore>) -> bool {
    store.is_some_and(|store| store.load().is_ok_and(|s| s.online_ai_enabled))
}

pub fn polish_authorized(store: &SettingsStore, profile: &ApiProfile) -> bool {
    store.load().is_ok_and(|s| {
        s.online_ai_enabled
            && s.online_polish.enabled
            && s.model_api
                .active()
                .and_then(|p| p.scope())
                .ok()
                .zip(profile.scope().ok())
                .is_some_and(|(a, b)| a == b)
    })
}

/// Only connections are merged. Confirmation is recipient-scoped; explicit polish disable is retained.
pub fn persist(store: &SettingsStore, value: &ModelApiSettings) -> Result<(), String> {
    value.validate().map_err(|e| e.to_string())?;
    let mut latest = store.load().map_err(|_| "读取模型设置失败，未保存。")?;
    let before = latest.model_api.active().and_then(|p| p.scope()).ok();
    let after = value.active().and_then(|p| p.scope()).ok();
    latest.model_api = value.clone();
    if before != after {
        latest.writing_send_consent = None;
    }
    store.save(&latest).map_err(|_| "保存模型设置失败。".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Settings;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn store() -> SettingsStore {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::var_os("FLOWER_MODEL_API_TEST_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("flower-model-api-tests"));
        SettingsStore::new(
            root.join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ))
            .join("settings.json"),
        )
    }

    fn custom() -> ApiProfile {
        ApiProfile {
            id: "custom-1".into(),
            name: "自定义连接".into(),
            protocol: ApiProtocol::ChatCompletions,
            base_url: "https://API.example.com:443/v1/".into(),
            model: "vendor/model-1".into(),
        }
    }

    #[test]
    fn model_api_legacy_json_is_read_without_rewriting() {
        let store = store();
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        let bytes = br#"{"online_polish":{"enabled":true},"writing_send_consent":"bigmodel/glm-4.7-flash/v1"}"#;
        std::fs::write(store.path(), bytes).unwrap();
        let settings = store.load().unwrap();
        let profile = settings.model_api.active().unwrap();
        assert!(profile.is_legacy());
        assert_eq!(
            profile.credential_target().unwrap(),
            "Flower/OnlineWriting/BigModel"
        );
        assert!(!super::super::consent::confirmation_required_for(
            Some(&store),
            &profile
        ));
        assert!(!online_enabled(Some(&store)));
        assert!(!polish_authorized(&store, &profile));
        assert_eq!(std::fs::read(store.path()).unwrap(), bytes);
        std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
    }

    #[test]
    fn model_api_base_urls_are_strict_and_do_not_invent_v1() {
        let mut p = custom();
        assert_eq!(
            p.normalized().unwrap().base_url,
            "https://api.example.com/v1"
        );
        assert_eq!(
            p.endpoint().unwrap(),
            ApiEndpoint {
                host: "api.example.com".into(),
                port: 443,
                path: "/v1/chat/completions".into()
            }
        );
        p.protocol = ApiProtocol::Responses;
        p.base_url = "https://api.example.com:8443".into();
        assert_eq!(p.endpoint().unwrap().path, "/responses");
        assert_eq!(p.endpoint().unwrap().port, 8443);
        for value in [
            "http://example.com",
            "https://user:key@example.com/v1",
            "https://example.com/v1?key=secret",
            "https://example.com/#x",
            "https://example.com/%2e%2e/x",
            "https://example.com/../x",
            "https://example.com/./x",
            "https://example.com\\evil/v1",
            "https://example.com\n/v1",
            "https://[::1]/v1",
            "https://example.com:0/v1",
            "https://example.com:65536/v1",
            "https://example.com:/v1",
            "https://-example.com/v1",
            "https://example..com/v1",
            "https://example.com/v1/responses/",
            "https://example.com/v1/chat/completions",
            "https://example.com/v1/models",
            "https:///v1",
        ] {
            p.base_url = value.into();
            assert!(p.normalized().is_err(), "{value}");
        }
    }

    #[test]
    fn model_api_identity_binds_destination_and_not_display_name() {
        let p = custom();
        let mut changed = p.clone();
        changed.name = "重命名".into();
        assert_eq!(p.scope(), changed.scope());
        changed.base_url = "https://api.example.com/v1".into();
        assert_eq!(p.scope(), changed.scope());
        changed.model = "other".into();
        assert_ne!(p.scope(), changed.scope());
        assert_eq!(p.credential_target(), changed.credential_target());
        changed.base_url = "https://other.example.com/v1".into();
        assert_ne!(p.credential_target(), changed.credential_target());
        changed = p.clone();
        changed.protocol = ApiProtocol::Responses;
        assert_ne!(p.credential_target(), changed.credential_target());
        changed = ApiProfile {
            id: "new-bigmodel".into(),
            ..ApiProfile::default()
        };
        assert!(!changed.is_legacy());
        assert_ne!(
            changed.credential_target(),
            ApiProfile::default().credential_target()
        );
        changed = ApiProfile::default();
        changed.base_url = "https://other.example.com/api/paas/v4".into();
        assert!(!changed.is_bigmodel());
        assert_ne!(
            changed.credential_target(),
            ApiProfile::default().credential_target()
        );
    }

    #[test]
    fn model_catalog_connection_does_not_invent_a_model_or_endpoint_prefix() {
        let mut p = custom();
        p.model.clear();
        assert!(p.normalized().is_err());
        assert_eq!(p.connection_normalized().unwrap().model, "");
        assert_eq!(p.catalog_endpoint().unwrap().path, "/v1/models");
        p.base_url = "https://api.deepseek.com".into();
        assert_eq!(p.catalog_endpoint().unwrap().path, "/models");
        assert!(p.is_deepseek());
        p.protocol = ApiProtocol::Responses;
        assert!(p.is_deepseek());
        p.base_url = "https://other.example.com".into();
        assert!(!p.is_deepseek());
        p.base_url = "https://api.deepseek.com:444".into();
        assert!(!p.is_deepseek());
        p.base_url = "https://user:secret@api.deepseek.com".into();
        assert!(p.connection_normalized().is_err());
    }

    #[test]
    fn model_api_legacy_key_is_same_endpoint_scoped_not_model_scoped() {
        let original = ApiProfile::default();
        let mut p = original.clone();
        p.model = "another-model".into();
        assert_ne!(p.scope(), original.scope());
        assert_eq!(p.credential_target(), original.credential_target());
        p.base_url = "https://open.bigmodel.cn/api/v1".into();
        assert!(p.is_coding_plan());
        assert!(p.is_bigmodel_api());
        assert!(!p.is_bigmodel());
        assert_ne!(p.credential_target(), original.credential_target());
        p.protocol = ApiProtocol::Responses;
        assert!(!p.is_bigmodel_api());
        assert!(p.is_coding_plan());
    }

    #[test]
    fn model_api_catalog_bounds_and_no_active_fail_closed() {
        let p = custom();
        let mut catalog = ModelApiSettings {
            profiles: vec![p.clone()],
            active_id: Some(p.id.clone()),
        };
        assert!(catalog.active().is_ok());
        catalog.profiles.push(p.clone());
        assert!(catalog.validate().is_err());
        catalog.profiles = vec![p.clone()];
        catalog.active_id = Some("missing".into());
        assert!(catalog.validate().is_err());
        catalog.active_id = None;
        assert!(catalog.validate().is_ok());
        assert!(catalog.active().is_err());
        catalog.profiles.clear();
        assert!(catalog.validate().is_ok());
        assert!(catalog.active().is_err());
        for i in 0..17 {
            catalog.profiles.push(ApiProfile {
                id: format!("id-{i}"),
                ..p.clone()
            });
        }
        assert!(catalog.validate().is_err());
        assert!(load_active(None).is_err());
    }

    #[test]
    fn model_api_persist_merges_latest_and_revokes_old_recipient() {
        let store = store();
        let mut settings = Settings {
            online_ai_enabled: true,
            ..Settings::default()
        };
        settings.online_polish.enabled = true;
        settings.writing_shortcuts.draft.key = 0x41;
        store.save(&settings).unwrap();
        let original = settings.model_api.active().unwrap();
        super::super::consent::remember_for(&store, &original, true).unwrap();
        let p = custom();
        let catalog = ModelApiSettings {
            profiles: vec![p.clone()],
            active_id: Some(p.id.clone()),
        };
        persist(&store, &catalog).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.writing_shortcuts.draft.key, 0x41);
        assert_eq!(loaded.model_api, catalog);
        assert_eq!(loaded.writing_send_consent, None);
        assert!(loaded.online_polish.enabled);
        assert!(!is_current(Some(&store), &original));
        assert!(!polish_authorized(&store, &original));
        let before = std::fs::read(store.path()).unwrap();
        assert!(super::super::consent::remember_for(&store, &original, true).is_err());
        assert_eq!(std::fs::read(store.path()).unwrap(), before);
        super::super::consent::remember_for(&store, &p, true).unwrap();
        assert!(!super::super::consent::confirmation_required_for(
            Some(&store),
            &p
        ));
        let mut renamed = catalog;
        renamed.profiles[0].name = "只改名字".into();
        persist(&store, &renamed).unwrap();
        assert!(!super::super::consent::confirmation_required_for(
            Some(&store),
            &p
        ));
        let mut disabled = store.load().unwrap();
        disabled.online_polish.enabled = false;
        store.save(&disabled).unwrap();
        renamed.profiles[0].model = "another-model".into();
        persist(&store, &renamed).unwrap();
        assert!(!store.load().unwrap().online_polish.enabled);
        std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
    }

    #[test]
    fn online_opt_in_is_explicit_and_polish_requires_both_switches() {
        let store = store();
        let mut settings = Settings::default();
        store.save(&settings).unwrap();
        let profile = settings.model_api.active().unwrap();
        assert!(!online_enabled(Some(&store)));
        assert!(!online_enabled(None));
        assert!(!polish_authorized(&store, &profile));
        settings.online_ai_enabled = true;
        store.save(&settings).unwrap();
        assert!(online_enabled(Some(&store)));
        assert!(polish_authorized(&store, &profile));
        settings.online_polish.enabled = false;
        store.save(&settings).unwrap();
        assert!(online_enabled(Some(&store)));
        assert!(!polish_authorized(&store, &profile));
        settings.online_ai_enabled = false;
        store.save(&settings).unwrap();
        persist(&store, &settings.model_api).unwrap();
        assert!(!online_enabled(Some(&store)));
        std::fs::write(store.path(), b"broken").unwrap();
        assert!(!online_enabled(Some(&store)));
        assert!(!polish_authorized(&store, &profile));
        std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
    }

    #[test]
    fn model_api_invalid_or_corrupt_settings_are_not_replaced() {
        let store = store();
        std::fs::create_dir_all(store.path().parent().unwrap()).unwrap();
        std::fs::write(store.path(), b"broken").unwrap();
        assert!(persist(&store, &ModelApiSettings::default()).is_err());
        assert_eq!(std::fs::read(store.path()).unwrap(), b"broken");
        assert!(load_active(Some(&store)).is_err());
        assert!(!polish_authorized(&store, &ApiProfile::default()));
        std::fs::remove_dir_all(store.path().parent().unwrap()).unwrap();
    }
}

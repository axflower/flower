//! Current-user Credential Manager storage; never serialize or log a key.
use super::WritingError;
use std::fmt;

pub struct SecretKey(String);

impl SecretKey {
    pub fn new(input: &str) -> Result<Self, WritingError> {
        // Control characters are forbidden even at the edges.
        if input.chars().any(char::is_control) {
            return Err(WritingError::Credential);
        }
        let value = input.trim();
        if value.is_empty()
            || value.len() > 2048
            || !value.bytes().all(|b| (0x21..=0x7e).contains(&b))
        {
            return Err(WritingError::Credential);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::ERROR_NOT_FOUND;
    use windows::Win32::Security::Credentials::*;

    struct Credential(*mut CREDENTIALW);
    impl Drop for Credential {
        fn drop(&mut self) {
            unsafe { CredFree(self.0.cast()) }
        }
    }

    pub fn load_key(target: &str) -> Result<Option<SecretKey>, WritingError> {
        let target: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
        let mut ptr = std::ptr::null_mut();
        unsafe {
            if let Err(error) = CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, 0, &mut ptr) {
                return if error.code() == ERROR_NOT_FOUND.to_hresult() {
                    Ok(None)
                } else {
                    Err(WritingError::Credential)
                };
            }
            if ptr.is_null() {
                return Err(WritingError::Credential);
            }
            let owned = Credential(ptr);
            let credential = &*owned.0;
            let size = credential.CredentialBlobSize as usize;
            if !(1..=2048).contains(&size) || credential.CredentialBlob.is_null() {
                return Err(WritingError::Credential);
            }
            let bytes = std::slice::from_raw_parts(credential.CredentialBlob, size);
            let text = std::str::from_utf8(bytes).map_err(|_| WritingError::Credential)?;
            SecretKey::new(text).map(Some)
        }
    }

    pub fn save_key(target: &str, key: &SecretKey) -> Result<(), WritingError> {
        let mut target: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
        let credential = CREDENTIALW {
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            CredentialBlobSize: key.expose().len() as u32,
            CredentialBlob: key.expose().as_ptr() as *mut u8,
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            ..Default::default()
        };
        unsafe { CredWriteW(&credential, 0) }.map_err(|_| WritingError::Credential)
    }

    pub fn delete_key(target: &str) -> Result<(), WritingError> {
        let target: Vec<u16> = target.encode_utf16().chain(Some(0)).collect();
        match unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, 0) } {
            Ok(()) => Ok(()),
            Err(error) if error.code() == ERROR_NOT_FOUND.to_hresult() => Ok(()),
            Err(_) => Err(WritingError::Credential),
        }
    }
}

pub fn load_key() -> Result<Option<SecretKey>, WritingError> {
    load_key_for(&super::config::ApiProfile::default())
}
pub fn save_key(key: &SecretKey) -> Result<(), WritingError> {
    save_key_for(&super::config::ApiProfile::default(), key)
}
pub fn delete_key() -> Result<(), WritingError> {
    delete_key_for(&super::config::ApiProfile::default())
}

pub fn load_key_for(
    profile: &super::config::ApiProfile,
) -> Result<Option<SecretKey>, WritingError> {
    let target = profile.credential_target()?;
    #[cfg(windows)]
    {
        platform::load_key(&target)
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        Err(WritingError::Unsupported)
    }
}

pub fn save_key_for(
    profile: &super::config::ApiProfile,
    key: &SecretKey,
) -> Result<(), WritingError> {
    let target = profile.credential_target()?;
    #[cfg(windows)]
    {
        platform::save_key(&target, key)
    }
    #[cfg(not(windows))]
    {
        let _ = (target, key);
        Err(WritingError::Unsupported)
    }
}

pub fn delete_key_for(profile: &super::config::ApiProfile) -> Result<(), WritingError> {
    let target = profile.credential_target()?;
    #[cfg(windows)]
    {
        platform::delete_key(&target)
    }
    #[cfg(not(windows))]
    {
        let _ = target;
        Err(WritingError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_identity_is_profile_scoped_without_accessing_credentials() {
        let legacy = super::super::config::ApiProfile::default();
        assert_eq!(
            legacy.credential_target().unwrap(),
            "Flower/OnlineWriting/BigModel"
        );
        let mut custom = legacy.clone();
        custom.id = "custom".into();
        let target = custom.credential_target().unwrap();
        assert_ne!(target, legacy.credential_target().unwrap());
        custom.model = "another-model".into();
        assert_eq!(custom.credential_target().unwrap(), target);
        custom.base_url = "https://example.com/v1".into();
        assert_ne!(custom.credential_target().unwrap(), target);
        let target = custom.credential_target().unwrap();
        custom.protocol = super::super::config::ApiProtocol::Responses;
        assert_ne!(custom.credential_target().unwrap(), target);
    }

    #[test]
    fn validates_memory_only() {
        for invalid in [
            "", "  ", "a b", "a\tb", "a\n", "\ra", "中文", "a\0", "a\u{7f}", "\tkey",
        ] {
            assert!(SecretKey::new(invalid).is_err());
        }
        assert!(SecretKey::new(&"a".repeat(2048)).is_ok());
        assert!(SecretKey::new(&"a".repeat(2049)).is_err());
        let key = SecretKey::new("  fake-key  ").unwrap();
        assert_eq!(key.expose(), "fake-key");
        assert_eq!(format!("{key:?}"), "[REDACTED]");
    }
}

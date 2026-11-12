//! Offline-readable setup instructions shared by writing and settings.

pub const SETUP_GUIDE: &str = include_str!("../../docs/model-api-setup.txt");

#[cfg(test)]
mod tests {
    #[test]
    fn guide_covers_standard_key_setup_and_explicit_use() {
        let text = super::SETUP_GUIDE;
        for required in [
            "模型标准 API",
            "Coding Plan",
            "保存密钥",
            "Ctrl+Alt",
            "Ctrl+Space",
            "启用在线AI功能",
            "点击“使用”",
            "1302",
            "Chat Completions",
            "Responses",
            "API 基址",
        ] {
            assert!(text.contains(required), "{required}");
        }
        assert!(!text.contains("Bearer YOUR_API_KEY"));
    }
}

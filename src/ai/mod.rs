//! Small, runtime-independent contracts for optional offline correction.
//! No inference runtime, host document access or network belongs here.

use serde::{Deserialize, Serialize};

pub mod ipc;

pub const AI_COMMENT: &str = "[Flower AI]";
pub const MIN_TEXT_CHARS: usize = 4;
pub const MIN_SPELLING_TEXT_CHARS: usize = 2;
pub const MAX_TEXT_CHARS: usize = 64;
pub const MAX_FRAME_BYTES: usize = 4096;
pub const DEBOUNCE_MS: u64 = 250;
pub const RESULT_TIMEOUT_MS: u64 = 5000;
pub const SERVER_IDLE_SECONDS: u64 = 60;
pub const SPELLING_MODEL_CALLS: usize = 32;
pub const SPELLING_CANDIDATES: usize = 4;
pub const SPELLING_TIMEOUT_MS: u64 = 3000;

/// A fixed refusal reason; contains no document text, paths, or host metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeBlockReason {
    Selection,
    Property,
    Value,
    Undeclared,
    InvalidValue,
    Interface,
    Read,
    Empty,
    InvalidList,
    Sensitive,
    Unsupported,
}

impl ScopeBlockReason {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Selection => "AI 范围受限 [S01]：无法取得当前选区",
            Self::Property => "AI 范围受限 [S02]：应用范围属性不可用",
            Self::Value => "AI 范围受限 [S03]：无法读取范围声明",
            Self::Undeclared => "AI 范围受限 [S04]：应用未提供范围声明",
            Self::InvalidValue => "AI 范围受限 [S05]：范围声明格式无效",
            Self::Interface => "AI 范围受限 [S06]：应用范围接口不可用",
            Self::Read => "AI 范围受限 [S07]：应用范围查询失败",
            Self::Empty => "AI 范围受限 [S08]：应用返回空范围列表",
            Self::InvalidList => "AI 范围受限 [S09]：应用范围列表无效",
            Self::Sensitive => "AI 范围受限 [S10]：密码、PIN或私密输入框",
            Self::Unsupported => "AI 范围受限 [S11]：当前输入类型未支持",
        }
    }
}

/// Fixed, text-free live diagnostics. Never a candidate or permission to infer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiStatus {
    Disabled,
    Unavailable,
    ModulePathFailed,
    UnsupportedPath,
    UnsafeDirectory,
    MissingHelper,
    MissingAssets,
    ClientFailed,
    TimerFailed,
    Waiting,
    Ineligible,
    ScopeBlocked(ScopeBlockReason),
    Checking,
    NoSuggestion,
    Suggested,
    Failed,
    EditFailed,
}

impl AiStatus {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Disabled => "AI 已关闭：在 Flower 设置中开启",
            Self::Unavailable => "AI 组件不可用：检查安装目录与完整包",
            Self::ModulePathFailed => "AI 不可用：无法定位输入法模块",
            Self::UnsupportedPath => "AI 不可用：安装路径形式不支持",
            Self::UnsafeDirectory => "AI 不可用：安装目录安全检查失败",
            Self::MissingHelper => "AI 不可用：安装目录缺少辅助程序",
            Self::MissingAssets => "AI 不可用：安装目录缺少模型目录",
            Self::ClientFailed => "AI 不可用：后台客户端初始化失败",
            Self::TimerFailed => "AI 不可用：空闲定时器初始化失败",
            Self::Waiting => "AI 等待停顿（需首页首选）",
            Self::Ineligible => "AI 等待 2–64 字完整拼音候选",
            Self::ScopeBlocked(reason) => reason.label(),
            Self::Checking => "AI 检查中：首次加载最多等待 5 秒",
            Self::NoSuggestion => "AI 已检查：无纠正建议",
            Self::Suggested => "AI 已提供 ✓ 纠正建议，请手动选择",
            Self::Failed => "AI 超时或组件故障：可重新输入后重试",
            Self::EditFailed => "AI 无法取得只读编辑会话",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiInput {
    pub raw: String,
    pub source: String,
    pub reading: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: u64,
    pub text: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub raw: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reading: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub id: u64,
    pub corrected: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reading: String,
}

/// Validate transport metadata without granting permission to inspect the host.
pub fn eligible_request(request: &Request) -> bool {
    (eligible_text(&request.text) && request.raw.is_empty() && request.reading.is_empty())
        || (eligible_spelling_text(&request.text)
            && !request.raw.is_empty()
            && (4..=128).contains(&request.raw.len())
            && request
                .raw
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'\'')
            && valid_reading(&request.reading, request.text.chars().count()))
}

fn valid_reading(reading: &str, characters: usize) -> bool {
    !reading.is_empty()
        && reading.len() <= 512
        && reading.split('\'').count() == characters
        && reading
            .split('\'')
            .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase()))
}

pub fn valid_response(request: &Request, response: &Response) -> bool {
    eligible_request(request)
        && response.id == request.id
        && match response.corrected.as_deref() {
            None => response.reading.is_empty(),
            Some(corrected) if response.reading.is_empty() => {
                valid_correction(&request.text, corrected)
            }
            Some(corrected) => spelling_change_span(
                &request.raw,
                &request.text,
                &request.reading,
                corrected,
                &response.reading,
            )
            .is_some(),
        }
}

/// A spelling repair may change a small syllable span, never unrelated text.
pub fn spelling_change_span(
    raw: &str,
    source: &str,
    reading: &str,
    corrected: &str,
    corrected_reading: &str,
) -> Option<(std::ops::Range<usize>, std::ops::Range<usize>)> {
    if source == corrected
        || !eligible_spelling_text(source)
        || !eligible_spelling_text(corrected)
        || !valid_reading(reading, source.chars().count())
        || !valid_reading(corrected_reading, corrected.chars().count())
        || !crate::engine::single_spelling_edit(raw, corrected_reading)
    {
        return None;
    }
    let from: Vec<_> = reading.split('\'').collect();
    let to: Vec<_> = corrected_reading.split('\'').collect();
    let old: Vec<_> = source.chars().collect();
    let new: Vec<_> = corrected.chars().collect();
    let mut prefix = 0;
    while prefix < from.len().min(to.len()) && from[prefix] == to[prefix] {
        if old[prefix] != new[prefix] {
            return None;
        }
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < (from.len() - prefix).min(to.len() - prefix)
        && from[from.len() - 1 - suffix] == to[to.len() - 1 - suffix]
    {
        if old[old.len() - 1 - suffix] != new[new.len() - 1 - suffix] {
            return None;
        }
        suffix += 1;
    }
    let old_range = prefix..from.len() - suffix;
    let new_range = prefix..to.len() - suffix;
    if old_range.len().max(new_range.len()) > 3
        || old.len().abs_diff(new.len()) > 2
        || (old_range.is_empty() && new_range.is_empty())
    {
        return None;
    }
    Some((old_range, new_range))
}

pub fn eligible_text(text: &str) -> bool {
    let count = text.chars().count();
    (MIN_TEXT_CHARS..=MAX_TEXT_CHARS).contains(&count)
        && text.chars().all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

pub fn eligible_spelling_text(text: &str) -> bool {
    (MIN_SPELLING_TEXT_CHARS..=MAX_TEXT_CHARS).contains(&text.chars().count())
        && text.chars().all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
}

/// Structural safety check, separate from model confidence filtering.
pub fn valid_correction(source: &str, corrected: &str) -> bool {
    if !eligible_text(source)
        || !eligible_text(corrected)
        || source.chars().count() != corrected.chars().count()
    {
        return false;
    }
    let changed = source
        .chars()
        .zip(corrected.chars())
        .filter(|(a, b)| a != b)
        .count();
    changed > 0 && changed <= 2 && changed * 4 <= source.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_pinyin_request_does_not_relax_plain_csc() {
        let mut request = Request {
            id: 1,
            text: "学校哦".into(),
            raw: "xuexo".into(),
            reading: "xue'xiao'o".into(),
        };
        assert!(eligible_request(&request));
        assert!(!eligible_text(&request.text));
        assert!(spelling_change_span("xuexo", "学校哦", "xue'xiao'o", "学习", "xue'xi").is_some());
        request.raw.clear();
        request.reading.clear();
        assert!(!eligible_request(&request));
        assert!(!valid_correction("学西", "学习"));
        assert!(!eligible_spelling_text("学"));
        assert!(!eligible_spelling_text("学习!"));
        for length in [1, 3, 4, 128, 129] {
            let request = Request {
                id: 2,
                text: "学习".into(),
                raw: "x".repeat(length),
                reading: "xue'xi".into(),
            };
            assert_eq!(eligible_request(&request), (4..=128).contains(&length));
        }
    }

    #[test]
    fn displayed_status_omits_product_version_without_losing_reason() {
        for status in [
            AiStatus::Disabled,
            AiStatus::NoSuggestion,
            AiStatus::ScopeBlocked(ScopeBlockReason::Undeclared),
        ] {
            let text = status.label();
            assert!(text.starts_with("AI "));
            assert!(!text.contains("Flower 0."));
            assert!(!text.contains(env!("CARGO_PKG_VERSION")));
        }
        assert!(AiStatus::ScopeBlocked(ScopeBlockReason::Undeclared)
            .label()
            .contains("[S04]"));
    }

    #[test]
    fn correction_bounds_reject_rewrites_and_non_han() {
        assert!(valid_correction("今天新情很好", "今天心情很好"));
        assert!(!valid_correction("今天心情很好", "今天心情很好"));
        assert!(!valid_correction("今天心情很好", "今天心情很好呀"));
        assert!(!valid_correction("今天新情很好", "今天心情很A"));
        assert!(!eligible_text("nihao"));
        assert!(!eligible_text("𠮷野家很好"));
        assert!(!valid_correction("今天新情很好", "天气实在不好"));
    }
}

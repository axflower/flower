/// A candidate produced for the current Pinyin input.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Candidate {
    pub text: String,
    /// Optional annotation for the candidate window, currently the Pinyin key.
    pub comment: String,
}

impl Candidate {
    pub(crate) const SPELLING_PREFIX: &str = "[Flower Spelling] ";
    pub fn new(text: impl Into<String>, comment: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            comment: comment.into(),
        }
    }

    pub fn is_ai(&self) -> bool {
        self.comment == crate::ai::AI_COMMENT
    }

    pub fn is_spelling(&self) -> bool {
        self.comment.starts_with(Self::SPELLING_PREFIX)
    }

    /// Display-only provenance; never part of committed text.
    pub fn display_text(&self) -> std::borrow::Cow<'_, str> {
        if self.is_ai() {
            std::borrow::Cow::Owned(format!("{} ✓ AI", self.text))
        } else if self.is_spelling() {
            std::borrow::Cow::Owned(format!("{} ✓ 拼写", self.text))
        } else {
            std::borrow::Cow::Borrowed(&self.text)
        }
    }
}

/// The complete state change returned after processing a key or selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearningEvent {
    /// Canonical, explicitly separated syllables, never raw unconverted input.
    pub pinyin: String,
    pub text: String,
}

/// Stable identity for one complete displayed choice; never grants write permission.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RankingCandidate {
    pub pinyin: String,
    pub text: String,
    pub consumed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RankingOrder {
    pub input: String,
    pub candidates: Vec<RankingCandidate>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionEvent {
    pub order: RankingOrder,
    pub selected: RankingCandidate,
}

/// A selected word, not permission to persist before a safe host commit.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompositionUpdate {
    /// True when the input session consumed the key.
    pub handled: bool,
    /// Display preedit: confirmed Chinese prefix plus the still-unselected Pinyin.
    pub preedit: String,
    /// Collapsed caret in UTF-16 units of preedit; None means the end.
    pub preedit_caret_utf16: Option<usize>,
    /// Candidates on the current page only.
    pub candidates: Vec<Candidate>,
    /// Zero-based highlighted candidate on the current page.
    pub highlighted: Option<usize>,
    /// Zero-based candidate page.
    pub page: usize,
    /// Number of candidate pages, or zero when there are no candidates.
    pub page_count: usize,
    /// Text to insert into the host document.
    pub commit: Option<String>,
    /// True when Escape or an explicit cancel cleared the composition.
    pub cancelled: bool,
}

impl CompositionUpdate {
    pub fn is_composing(&self) -> bool {
        !self.preedit.is_empty()
    }

    pub fn committed_text(&self) -> Option<&str> {
        self.commit.as_deref()
    }
}

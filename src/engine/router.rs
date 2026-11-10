use super::{CompositionUpdate, EngineError, KeyEvent, Lexicon, PinyinSession, DEFAULT_PAGE_SIZE};

/// Routes platform-independent key events into one owned Pinyin session.
///
/// The router is the only mutable entry point for key events. Read-only
/// callers can preview a key with [`EngineRouter::test_key`] without changing
/// the active composition.
#[derive(Debug, Clone)]
pub struct EngineRouter {
    session: PinyinSession,
}

impl EngineRouter {
    pub fn project_chinese_script(&mut self, script: crate::ChineseScript) -> CompositionUpdate {
        self.session.project_chinese_script(script)
    }
    pub fn set_chinese_script(&mut self, script: crate::ChineseScript) -> bool {
        self.session.set_chinese_script(script)
    }
    /// Creates a router backed by the shared embedded lexicon.
    pub fn new() -> Result<Self, EngineError> {
        Self::with_page_size(DEFAULT_PAGE_SIZE)
    }

    /// Creates a router with a custom candidate page size.
    pub fn with_page_size(page_size: usize) -> Result<Self, EngineError> {
        PinyinSession::with_page_size(page_size).map(Self::from_session)
    }

    /// Creates a router from caller-provided TSV data.
    pub fn from_lexicon_tsv(tsv: &str, page_size: usize) -> Result<Self, EngineError> {
        PinyinSession::from_lexicon_tsv(tsv, page_size).map(Self::from_session)
    }

    /// Creates a router from an already validated lexicon.
    pub fn from_lexicon(lexicon: Lexicon, page_size: usize) -> Result<Self, EngineError> {
        PinyinSession::from_lexicon(lexicon, page_size).map(Self::from_session)
    }

    /// Reports whether a key would be handled without changing the session.
    pub fn test_key(&self, event: KeyEvent) -> Result<bool, EngineError> {
        let mut preview = self.session.clone();
        preview.process_key(event).map(|update| update.handled)
    }

    /// Processes one key and advances the active composition.
    pub fn process_key(&mut self, event: KeyEvent) -> Result<CompositionUpdate, EngineError> {
        self.session.process_key(event)
    }

    /// Explicitly commit the original spelling without learning.
    pub fn commit_raw(&mut self) -> CompositionUpdate {
        self.session.commit_raw()
    }

    pub fn ai_input(&self) -> Option<crate::ai::AiInput> {
        self.session.ai_input()
    }

    pub fn apply_ai_candidate(
        &mut self,
        expected: &crate::ai::AiInput,
        corrected: &str,
    ) -> Option<CompositionUpdate> {
        self.session.apply_ai_candidate(expected, corrected)
    }

    pub fn apply_ai_spelling_candidate(
        &mut self,
        expected: &crate::ai::AiInput,
        corrected: &str,
        reading: &str,
    ) -> Option<CompositionUpdate> {
        self.session
            .apply_ai_spelling_candidate(expected, corrected, reading)
    }

    /// Selects and commits a zero-based candidate from the current page.
    pub fn select_candidate(&mut self, index: usize) -> Result<CompositionUpdate, EngineError> {
        self.session.select_candidate(index)
    }

    /// Moves to the next candidate page, if one exists.
    pub fn next_page(&mut self) -> Result<CompositionUpdate, EngineError> {
        self.session.next_page()
    }

    /// Moves to the previous candidate page, if one exists.
    pub fn previous_page(&mut self) -> Result<CompositionUpdate, EngineError> {
        self.session.previous_page()
    }

    /// Returns true while a composition is active.
    pub fn is_composing(&self) -> bool {
        self.session.is_composing()
    }

    /// Returns the current raw Pinyin input.
    pub fn preedit(&self) -> &str {
        self.session.preedit()
    }

    pub fn pending_input(&self) -> &str {
        self.session.pending_input()
    }

    pub fn pending_learning(&self) -> &[super::LearningEvent] {
        self.session.pending_learning()
    }

    pub fn pending_phrase_learning(&self) -> &[super::LearningEvent] {
        self.session.pending_phrase_learning()
    }

    pub fn set_auto_phrase_lexicon(&mut self, lexicon: Option<std::sync::Arc<Lexicon>>) {
        self.session.set_auto_phrase_lexicon(lexicon);
    }

    pub fn pending_promotions(&self) -> &[super::PromotionEvent] {
        self.session.pending_promotions()
    }

    pub fn set_candidate_width(&mut self, width: Option<u32>) -> CompositionUpdate {
        self.session.set_candidate_width(width)
    }

    pub fn set_ranking_orders(&mut self, orders: std::sync::Arc<Vec<super::RankingOrder>>) {
        self.session.set_ranking_orders(orders);
    }

    /// Returns the owned session for read-only inspection.
    pub fn session(&self) -> &PinyinSession {
        &self.session
    }

    fn from_session(session: PinyinSession) -> Self {
        Self { session }
    }
}

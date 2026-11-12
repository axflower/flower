use super::{
    decoder::{decode, Choice},
    LearningEvent,
};
use super::{ranking_indices, PromotionEvent, RankingCandidate, RankingOrder};
use super::{Candidate, CompositionUpdate, EngineError, Key, KeyEvent, Lexicon};
use std::{ops::Range, sync::Arc};

#[derive(Debug, Clone)]
struct Confirmed {
    raw: String,
    text: String,
    reading: String,
    events: Vec<LearningEvent>,
}

/// Default number of candidates shown on one page.
pub const DEFAULT_PAGE_SIZE: usize = 7;

/// Mutable Pinyin composition state.
#[derive(Debug, Clone)]
pub struct PinyinSession {
    chinese_script: crate::ChineseScript,
    lexicon: Lexicon,
    auto_phrase_lexicon: Option<Arc<Lexicon>>,
    page_size: usize,
    candidate_width: Option<u32>,
    page_ranges: Vec<Range<usize>>,
    ranking_orders: Arc<Vec<RankingOrder>>,
    pending_promotions: Vec<PromotionEvent>,
    preedit: String,
    raw: String,
    raw_caret: usize,
    confirmed: Vec<Confirmed>,
    choices: Vec<Choice>,
    // Captured from the pending ordinary body before AI replaces its identity.
    long_pending_body: bool,
    pending_learning: Vec<LearningEvent>,
    pending_phrase_learning: Vec<LearningEvent>,
    candidates: Vec<Candidate>,
    page: usize,
    highlighted: Option<usize>,
    double_quote_open: bool,
    single_quote_open: bool,
}

/// Stable name for hosts that associate one session with one input context.
pub type EngineSession = PinyinSession;

impl PinyinSession {
    /// Creates a session backed by the shared embedded lexicon.
    pub fn new() -> Result<Self, EngineError> {
        Self::with_page_size(DEFAULT_PAGE_SIZE)
    }

    /// Creates a session with a custom candidate page size.
    pub fn with_page_size(page_size: usize) -> Result<Self, EngineError> {
        let lexicon = Lexicon::builtin()?;
        Self::from_lexicon(lexicon, page_size)
    }

    /// Creates a session from caller-provided TSV data.
    pub fn from_lexicon_tsv(tsv: &str, page_size: usize) -> Result<Self, EngineError> {
        let lexicon = Lexicon::from_tsv(tsv)?;
        Self::from_lexicon(lexicon, page_size)
    }

    /// Creates a session from an already validated lexicon.
    pub fn from_lexicon(lexicon: Lexicon, page_size: usize) -> Result<Self, EngineError> {
        if page_size == 0 {
            return Err(EngineError::InvalidPageSize { page_size });
        }
        if lexicon.is_empty() && !lexicon.is_dormant() {
            return Err(EngineError::EmptyLexicon);
        }

        Ok(Self {
            chinese_script: crate::ChineseScript::default(),
            lexicon,
            auto_phrase_lexicon: None,
            page_size,
            candidate_width: None,
            page_ranges: Vec::new(),
            ranking_orders: Arc::new(Vec::new()),
            pending_promotions: Vec::new(),
            preedit: String::new(),
            raw: String::new(),
            raw_caret: 0,
            confirmed: Vec::new(),
            choices: Vec::new(),
            long_pending_body: false,
            pending_learning: Vec::new(),
            pending_phrase_learning: Vec::new(),
            candidates: Vec::new(),
            page: 0,
            highlighted: None,
            double_quote_open: true,
            single_quote_open: true,
        })
    }

    pub(crate) fn initialize_dormant_lexicon(&mut self, lexicon: Lexicon) {
        debug_assert!(self.lexicon.is_dormant() && !self.is_composing());
        self.lexicon = lexicon;
    }

    /// Processes one key and returns a snapshot of the resulting composition.
    ///
    /// Command-style shortcuts are never consumed. Digit keys `1..=9` select
    /// candidates on the current page. Space and Enter commit the highlighted
    /// candidate, or the raw Pinyin text if no candidate exists.
    pub fn process_key(&mut self, event: KeyEvent) -> Result<CompositionUpdate, EngineError> {
        if event.is_script_toggle() {
            let script = match self.chinese_script {
                crate::ChineseScript::Simplified => crate::ChineseScript::Traditional,
                crate::ChineseScript::Traditional => crate::ChineseScript::Simplified,
            };
            return Ok(self.project_chinese_script(script));
        }
        if event.is_passthrough() {
            return Ok(self.snapshot(false));
        }

        // Platform-neutral callers can also supply Shift plus a digit directly.
        let key = match event.key {
            Key::Char(c) if event.modifiers.shift && c.is_ascii_digit() => {
                Key::Char(b")!@#$%^&*("[(c as u8 - b'0') as usize] as char)
            }
            key => key,
        };
        if let Key::Char(character) = key {
            if let Some(punctuation) = self.chinese_punctuation(character) {
                let mut update = if self.is_composing() {
                    self.commit_current()?
                } else {
                    self.pending_learning.clear();
                    self.pending_promotions.clear();
                    self.pending_phrase_learning.clear();
                    self.snapshot(true)
                };
                update
                    .commit
                    .get_or_insert_with(String::new)
                    .push_str(punctuation);
                return Ok(update);
            }
        }

        match key {
            Key::Char(character) if character.is_ascii_alphabetic() => {
                if self.raw.len() + self.confirmed.iter().map(|s| s.raw.len()).sum::<usize>() >= 128
                {
                    return Err(EngineError::InputTooLong);
                }
                self.pending_learning.clear();
                self.pending_promotions.clear();
                self.pending_phrase_learning.clear();
                self.raw
                    .insert(self.raw_caret, character.to_ascii_lowercase());
                self.raw_caret += 1;
                self.refresh_candidates();
                Ok(self.snapshot(true))
            }
            Key::Char('\'') if self.is_composing() => {
                if self.raw_caret > 0
                    && self.raw.as_bytes()[self.raw_caret - 1] != b'\''
                    && self.raw.as_bytes().get(self.raw_caret) != Some(&b'\'')
                {
                    if self.raw.len() + self.confirmed.iter().map(|s| s.raw.len()).sum::<usize>()
                        >= 128
                    {
                        return Err(EngineError::InputTooLong);
                    }
                    self.pending_learning.clear();
                    self.pending_promotions.clear();
                    self.pending_phrase_learning.clear();
                    self.raw.insert(self.raw_caret, '\'');
                    self.raw_caret += 1;
                    self.refresh_candidates();
                }
                Ok(self.snapshot(true))
            }
            Key::Char(character) if character.is_ascii_digit() && character != '0' => {
                if !self.is_composing() {
                    return Ok(self.snapshot(false));
                }

                let index = usize::from(character as u8 - b'1');
                let candidate_count = self.current_page().len();
                if index >= candidate_count {
                    return Ok(self.snapshot(true));
                }
                self.select_candidate(index)
            }
            Key::Backspace => {
                if !self.is_composing() {
                    return Ok(self.snapshot(false));
                }

                if self.raw_caret == 0 {
                    if let Some(segment) = self.confirmed.pop() {
                        self.raw_caret = segment.raw.len();
                        self.raw.insert_str(0, &segment.raw);
                    } else {
                        return Ok(self.snapshot(true));
                    }
                } else {
                    self.raw_caret -= 1;
                    self.raw.remove(self.raw_caret);
                }
                self.pending_learning.clear();
                self.pending_promotions.clear();
                self.pending_phrase_learning.clear();
                if self.raw.is_empty() && self.confirmed.is_empty() {
                    self.clear_composition();
                } else {
                    self.refresh_candidates();
                }
                Ok(self.snapshot(true))
            }
            Key::Delete => {
                if !self.is_composing() {
                    return Ok(self.snapshot(false));
                }
                if self.raw_caret < self.raw.len() {
                    self.raw.remove(self.raw_caret);
                    self.pending_learning.clear();
                    self.pending_promotions.clear();
                    self.pending_phrase_learning.clear();
                    if self.raw.is_empty() && self.confirmed.is_empty() {
                        self.clear_composition();
                    } else {
                        self.refresh_candidates();
                    }
                }
                Ok(self.snapshot(true))
            }
            Key::Enter => {
                if !self.is_composing() {
                    return Ok(self.snapshot(false));
                }
                self.commit_current()
            }
            Key::Space => {
                if !self.is_composing() {
                    return Ok(self.snapshot(false));
                }
                if let Some(index) = self.highlighted {
                    self.select_candidate(index)
                } else {
                    self.commit_literal()
                }
            }
            Key::Escape => {
                if !self.is_composing() {
                    return Ok(self.snapshot(false));
                }
                self.clear_composition();
                let mut update = self.snapshot(true);
                update.cancelled = true;
                Ok(update)
            }
            Key::PageDown | Key::Char('+' | '=') => {
                if !self.is_composing() {
                    return Ok(self.snapshot(false));
                }
                self.change_page(1);
                Ok(self.snapshot(true))
            }
            Key::PageUp | Key::Char('-') => {
                if !self.is_composing() {
                    return Ok(self.snapshot(false));
                }
                self.change_page(-1);
                Ok(self.snapshot(true))
            }
            Key::Left => {
                if !self.is_composing() {
                    return Ok(self.snapshot(false));
                }
                if self.raw_caret < self.raw.len()
                    || self.highlighted.map_or(true, |index| index == 0)
                {
                    self.raw_caret = self.raw_caret.saturating_sub(1);
                } else {
                    self.move_highlight(-1);
                }
                Ok(self.snapshot(true))
            }
            Key::Right => {
                if !self.is_composing() {
                    return Ok(self.snapshot(false));
                }
                if self.raw_caret < self.raw.len() {
                    self.raw_caret += 1;
                } else {
                    self.move_highlight(1);
                }
                Ok(self.snapshot(true))
            }
            Key::Up | Key::Down => {
                if !self.is_composing() {
                    return Ok(self.snapshot(false));
                }
                self.raw_caret = self.raw.len();
                self.move_highlight(if key == Key::Up { -1 } else { 1 });
                Ok(self.snapshot(true))
            }
            Key::Char(_) | Key::Other => Ok(self.snapshot(false)),
        }
    }

    fn chinese_punctuation(&mut self, character: char) -> Option<&'static str> {
        Some(match character {
            ',' => "，",
            '.' => "。",
            '?' => "？",
            '!' => "！",
            ';' => "；",
            ':' => "：",
            '(' => "（",
            ')' => "）",
            '[' => "【",
            ']' => "】",
            '<' => "《",
            '>' => "》",
            '/' | '\\' => "、",
            '~' => "～",
            '^' => "……",
            '_' => "——",
            '@' => "@",
            '#' => "#",
            '$' => "$",
            '%' => "%",
            '&' => "&",
            '*' => "*",
            '+' => "+",
            '{' => "{",
            '}' => "}",
            '|' => "|",
            '`' => "`",
            '"' => {
                let text = if self.double_quote_open { "“" } else { "”" };
                self.double_quote_open = !self.double_quote_open;
                text
            }
            '\'' if !self.is_composing() => {
                let text = if self.single_quote_open { "‘" } else { "’" };
                self.single_quote_open = !self.single_quote_open;
                text
            }
            _ => return None,
        })
    }

    /// Selects and commits a zero-based candidate from the current page.
    pub fn select_candidate(&mut self, index: usize) -> Result<CompositionUpdate, EngineError> {
        if !self.is_composing() {
            return Err(EngineError::NoActiveComposition);
        }

        let candidate_count = self.current_page().len();
        if index >= candidate_count {
            return Err(EngineError::CandidateOutOfRange {
                index,
                candidate_count,
            });
        }
        let choice = self
            .choices
            .get(self.page_ranges[self.page].start + index)
            .cloned()
            .ok_or(EngineError::CandidateOutOfRange {
                index,
                candidate_count,
            })?;

        if choice.candidate.is_ai()
            || choice.candidate.is_spelling()
            || choice.candidate.is_completion()
        {
            let corrected = if choice.candidate.is_spelling() || choice.candidate.is_completion() {
                self.confirmed
                    .iter()
                    .map(|s| s.text.as_str())
                    .collect::<String>()
                    + &choice.candidate.text
            } else {
                choice.candidate.text
            };
            self.clear_composition();
            let mut update = self.snapshot(true);
            update.commit = Some(self.chinese_script.convert(&corrected));
            return Ok(update);
        }
        let promotion = (choice.consumed == self.raw.len()).then(|| PromotionEvent {
            order: RankingOrder {
                input: self.raw.clone(),
                candidates: self
                    .choices
                    .iter()
                    .filter(|c| {
                        c.consumed == self.raw.len()
                            && !c.candidate.is_ai()
                            && !c.candidate.is_spelling()
                            && !c.candidate.is_completion()
                    })
                    .map(Self::ranking_candidate)
                    .collect(),
            },
            selected: Self::ranking_candidate(&choice),
        });
        let raw = self.raw[..choice.consumed].to_owned();
        self.raw.drain(..choice.consumed);
        self.raw_caret = self.raw.len();
        self.confirmed.push(Confirmed {
            raw,
            text: choice.candidate.text,
            reading: choice.candidate.comment,
            events: choice.events,
        });
        if self.raw.is_empty() {
            let update = self.commit_literal()?;
            self.pending_promotions.extend(promotion);
            Ok(update)
        } else {
            self.pending_learning.clear();
            self.refresh_candidates();
            Ok(self.snapshot(true))
        }
    }

    /// Commits the highlighted candidate or the raw Pinyin input.
    pub fn commit(&mut self) -> Result<CompositionUpdate, EngineError> {
        if !self.is_composing() {
            return Err(EngineError::NoActiveComposition);
        }
        self.commit_current()
    }

    /// Cancels the active composition without committing text.
    pub fn cancel(&mut self) -> Result<CompositionUpdate, EngineError> {
        if !self.is_composing() {
            return Err(EngineError::NoActiveComposition);
        }
        self.clear_composition();
        let mut update = self.snapshot(true);
        update.cancelled = true;
        Ok(update)
    }

    /// Moves to the next candidate page, if one exists.
    pub fn next_page(&mut self) -> Result<CompositionUpdate, EngineError> {
        if !self.is_composing() {
            return Err(EngineError::NoActiveComposition);
        }
        self.change_page(1);
        Ok(self.snapshot(true))
    }

    /// Moves to the previous candidate page, if one exists.
    pub fn previous_page(&mut self) -> Result<CompositionUpdate, EngineError> {
        if !self.is_composing() {
            return Err(EngineError::NoActiveComposition);
        }
        self.change_page(-1);
        Ok(self.snapshot(true))
    }

    /// Changes the candidate page size while preserving valid page state.
    pub fn set_page_size(&mut self, page_size: usize) -> Result<(), EngineError> {
        if page_size == 0 {
            return Err(EngineError::InvalidPageSize { page_size });
        }

        self.page_size = page_size;
        let selected = self.selected_identity();
        self.apply_first_page_quotas();
        self.rebuild_pages();
        self.restore_selected_identity(selected);
        Ok(())
    }

    /// Script changes are accepted only with an empty composition.
    /// Reprojects without changing raw input, confirmed identities or learning.
    pub fn project_chinese_script(&mut self, script: crate::ChineseScript) -> CompositionUpdate {
        self.chinese_script = script;
        self.pending_learning.clear();
        self.pending_phrase_learning.clear();
        self.pending_promotions.clear();
        self.refresh_candidates();
        self.snapshot(true)
    }

    /// Initialization-only setter; active composition uses projection above.
    pub fn set_chinese_script(&mut self, script: crate::ChineseScript) -> bool {
        if self.is_composing() {
            return false;
        }
        self.chinese_script = script;
        true
    }

    /// Retains the host's width hint for compatibility. Pages always use the
    /// configured count, preserving the absolute highlighted candidate.
    pub fn set_candidate_width(&mut self, width: Option<u32>) -> CompositionUpdate {
        self.candidate_width = width;
        self.rebuild_pages();
        self.snapshot(true)
    }

    /// Changes the next decode's ordering, never the currently displayed choices.
    pub fn set_ranking_orders(&mut self, orders: Arc<Vec<RankingOrder>>) {
        self.ranking_orders = orders;
    }

    /// Events require a successful, safe host commit before they may be applied.
    pub fn pending_promotions(&self) -> &[PromotionEvent] {
        &self.pending_promotions
    }

    pub fn page_size(&self) -> usize {
        self.page_size
    }

    pub fn preedit(&self) -> &str {
        &self.preedit
    }

    pub fn pending_input(&self) -> &str {
        &self.raw
    }

    /// Events are not permission to persist: the host must first confirm a safe successful edit.
    pub fn pending_learning(&self) -> &[LearningEvent] {
        &self.pending_learning
    }

    /// Novel words formed by at least two explicit ordinary selections.
    /// Successful host editing and privacy checks are still required.
    pub fn pending_phrase_learning(&self) -> &[LearningEvent] {
        &self.pending_phrase_learning
    }

    /// Replace the small personal index only between compositions.
    pub fn set_auto_phrase_lexicon(&mut self, lexicon: Option<Arc<Lexicon>>) {
        if !self.is_composing() {
            self.auto_phrase_lexicon = lexicon;
        }
    }

    pub fn page(&self) -> usize {
        self.page
    }

    pub fn page_count(&self) -> usize {
        self.page_ranges.len()
    }

    pub fn highlighted(&self) -> Option<usize> {
        self.highlighted
    }

    pub fn is_composing(&self) -> bool {
        !self.preedit.is_empty()
    }

    /// Candidates on the current page.
    pub fn candidates_on_current_page(&self) -> &[Candidate] {
        self.current_page()
    }

    fn commit_current(&mut self) -> Result<CompositionUpdate, EngineError> {
        if let Some(index) = self.highlighted {
            let absolute = self.page_ranges[self.page].start + index;
            if self
                .choices
                .get(absolute)
                .is_some_and(|choice| choice.consumed == self.raw.len())
            {
                return self.select_candidate(index);
            }
        }
        self.commit_literal()
    }

    /// Commit the complete original spelling, including selected but uncommitted segments.
    /// Explicit literal conversion never creates learning or ranking events.
    pub fn commit_raw(&mut self) -> CompositionUpdate {
        if !self.is_composing() {
            return self.snapshot(false);
        }
        let text = self
            .confirmed
            .iter()
            .map(|segment| segment.raw.as_str())
            .collect::<String>()
            + &self.raw;
        self.clear_composition();
        let mut update = self.snapshot(true);
        update.commit = Some(text);
        update
    }

    /// Source for a suggestion, not permission to cross the host privacy boundary.
    pub fn ai_input(&self) -> Option<crate::ai::AiInput> {
        if !self.is_composing()
            || self.raw_caret != self.raw.len()
            || self.page != 0
            || self.highlighted != Some(0)
        {
            return None;
        }
        let choice = self.choices.iter().find(|c| {
            c.consumed == self.raw.len()
                && !c.candidate.is_spelling()
                && !c.candidate.is_ai()
                && !c.candidate.is_completion()
        })?;
        let prefix: String = self.confirmed.iter().map(|s| s.text.as_str()).collect();
        let source = prefix + &choice.candidate.text;
        if !crate::ai::eligible_spelling_text(&source) {
            return None;
        }
        let raw: String = self.confirmed.iter().map(|s| s.raw.as_str()).collect();
        let reading = self
            .confirmed
            .iter()
            .map(|s| s.reading.as_str())
            .chain(std::iter::once(choice.candidate.comment.as_str()))
            .filter(|reading| !reading.is_empty())
            .collect::<Vec<_>>()
            .join("'");
        let input = crate::ai::AiInput {
            reading,
            raw: raw + &self.raw,
            source,
        };
        (4..=128).contains(&input.raw.len()).then_some(input)
    }

    pub fn apply_ai_candidate(
        &mut self,
        expected: &crate::ai::AiInput,
        corrected: &str,
    ) -> Option<CompositionUpdate> {
        if self.ai_input().as_ref() != Some(expected)
            || !crate::ai::valid_correction(&expected.source, corrected)
        {
            return None;
        }
        Some(self.insert_ai_candidate(corrected))
    }

    pub fn apply_ai_spelling_candidate(
        &mut self,
        expected: &crate::ai::AiInput,
        corrected: &str,
        reading: &str,
    ) -> Option<CompositionUpdate> {
        if self.ai_input().as_ref() != Some(expected)
            || crate::ai::spelling_change_span(
                &expected.raw,
                &expected.source,
                &expected.reading,
                corrected,
                reading,
            )
            .is_none()
        {
            return None;
        }
        Some(self.insert_ai_candidate(corrected))
    }

    fn insert_ai_candidate(&mut self, corrected: &str) -> CompositionUpdate {
        let selected = self.selected_identity();
        let prefix: String = self.confirmed.iter().map(|s| s.text.as_str()).collect();
        let display = self.chinese_script.convert(corrected);
        self.choices.retain(|c| {
            !c.candidate.is_ai()
                && !c.candidate.is_spelling()
                && (c.consumed != self.raw.len()
                    || Self::candidate_output(self.chinese_script, &prefix, &c.candidate)
                        != display)
        });
        let mut choice = Choice::word(
            Candidate::new(corrected, crate::ai::AI_COMMENT),
            self.raw.len(),
            1,
        );
        choice.events.clear();
        let insertion = self
            .choices
            .iter()
            .position(|c| {
                c.consumed == self.raw.len()
                    && !c.candidate.is_ai()
                    && !c.candidate.is_spelling()
                    && !c.candidate.is_completion()
            })
            .map_or(0, |index| index + 1);
        self.choices.insert(insertion, choice);
        self.rebuild_display_candidates();
        self.apply_first_page_quotas();
        self.rebuild_pages();
        self.restore_selected_identity(selected);
        self.snapshot(true)
    }

    fn commit_literal(&mut self) -> Result<CompositionUpdate, EngineError> {
        let text = self
            .confirmed
            .iter()
            .map(|s| s.text.as_str())
            .collect::<String>()
            + &self.raw;
        let events: Vec<_> = self
            .confirmed
            .iter()
            .flat_map(|s| s.events.clone())
            .collect();
        let phrase = self.completed_phrase();
        self.clear_composition();
        self.pending_learning = events;
        self.pending_phrase_learning.extend(phrase);
        let mut update = self.snapshot(true);
        update.commit = Some(self.chinese_script.convert(&text));
        Ok(update)
    }

    fn completed_phrase(&self) -> Option<LearningEvent> {
        if !self.raw.is_empty() || self.confirmed.len() < 2 {
            return None;
        }
        let text: String = self.confirmed.iter().map(|s| s.text.as_str()).collect();
        let pinyin = self
            .confirmed
            .iter()
            .map(|s| s.reading.as_str())
            .collect::<Vec<_>>()
            .join("'");
        let characters = text.chars().count();
        if !(2..=32).contains(&characters)
            || !text.chars().all(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
            || pinyin.split('\'').count() != characters
            || pinyin.bytes().filter(|&b| b != b'\'').count() > 128
            || !pinyin.split('\'').all(|s| {
                !s.is_empty()
                    && s.bytes().all(|b| b.is_ascii_lowercase())
                    && s.bytes().any(|b| b"aeiouv".contains(&b))
            })
            || self.lexicon.contains_exact_pair(&pinyin, &text)
            || self
                .auto_phrase_lexicon
                .as_ref()
                .is_some_and(|l| l.contains_exact_pair(&pinyin, &text))
        {
            return None;
        }
        Some(LearningEvent { pinyin, text })
    }

    fn refresh_candidates(&mut self) {
        self.preedit = self.chinese_script.convert(
            &(self
                .confirmed
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>()
                + &self.raw),
        );
        self.choices = self
            .auto_phrase_lexicon
            .as_ref()
            .map(|lexicon| decode(lexicon, &self.raw))
            .unwrap_or_default();
        self.choices.extend(decode(&self.lexicon, &self.raw));
        let mut seen_choices = std::collections::HashSet::new();
        self.choices.retain(|choice| {
            seen_choices.insert((
                choice.candidate.text.clone(),
                choice.candidate.comment.clone(),
                choice.consumed,
            ))
        });
        let (complete, partial): (Vec<_>, Vec<_>) = self
            .choices
            .drain(..)
            .partition(|choice| choice.consumed == self.raw.len());
        let identities: Vec<_> = complete.iter().map(Self::ranking_candidate).collect();
        if let Some(saved) = self
            .ranking_orders
            .iter()
            .find(|order| order.input == self.raw)
        {
            self.choices = ranking_indices(&identities, saved)
                .into_iter()
                .map(|index| complete[index].clone())
                .collect();
        } else {
            self.choices = complete;
        }
        self.choices.extend(partial);
        // Classify the pending body, never its display projection (which can
        // include already confirmed segments). Stable sorting preserves ranking
        // within each prefix tier.
        let long = self
            .choices
            .iter()
            .find(|c| c.consumed == self.raw.len())
            .is_some_and(|c| c.candidate.text.chars().count() > 3);
        self.long_pending_body = long;
        if long {
            let mut kept_full = false;
            self.choices.retain(|c| {
                if c.consumed == self.raw.len() {
                    let keep = !kept_full;
                    kept_full = true;
                    keep
                } else {
                    true
                }
            });
            self.choices.sort_by_key(|c| {
                if c.consumed == self.raw.len() {
                    0
                } else if (2..=3).contains(&c.candidate.text.chars().count()) {
                    1
                } else if c.candidate.text.chars().count() == 1 {
                    2
                } else {
                    3
                }
            });
        }
        let mut proposals = Vec::new();
        if !self
            .auto_phrase_lexicon
            .as_ref()
            .is_some_and(|lexicon| !lexicon.exact_choices(&self.raw).is_empty())
        {
            proposals.extend(self.lexicon.short_spelling_choices(&self.raw));
            if let Some(source) = self.choices.iter().find(|c| c.consumed == self.raw.len()) {
                proposals.extend(
                    self.lexicon
                        .extra_syllable_choices(&self.raw, &source.candidate),
                );
            }
            proposals.extend(self.lexicon.composed_spelling_choices(&self.raw));
        }
        let mut spelling = Vec::new();
        for choice in proposals {
            self.choices.retain(|c| {
                c.consumed != self.raw.len() || c.candidate.text != choice.candidate.text
            });
            spelling.push(choice);
            if spelling.len() == 1 {
                break;
            }
        }
        let insertion = usize::from(!self.choices.is_empty());
        self.choices.splice(insertion..insertion, spelling);
        if self.raw_caret == self.raw.len() {
            let mut completion_budget = (512, 4096);
            let completion = self
                .lexicon
                .completion_choice(&self.raw, &mut completion_budget)
                .or_else(|| {
                    self.auto_phrase_lexicon
                        .as_ref()?
                        .completion_choice(&self.raw, &mut completion_budget)
                });
            if let Some(completion) = completion {
                let prefix: String = self.confirmed.iter().map(|c| c.text.as_str()).collect();
                let output =
                    Self::candidate_output(self.chinese_script, &prefix, &completion.candidate);
                if !self.choices.iter().any(|c| {
                    c.consumed == self.raw.len()
                        && Self::candidate_output(self.chinese_script, &prefix, &c.candidate)
                            == output
                }) {
                    let mut insertion = usize::from(!self.choices.is_empty());
                    if self
                        .choices
                        .get(insertion)
                        .is_some_and(|c| c.candidate.is_spelling())
                    {
                        insertion += 1;
                    }
                    self.choices.insert(insertion, completion);
                }
            }
        }
        self.rebuild_display_candidates();
        self.apply_first_page_quotas();
        self.page = 0;
        self.highlighted = if self.candidates.is_empty() {
            None
        } else {
            Some(0)
        };
        self.page_ranges.clear();
        self.rebuild_pages();
    }

    /// Reorder real choices only, after display deduplication. The unselected
    /// remainder stays reachable and ranking within each prefix tier is stable.
    fn apply_first_page_quotas(&mut self) {
        if !self.long_pending_body {
            if let Some(index) = self
                .choices
                .iter()
                .position(|c| c.candidate.is_completion())
            {
                let choice = self.choices.remove(index);
                let candidate = self.candidates.remove(index);
                let mut insertion = usize::from(!self.choices.is_empty());
                if self
                    .choices
                    .get(insertion)
                    .is_some_and(|c| c.candidate.is_ai() || c.candidate.is_spelling())
                {
                    insertion += 1;
                }
                self.choices.insert(insertion, choice);
                self.candidates.insert(insertion, candidate);
            }
            return;
        }
        let mut tiers: [Vec<usize>; 6] = Default::default();
        for (index, c) in self.choices.iter().enumerate() {
            let tier = if c.candidate.is_ai() || c.candidate.is_spelling() {
                1
            } else if c.candidate.is_completion() {
                2
            } else if c.consumed == self.raw.len() {
                0
            } else {
                match c.candidate.text.chars().count() {
                    2..=3 => 3,
                    1 => 4,
                    _ => 5,
                }
            };
            tiers[tier].push(index);
        }
        let mut order = Vec::new();
        order.extend(tiers[0].iter().take(1).copied());
        order.extend(tiers[1].iter().take(1).copied());
        order.extend(tiers[2].iter().take(1).copied());
        let slots = self.page_size.saturating_sub(order.len());
        let singles = slots.min(3).min(tiers[4].len());
        let words = (slots - singles).min(tiers[3].len());
        // If words run short, fill from singles before considering other prefixes.
        let singles = (slots - words).min(tiers[4].len());
        order.extend(tiers[3].iter().take(words).copied());
        order.extend(tiers[4].iter().take(singles).copied());
        let remaining = self.page_size.saturating_sub(order.len());
        order.extend(tiers[5].iter().take(remaining).copied());
        for tier in &tiers {
            for &index in tier {
                if !order.contains(&index) {
                    order.push(index);
                }
            }
        }
        self.choices = order.iter().map(|&i| self.choices[i].clone()).collect();
        self.candidates = order.iter().map(|&i| self.candidates[i].clone()).collect();
    }

    fn selected_identity(&self) -> Option<RankingCandidate> {
        let absolute = self.page_ranges.get(self.page)?.start + self.highlighted?;
        self.choices.get(absolute).map(Self::ranking_candidate)
    }

    fn restore_selected_identity(&mut self, selected: Option<RankingCandidate>) {
        let absolute = selected
            .and_then(|identity| {
                self.choices
                    .iter()
                    .position(|c| Self::ranking_candidate(c) == identity)
            })
            .unwrap_or(0);
        self.page = self
            .page_ranges
            .iter()
            .position(|r| r.contains(&absolute))
            .unwrap_or(0);
        self.highlighted = self.page_ranges.get(self.page).map(|r| absolute - r.start);
    }

    fn ranking_candidate(choice: &Choice) -> RankingCandidate {
        RankingCandidate {
            pinyin: choice.candidate.comment.clone(),
            text: choice.candidate.text.clone(),
            consumed: choice.consumed,
        }
    }

    /// AI candidates already contain the complete sentence; other candidates
    /// replace only the pending suffix but must convert with its raw context.
    fn candidate_output(
        script: crate::ChineseScript,
        prefix: &str,
        candidate: &Candidate,
    ) -> String {
        if candidate.is_ai() {
            script.convert(&candidate.text)
        } else {
            script.convert(&format!("{prefix}{}", candidate.text))
        }
    }

    fn candidate_projection(
        script: crate::ChineseScript,
        prefix: &str,
        displayed_prefix: &str,
        candidate: &Candidate,
    ) -> String {
        let output = Self::candidate_output(script, prefix, candidate);
        if candidate.is_ai() {
            output
        } else {
            // Context may rewrite the confirmed (but still uncommitted) prefix.
            // In that case show the entire replacement, never slice by length.
            output
                .strip_prefix(displayed_prefix)
                .unwrap_or(&output)
                .to_owned()
        }
    }

    fn rebuild_display_candidates(&mut self) {
        let script = self.chinese_script;
        let prefix: String = self.confirmed.iter().map(|s| s.text.as_str()).collect();
        let displayed_prefix = script.convert(&prefix);
        let mut seen = std::collections::HashSet::new();
        self.candidates.clear();
        self.choices.retain(|choice| {
            let text =
                Self::candidate_projection(script, &prefix, &displayed_prefix, &choice.candidate);
            if !seen.insert((
                text.clone(),
                choice.candidate.comment.clone(),
                choice.consumed,
            )) {
                return false;
            }
            self.candidates
                .push(Candidate::new(text, choice.candidate.comment.clone()));
            true
        });
    }

    fn rebuild_pages(&mut self) {
        let absolute = self
            .page_ranges
            .get(self.page)
            .and_then(|range| self.highlighted.map(|index| range.start + index))
            .unwrap_or(0);
        self.page_ranges.clear();
        let mut start = 0;
        while start < self.candidates.len() {
            let end = start
                .saturating_add(self.page_size)
                .min(self.candidates.len());
            self.page_ranges.push(start..end);
            start = end;
        }
        self.page = self
            .page_ranges
            .iter()
            .position(|range| range.contains(&absolute))
            .unwrap_or(0);
        self.highlighted = self
            .page_ranges
            .get(self.page)
            .map(|range| absolute.saturating_sub(range.start).min(range.len() - 1));
    }

    fn current_page(&self) -> &[Candidate] {
        self.page_ranges
            .get(self.page)
            .map(|range| &self.candidates[range.clone()])
            .unwrap_or(&[])
    }

    fn change_page(&mut self, delta: isize) {
        self.raw_caret = self.raw.len();
        let page_count = self.page_count();
        if page_count == 0 {
            self.page = 0;
            self.highlighted = None;
            return;
        }

        if delta < 0 {
            self.page = self.page.saturating_sub(delta.unsigned_abs());
        } else {
            self.page = self.page.saturating_add(delta as usize).min(page_count - 1);
        }
        self.highlighted = if self.current_page().is_empty() {
            None
        } else {
            Some(0)
        };
    }

    fn move_highlight(&mut self, delta: isize) {
        let candidate_count = self.current_page().len();
        if candidate_count == 0 {
            self.highlighted = None;
            return;
        }

        let current = self.highlighted.unwrap_or(0).min(candidate_count - 1);
        let next = if delta < 0 {
            current.saturating_sub(delta.unsigned_abs())
        } else {
            current
                .saturating_add(delta as usize)
                .min(candidate_count - 1)
        };
        self.highlighted = Some(next);
    }

    fn clear_composition(&mut self) {
        self.long_pending_body = false;
        self.preedit.clear();
        self.raw.clear();
        self.raw_caret = 0;
        self.confirmed.clear();
        self.choices.clear();
        self.pending_learning.clear();
        self.pending_promotions.clear();
        self.pending_phrase_learning.clear();
        self.candidates.clear();
        self.page_ranges.clear();
        self.page = 0;
        self.highlighted = None;
    }

    fn snapshot(&self, handled: bool) -> CompositionUpdate {
        CompositionUpdate {
            handled,
            preedit: self.preedit.clone(),
            preedit_caret_utf16: Some(
                self.chinese_script
                    .convert(
                        &self
                            .confirmed
                            .iter()
                            .map(|segment| segment.text.as_str())
                            .collect::<String>(),
                    )
                    .encode_utf16()
                    .count()
                    + self.raw_caret,
            ),
            candidates: self.current_page().to_vec(),
            highlighted: self.highlighted,
            page: self.page,
            page_count: self.page_count(),
            commit: None,
            cancelled: false,
        }
    }
}

#[cfg(test)]
mod candidate_contract_tests {
    use super::*;

    #[test]
    fn fixed_seven_pages_ignore_width_and_keep_absolute_numeric_selection() {
        let words: String = (0..16u32)
            .map(|i| {
                format!(
                    "shi\t{}\t{}\n",
                    char::from_u32(0x4e00 + i).unwrap(),
                    100 - i
                )
            })
            .collect();
        let mut s = PinyinSession::from_lexicon_tsv(&words, 7).unwrap();
        type_text(&mut s, "shi");
        let all = s.candidates.clone();
        assert_eq!(all.len(), 16);
        for width in [Some(1), Some(10000), None] {
            let update = s.set_candidate_width(width);
            assert_eq!(update.candidates.len(), 7);
            assert_eq!(s.page_ranges, vec![0..7, 7..14, 14..16]);
        }
        s.next_page().unwrap();
        s.process_key(KeyEvent::plain(Key::Right)).unwrap();
        let update = s.set_candidate_width(Some(1));
        assert_eq!(update.highlighted, Some(1));
        assert_eq!(update.candidates[1], all[8]);
        assert_eq!(
            s.process_key(KeyEvent::plain(Key::Char('2')))
                .unwrap()
                .commit,
            Some(all[8].text.clone())
        );
        type_text(&mut s, "shi");
        s.next_page().unwrap();
        s.next_page().unwrap();
        let before = s.snapshot(false);
        assert!(
            s.process_key(KeyEvent::plain(Key::Char('3')))
                .unwrap()
                .handled
        );
        assert_eq!(s.current_page(), before.candidates);
        assert_eq!(
            s.select_candidate(1).unwrap().commit,
            Some(all[15].text.clone())
        );
    }

    fn type_text(s: &mut PinyinSession, raw: &str) {
        for c in raw.chars() {
            s.process_key(KeyEvent::plain(Key::Char(c))).unwrap();
        }
    }

    fn select_text(s: &mut PinyinSession, text: &str) -> CompositionUpdate {
        for _ in 0..s.page_count() {
            if let Some(index) = s.current_page().iter().position(|c| c.text == text) {
                return s.select_candidate(index).unwrap();
            }
            s.next_page().unwrap();
        }
        panic!("missing {text}");
    }

    #[test]
    fn builtin_sentence_has_one_full_and_real_word_then_single_prefixes() {
        let mut s = PinyinSession::new().unwrap();
        type_text(&mut s, "jintianwomengongzuo");
        let ordinary: Vec<_> = s
            .choices
            .iter()
            .filter(|c| !c.candidate.is_spelling() && !c.candidate.is_ai())
            .collect();
        assert_eq!(
            ordinary
                .iter()
                .filter(|c| c.consumed == s.raw.len())
                .count(),
            1
        );
        assert!(ordinary[0].candidate.text.chars().count() > 3);
        let prefixes: Vec<_> = ordinary
            .iter()
            .filter(|c| c.consumed < s.raw.len())
            .collect();
        assert!(prefixes
            .iter()
            .any(|c| (2..=3).contains(&c.candidate.text.chars().count())));
        assert!(prefixes
            .iter()
            .any(|c| c.candidate.text.chars().count() == 1));
        for choice in &prefixes {
            assert!(s
                .lexicon
                .contains_exact_pair(&choice.candidate.comment, &choice.candidate.text));
            let reading = choice.candidate.comment.replace('\'', "");
            assert_eq!(&s.raw[..choice.consumed], reading);
        }
        let tiers: Vec<_> = s.choices[..7]
            .iter()
            .skip(1)
            .map(|c| match c.candidate.text.chars().count() {
                2..=3 => 0,
                1 => 1,
                _ => 2,
            })
            .collect();
        assert_eq!(tiers, [0, 0, 0, 1, 1, 1]);
        let input = s.ai_input().unwrap();
        let update = s.apply_ai_candidate(&input, "今天我们工做").unwrap();
        assert_eq!(update.highlighted, Some(0));
        assert!(update.candidates[1].is_ai());
        assert_eq!(
            s.choices[..7]
                .iter()
                .skip(2)
                .map(|c| match c.candidate.text.chars().count() {
                    2..=3 => 0,
                    1 => 1,
                    _ => 2,
                })
                .collect::<Vec<_>>(),
            [0, 0, 1, 1, 1]
        );
        for choice in &s.choices[2..7] {
            assert!(choice.consumed < s.raw.len());
            assert!(s
                .lexicon
                .contains_exact_pair(&choice.candidate.comment, &choice.candidate.text));
            assert_eq!(
                &s.raw[..choice.consumed],
                choice.candidate.comment.replace('\'', "")
            );
        }
        assert!(select_text(&mut s, "今天").commit.is_none());
        assert_eq!(s.pending_input(), "womengongzuo");
        assert!(select_text(&mut s, "我们").commit.is_none());
        assert_eq!(s.pending_input(), "gongzuo");
        assert_eq!(
            select_text(&mut s, "工作").commit.as_deref(),
            Some("今天我们工作")
        );
        assert!(!s.pending_learning().is_empty());
        assert!(s
            .pending_learning()
            .iter()
            .all(|e| !e.pinyin.contains("AI")));
    }

    #[test]
    fn short_pending_body_keeps_ambiguities_with_multiple_confirmed_segments() {
        let mut s = PinyinSession::from_lexicon_tsv(
            "jin'tian\t今天\t100\nwo'men\t我们\t100\nwo'men\t我门\t90\ngong'zuo\t工作\t100\ngong'zuo\t工做\t90", 7,
        ).unwrap();
        type_text(&mut s, "jintianwomengongzuo");
        select_text(&mut s, "今天");
        select_text(&mut s, "我们");
        assert_eq!(s.pending_input(), "gongzuo");
        assert!(s.candidates.iter().any(|c| c.text == "工作"));
        assert!(s.candidates.iter().any(|c| c.text == "工做"));
    }

    #[test]
    fn spelling_proposals_keep_the_original_top_one_only() {
        let lexicon = Lexicon::from_tsv("jin'tian\t今天\t100\njin'tian\t津田\t90").unwrap();
        let proposals = lexicon.short_spelling_choices("jintain");
        assert!(proposals.len() >= 2);
        let top = proposals[0].candidate.text.clone();
        let mut s = PinyinSession::from_lexicon(lexicon, 7).unwrap();
        type_text(&mut s, "jintain");
        let corrections: Vec<_> = s
            .choices
            .iter()
            .filter(|c| c.candidate.is_spelling())
            .collect();
        assert_eq!(corrections.len(), 1);
        assert_eq!(corrections[0].candidate.text, top);
    }

    #[test]
    fn ai_display_equivalent_full_keeps_quotas_raw_and_source_without_learning() {
        let mut s = PinyinSession::from_lexicon_tsv(
            "jin'tian'tou'fa'hen'hao\t今天头发很好\t1000\njin'tian\t今天\t100\njin'tian\t津田\t90\njin'tian\t金田\t80\njin'tian\t近天\t70\njin\t今\t100\njin\t金\t90\njin\t近\t80\njin\t进\t70",
            7,
        ).unwrap();
        type_text(&mut s, "jintiantoufahenhao");
        let input = s.ai_input().unwrap();
        assert_eq!(input.source, "今天头发很好");
        let update = s.apply_ai_candidate(&input, "今天頭发很好").unwrap();
        assert_eq!(update.highlighted, Some(0));
        assert_eq!(update.candidates[0].text, "今天头发很好");
        assert!(update.candidates[0].is_ai());
        assert_eq!(
            s.choices
                .iter()
                .filter(|c| c.consumed == s.raw.len())
                .count(),
            1
        );
        assert_eq!(
            update
                .candidates
                .iter()
                .map(|c| c.text.chars().count())
                .collect::<Vec<_>>(),
            [6, 2, 2, 2, 1, 1, 1]
        );
        assert_eq!(s.choices[0].consumed, s.raw.len());
        assert!(s.choices[0].events.is_empty());
        for choice in &s.choices[1..7] {
            assert!(s
                .lexicon
                .contains_exact_pair(&choice.candidate.comment, &choice.candidate.text));
            assert_eq!(
                &s.raw[..choice.consumed],
                choice.candidate.comment.replace('\'', "")
            );
        }
        // Reapplying the layout (including resizing) must not need an ordinary full.
        s.set_page_size(4).unwrap();
        assert_eq!(
            s.current_page()
                .iter()
                .map(|c| c.text.chars().count())
                .collect::<Vec<_>>(),
            [6, 1, 1, 1]
        );
        s.set_page_size(7).unwrap();
        assert_eq!(
            s.select_candidate(0).unwrap().commit.as_deref(),
            Some("今天头发很好")
        );
        assert!(s.pending_learning().is_empty());
        assert!(s.pending_promotions().is_empty());
        assert!(s.pending_phrase_learning().is_empty());
    }

    #[test]
    fn ai_equivalent_with_confirmed_prefix_does_not_classify_short_body_as_long() {
        let mut s = PinyinSession::from_lexicon_tsv(
            "jin'tian\t今天\t100\nwo'men\t我们\t100\ntou'fa\t头发\t100\ntou'fa\t投法\t90",
            7,
        )
        .unwrap();
        type_text(&mut s, "jintianwomentoufa");
        select_text(&mut s, "今天");
        select_text(&mut s, "我们");
        assert!(!s.long_pending_body);
        let input = s.ai_input().unwrap();
        assert_eq!(input.source, "今天我们头发");
        s.apply_ai_candidate(&input, "今天我們头发").unwrap();
        assert!(!s.long_pending_body);
        assert_eq!(s.choices[0].consumed, "toufa".len());
        assert!(s.candidates.iter().any(|c| c.text == "投法"));
        assert_eq!(
            s.candidates
                .iter()
                .filter(|c| c.text == "今天我们头发" && c.is_ai())
                .count(),
            1
        );
        let index = s.candidates.iter().position(|c| c.is_ai()).unwrap();
        assert_eq!(
            s.select_candidate(index).unwrap().commit.as_deref(),
            Some("今天我们头发")
        );
        assert!(s.pending_learning().is_empty());
        assert!(s.pending_promotions().is_empty());
        assert!(s.pending_phrase_learning().is_empty());
    }

    #[test]
    fn ai_replaces_corrections_and_deduplicates_without_learning() {
        let mut s =
            PinyinSession::from_lexicon_tsv("jin'tian'xin'qing'hen'hao\t今天新情很好\t100", 7)
                .unwrap();
        type_text(&mut s, "jintianxinqinghenhao");
        let input = s.ai_input().unwrap();
        // Exercise the same insertion boundary even when a correction and an
        // ordinary equivalent are both present before the result arrives.
        s.choices.push(Choice::word(
            Candidate::new("今天心情很好", "jin'tian'xin'qing'hen'hao"),
            s.raw.len(),
            1,
        ));
        s.choices.push(Choice::word(
            Candidate::new(
                "今天新晴很好",
                format!("{}test", Candidate::SPELLING_PREFIX),
            ),
            s.raw.len(),
            1,
        ));
        let first = s.apply_ai_candidate(&input, "今天心情很好").unwrap();
        assert_eq!(
            first
                .candidates
                .iter()
                .filter(|c| c.text == "今天心情很好")
                .count(),
            1
        );
        assert!(first
            .candidates
            .iter()
            .any(|c| c.text == "今天心情很好" && c.is_ai()));
        let replaced = s.apply_ai_candidate(&input, "今天新晴很好").unwrap();
        assert_eq!(
            replaced
                .candidates
                .iter()
                .filter(|c| c.is_ai() || c.is_spelling())
                .count(),
            1
        );
        assert!(replaced
            .candidates
            .iter()
            .any(|c| c.text == "今天新晴很好" && c.is_ai()));
        assert!(!replaced.candidates.iter().any(|c| c.text == "今天心情很好"));
        let before = s.candidates.clone();
        assert!(s.apply_ai_candidate(&input, "invalid").is_none());
        assert_eq!(s.candidates, before);
        assert_eq!(
            select_text(&mut s, "今天新晴很好").commit.as_deref(),
            Some("今天新晴很好")
        );
        assert!(s.pending_learning().is_empty());
        assert!(s.pending_phrase_learning().is_empty());
        assert!(s.pending_promotions().is_empty());
        assert!(s.apply_ai_candidate(&input, "今天心情很好").is_none());
    }
}

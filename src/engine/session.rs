use super::{
    decoder::{decode, Choice},
    LearningEvent,
};
use super::{
    paging::candidate_cell_width, ranking_indices, PromotionEvent, RankingCandidate, RankingOrder,
};
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
pub const DEFAULT_PAGE_SIZE: usize = 9;

/// Mutable Pinyin composition state.
#[derive(Debug, Clone)]
pub struct PinyinSession {
    lexicon: Lexicon,
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
    pending_learning: Vec<LearningEvent>,
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
        if lexicon.is_empty() {
            return Err(EngineError::EmptyLexicon);
        }

        Ok(Self {
            lexicon,
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
            pending_learning: Vec::new(),
            candidates: Vec::new(),
            page: 0,
            highlighted: None,
            double_quote_open: true,
            single_quote_open: true,
        })
    }

    /// Processes one key and returns a snapshot of the resulting composition.
    ///
    /// Command-style shortcuts are never consumed. Digit keys `1..=9` select
    /// candidates on the current page. Space and Enter commit the highlighted
    /// candidate, or the raw Pinyin text if no candidate exists.
    pub fn process_key(&mut self, event: KeyEvent) -> Result<CompositionUpdate, EngineError> {
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

        if choice.candidate.is_ai() || choice.candidate.is_spelling() {
            let corrected = if choice.candidate.is_spelling() {
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
            update.commit = Some(corrected);
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
        self.rebuild_pages();
        Ok(())
    }

    /// Reflows pages, preserving the absolute highlighted candidate.
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
            || self.choices.iter().any(|c| c.candidate.is_ai())
        {
            return None;
        }
        let choice = self
            .choices
            .iter()
            .find(|c| c.consumed == self.raw.len() && !c.candidate.is_spelling())?;
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
        let prefix: String = self.confirmed.iter().map(|s| s.text.as_str()).collect();
        self.choices.retain(|c| {
            c.consumed != self.raw.len() || format!("{}{}", prefix, c.candidate.text) != corrected
        });
        let mut choice = Choice::word(
            Candidate::new(corrected, crate::ai::AI_COMMENT),
            self.raw.len(),
            1,
        );
        choice.events.clear();
        self.choices.insert(1, choice);
        self.candidates = self.choices.iter().map(|c| c.candidate.clone()).collect();
        self.rebuild_pages();
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
        self.clear_composition();
        self.pending_learning = events;
        let mut update = self.snapshot(true);
        update.commit = Some(text);
        Ok(update)
    }

    fn refresh_candidates(&mut self) {
        self.preedit = self
            .confirmed
            .iter()
            .map(|s| s.text.as_str())
            .collect::<String>()
            + &self.raw;
        self.choices = decode(&self.lexicon, &self.raw);
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
        let mut proposals = self.lexicon.short_spelling_choices(&self.raw);
        if let Some(source) = self.choices.iter().find(|c| c.consumed == self.raw.len()) {
            proposals.extend(
                self.lexicon
                    .extra_syllable_choices(&self.raw, &source.candidate),
            );
        }
        proposals.extend(self.lexicon.composed_spelling_choices(&self.raw));
        let mut spelling = Vec::new();
        for choice in proposals {
            if self
                .choices
                .iter()
                .any(|c| c.consumed == self.raw.len() && c.candidate.text == choice.candidate.text)
                || spelling
                    .iter()
                    .any(|c: &super::decoder::Choice| c.candidate.text == choice.candidate.text)
            {
                continue;
            }
            self.choices
                .retain(|c| c.candidate.text != choice.candidate.text);
            spelling.push(choice);
            if spelling.len() == 4 {
                break;
            }
        }
        let insertion = usize::from(!self.choices.is_empty());
        self.choices.splice(insertion..insertion, spelling);
        self.candidates = self
            .choices
            .iter()
            .map(|choice| choice.candidate.clone())
            .collect();
        self.page = 0;
        self.highlighted = if self.candidates.is_empty() {
            None
        } else {
            Some(0)
        };
        self.page_ranges.clear();
        self.rebuild_pages();
    }

    fn ranking_candidate(choice: &Choice) -> RankingCandidate {
        RankingCandidate {
            pinyin: choice.candidate.comment.clone(),
            text: choice.candidate.text.clone(),
            consumed: choice.consumed,
        }
    }

    fn rebuild_pages(&mut self) {
        let absolute = self
            .page_ranges
            .get(self.page)
            .and_then(|range| self.highlighted.map(|index| range.start + index))
            .unwrap_or(0);
        self.page_ranges.clear();
        let limit = if self.candidate_width.is_some() {
            self.page_size.min(9)
        } else {
            self.page_size
        };
        let mut start = 0;
        while start < self.candidates.len() {
            let mut end = start;
            let mut used = 0u32;
            while end < self.candidates.len() && end - start < limit {
                let width = candidate_cell_width(&self.candidates[end].display_text(), end - start);
                if let Some(budget) = self.candidate_width {
                    if end > start && used.saturating_add(width) > budget {
                        break;
                    }
                }
                used = used.saturating_add(width);
                end += 1;
            }
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
        self.preedit.clear();
        self.raw.clear();
        self.raw_caret = 0;
        self.confirmed.clear();
        self.choices.clear();
        self.pending_learning.clear();
        self.pending_promotions.clear();
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
                self.confirmed
                    .iter()
                    .map(|segment| segment.text.encode_utf16().count())
                    .sum::<usize>()
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
